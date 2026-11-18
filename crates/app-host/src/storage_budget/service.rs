use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use mera_backend_worker_host::WorkerStorePaths;
use mera_config::{AppConfig, AppPaths, FIFTY_GIB_BYTES, StorageBudgetConfig};
use mera_core::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
    DiagnosticTargetDomain,
};
use mera_core::model::DiagnosticId;

use super::accounting::{
    AccountingState, ScanSnapshot, StorageCategory, dir_size_sync, scan_workspace_sync,
};
use super::recovery::{
    CacheCandidate, CleanupEntryStatus, CleanupJournal, CleanupJournalEntry, CleanupRunState,
    RecoveryHook, StagingKind, collect_cache_candidates_sync, is_advisory_lock_held,
    is_allowed_cache_path, journal_path, new_journal, remove_cache_path, staging_kind_for_path,
    write_journal,
};
use crate::dto::{
    ProjectStorageDto, StorageCategoriesDto, StorageCleanResultDto, StoragePreflightInputDto,
    StoragePreflightReason, StoragePreflightResultDto, StorageStatusDto, StorageWriteKind,
};
use crate::{AppHostError, AppHostResult, ModStorePaths};

/// Library writes at or below 10 MiB do not prompt for storage budget confirmation.
pub const PREFLIGHT_LIBRARY_THRESHOLD_BYTES: u64 = 10 * 1024 * 1024;

/// Physical disk capacity and available space snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSpaceInfo {
    pub available_bytes: u64,
    pub total_bytes: u64,
}

/// Abstraction for querying filesystem capacity and available space.
pub trait DiskSpaceProvider: Send + Sync + std::fmt::Debug {
    fn disk_space(&self, path: &Path) -> AppHostResult<DiskSpaceInfo>;
}

/// Default [`DiskSpaceProvider`] backed by `fs2::available_space` / `fs2::total_space`.
#[derive(Debug, Default)]
pub struct Fs2DiskSpaceProvider;

impl DiskSpaceProvider for Fs2DiskSpaceProvider {
    fn disk_space(&self, path: &Path) -> AppHostResult<DiskSpaceInfo> {
        let probe = existing_ancestor(path);
        let available_bytes = fs2::available_space(probe).map_err(|e| AppHostError::Io {
            path: probe.to_path_buf(),
            message: format!("failed to query available disk space: {e}"),
        })?;
        let total_bytes = fs2::total_space(probe).map_err(|e| AppHostError::Io {
            path: probe.to_path_buf(),
            message: format!("failed to query total disk space: {e}"),
        })?;
        Ok(DiskSpaceInfo {
            available_bytes,
            total_bytes,
        })
    }
}

fn existing_ancestor(path: &Path) -> &Path {
    path.ancestors()
        .find(|ancestor| ancestor.exists())
        .unwrap_or(path)
}

struct ScanGuard<'a> {
    flag: &'a AtomicBool,
}

impl<'a> ScanGuard<'a> {
    fn enter(flag: &'a AtomicBool) -> Self {
        flag.store(true, Ordering::SeqCst);
        Self { flag }
    }
}

impl Drop for ScanGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

/// Host-level service managing storage accounting, total budget enforcement,
/// 10% disk free floor checks, preflight write confirmation, and crash-safe cache recovery.
#[derive(Debug)]
pub struct StorageBudgetService {
    inner: Arc<StorageBudgetInner>,
}

#[derive(Debug)]
struct StorageBudgetInner {
    config: Arc<AppConfig>,
    worker_paths: WorkerStorePaths,
    mod_paths: ModStorePaths,
    disk_space: Arc<dyn DiskSpaceProvider>,
    accounting: RwLock<AccountingState>,
    active_staging: RwLock<HashSet<PathBuf>>,
    priority3_hooks: RwLock<Vec<Arc<dyn RecoveryHook>>>,
    scan_running: AtomicBool,
    recovery_lock: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for dyn RecoveryHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryHook")
            .field("name", &self.name())
            .field("priority", &self.priority())
            .finish()
    }
}

impl Clone for StorageBudgetService {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl StorageBudgetService {
    /// Construct a new service using the default [`Fs2DiskSpaceProvider`].
    pub fn new(
        config: Arc<AppConfig>,
        worker_paths: WorkerStorePaths,
        mod_paths: ModStorePaths,
    ) -> Self {
        Self::with_disk_space_provider(
            config,
            worker_paths,
            mod_paths,
            Arc::new(Fs2DiskSpaceProvider),
        )
    }

    /// Construct a new service with a custom [`DiskSpaceProvider`] (primarily for tests).
    pub fn with_disk_space_provider(
        config: Arc<AppConfig>,
        worker_paths: WorkerStorePaths,
        mod_paths: ModStorePaths,
        disk_space: Arc<dyn DiskSpaceProvider>,
    ) -> Self {
        Self {
            inner: Arc::new(StorageBudgetInner {
                config,
                worker_paths,
                mod_paths,
                disk_space,
                accounting: RwLock::new(AccountingState::default()),
                active_staging: RwLock::new(HashSet::new()),
                priority3_hooks: RwLock::new(Vec::new()),
                scan_running: AtomicBool::new(false),
                recovery_lock: tokio::sync::Mutex::new(()),
            }),
        }
    }

    pub fn app_paths(&self) -> &AppPaths {
        self.inner.config.paths()
    }

    pub fn worker_store_paths(&self) -> &WorkerStorePaths {
        &self.inner.worker_paths
    }

    pub fn mod_store_paths(&self) -> &ModStorePaths {
        &self.inner.mod_paths
    }

    /// Path to the crash-safe cleanup journal under `config_dir`.
    pub fn cleanup_journal_path(&self) -> PathBuf {
        journal_path(self.inner.config.paths())
    }

    /// Record an incremental byte addition in `category`.
    pub fn record_added(&self, category: StorageCategory, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let delta = i64::try_from(bytes).unwrap_or(i64::MAX);
        self.record_delta(category, delta);
    }

    /// Record an incremental byte removal in `category`.
    pub fn record_removed(&self, category: StorageCategory, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let delta = i64::try_from(bytes).map(|v| -v).unwrap_or(i64::MIN);
        self.record_delta(category, delta);
    }

    /// Apply a signed byte delta to `category`.
    pub fn record_delta(&self, category: StorageCategory, delta_bytes: i64) {
        if delta_bytes == 0 {
            return;
        }
        self.inner
            .accounting
            .write()
            .expect("storage accounting lock poisoned")
            .apply_delta(category, delta_bytes);
    }

    pub(crate) fn record_project_deleted(&self, project_id: &str) {
        self.inner
            .accounting
            .write()
            .expect("storage accounting lock poisoned")
            .remove_project(project_id);
    }

    /// Return the current incrementally tracked categories without scanning disk.
    pub fn cached_categories(&self) -> StorageCategoriesDto {
        self.inner
            .accounting
            .read()
            .expect("storage accounting lock poisoned")
            .categories
            .clone()
    }

    /// Return the current incrementally tracked per-project breakdown without scanning disk.
    pub fn cached_projects(&self) -> Vec<ProjectStorageDto> {
        self.inner
            .accounting
            .read()
            .expect("storage accounting lock poisoned")
            .projects
            .values()
            .cloned()
            .collect()
    }

    /// Mark a staging directory as actively in use so auto-recovery skips it.
    pub fn register_active_staging(&self, path: impl Into<PathBuf>) {
        self.inner
            .active_staging
            .write()
            .expect("active staging lock poisoned")
            .insert(path.into());
    }

    /// Unmark an active staging directory when promotion or cancellation completes.
    pub fn unregister_active_staging(&self, path: &Path) {
        self.inner
            .active_staging
            .write()
            .expect("active staging lock poisoned")
            .remove(path);
    }

    /// Register an extensible Priority 3 recovery hook (e.g. future file history pruning).
    pub fn register_recovery_hook(&self, hook: Arc<dyn RecoveryHook>) {
        self.inner
            .priority3_hooks
            .write()
            .expect("priority3 hooks lock poisoned")
            .push(hook);
    }

    /// Spawn a background calibration and recovery task if a Tokio runtime is active.
    pub fn spawn_startup_calibration(&self) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let weak = Arc::downgrade(&self.inner);
        handle.spawn(async move {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let svc = StorageBudgetService { inner };
            if let Err(err) = svc.startup_calibrate_and_recover().await {
                tracing::warn!(reason = %err, "background storage calibration failed");
            }
        });
    }

    /// Perform a full filesystem accounting scan and return the fresh category breakdown
    /// without modifying the cached incremental state.
    pub async fn full_scan(&self) -> AppHostResult<StorageCategoriesDto> {
        Ok(self.full_scan_snapshot().await?.categories)
    }

    async fn full_scan_snapshot(&self) -> AppHostResult<ScanSnapshot> {
        let _scan_guard = ScanGuard::enter(&self.inner.scan_running);
        let app_paths = self.inner.config.paths().clone();
        let worker_paths = self.inner.worker_paths.clone();
        let mod_paths = self.inner.mod_paths.clone();
        tokio::task::spawn_blocking(move || {
            scan_workspace_sync(&app_paths, &worker_paths, &mod_paths)
        })
        .await
        .map_err(|e| AppHostError::Io {
            path: self.inner.config.paths().base_path().to_path_buf(),
            message: format!("storage scan task panicked: {e}"),
        })
    }

    /// Run a full filesystem scan and replace the incremental accounting baseline.
    pub async fn calibrate(&self) -> AppHostResult<StorageCategoriesDto> {
        let _guard = self.inner.recovery_lock.lock().await;
        self.calibrate_locked().await
    }

    /// Run startup maintenance: resume any interrupted cleanup journal, calibrate,
    /// and execute automatic recovery if budget or disk floor is violated.
    pub async fn startup_calibrate_and_recover(&self) -> AppHostResult<StorageStatusDto> {
        let _guard = self.inner.recovery_lock.lock().await;
        self.resume_incomplete_journal_locked().await?;
        self.calibrate_locked().await?;
        let cfg = self.load_budget_config().await?;
        self.recover_if_needed_locked(&cfg, "startup_recovery")
            .await?;
        self.build_status_from_config(&cfg)
    }

    /// Return the current [`StorageStatusDto`].
    ///
    /// Performs an initial calibration scan if no calibration has run yet; otherwise
    /// uses the incrementally maintained breakdown and live disk space metrics.
    pub async fn status(&self) -> AppHostResult<StorageStatusDto> {
        let needs_initial_scan = self
            .inner
            .accounting
            .read()
            .expect("storage accounting lock poisoned")
            .last_calibrated_at
            .is_none();

        if needs_initial_scan {
            let _guard = self.inner.recovery_lock.lock().await;
            let still_uncalibrated = self
                .inner
                .accounting
                .read()
                .expect("storage accounting lock poisoned")
                .last_calibrated_at
                .is_none();
            if still_uncalibrated {
                self.calibrate_locked().await?;
            }
        }

        let cfg = self.load_budget_config().await?;
        self.build_status_from_config(&cfg)
    }

    /// Persist a new optional total storage budget (`None` for unlimited), trigger automatic
    /// cache recovery if the new limit or disk floor is exceeded, and return updated status.
    pub async fn set_budget(&self, limit_bytes: Option<u64>) -> AppHostResult<StorageStatusDto> {
        if !StorageBudgetConfig::is_valid_limit_bytes(limit_bytes) {
            return Err(AppHostError::Diagnostic(Box::new(storage_caller_error(
                "INVALID_LIMIT",
                "storage budget limit_bytes must be greater than 0 when specified",
            ))));
        }

        let _guard = self.inner.recovery_lock.lock().await;
        let handle = self.inner.config.config::<StorageBudgetConfig>()?;
        let (cfg, _) = handle
            .update(|doc| {
                doc.limit_bytes = limit_bytes;
            })
            .await?;

        if self
            .inner
            .accounting
            .read()
            .expect("storage accounting lock poisoned")
            .last_calibrated_at
            .is_none()
        {
            self.calibrate_locked().await?;
        }

        self.recover_if_needed_locked(&cfg, "budget_updated")
            .await?;
        self.build_status_from_config(&cfg)
    }

    /// Persist `nudge_dismissed = true` and return updated [`StorageStatusDto`].
    pub async fn dismiss_nudge(&self) -> AppHostResult<StorageStatusDto> {
        let handle = self.inner.config.config::<StorageBudgetConfig>()?;
        let (cfg, _) = handle
            .update(|doc| {
                doc.nudge_dismissed = true;
            })
            .await?;

        let needs_initial_scan = self
            .inner
            .accounting
            .read()
            .expect("storage accounting lock poisoned")
            .last_calibrated_at
            .is_none();
        if needs_initial_scan {
            let _guard = self.inner.recovery_lock.lock().await;
            self.calibrate_locked().await?;
        }

        self.build_status_from_config(&cfg)
    }

    /// Manually reclaim all Priority 1 and Priority 2 cache targets (skipping active staging),
    /// recording a crash-safe cleanup journal before deleting.
    pub async fn clean_cache(&self) -> AppHostResult<StorageCleanResultDto> {
        let _guard = self.inner.recovery_lock.lock().await;
        let candidates = self.collect_candidates().await?;
        let (freed_bytes, deleted_files) = self
            .execute_cleanup_batch(&candidates, "manual_clean_cache")
            .await?;
        self.calibrate_locked().await?;
        let cfg = self.load_budget_config().await?;
        let status = self.build_status_from_config(&cfg)?;
        Ok(StorageCleanResultDto {
            freed_bytes,
            deleted_files,
            status,
        })
    }

    /// Evaluate whether a planned write of `estimated_bytes` for `kind` requires user
    /// confirmation based on the configured budget and disk free floor.
    pub async fn preflight_write(
        &self,
        kind: StorageWriteKind,
        estimated_bytes: u64,
    ) -> AppHostResult<StoragePreflightResultDto> {
        self.preflight_write_with_paths(kind, estimated_bytes, &[])
            .await
    }

    /// Evaluate a [`StoragePreflightInputDto`] (summing any supplied local `paths` sizes).
    pub async fn preflight_write_input(
        &self,
        input: StoragePreflightInputDto,
    ) -> AppHostResult<StoragePreflightResultDto> {
        self.preflight_write_with_paths(input.kind, input.estimated_bytes, &input.paths)
            .await
    }

    /// Evaluate whether a planned write of `estimated_bytes` plus any local `paths` sizes
    /// for `kind` requires user confirmation.
    pub async fn preflight_write_with_paths(
        &self,
        kind: StorageWriteKind,
        estimated_bytes: u64,
        paths: &[PathBuf],
    ) -> AppHostResult<StoragePreflightResultDto> {
        let paths_bytes = if paths.is_empty() {
            0
        } else {
            let owned_paths = paths.to_vec();
            tokio::task::spawn_blocking(move || {
                owned_paths
                    .iter()
                    .fold(0u64, |acc, path| acc.saturating_add(dir_size_sync(path)))
            })
            .await
            .map_err(|e| AppHostError::Io {
                path: self.inner.config.paths().base_path().to_path_buf(),
                message: format!("preflight path size task panicked: {e}"),
            })?
        };
        let total_estimated_bytes = estimated_bytes.saturating_add(paths_bytes);

        let status = self.status().await?;
        let current_total = status.total_bytes;
        let projected_total = current_total.saturating_add(total_estimated_bytes);
        let limit_bytes = status.limit_bytes;

        let over_budget = limit_bytes.is_some_and(|limit| current_total > limit);
        let would_exceed_budget = limit_bytes.is_some_and(|limit| projected_total > limit);

        let disk_floor_bytes = status.disk_floor_bytes;
        let available_bytes = status.disk_available_bytes;
        let below_disk_floor = match (status.disk_total_bytes, available_bytes, disk_floor_bytes) {
            (Some(total), Some(avail), Some(floor)) => total > 0 && avail < floor,
            _ => false,
        };
        let would_drop_below_disk_floor =
            match (status.disk_total_bytes, available_bytes, disk_floor_bytes) {
                (Some(total), Some(avail), Some(floor)) => {
                    total > 0 && avail.saturating_sub(total_estimated_bytes) < floor
                }
                _ => false,
            };

        let pressure =
            over_budget || would_exceed_budget || below_disk_floor || would_drop_below_disk_floor;

        let requires_confirm = match kind {
            StorageWriteKind::Library => {
                total_estimated_bytes > PREFLIGHT_LIBRARY_THRESHOLD_BYTES && pressure
            }
            StorageWriteKind::Model | StorageWriteKind::Worker | StorageWriteKind::Mod => pressure,
        };

        let reason = if !requires_confirm {
            None
        } else if over_budget {
            Some(StoragePreflightReason::OverBudget)
        } else if would_exceed_budget {
            Some(StoragePreflightReason::WouldExceedBudget)
        } else {
            Some(StoragePreflightReason::BelowDiskFloor)
        };

        Ok(StoragePreflightResultDto {
            requires_confirm,
            reason,
            projected_total_bytes: projected_total,
            limit_bytes,
            available_bytes,
            estimated_bytes: total_estimated_bytes,
            current_total_bytes: current_total,
            disk_floor_bytes,
            over_budget,
            would_exceed_budget,
            below_disk_floor,
            would_drop_below_disk_floor,
        })
    }

    /// Run automatic budget/disk-floor recovery if needed.
    pub async fn recover_if_needed(&self) -> AppHostResult<u64> {
        let _guard = self.inner.recovery_lock.lock().await;
        let cfg = self.load_budget_config().await?;
        self.recover_if_needed_locked(&cfg, "auto_recovery").await
    }

    async fn calibrate_locked(&self) -> AppHostResult<StorageCategoriesDto> {
        let snapshot = self.full_scan_snapshot().await?;
        let categories = snapshot.categories.clone();
        let now = chrono::Utc::now().to_rfc3339();
        self.inner
            .accounting
            .write()
            .expect("storage accounting lock poisoned")
            .set_calibrated(snapshot, now);
        Ok(categories)
    }

    async fn load_budget_config(&self) -> AppHostResult<StorageBudgetConfig> {
        let handle = self.inner.config.config::<StorageBudgetConfig>()?;
        let (cfg, _) = handle.load().await?;
        Ok(cfg)
    }

    fn query_disk_space_opt(&self) -> Option<DiskSpaceInfo> {
        self.inner
            .disk_space
            .disk_space(self.inner.config.paths().base_path())
            .ok()
    }

    fn build_status_from_config(
        &self,
        cfg: &StorageBudgetConfig,
    ) -> AppHostResult<StorageStatusDto> {
        let (categories, projects, last_calibrated_at) = {
            let guard = self
                .inner
                .accounting
                .read()
                .expect("storage accounting lock poisoned");
            (
                guard.categories.clone(),
                guard.projects.values().cloned().collect(),
                guard.last_calibrated_at.clone(),
            )
        };
        let total_bytes = categories.total_bytes();
        let limit_bytes = cfg.effective_limit_bytes();
        let disk_floor_ratio = cfg.effective_disk_floor_ratio();

        let disk = self.query_disk_space_opt();
        let disk_total_bytes = disk.map(|d| d.total_bytes);
        let disk_available_bytes = disk.map(|d| d.available_bytes);
        let disk_floor_bytes =
            disk_total_bytes.map(|total| compute_disk_floor_bytes(total, disk_floor_ratio));

        let over_budget = limit_bytes.is_some_and(|limit| total_bytes > limit);
        let below_disk_floor = match (disk_total_bytes, disk_available_bytes, disk_floor_bytes) {
            (Some(total), Some(avail), Some(floor)) => total > 0 && avail < floor,
            _ => false,
        };
        let show_fifty_gb_nudge =
            limit_bytes.is_none() && !cfg.nudge_dismissed && total_bytes > FIFTY_GIB_BYTES;

        Ok(StorageStatusDto {
            total_bytes,
            limit_bytes,
            disk_total_bytes,
            disk_available_bytes,
            disk_floor_ratio,
            disk_floor_bytes,
            over_budget,
            below_disk_floor,
            show_fifty_gb_nudge,
            nudge_dismissed: cfg.nudge_dismissed,
            scan_running: self.inner.scan_running.load(Ordering::SeqCst),
            last_calibrated_at,
            categories,
            projects,
        })
    }

    async fn collect_candidates(&self) -> AppHostResult<Vec<CacheCandidate>> {
        let app_paths = self.inner.config.paths().clone();
        let worker_paths = self.inner.worker_paths.clone();
        let mod_paths = self.inner.mod_paths.clone();
        let active_staging = self
            .inner
            .active_staging
            .read()
            .expect("active staging lock poisoned")
            .clone();
        tokio::task::spawn_blocking(move || {
            collect_cache_candidates_sync(&app_paths, &worker_paths, &mod_paths, &active_staging)
        })
        .await
        .map_err(|e| AppHostError::Io {
            path: self.inner.config.paths().base_path().to_path_buf(),
            message: format!("cache candidate scan panicked: {e}"),
        })
    }

    async fn recover_if_needed_locked(
        &self,
        cfg: &StorageBudgetConfig,
        reason: &str,
    ) -> AppHostResult<u64> {
        let limit_bytes = cfg.effective_limit_bytes();
        let floor_ratio = cfg.effective_disk_floor_ratio();
        let disk = self.query_disk_space_opt();
        let disk_total_bytes = disk.map_or(0, |d| d.total_bytes);
        let disk_available_bytes = disk.map_or(0, |d| d.available_bytes);
        let disk_floor_bytes = compute_disk_floor_bytes(disk_total_bytes, floor_ratio);

        let mut current_total = self.cached_categories().total_bytes();
        let limit_exceeded = limit_bytes.is_some_and(|limit| current_total > limit);
        let floor_violated = disk_total_bytes > 0 && disk_available_bytes < disk_floor_bytes;

        if !limit_exceeded && !floor_violated {
            return Ok(0);
        }

        let target_total = limit_bytes.map(recovery_target_bytes);
        let mut simulated_available = disk_available_bytes;
        let candidates = self.collect_candidates().await?;

        let mut selected = Vec::new();
        for candidate in candidates {
            let need_budget =
                limit_exceeded && target_total.is_some_and(|target| current_total > target);
            let need_floor = floor_violated && simulated_available < disk_floor_bytes;
            if !need_budget && !need_floor {
                break;
            }
            current_total = current_total.saturating_sub(candidate.bytes);
            simulated_available = simulated_available.saturating_add(candidate.bytes);
            selected.push(candidate);
        }

        let (mut total_freed, _) = if selected.is_empty() {
            (0, 0)
        } else {
            self.execute_cleanup_batch(&selected, reason).await?
        };

        if total_freed > 0 {
            self.calibrate_locked().await?;
        }

        // Invoke Priority 3 extensible hooks only if Priority 1 & 2 were insufficient.
        let post_disk = self.query_disk_space_opt();
        let post_available = post_disk.map_or(0, |d| d.available_bytes);
        let post_total = self.cached_categories().total_bytes();
        let still_over_target =
            limit_exceeded && target_total.is_some_and(|target| post_total > target);
        let still_below_floor = floor_violated && post_available < disk_floor_bytes;

        if still_over_target || still_below_floor {
            let hooks = self
                .inner
                .priority3_hooks
                .read()
                .expect("priority3 hooks lock poisoned")
                .clone();

            let mut remaining_needed = 0u64;
            if still_over_target && let Some(target) = target_total {
                remaining_needed = remaining_needed.max(post_total.saturating_sub(target));
            }
            if still_below_floor {
                remaining_needed =
                    remaining_needed.max(disk_floor_bytes.saturating_sub(post_available));
            }

            for hook in hooks {
                if remaining_needed == 0 {
                    break;
                }
                let freed = hook.reclaim(remaining_needed).await?;
                total_freed = total_freed.saturating_add(freed);
                remaining_needed = remaining_needed.saturating_sub(freed);
            }
            if total_freed > 0 {
                self.calibrate_locked().await?;
            }
        }

        Ok(total_freed)
    }

    async fn execute_cleanup_batch(
        &self,
        candidates: &[CacheCandidate],
        reason: &str,
    ) -> AppHostResult<(u64, usize)> {
        if candidates.is_empty() {
            return Ok((0, 0));
        }

        let entries: Vec<CleanupJournalEntry> = candidates
            .iter()
            .map(|c| CleanupJournalEntry {
                path: c.path.clone(),
                priority: c.priority,
                kind: c.kind.to_owned(),
                estimated_bytes: c.bytes,
                freed_bytes: 0,
                deleted_files: 0,
                status: CleanupEntryStatus::Pending,
            })
            .collect();

        let mut journal = new_journal(reason, entries);
        let j_path = self.cleanup_journal_path();
        write_journal(&j_path, &journal).await?;

        let (total_freed, total_deleted) = self.apply_journal_entries(&mut journal).await;

        journal.state = CleanupRunState::Completed;
        journal.completed_at = Some(chrono::Utc::now().to_rfc3339());
        write_journal(&j_path, &journal).await?;

        self.record_removed(StorageCategory::Cache, total_freed);
        Ok((total_freed, total_deleted))
    }

    async fn resume_incomplete_journal_locked(&self) -> AppHostResult<()> {
        let j_path = self.cleanup_journal_path();
        let bytes = match tokio::fs::read(&j_path).await {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(AppHostError::Io {
                    path: j_path,
                    message: format!("failed to read storage cleanup journal: {err}"),
                });
            }
        };

        let Ok(mut journal) = serde_json::from_slice::<CleanupJournal>(&bytes) else {
            return Ok(());
        };
        if journal.state != CleanupRunState::InProgress {
            return Ok(());
        }

        let (freed, _) = self.apply_journal_entries(&mut journal).await;
        journal.state = CleanupRunState::Completed;
        journal.completed_at = Some(chrono::Utc::now().to_rfc3339());
        write_journal(&j_path, &journal).await?;
        self.record_removed(StorageCategory::Cache, freed);
        Ok(())
    }

    async fn apply_journal_entries(&self, journal: &mut CleanupJournal) -> (u64, usize) {
        let app_paths = self.inner.config.paths();
        let worker_paths = &self.inner.worker_paths;
        let mod_paths = &self.inner.mod_paths;

        let mut total_freed = 0u64;
        let mut total_deleted = 0usize;

        for entry in &mut journal.entries {
            if entry.status != CleanupEntryStatus::Pending {
                continue;
            }
            if !is_allowed_cache_path(&entry.path, app_paths, worker_paths, mod_paths) {
                entry.status = CleanupEntryStatus::Failed {
                    error: "refused to delete non-cache path".to_owned(),
                };
                continue;
            }
            if self.is_staging_entry_active(&entry.path) {
                entry.status = CleanupEntryStatus::SkippedActive;
                continue;
            }
            match remove_cache_path(&entry.path).await {
                Ok((files, bytes)) => {
                    entry.deleted_files = files;
                    entry.freed_bytes = bytes;
                    entry.status = CleanupEntryStatus::Completed;
                    total_deleted = total_deleted.saturating_add(files);
                    total_freed = total_freed.saturating_add(bytes);
                }
                Err(error) => {
                    entry.status = CleanupEntryStatus::Failed { error };
                }
            }
        }

        (total_freed, total_deleted)
    }

    fn is_staging_entry_active(&self, path: &Path) -> bool {
        let in_active_set = self
            .inner
            .active_staging
            .read()
            .expect("active staging lock poisoned")
            .contains(path);
        if in_active_set {
            return true;
        }
        match staging_kind_for_path(path, &self.inner.worker_paths, &self.inner.mod_paths) {
            Some(StagingKind::Worker) => {
                is_advisory_lock_held(&self.inner.worker_paths.install_lock_path())
            }
            Some(StagingKind::Mod) => is_advisory_lock_held(&self.inner.mod_paths.lock_file()),
            None => false,
        }
    }
}

fn compute_disk_floor_bytes(disk_total_bytes: u64, ratio: f64) -> u64 {
    if disk_total_bytes == 0 {
        return 0;
    }
    let raw = (disk_total_bytes as f64) * ratio;
    if !raw.is_finite() || raw <= 0.0 {
        0
    } else if raw >= u64::MAX as f64 {
        u64::MAX
    } else {
        raw.round() as u64
    }
}

fn recovery_target_bytes(limit_bytes: u64) -> u64 {
    ((limit_bytes as u128 * 90) / 100) as u64
}

fn storage_caller_error(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(
        DiagnosticId::new(format!("storage_budget:{code}")),
        DiagnosticCode::new(format!("STORAGE_BUDGET/CALLER_{code}")),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("storage_budget"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new("storage_budget")),
    )
}
