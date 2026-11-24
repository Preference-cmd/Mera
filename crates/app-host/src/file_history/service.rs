use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use mera_config::AppPaths;
use mera_core::command::CommandActor;
use mera_core::event::Timestamp;
use mera_core::model::{FileSnapshotId, ProjectId, WorkspaceFolderId};
use mera_core::task::CancelToken;
use tokio::sync::Mutex as AsyncMutex;

use super::recovery::{FileHistoryRecoveryHook, clean_project_history_sync};
use super::restore::restore_snapshot_sync;
use super::scanner::{scan_project_sync, snapshot_single_file_sync};
use super::store::{
    append_index_lines_sync, read_ignore_rules_sync, read_index_records_sync,
    write_ignore_rules_sync,
};
use super::types::{
    DEFAULT_MAX_FILE_BYTES, FileHistoryCleanOutcome, FileHistoryScanStatus, FileSnapshotRecord,
    IndexLine, RestoreOutcome, ScanOptions, ScanPhase, ScanSummary, SnapshotSource,
    SystemTrashMover, TrashMover, file_history_caller_error, normalize_rel_path,
};
use crate::project_service::ProjectService;
use crate::storage_budget::{RecoveryHook, StorageBudgetService};
use crate::{AppHostError, AppHostResult};

struct ProjectScanSlot {
    status: Arc<Mutex<FileHistoryScanStatus>>,
    active_cancel: Mutex<Option<CancelToken>>,
}

impl ProjectScanSlot {
    fn new(project_id: ProjectId) -> Self {
        Self {
            status: Arc::new(Mutex::new(FileHistoryScanStatus::idle(project_id))),
            active_cancel: Mutex::new(None),
        }
    }
}

/// Host-side service managing per-project workspace file history, incremental
/// scanning, single-file restore, ignore rules, and protected reclamation.
pub struct FileHistoryService {
    paths: AppPaths,
    project_service: Arc<ProjectService>,
    max_file_bytes: AtomicU64,
    trash_mover: RwLock<Arc<dyn TrashMover>>,
    storage_budget: RwLock<Option<Arc<StorageBudgetService>>>,
    project_locks: RwLock<HashMap<ProjectId, Arc<AsyncMutex<()>>>>,
    scan_slots: RwLock<HashMap<ProjectId, Arc<ProjectScanSlot>>>,
}

impl std::fmt::Debug for FileHistoryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileHistoryService")
            .field("paths", &self.paths)
            .field(
                "max_file_bytes",
                &self.max_file_bytes.load(Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

impl FileHistoryService {
    pub fn new(paths: AppPaths, project_service: Arc<ProjectService>) -> Self {
        Self {
            paths,
            project_service,
            max_file_bytes: AtomicU64::new(DEFAULT_MAX_FILE_BYTES),
            trash_mover: RwLock::new(Arc::new(SystemTrashMover)),
            storage_budget: RwLock::new(None),
            project_locks: RwLock::new(HashMap::new()),
            scan_slots: RwLock::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn with_max_file_bytes(self, max_file_bytes: u64) -> Self {
        self.set_max_file_bytes(max_file_bytes);
        self
    }

    pub fn set_max_file_bytes(&self, max_file_bytes: u64) {
        self.max_file_bytes.store(max_file_bytes, Ordering::Relaxed);
    }

    #[must_use]
    pub fn with_trash_mover(self, trash_mover: Arc<dyn TrashMover>) -> Self {
        self.set_trash_mover(trash_mover);
        self
    }

    pub fn set_trash_mover(&self, trash_mover: Arc<dyn TrashMover>) {
        *self
            .trash_mover
            .write()
            .expect("file history trash mover lock poisoned") = trash_mover;
    }

    pub fn set_storage_budget(&self, storage_budget: Arc<StorageBudgetService>) {
        *self
            .storage_budget
            .write()
            .expect("file history storage_budget slot poisoned") = Some(storage_budget);
    }

    pub fn recovery_hook(self: &Arc<Self>) -> Arc<dyn RecoveryHook> {
        Arc::new(FileHistoryRecoveryHook::new(Arc::clone(self)))
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub(crate) fn project_lock(&self, project_id: &ProjectId) -> Arc<AsyncMutex<()>> {
        if let Some(lock) = self
            .project_locks
            .read()
            .expect("file history project locks poisoned")
            .get(project_id)
            .cloned()
        {
            return lock;
        }
        let mut guard = self
            .project_locks
            .write()
            .expect("file history project locks poisoned");
        guard
            .entry(project_id.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    fn scan_slot(&self, project_id: &ProjectId) -> Arc<ProjectScanSlot> {
        if let Some(slot) = self
            .scan_slots
            .read()
            .expect("file history scan slots poisoned")
            .get(project_id)
            .cloned()
        {
            return slot;
        }
        let mut guard = self
            .scan_slots
            .write()
            .expect("file history scan slots poisoned");
        guard
            .entry(project_id.clone())
            .or_insert_with(|| Arc::new(ProjectScanSlot::new(project_id.clone())))
            .clone()
    }

    fn history_dir(&self, project_id: &ProjectId) -> std::path::PathBuf {
        self.paths.project_history_dir(project_id.as_str())
    }

    fn storage_budget_opt(&self) -> Option<Arc<StorageBudgetService>> {
        self.storage_budget
            .read()
            .expect("file history storage_budget slot poisoned")
            .as_ref()
            .cloned()
    }

    async fn is_under_pressure(&self) -> bool {
        match self.storage_budget_opt() {
            Some(budget) => budget.is_under_pressure().await,
            None => false,
        }
    }

    pub(crate) fn record_history_added(&self, project_id: &ProjectId, bytes: u64) {
        if bytes > 0
            && let Some(budget) = self.storage_budget_opt()
        {
            budget.record_project_history_added(project_id.as_str(), bytes);
        }
    }

    pub(crate) fn record_history_removed(&self, project_id: &ProjectId, bytes: u64) {
        if bytes > 0
            && let Some(budget) = self.storage_budget_opt()
        {
            budget.record_project_history_removed(project_id.as_str(), bytes);
        }
    }

    /// Scan every workspace folder of `project_id` and record changed/new/deleted files.
    pub async fn scan_project(
        &self,
        project_id: &ProjectId,
        options: ScanOptions,
    ) -> AppHostResult<ScanSummary> {
        let project = self.project_service.load_project(project_id).await?;
        let folders = project.folders().to_vec();
        let slot = self.scan_slot(project_id);
        let started_at = jiff::Timestamp::now().to_string();

        {
            let mut active = slot
                .active_cancel
                .lock()
                .expect("scan active_cancel lock poisoned");
            *active = Some(options.cancel.clone());
        }
        {
            let mut status = slot.status.lock().expect("scan status lock poisoned");
            *status = FileHistoryScanStatus {
                project_id: project_id.clone(),
                phase: ScanPhase::Scanning,
                ready: false,
                visited_files: 0,
                hashed_files: 0,
                recorded_snapshots: 0,
                skipped_ignored: 0,
                skipped_oversized: 0,
                skipped_over_budget: 0,
                deleted_files: 0,
                cancelled: false,
                error: None,
                started_at: Some(started_at.clone()),
                finished_at: None,
            };
        }

        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;
        let under_pressure = self.is_under_pressure().await;
        let max_file_bytes = self.max_file_bytes.load(Ordering::Relaxed);
        let history_dir = self.history_dir(project_id);
        let live_status = Arc::clone(&slot.status);
        let options_for_task = options.clone();

        let join_res = tokio::task::spawn_blocking(move || {
            scan_project_sync(
                &history_dir,
                &folders,
                max_file_bytes,
                under_pressure,
                &options_for_task,
                Some(&live_status),
            )
        })
        .await;

        {
            let mut active = slot
                .active_cancel
                .lock()
                .expect("scan active_cancel lock poisoned");
            *active = None;
        }

        let finished_at = jiff::Timestamp::now().to_string();
        match join_res {
            Ok(Ok(summary)) => {
                self.record_history_added(project_id, summary.added_blob_bytes);
                let mut status = slot.status.lock().expect("scan status lock poisoned");
                status.phase = if summary.cancelled {
                    ScanPhase::Cancelled
                } else {
                    ScanPhase::Ready
                };
                status.ready = !summary.cancelled;
                status.visited_files = summary.visited_files;
                status.hashed_files = summary.hashed_files;
                status.recorded_snapshots = summary.recorded_snapshots;
                status.skipped_ignored = summary.skipped_ignored;
                status.skipped_oversized = summary.skipped_oversized;
                status.skipped_over_budget = summary.skipped_over_budget;
                status.deleted_files = summary.deleted_files;
                status.cancelled = summary.cancelled;
                status.finished_at = Some(finished_at);
                Ok(summary)
            }
            Ok(Err(err)) => {
                let mut status = slot.status.lock().expect("scan status lock poisoned");
                status.phase = ScanPhase::Failed;
                status.ready = false;
                status.error = Some(err.to_string());
                status.finished_at = Some(finished_at);
                Err(err)
            }
            Err(join_err) => {
                let err = AppHostError::Io {
                    path: self.history_dir(project_id),
                    message: format!("file history scan task panicked: {join_err}"),
                };
                let mut status = slot.status.lock().expect("scan status lock poisoned");
                status.phase = ScanPhase::Failed;
                status.ready = false;
                status.error = Some(err.to_string());
                status.finished_at = Some(finished_at);
                Err(err)
            }
        }
    }

    /// Start an asynchronous background scan for `project_id` and return immediately
    /// with the live [`FileHistoryScanStatus`].
    pub async fn start_background_scan(
        self: &Arc<Self>,
        project_id: &ProjectId,
        options: ScanOptions,
    ) -> AppHostResult<FileHistoryScanStatus> {
        self.project_service.load_project(project_id).await?;
        let slot = self.scan_slot(project_id);

        {
            let active = slot
                .active_cancel
                .lock()
                .expect("scan active_cancel lock poisoned");
            if active.is_some() {
                let status = slot.status.lock().expect("scan status lock poisoned");
                return Ok(status.clone());
            }
        }

        let started_at = jiff::Timestamp::now().to_string();
        {
            let mut active = slot
                .active_cancel
                .lock()
                .expect("scan active_cancel lock poisoned");
            *active = Some(options.cancel.clone());
        }
        let initial_status = {
            let mut status = slot.status.lock().expect("scan status lock poisoned");
            *status = FileHistoryScanStatus {
                project_id: project_id.clone(),
                phase: ScanPhase::Scanning,
                ready: false,
                visited_files: 0,
                hashed_files: 0,
                recorded_snapshots: 0,
                skipped_ignored: 0,
                skipped_oversized: 0,
                skipped_over_budget: 0,
                deleted_files: 0,
                cancelled: false,
                error: None,
                started_at: Some(started_at),
                finished_at: None,
            };
            status.clone()
        };

        let svc = Arc::clone(self);
        let pid = project_id.clone();
        tokio::spawn(async move {
            let _ = svc.scan_project(&pid, options).await;
        });

        Ok(initial_status)
    }

    /// Return the latest [`FileHistoryScanStatus`] for `project_id`.
    pub fn scan_status(&self, project_id: &ProjectId) -> FileHistoryScanStatus {
        let slot = self.scan_slot(project_id);
        let guard = slot.status.lock().expect("scan status lock poisoned");
        guard.clone()
    }

    /// Cancel an active scan for `project_id`, returning whether an active scan was signalled.
    pub fn cancel_scan(&self, project_id: &ProjectId) -> bool {
        let slot = self.scan_slot(project_id);
        let guard = slot
            .active_cancel
            .lock()
            .expect("scan active_cancel lock poisoned");
        if let Some(token) = guard.as_ref() {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Capture a single-file snapshot (manual, ACP write, etc.).
    #[allow(clippy::too_many_arguments)]
    pub async fn snapshot_file(
        &self,
        project_id: &ProjectId,
        folder_id: &WorkspaceFolderId,
        rel_path: &str,
        source: SnapshotSource,
        turn_id: Option<String>,
        actor: Option<CommandActor>,
        note: Option<String>,
        pinned: bool,
    ) -> AppHostResult<Option<FileSnapshotRecord>> {
        let project = self.project_service.load_project(project_id).await?;
        let folder = project
            .folders()
            .iter()
            .find(|f| f.id() == folder_id)
            .cloned()
            .ok_or_else(|| {
                file_history_caller_error(
                    "UNKNOWN_FOLDER",
                    format!("unknown workspace folder `{folder_id}` in project `{project_id}`"),
                )
            })?;

        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;
        let under_pressure = self.is_under_pressure().await;
        let max_file_bytes = self.max_file_bytes.load(Ordering::Relaxed);
        let history_dir = self.history_dir(project_id);
        let raw_rel = rel_path.to_owned();
        let options = ScanOptions {
            source,
            turn_id,
            actor,
            note,
            pinned,
            cancel: CancelToken::new(),
        };

        let (record, added_bytes) = tokio::task::spawn_blocking(move || {
            snapshot_single_file_sync(
                &history_dir,
                &folder,
                &raw_rel,
                max_file_bytes,
                under_pressure,
                &options,
            )
        })
        .await
        .map_err(|err| AppHostError::Io {
            path: self.history_dir(project_id),
            message: format!("snapshot task panicked: {err}"),
        })??;

        self.record_history_added(project_id, added_bytes);
        Ok(record)
    }

    /// List snapshot records for `project_id`, optionally filtered by `folder_id` and `rel_path`,
    /// ordered newest-first.
    pub async fn list_history(
        &self,
        project_id: &ProjectId,
        folder_id: Option<&WorkspaceFolderId>,
        rel_path: Option<&str>,
    ) -> AppHostResult<Vec<FileSnapshotRecord>> {
        self.project_service.load_project(project_id).await?;
        let normalized_rel = match rel_path {
            Some(raw) => Some(normalize_rel_path(raw)?),
            None => None,
        };
        let folder_filter = folder_id.cloned();
        let history_dir = self.history_dir(project_id);

        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;

        tokio::task::spawn_blocking(move || {
            let mut records = read_index_records_sync(&history_dir)?;
            records.retain(|rec| {
                let folder_ok = folder_filter
                    .as_ref()
                    .is_none_or(|fid| &rec.folder_id == fid);
                let path_ok = normalized_rel.as_ref().is_none_or(|rp| &rec.rel_path == rp);
                folder_ok && path_ok
            });
            records.reverse();
            Ok(records)
        })
        .await
        .map_err(|err| AppHostError::Io {
            path: self.history_dir(project_id),
            message: format!("list history task panicked: {err}"),
        })?
    }

    /// Toggle `pinned` status on an existing snapshot record by appending a `SetPin` event.
    pub async fn set_pinned(
        &self,
        project_id: &ProjectId,
        snapshot_id: &FileSnapshotId,
        pinned: bool,
    ) -> AppHostResult<FileSnapshotRecord> {
        self.project_service.load_project(project_id).await?;
        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;
        let history_dir = self.history_dir(project_id);
        let target_id = snapshot_id.clone();

        tokio::task::spawn_blocking(move || {
            let records = read_index_records_sync(&history_dir)?;
            let mut found = records
                .into_iter()
                .find(|r| r.id == target_id)
                .ok_or_else(|| {
                    file_history_caller_error(
                        "SNAPSHOT_NOT_FOUND",
                        format!("unknown file snapshot `{target_id}`"),
                    )
                })?;

            let updated_at = Timestamp::new(jiff::Timestamp::now().to_string());
            append_index_lines_sync(
                &history_dir,
                &[IndexLine::SetPin {
                    id: target_id,
                    pinned,
                    updated_at,
                }],
            )?;
            found.pinned = pinned;
            Ok(found)
        })
        .await
        .map_err(|err| AppHostError::Io {
            path: self.history_dir(project_id),
            message: format!("set_pinned task panicked: {err}"),
        })?
    }

    /// Restore a single snapshot back into its workspace folder, capturing a `PreRestore`
    /// snapshot of the current on-disk state first so the restore itself can be undone.
    pub async fn restore_file(
        &self,
        project_id: &ProjectId,
        snapshot_id: &FileSnapshotId,
        actor: Option<CommandActor>,
    ) -> AppHostResult<RestoreOutcome> {
        let project = self.project_service.load_project(project_id).await?;
        let folders = project.folders().to_vec();
        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;

        let history_dir = self.history_dir(project_id);
        let snap_id = snapshot_id.clone();
        let max_file_bytes = self.max_file_bytes.load(Ordering::Relaxed);
        let trash_mover = self
            .trash_mover
            .read()
            .expect("file history trash mover lock poisoned")
            .clone();

        let (outcome, added_bytes) = tokio::task::spawn_blocking(move || {
            restore_snapshot_sync(
                &history_dir,
                &folders,
                &snap_id,
                max_file_bytes,
                actor,
                trash_mover.as_ref(),
            )
        })
        .await
        .map_err(|err| AppHostError::Io {
            path: self.history_dir(project_id),
            message: format!("restore file task panicked: {err}"),
        })??;

        self.record_history_added(project_id, added_bytes);
        Ok(outcome)
    }

    /// Read the per-project `ignore.txt` rules.
    pub async fn get_ignore_rules(&self, project_id: &ProjectId) -> AppHostResult<String> {
        self.project_service.load_project(project_id).await?;
        let history_dir = self.history_dir(project_id);
        tokio::task::spawn_blocking(move || read_ignore_rules_sync(&history_dir))
            .await
            .map_err(|err| AppHostError::Io {
                path: self.history_dir(project_id),
                message: format!("read ignore rules task panicked: {err}"),
            })?
    }

    /// Validate and save the per-project `ignore.txt` rules.
    pub async fn set_ignore_rules(&self, project_id: &ProjectId, rules: &str) -> AppHostResult<()> {
        self.project_service.load_project(project_id).await?;
        let history_dir = self.history_dir(project_id);
        let rules_owned = rules.to_owned();
        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;
        tokio::task::spawn_blocking(move || write_ignore_rules_sync(&history_dir, &rules_owned))
            .await
            .map_err(|err| AppHostError::Io {
                path: self.history_dir(project_id),
                message: format!("write ignore rules task panicked: {err}"),
            })?
    }

    /// Remove all unprotected history records for `project_id`, compact `index.jsonl`,
    /// collect orphan blobs, and update storage accounting.
    pub async fn clean_project_history(
        &self,
        project_id: &ProjectId,
    ) -> AppHostResult<FileHistoryCleanOutcome> {
        self.project_service.load_project(project_id).await?;
        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;
        let pid = project_id.clone();
        let history_dir = self.history_dir(project_id);

        let outcome =
            tokio::task::spawn_blocking(move || clean_project_history_sync(&pid, &history_dir))
                .await
                .map_err(|err| AppHostError::Io {
                    path: self.history_dir(project_id),
                    message: format!("clean project history task panicked: {err}"),
                })??;

        self.record_history_removed(project_id, outcome.freed_bytes);
        Ok(outcome)
    }
}
