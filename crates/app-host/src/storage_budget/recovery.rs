use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use fs2::FileExt;
use mera_backend_worker_host::WorkerStorePaths;
use mera_config::{AppPaths, atomic_write};
use serde::{Deserialize, Serialize};

use super::accounting::dir_size_sync;
use crate::{AppHostError, AppHostResult, ModStorePaths};

pub(crate) const CLEANUP_JOURNAL_FILENAME: &str = "storage_cleanup_journal.json";

/// Extensible recovery hook slot (e.g. for future Priority 3 file history reclamation).
#[async_trait]
pub trait RecoveryHook: Send + Sync {
    /// Human-readable name for logging and diagnostics.
    fn name(&self) -> &'static str;

    /// Recovery priority tier (`3` for future file history).
    fn priority(&self) -> u8;

    /// Estimate bytes that could currently be reclaimed by this hook.
    async fn estimate_reclaimable_bytes(&self) -> u64;

    /// Reclaim up to `target_bytes`, returning the actual bytes freed.
    async fn reclaim(&self, target_bytes: u64) -> AppHostResult<u64>;
}

/// Overall state of a crash-safe cleanup journal run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupRunState {
    InProgress,
    Completed,
}

/// Per-entry status recorded in [`CleanupJournal`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupEntryStatus {
    Pending,
    Completed,
    SkippedActive,
    Failed { error: String },
}

/// Single planned or executed cache cleanup target in [`CleanupJournal`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupJournalEntry {
    pub path: PathBuf,
    pub priority: u8,
    pub kind: String,
    pub estimated_bytes: u64,
    pub freed_bytes: u64,
    pub deleted_files: usize,
    pub status: CleanupEntryStatus,
}

/// Crash-safe cleanup journal persisted atomically before deleting cache targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupJournal {
    pub schema_version: u32,
    pub run_id: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub reason: String,
    pub state: CleanupRunState,
    pub entries: Vec<CleanupJournalEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CacheCandidate {
    pub(crate) path: PathBuf,
    pub(crate) priority: u8,
    pub(crate) kind: &'static str,
    pub(crate) bytes: u64,
    pub(crate) modified_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StagingKind {
    Worker,
    Mod,
}

pub(crate) fn journal_path(app_paths: &AppPaths) -> PathBuf {
    app_paths.config_dir().join(CLEANUP_JOURNAL_FILENAME)
}

/// Check whether an advisory lock file is currently held by an active installer.
pub(crate) fn is_advisory_lock_held(lock_path: &Path) -> bool {
    if !lock_path.exists() {
        return false;
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
    else {
        return false;
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(_) => false,
    }
}

/// Collect reclaimable cache candidates ordered by `(priority, modified_secs, path)`.
///
/// Active worker/mod staging directories are skipped so in-flight installations
/// are never disturbed.
pub(crate) fn collect_cache_candidates_sync(
    app_paths: &AppPaths,
    worker_paths: &WorkerStorePaths,
    mod_paths: &ModStorePaths,
    active_staging: &HashSet<PathBuf>,
) -> Vec<CacheCandidate> {
    let mut candidates = Vec::new();

    // Priority 1: ephemeral tmp, library scratch, project scratches, abandoned staging.
    collect_children_candidates(&app_paths.tmp_dir(), 1, "tmp", &mut candidates);
    collect_children_candidates(
        &app_paths.library_scratch_dir(),
        1,
        "library_scratch",
        &mut candidates,
    );

    if let Ok(entries) = std::fs::read_dir(app_paths.projects_dir()) {
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() || !ft.is_dir() {
                continue;
            }
            let scratch = entry.path().join("scratch");
            collect_children_candidates(&scratch, 1, "project_scratch", &mut candidates);
        }
    }

    if !is_advisory_lock_held(&worker_paths.install_lock_path()) {
        collect_staging_candidates(
            &worker_paths.staging_base(),
            1,
            "worker_staging",
            active_staging,
            &mut candidates,
        );
    }

    if !is_advisory_lock_held(&mod_paths.lock_file()) {
        collect_staging_candidates(
            &mod_paths.staging_root(),
            1,
            "mod_staging",
            active_staging,
            &mut candidates,
        );
    }

    // Priority 2: thumbnail caches and mod uv-cache.
    collect_children_candidates(
        &app_paths.thumbnails_dir(),
        2,
        "thumbnail_cache",
        &mut candidates,
    );
    collect_children_candidates(
        &app_paths.base_path().join("cache").join("thumbnails"),
        2,
        "thumbnail_cache",
        &mut candidates,
    );
    collect_children_candidates(
        &app_paths.library_dir().join("thumbnails"),
        2,
        "thumbnail_cache",
        &mut candidates,
    );
    if let Ok(entries) = std::fs::read_dir(app_paths.projects_dir()) {
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() || !ft.is_dir() {
                continue;
            }
            let thumbs = entry.path().join("thumbnails");
            collect_children_candidates(&thumbs, 2, "thumbnail_cache", &mut candidates);
        }
    }
    collect_children_candidates(&mod_paths.uv_cache_dir(), 2, "uv_cache", &mut candidates);

    candidates.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.modified_secs.cmp(&b.modified_secs))
            .then_with(|| a.path.cmp(&b.path))
    });
    candidates
}

fn collect_children_candidates(
    root: &Path,
    priority: u8,
    kind: &'static str,
    out: &mut Vec<CacheCandidate>,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let bytes = if meta.file_type().is_symlink() {
            0
        } else if meta.is_dir() {
            dir_size_sync(&path)
        } else {
            meta.len()
        };
        let modified_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.push(CacheCandidate {
            path,
            priority,
            kind,
            bytes,
            modified_secs,
        });
    }
}

fn collect_staging_candidates(
    staging_root: &Path,
    priority: u8,
    kind: &'static str,
    active_staging: &HashSet<PathBuf>,
    out: &mut Vec<CacheCandidate>,
) {
    let Ok(entries) = std::fs::read_dir(staging_root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if active_staging.contains(&path) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let bytes = if meta.file_type().is_symlink() {
            0
        } else if meta.is_dir() {
            dir_size_sync(&path)
        } else {
            meta.len()
        };
        let modified_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.push(CacheCandidate {
            path,
            priority,
            kind,
            bytes,
            modified_secs,
        });
    }
}

/// Strictly verify that `candidate` lies inside one of the known reclaimable cache roots
/// without `..` traversal or symlinked parent components, so protected user data can never
/// be deleted even if a cleanup journal file was corrupted.
pub(crate) fn is_allowed_cache_path(
    candidate: &Path,
    app_paths: &AppPaths,
    worker_paths: &WorkerStorePaths,
    mod_paths: &ModStorePaths,
) -> bool {
    if candidate
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return false;
    }

    let fixed_roots = [
        app_paths.tmp_dir(),
        app_paths.library_scratch_dir(),
        app_paths.thumbnails_dir(),
        app_paths.base_path().join("cache").join("thumbnails"),
        app_paths.library_dir().join("thumbnails"),
        worker_paths.staging_base(),
        mod_paths.staging_root(),
        mod_paths.uv_cache_dir(),
    ];

    if fixed_roots
        .iter()
        .any(|root| is_strict_child(candidate, root))
    {
        return true;
    }

    // Check `projects/<single_id>/(scratch|thumbnails)/<child>`.
    let projects_dir = app_paths.projects_dir();
    if let Ok(rel) = candidate.strip_prefix(projects_dir) {
        let mut comps = rel.components();
        let Some(Component::Normal(_project_id)) = comps.next() else {
            return false;
        };
        let Some(Component::Normal(subdir)) = comps.next() else {
            return false;
        };
        if (subdir == "scratch" || subdir == "thumbnails") && comps.next().is_some() {
            return true;
        }
    }

    false
}

fn is_strict_child(candidate: &Path, root: &Path) -> bool {
    match candidate.strip_prefix(root) {
        Ok(rel) => rel.components().next().is_some(),
        Err(_) => false,
    }
}

pub(crate) fn staging_kind_for_path(
    candidate: &Path,
    worker_paths: &WorkerStorePaths,
    mod_paths: &ModStorePaths,
) -> Option<StagingKind> {
    if is_strict_child(candidate, &worker_paths.staging_base()) {
        Some(StagingKind::Worker)
    } else if is_strict_child(candidate, &mod_paths.staging_root()) {
        Some(StagingKind::Mod)
    } else {
        None
    }
}

pub(crate) async fn write_journal(path: &Path, journal: &CleanupJournal) -> AppHostResult<()> {
    let bytes = serde_json::to_vec_pretty(journal).map_err(|e| AppHostError::Io {
        path: path.to_path_buf(),
        message: format!("failed to serialize storage cleanup journal: {e}"),
    })?;
    atomic_write(path, bytes)
        .await
        .map_err(AppHostError::BootstrapConfig)
}

pub(crate) fn new_journal(
    reason: impl Into<String>,
    entries: Vec<CleanupJournalEntry>,
) -> CleanupJournal {
    static RUN_NONCE: AtomicU64 = AtomicU64::new(1);
    let nonce = RUN_NONCE.fetch_add(1, Ordering::Relaxed);
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    CleanupJournal {
        schema_version: 1,
        run_id: format!("cleanup-{nanos:x}-{nonce:x}"),
        started_at: chrono::Utc::now().to_rfc3339(),
        completed_at: None,
        reason: reason.into(),
        state: CleanupRunState::InProgress,
        entries,
    }
}

/// Delete a file, symlink, or directory tree using `symlink_metadata` so symlinks
/// are unlinked themselves and never followed into external targets.
pub(crate) async fn remove_cache_path(path: &Path) -> Result<(usize, u64), String> {
    let meta = match tokio::fs::symlink_metadata(path).await {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(err) => return Err(err.to_string()),
    };

    if meta.file_type().is_symlink() {
        return match tokio::fs::remove_file(path).await {
            Ok(()) => Ok((1, 0)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok((0, 0)),
            Err(err) => Err(err.to_string()),
        };
    }

    if meta.is_file() {
        let len = meta.len();
        return if crate::asset_store::remove_store_file(path).await {
            Ok((1, len))
        } else {
            Err(format!("failed to remove file `{}`", path.display()))
        };
    }

    if meta.is_dir() {
        let mut files = 0usize;
        let mut bytes = 0u64;
        let mut rd = tokio::fs::read_dir(path)
            .await
            .map_err(|err| err.to_string())?;
        while let Some(entry) = rd.next_entry().await.map_err(|err| err.to_string())? {
            let (child_files, child_bytes) = Box::pin(remove_cache_path(&entry.path())).await?;
            files = files.saturating_add(child_files);
            bytes = bytes.saturating_add(child_bytes);
        }
        if let Err(err) = tokio::fs::remove_dir(path).await
            && err.kind() != std::io::ErrorKind::NotFound
        {
            return Err(err.to_string());
        }
        return Ok((files, bytes));
    }

    Ok((0, 0))
}
