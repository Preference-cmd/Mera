//! Global asset library service.
//!
//! Manages the shared application-wide asset library stored at `<base>/library/`.
//! Contains its own `AssetStore`, `StepLog`, and a lazily loaded `DocumentSession<LibraryIndexDocument>`
//! persisted to `<base>/library/index.json`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use mera_config::AppPaths;
use mera_core::command::{CommandBatch, CommandStatus, OperationContext};
use mera_core::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
    DiagnosticTargetDomain,
};
use mera_core::document::DocumentSession;
use mera_core::event::{Event, EventBus, Timestamp};
use mera_core::library::{LibraryCommand, LibraryEntry, LibraryIndexDocument, LibrarySource};
use mera_core::model::{AssetId, CommandBatchId, DiagnosticId, ProjectId, StepId};
use mera_core::step::{StepOutcome, StepRecord};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;

use crate::asset_store::AssetStore;
use crate::document_store::{read_document, write_document};
use crate::error::AppHostResult;
use crate::step_log::StepLog;
use crate::storage_budget::{StorageBudgetService, StorageCategory};

pub fn library_caller_error(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(
        DiagnosticId::new(format!("library:{code}")),
        DiagnosticCode::new(format!("LIBRARY/CALLER_{code}")),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("library"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new("library")),
    )
}

pub fn library_internal_error(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(
        DiagnosticId::new(format!("library:{code}")),
        DiagnosticCode::new(format!("LIBRARY/INTERNAL_{code}")),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("library"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new("library")),
    )
}

/// An added item returned by [`LibraryService::add`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedItem {
    pub id: AssetId,
    pub name: String,
    pub deduplicated: bool,
}

/// An item to collect into the global library from a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectItem {
    pub asset_id: AssetId,
    pub path: PathBuf,
    pub name: Option<String>,
    pub media_type: Option<String>,
}

/// Result of adding files or collecting items to the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddResult {
    pub items: Vec<AddedItem>,
    pub version: u64,
}

/// Result of purging removed entries and unreferenced assets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeResult {
    pub purged_entries: usize,
    pub deleted_files: usize,
    pub freed_bytes: u64,
    pub version: u64,
}

/// Filter for listing library items.
#[derive(Debug, Clone, Default)]
pub struct LibraryListFilter {
    pub name_contains: Option<String>,
    pub tag: Option<String>,
    pub media_type: Option<String>,
    pub include_removed: bool,
}

/// A resolved item in the global library for display or search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryItem {
    pub id: AssetId,
    pub name: String,
    pub tags: Vec<String>,
    pub media_type: String,
    pub size: u64,
    pub hash: String,
    pub added_at: String,
    pub removed: bool,
    pub source: LibrarySource,
}

/// The application-wide library service.
pub struct LibraryService {
    paths: AppPaths,
    asset_store: AssetStore,
    step_log: StepLog,
    session: AsyncMutex<Option<DocumentSession<LibraryIndexDocument>>>,
    event_bus: EventBus,
    storage_budget: RwLock<Option<Arc<StorageBudgetService>>>,
}

impl std::fmt::Debug for LibraryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibraryService")
            .field("paths", &self.paths)
            .finish()
    }
}

impl LibraryService {
    pub fn new(paths: AppPaths) -> Self {
        Self::with_event_bus(paths, EventBus::new())
    }

    pub fn with_event_bus(paths: AppPaths, event_bus: EventBus) -> Self {
        let library_dir = paths.library_dir().to_path_buf();
        Self {
            paths,
            asset_store: AssetStore::new(&library_dir),
            step_log: StepLog::new(&library_dir),
            session: AsyncMutex::new(None),
            event_bus,
            storage_budget: RwLock::new(None),
        }
    }

    pub(crate) fn set_storage_budget(&self, service: Arc<StorageBudgetService>) {
        let cb_service = Arc::clone(&service);
        self.asset_store.set_on_new_file(Arc::new(move |bytes| {
            cb_service.record_added(StorageCategory::Library, bytes);
        }));
        *self
            .storage_budget
            .write()
            .expect("library storage_budget slot poisoned") = Some(service);
    }

    async fn sync_library_removed_bytes(&self) {
        let budget = self
            .storage_budget
            .read()
            .expect("library storage_budget slot poisoned")
            .as_ref()
            .cloned();
        if let Some(budget) = budget {
            budget.refresh_library_removed_bytes().await;
        }
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn asset_store(&self) -> &AssetStore {
        &self.asset_store
    }

    pub fn step_log(&self) -> &StepLog {
        &self.step_log
    }

    fn index_path(&self) -> PathBuf {
        self.paths.library_dir().join("index.json")
    }

    /// Emit a `library.changed` notification on the event bus.
    pub fn emit_changed(&self, doc: &LibraryIndexDocument) {
        self.event_bus.publish(Event::Document {
            kind: "library.changed".to_owned(),
            project_id: "library".to_owned(),
            document_id: "library".to_owned(),
            version: doc.version().get(),
        });
    }

    async fn ensure_session_loaded<'a>(
        &self,
        guard: &'a mut Option<DocumentSession<LibraryIndexDocument>>,
    ) -> AppHostResult<&'a mut DocumentSession<LibraryIndexDocument>> {
        if guard.is_none() {
            let path = self.index_path();
            let doc: Option<LibraryIndexDocument> = read_document(&path, "library").await?;
            let doc = doc.unwrap_or_default();
            *guard = Some(DocumentSession::new(doc));
        }
        Ok(guard.as_mut().expect("session loaded"))
    }

    async fn save_locked(
        &self,
        session: &DocumentSession<LibraryIndexDocument>,
    ) -> AppHostResult<()> {
        let path = self.index_path();
        write_document(&path, session.document(), "library").await
    }

    /// Read the current snapshot of the library document.
    pub async fn snapshot(&self) -> AppHostResult<LibraryIndexDocument> {
        let mut guard = self.session.lock().await;
        let session = self.ensure_session_loaded(&mut guard).await?;
        Ok(session.document().clone())
    }

    /// Add files at `paths` into the global library.
    ///
    /// Writes one step record to `library/steps.jsonl`.
    /// On any per-file error, fails the whole call with a caller diagnostic and adds no entries.
    pub async fn add(
        &self,
        paths: Vec<PathBuf>,
        context: &OperationContext,
    ) -> Result<AddResult, Diagnostic> {
        if paths.is_empty() {
            return Err(library_caller_error(
                "EMPTY_PATHS",
                "paths must not be empty",
            ));
        }

        static STEP_COUNTER: AtomicU64 = AtomicU64::new(1);
        let count = STEP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = jiff::Timestamp::now().as_nanosecond();
        let library_step_id = StepId::new(format!("step-{nanos:x}-{count:x}"));

        let start_time = Timestamp::new(jiff::Timestamp::now().to_string());
        let paths_json: Vec<serde_json::Value> = paths
            .iter()
            .map(|p| serde_json::Value::String(p.to_string_lossy().to_string()))
            .collect();
        let mut step_record = StepRecord::new_running(
            library_step_id.clone(),
            "library.add".to_owned(),
            "0.1.0".to_owned(),
            serde_json::json!({ "paths": paths_json }),
            context.clone(),
            start_time,
        );

        self.step_log.record_start(&step_record).await?;

        // Validate and hash each file upfront before ingesting anything
        struct PreparedFile {
            path: PathBuf,
            hash: String,
        }

        let mut prepared = Vec::with_capacity(paths.len());
        for path in &paths {
            let meta = match tokio::fs::symlink_metadata(path).await {
                Ok(m) => m,
                Err(e) => {
                    let diag = library_caller_error(
                        "FILE_NOT_FOUND",
                        format!("failed to read source asset `{}`: {e}", path.display()),
                    );
                    let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                    step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                    let _ = self.step_log.record_finish(&step_record).await;
                    return Err(diag);
                }
            };

            if meta.file_type().is_symlink() {
                let diag = library_caller_error(
                    "SYMLINK_REJECTED",
                    format!("symlink `{}` is not allowed", path.display()),
                );
                let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            }

            if !meta.is_file() {
                let diag = library_caller_error(
                    "NOT_A_REGULAR_FILE",
                    format!("source `{}` is not a regular file", path.display()),
                );
                let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            }

            let bytes = match tokio::fs::read(path).await {
                Ok(b) => b,
                Err(e) => {
                    let diag = library_caller_error(
                        "IO",
                        format!("failed to read `{}`: {e}", path.display()),
                    );
                    let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                    step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                    let _ = self.step_log.record_finish(&step_record).await;
                    return Err(diag);
                }
            };

            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let hash = format!("{:x}", hasher.finalize());

            prepared.push(PreparedFile {
                path: path.clone(),
                hash,
            });
        }

        // Lock session and index
        let mut session_guard = self.session.lock().await;
        let session = match self.ensure_session_loaded(&mut session_guard).await {
            Ok(s) => s,
            Err(e) => {
                let diag = library_internal_error("SESSION_LOAD_FAILED", e.to_string());
                let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            }
        };

        // Find existing active entries by hash
        let asset_records = match self.asset_store.list().await {
            Ok(records) => records,
            Err(err) => {
                let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                step_record.finish(StepOutcome::failed(err.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(err);
            }
        };

        let asset_hash_by_id: HashMap<&AssetId, &str> = asset_records
            .iter()
            .map(|r| (&r.id, r.hash.as_str()))
            .collect();

        let mut active_by_hash: HashMap<&str, (AssetId, String)> = HashMap::new();
        for (id, entry) in session.document().entries() {
            if !entry.removed
                && let Some(&hash) = asset_hash_by_id.get(id)
            {
                active_by_hash.insert(hash, (id.clone(), entry.name.clone()));
            }
        }

        let mut items = Vec::new();
        let mut new_entries: Vec<(AssetId, LibraryEntry)> = Vec::new();
        let mut seen_in_call = HashSet::new();

        for file in prepared {
            if !seen_in_call.insert(file.hash.clone()) {
                // Two identical files in one call yield one item
                continue;
            }

            if let Some((existing_id, existing_name)) = active_by_hash.get(file.hash.as_str()) {
                items.push(AddedItem {
                    id: existing_id.clone(),
                    name: existing_name.clone(),
                    deduplicated: true,
                });
            } else {
                let record = match self
                    .asset_store
                    .ingest_file(library_step_id.clone(), &file.path, None)
                    .await
                {
                    Ok(r) => r,
                    Err(diag) => {
                        let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                        step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                        let _ = self.step_log.record_finish(&step_record).await;
                        return Err(diag);
                    }
                };

                let file_name = file
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "file".to_owned());

                let entry = LibraryEntry::new(
                    file_name.clone(),
                    Vec::new(),
                    false,
                    jiff::Timestamp::now().to_string(),
                    LibrarySource::File {
                        path: file.path.to_string_lossy().into_owned(),
                    },
                );

                new_entries.push((record.id.clone(), entry));
                items.push(AddedItem {
                    id: record.id,
                    name: file_name,
                    deduplicated: false,
                });
            }
        }

        let version = if !new_entries.is_empty() {
            static BATCH_COUNTER: AtomicU64 = AtomicU64::new(1);
            let count = BATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = jiff::Timestamp::now().as_nanosecond();
            let batch = CommandBatch::new(
                CommandBatchId::new(format!("batch-lib-add-{nanos:x}-{count:x}")),
                context.clone(),
                session.document().version(),
                Timestamp::new(jiff::Timestamp::now().to_string()),
                vec![LibraryCommand::AddEntries {
                    entries: new_entries,
                }],
            );

            let res = session.apply_batch(&(), batch);
            if res.status() == CommandStatus::Rejected {
                let diag =
                    res.diagnostics().first().cloned().unwrap_or_else(|| {
                        library_caller_error("REJECTED", "add entries rejected")
                    });
                let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            }

            if let Err(e) = self.save_locked(session).await {
                let diag = library_internal_error("SAVE_FAILED", e.to_string());
                let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            }

            self.emit_changed(session.document());
            session.document().version().get()
        } else {
            session.document().version().get()
        };

        drop(session_guard);
        self.sync_library_removed_bytes().await;

        let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
        let items_val =
            serde_json::to_value(&items).unwrap_or(serde_json::Value::Array(Vec::new()));
        step_record.finish(
            StepOutcome::Succeeded {
                output: serde_json::json!({
                    "items": items_val,
                    "version": version,
                }),
            },
            finish_time,
        );

        self.step_log.record_finish(&step_record).await?;

        Ok(AddResult { items, version })
    }

    /// List items in the global library matching `filter`.
    pub async fn list(&self, filter: &LibraryListFilter) -> Result<Vec<LibraryItem>, Diagnostic> {
        let doc = self
            .snapshot()
            .await
            .map_err(|e| library_internal_error("LOAD_FAILED", e.to_string()))?;

        let records = self.asset_store.list().await?;
        let records_by_id: HashMap<AssetId, _> =
            records.into_iter().map(|r| (r.id.clone(), r)).collect();

        let mut items = Vec::new();
        for (id, entry) in doc.entries() {
            if !filter.include_removed && entry.removed {
                continue;
            }

            if let Some(needle) = &filter.name_contains
                && !needle.is_empty()
                && !entry.name.contains(needle)
            {
                continue;
            }

            if let Some(tag) = &filter.tag
                && !entry.tags.contains(tag)
            {
                continue;
            }

            let (media_type, size, hash) = match records_by_id.get(id) {
                Some(r) => (r.media_type.clone(), r.size, r.hash.clone()),
                None => ("application/octet-stream".to_string(), 0, String::new()),
            };

            if let Some(pattern) = &filter.media_type
                && !crate::asset_store::matches_media_type_pattern(pattern, &media_type)
            {
                continue;
            }

            items.push(LibraryItem {
                id: id.clone(),
                name: entry.name.clone(),
                tags: entry.tags.clone(),
                media_type,
                size,
                hash,
                added_at: entry.added_at.clone(),
                removed: entry.removed,
                source: entry.source.clone(),
            });
        }

        // Newest first
        items.sort_by(|a, b| {
            b.added_at
                .cmp(&a.added_at)
                .then_with(|| b.id.as_str().cmp(a.id.as_str()))
        });

        Ok(items)
    }

    /// Rename an active entry.
    pub async fn rename(
        &self,
        id: AssetId,
        name: String,
        context: &OperationContext,
    ) -> Result<u64, Diagnostic> {
        let mut guard = self.session.lock().await;
        let session = self
            .ensure_session_loaded(&mut guard)
            .await
            .map_err(|e| library_internal_error("SESSION_LOAD_FAILED", e.to_string()))?;

        static BATCH_COUNTER: AtomicU64 = AtomicU64::new(1);
        let count = BATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = jiff::Timestamp::now().as_nanosecond();
        let batch = CommandBatch::new(
            CommandBatchId::new(format!("batch-lib-rename-{nanos:x}-{count:x}")),
            context.clone(),
            session.document().version(),
            Timestamp::new(jiff::Timestamp::now().to_string()),
            vec![LibraryCommand::Rename { id, name }],
        );

        let res = session.apply_batch(&(), batch);
        if res.status() == CommandStatus::Rejected {
            return Err(res
                .diagnostics()
                .first()
                .cloned()
                .unwrap_or_else(|| library_caller_error("REJECTED", "rename rejected")));
        }

        if res.status() == CommandStatus::Applied {
            self.save_locked(session)
                .await
                .map_err(|e| library_internal_error("SAVE_FAILED", e.to_string()))?;
            self.emit_changed(session.document());
        }

        Ok(session.document().version().get())
    }

    /// Set tags for an active entry.
    pub async fn set_tags(
        &self,
        id: AssetId,
        tags: Vec<String>,
        context: &OperationContext,
    ) -> Result<u64, Diagnostic> {
        let mut guard = self.session.lock().await;
        let session = self
            .ensure_session_loaded(&mut guard)
            .await
            .map_err(|e| library_internal_error("SESSION_LOAD_FAILED", e.to_string()))?;

        static BATCH_COUNTER: AtomicU64 = AtomicU64::new(1);
        let count = BATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = jiff::Timestamp::now().as_nanosecond();
        let batch = CommandBatch::new(
            CommandBatchId::new(format!("batch-lib-tags-{nanos:x}-{count:x}")),
            context.clone(),
            session.document().version(),
            Timestamp::new(jiff::Timestamp::now().to_string()),
            vec![LibraryCommand::SetTags { id, tags }],
        );

        let res = session.apply_batch(&(), batch);
        if res.status() == CommandStatus::Rejected {
            return Err(res
                .diagnostics()
                .first()
                .cloned()
                .unwrap_or_else(|| library_caller_error("REJECTED", "set tags rejected")));
        }

        if res.status() == CommandStatus::Applied {
            self.save_locked(session)
                .await
                .map_err(|e| library_internal_error("SAVE_FAILED", e.to_string()))?;
            self.emit_changed(session.document());
        }

        Ok(session.document().version().get())
    }

    /// Mark entries as removed.
    pub async fn remove(
        &self,
        ids: Vec<AssetId>,
        context: &OperationContext,
    ) -> Result<u64, Diagnostic> {
        let mut guard = self.session.lock().await;
        let session = self
            .ensure_session_loaded(&mut guard)
            .await
            .map_err(|e| library_internal_error("SESSION_LOAD_FAILED", e.to_string()))?;

        static BATCH_COUNTER: AtomicU64 = AtomicU64::new(1);
        let count = BATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = jiff::Timestamp::now().as_nanosecond();
        let batch = CommandBatch::new(
            CommandBatchId::new(format!("batch-lib-remove-{nanos:x}-{count:x}")),
            context.clone(),
            session.document().version(),
            Timestamp::new(jiff::Timestamp::now().to_string()),
            vec![LibraryCommand::Remove { ids }],
        );

        let res = session.apply_batch(&(), batch);
        if res.status() == CommandStatus::Rejected {
            return Err(res
                .diagnostics()
                .first()
                .cloned()
                .unwrap_or_else(|| library_caller_error("REJECTED", "remove rejected")));
        }

        if res.status() == CommandStatus::Applied {
            self.save_locked(session)
                .await
                .map_err(|e| library_internal_error("SAVE_FAILED", e.to_string()))?;
            self.emit_changed(session.document());
            let version = session.document().version().get();
            drop(guard);
            self.sync_library_removed_bytes().await;
            return Ok(version);
        }

        Ok(session.document().version().get())
    }

    /// Undo the last applied command batch in the library index document.
    pub async fn undo(&self) -> Result<Option<u64>, Diagnostic> {
        let mut guard = self.session.lock().await;
        let session = self
            .ensure_session_loaded(&mut guard)
            .await
            .map_err(|e| library_internal_error("SESSION_LOAD_FAILED", e.to_string()))?;

        let Some(res) = session.undo() else {
            return Ok(None);
        };

        self.save_locked(session)
            .await
            .map_err(|e| library_internal_error("SAVE_FAILED", e.to_string()))?;
        self.emit_changed(session.document());
        drop(guard);
        self.sync_library_removed_bytes().await;

        Ok(Some(res.version().get()))
    }

    /// Redo the last undone command batch in the library index document.
    pub async fn redo(&self) -> Result<Option<u64>, Diagnostic> {
        let mut guard = self.session.lock().await;
        let session = self
            .ensure_session_loaded(&mut guard)
            .await
            .map_err(|e| library_internal_error("SESSION_LOAD_FAILED", e.to_string()))?;

        let Some(res) = session.redo() else {
            return Ok(None);
        };

        self.save_locked(session)
            .await
            .map_err(|e| library_internal_error("SAVE_FAILED", e.to_string()))?;
        self.emit_changed(session.document());
        drop(guard);
        self.sync_library_removed_bytes().await;

        Ok(Some(res.version().get()))
    }

    /// Collect project assets into the global library.
    ///
    /// Writes one step record to `library/steps.jsonl`.
    /// On any per-item error, fails the whole call with a caller diagnostic and adds no entries.
    pub async fn collect(
        &self,
        project_id: ProjectId,
        items: Vec<CollectItem>,
        context: &OperationContext,
    ) -> Result<AddResult, Diagnostic> {
        if items.is_empty() {
            return Err(library_caller_error(
                "EMPTY_ITEMS",
                "items must not be empty",
            ));
        }

        static STEP_COUNTER: AtomicU64 = AtomicU64::new(1);
        let count = STEP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = jiff::Timestamp::now().as_nanosecond();
        let library_step_id = StepId::new(format!("step-{nanos:x}-{count:x}"));
        let start_time = Timestamp::new(jiff::Timestamp::now().to_string());

        struct PreparedCollectItem {
            asset_id: AssetId,
            path: PathBuf,
            hash: String,
            name: Option<String>,
            media_type: Option<String>,
        }

        let mut prepared = Vec::with_capacity(items.len());
        for item in items {
            if let Some(ref n) = item.name {
                let trimmed = n.trim();
                let char_count = trimmed.chars().count();
                if char_count == 0 || char_count > 256 {
                    return Err(library_caller_error(
                        "INVALID_NAME",
                        format!(
                            "library entry name must be between 1 and 256 characters (got {char_count})"
                        ),
                    ));
                }
            }

            let meta = match tokio::fs::symlink_metadata(&item.path).await {
                Ok(m) => m,
                Err(e) => {
                    return Err(library_caller_error(
                        "FILE_NOT_FOUND",
                        format!("failed to read source asset `{}`: {e}", item.path.display()),
                    ));
                }
            };

            if meta.file_type().is_symlink() {
                return Err(library_caller_error(
                    "SYMLINK_REJECTED",
                    format!("symlink `{}` is not allowed", item.path.display()),
                ));
            }

            if !meta.is_file() {
                return Err(library_caller_error(
                    "NOT_A_REGULAR_FILE",
                    format!("source `{}` is not a regular file", item.path.display()),
                ));
            }

            let (hash, _) = match AssetStore::stream_hash_and_size(&item.path).await {
                Ok(res) => res,
                Err(e) => return Err(e),
            };

            prepared.push(PreparedCollectItem {
                asset_id: item.asset_id,
                path: item.path,
                hash,
                name: item.name,
                media_type: item.media_type,
            });
        }

        let items_input_json: Vec<serde_json::Value> = prepared
            .iter()
            .map(|p| {
                serde_json::json!({
                    "asset_id": p.asset_id.as_str(),
                    "hash": p.hash,
                })
            })
            .collect();

        let mut step_record = StepRecord::new_running(
            library_step_id.clone(),
            "library.collect".to_owned(),
            "0.1.0".to_owned(),
            serde_json::json!({
                "project_id": project_id.as_str(),
                "items": items_input_json,
            }),
            context.clone(),
            start_time,
        );

        self.step_log.record_start(&step_record).await?;

        let mut session_guard = self.session.lock().await;
        let session = match self.ensure_session_loaded(&mut session_guard).await {
            Ok(s) => s,
            Err(e) => {
                let diag = library_internal_error("SESSION_LOAD_FAILED", e.to_string());
                let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            }
        };

        let asset_records = match self.asset_store.list().await {
            Ok(records) => records,
            Err(err) => {
                let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                step_record.finish(StepOutcome::failed(err.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(err);
            }
        };

        let asset_hash_by_id: HashMap<&AssetId, &str> = asset_records
            .iter()
            .map(|r| (&r.id, r.hash.as_str()))
            .collect();

        let mut active_by_hash: HashMap<&str, (AssetId, String)> = HashMap::new();
        for (id, entry) in session.document().entries() {
            if !entry.removed
                && let Some(&hash) = asset_hash_by_id.get(id)
            {
                active_by_hash.insert(hash, (id.clone(), entry.name.clone()));
            }
        }

        let mut items_out = Vec::new();
        let mut new_entries: Vec<(AssetId, LibraryEntry)> = Vec::new();
        let mut seen_in_call = HashSet::new();

        for item in prepared {
            if !seen_in_call.insert(item.hash.clone()) {
                continue;
            }

            if let Some((existing_id, existing_name)) = active_by_hash.get(item.hash.as_str()) {
                items_out.push(AddedItem {
                    id: existing_id.clone(),
                    name: existing_name.clone(),
                    deduplicated: true,
                });
            } else {
                let record = match self
                    .asset_store
                    .ingest_file(
                        library_step_id.clone(),
                        &item.path,
                        item.media_type.as_deref(),
                    )
                    .await
                {
                    Ok(r) => r,
                    Err(diag) => {
                        let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                        step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                        let _ = self.step_log.record_finish(&step_record).await;
                        return Err(diag);
                    }
                };

                let entry_name = match item.name {
                    Some(n) => n.trim().to_string(),
                    None => {
                        let ext = crate::asset_store::extension_for_media_type(&record.media_type);
                        let hash_prefix = if record.hash.len() >= 8 {
                            &record.hash[..8]
                        } else {
                            &record.hash
                        };
                        format!("collected-{hash_prefix}.{ext}")
                    }
                };

                let entry = LibraryEntry::new(
                    entry_name.clone(),
                    Vec::new(),
                    false,
                    jiff::Timestamp::now().to_string(),
                    LibrarySource::Project {
                        project_id: project_id.clone(),
                        asset_id: item.asset_id.clone(),
                    },
                );

                new_entries.push((record.id.clone(), entry));
                items_out.push(AddedItem {
                    id: record.id,
                    name: entry_name,
                    deduplicated: false,
                });
            }
        }

        let version = if !new_entries.is_empty() {
            static BATCH_COUNTER: AtomicU64 = AtomicU64::new(1);
            let b_count = BATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
            let b_nanos = jiff::Timestamp::now().as_nanosecond();
            let batch = CommandBatch::new(
                CommandBatchId::new(format!("batch-lib-collect-{b_nanos:x}-{b_count:x}")),
                context.clone(),
                session.document().version(),
                Timestamp::new(jiff::Timestamp::now().to_string()),
                vec![LibraryCommand::AddEntries {
                    entries: new_entries,
                }],
            );

            let res = session.apply_batch(&(), batch);
            if res.status() == CommandStatus::Rejected {
                let diag =
                    res.diagnostics().first().cloned().unwrap_or_else(|| {
                        library_caller_error("REJECTED", "add entries rejected")
                    });
                let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            }

            if let Err(e) = self.save_locked(session).await {
                let diag = library_internal_error("SAVE_FAILED", e.to_string());
                let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            }

            self.emit_changed(session.document());
            session.document().version().get()
        } else {
            session.document().version().get()
        };

        drop(session_guard);
        self.sync_library_removed_bytes().await;

        let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
        let items_val =
            serde_json::to_value(&items_out).unwrap_or(serde_json::Value::Array(Vec::new()));
        step_record.finish(
            StepOutcome::Succeeded {
                output: serde_json::json!({
                    "items": items_val,
                    "version": version,
                }),
            },
            finish_time,
        );

        self.step_log.record_finish(&step_record).await?;

        Ok(AddResult {
            items: items_out,
            version,
        })
    }

    /// Purge dropped library entries and unreferenced content files.
    ///
    /// Drops every removed entry from the index, replaces the session with a fresh
    /// `DocumentSession` (clearing undo/redo history), and deletes unreferenced
    /// content files under `library/assets/`, stray `.tmp-*` files, and leftover `scratch/` files.
    ///
    /// Note that `assets.jsonl` stays strictly append-only: historic asset records for
    /// deleted content remain in the log as an immutable audit trail.
    pub async fn purge(&self, context: &OperationContext) -> Result<PurgeResult, Diagnostic> {
        static STEP_COUNTER: AtomicU64 = AtomicU64::new(1);
        let count = STEP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = jiff::Timestamp::now().as_nanosecond();
        let library_step_id = StepId::new(format!("step-{nanos:x}-{count:x}"));
        let start_time = Timestamp::new(jiff::Timestamp::now().to_string());

        let mut step_record = StepRecord::new_running(
            library_step_id.clone(),
            "library.purge".to_owned(),
            "0.1.0".to_owned(),
            serde_json::json!({}),
            context.clone(),
            start_time,
        );

        self.step_log.record_start(&step_record).await?;

        let mut guard = self.session.lock().await;
        let session = match self.ensure_session_loaded(&mut guard).await {
            Ok(s) => s,
            Err(e) => {
                let diag = library_internal_error("SESSION_LOAD_FAILED", e.to_string());
                let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            }
        };

        // Resolve the content hash of every active entry before changing
        // anything. An active entry whose asset record cannot be found would
        // make its content look unreferenced, so purge refuses to run rather
        // than delete it.
        let asset_records = match self.asset_store.list().await {
            Ok(records) => records,
            Err(err) => {
                let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                step_record.finish(StepOutcome::failed(err.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(err);
            }
        };
        let asset_hash_by_id: HashMap<&AssetId, &str> = asset_records
            .iter()
            .map(|r| (&r.id, r.hash.as_str()))
            .collect();
        let mut active_hashes: HashSet<String> = HashSet::new();
        for (id, entry) in session.document().entries() {
            if entry.removed {
                continue;
            }
            let Some(&hash) = asset_hash_by_id.get(id) else {
                let diag = library_internal_error(
                    "ASSET_RECORD_MISSING",
                    format!("active library entry `{id}` has no asset record; purge aborted"),
                );
                let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            };
            active_hashes.insert(hash.to_owned());
        }

        let removed_count = session
            .document()
            .entries()
            .values()
            .filter(|e| e.removed)
            .count();

        static BATCH_COUNTER: AtomicU64 = AtomicU64::new(1);
        let b_count = BATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let b_nanos = jiff::Timestamp::now().as_nanosecond();
        let batch = CommandBatch::new(
            CommandBatchId::new(format!("batch-lib-purge-{b_nanos:x}-{b_count:x}")),
            context.clone(),
            session.document().version(),
            Timestamp::new(jiff::Timestamp::now().to_string()),
            vec![LibraryCommand::Purge],
        );

        let res = session.apply_batch(&(), batch);
        if res.status() == CommandStatus::Rejected {
            let diag = res
                .diagnostics()
                .first()
                .cloned()
                .unwrap_or_else(|| library_caller_error("REJECTED", "purge command rejected"));
            let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
            step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
            let _ = self.step_log.record_finish(&step_record).await;
            return Err(diag);
        }

        // Replace the session with a fresh DocumentSession so undo/redo history is cleared.
        let fresh_doc = session.document().clone();
        *guard = Some(DocumentSession::new(fresh_doc));
        let session = guard.as_mut().unwrap();

        if let Err(e) = self.save_locked(session).await {
            let diag = library_internal_error("SAVE_FAILED", e.to_string());
            let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
            step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
            let _ = self.step_log.record_finish(&step_record).await;
            return Err(diag);
        }

        let version = session.document().version().get();

        // Delete unreferenced files in library/assets/ and leftover library/scratch/
        let mut deleted_files = 0usize;
        let mut freed_bytes = 0u64;

        let assets_dir = self.paths.library_dir().join("assets");
        if assets_dir.exists()
            && let Ok(mut rd) = tokio::fs::read_dir(&assets_dir).await
        {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let path = entry.path();
                let file_name = entry.file_name();
                let name_str = file_name.to_string_lossy();
                if name_str.starts_with(".tmp-") {
                    let (files, bytes) = remove_path_and_count(&path).await;
                    deleted_files += files;
                    freed_bytes += bytes;
                } else if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false)
                    && let Ok(mut sub_rd) = tokio::fs::read_dir(&path).await
                {
                    while let Ok(Some(sub_entry)) = sub_rd.next_entry().await {
                        let sub_path = sub_entry.path();
                        let sub_name = sub_entry.file_name();
                        let sub_name_str = sub_name.to_string_lossy();
                        if sub_name_str.starts_with(".tmp-")
                            || !active_hashes.contains(sub_name_str.as_ref())
                        {
                            let (files, bytes) = remove_path_and_count(&sub_path).await;
                            deleted_files += files;
                            freed_bytes += bytes;
                        }
                    }
                }
            }
        }

        let scratch_dir = self.paths.library_dir().join("scratch");
        let mut scratch_freed = 0u64;
        if scratch_dir.exists()
            && let Ok(mut rd) = tokio::fs::read_dir(&scratch_dir).await
        {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let (files, bytes) = remove_path_and_count(&entry.path()).await;
                deleted_files += files;
                freed_bytes += bytes;
                scratch_freed += bytes;
            }
        }

        if let Some(budget) = self
            .storage_budget
            .read()
            .expect("library storage_budget slot poisoned")
            .as_ref()
            .cloned()
        {
            let assets_freed = freed_bytes.saturating_sub(scratch_freed);
            budget.record_removed(StorageCategory::Library, assets_freed);
            budget.record_removed(StorageCategory::Cache, scratch_freed);
            budget.clear_library_removed_bytes();
        }

        self.emit_changed(session.document());

        let finish_time = Timestamp::new(jiff::Timestamp::now().to_string());
        step_record.finish(
            StepOutcome::Succeeded {
                output: serde_json::json!({
                    "purged_entries": removed_count,
                    "deleted_files": deleted_files,
                    "freed_bytes": freed_bytes,
                    "version": version,
                }),
            },
            finish_time,
        );

        self.step_log.record_finish(&step_record).await?;

        Ok(PurgeResult {
            purged_entries: removed_count,
            deleted_files,
            freed_bytes,
            version,
        })
    }
}

async fn remove_path_and_count(path: &Path) -> (usize, u64) {
    let mut files = 0usize;
    let mut bytes = 0u64;
    // `symlink_metadata` so a symlink is removed itself, never followed.
    if let Ok(meta) = tokio::fs::symlink_metadata(path).await {
        if meta.is_dir() {
            if let Ok(mut rd) = tokio::fs::read_dir(path).await {
                while let Ok(Some(child)) = rd.next_entry().await {
                    let (c_files, c_bytes) = Box::pin(remove_path_and_count(&child.path())).await;
                    files += c_files;
                    bytes += c_bytes;
                }
            }
            let _ = tokio::fs::remove_dir(path).await;
        } else if crate::asset_store::remove_store_file(path).await {
            bytes += meta.len();
            files += 1;
        }
    }
    (files, bytes)
}
