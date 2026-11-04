//! Global asset library service.
//!
//! Manages the shared application-wide asset library stored at `<base>/library/`.
//! Contains its own `AssetStore`, `StepLog`, and a lazily loaded `DocumentSession<LibraryIndexDocument>`
//! persisted to `<base>/library/index.json`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use reimagine_config::AppPaths;
use reimagine_core::command::{CommandBatch, CommandStatus, OperationContext};
use reimagine_core::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
    DiagnosticTargetDomain,
};
use reimagine_core::document::DocumentSession;
use reimagine_core::event::{Event, EventBus, Timestamp};
use reimagine_core::library::{LibraryCommand, LibraryEntry, LibraryIndexDocument};
use reimagine_core::model::{AssetId, CommandBatchId, DiagnosticId, StepId};
use reimagine_core::step::{StepOutcome, StepRecord};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;

use crate::asset_store::AssetStore;
use crate::document_store::{read_document, write_document};
use crate::error::AppHostResult;
use crate::step_log::StepLog;

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

/// Result of adding files to the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddResult {
    pub items: Vec<AddedItem>,
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
}

/// The application-wide library service.
pub struct LibraryService {
    paths: AppPaths,
    asset_store: AssetStore,
    step_log: StepLog,
    session: AsyncMutex<Option<DocumentSession<LibraryIndexDocument>>>,
    event_bus: EventBus,
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
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let library_step_id = StepId::new(format!("step-{nanos:x}-{count:x}"));

        let start_time = Timestamp::new(chrono::Utc::now().to_rfc3339());
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
                    let finish_time = Timestamp::new(chrono::Utc::now().to_rfc3339());
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
                let finish_time = Timestamp::new(chrono::Utc::now().to_rfc3339());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            }

            if !meta.is_file() {
                let diag = library_caller_error(
                    "NOT_A_REGULAR_FILE",
                    format!("source `{}` is not a regular file", path.display()),
                );
                let finish_time = Timestamp::new(chrono::Utc::now().to_rfc3339());
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
                    let finish_time = Timestamp::new(chrono::Utc::now().to_rfc3339());
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
                let finish_time = Timestamp::new(chrono::Utc::now().to_rfc3339());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            }
        };

        // Find existing active entries by hash
        let asset_records = match self.asset_store.list().await {
            Ok(records) => records,
            Err(err) => {
                let finish_time = Timestamp::new(chrono::Utc::now().to_rfc3339());
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
                        let finish_time = Timestamp::new(chrono::Utc::now().to_rfc3339());
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
                    chrono::Utc::now().to_rfc3339(),
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
            let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
            let batch = CommandBatch::new(
                CommandBatchId::new(format!("batch-lib-add-{nanos:x}-{count:x}")),
                context.clone(),
                session.document().version(),
                Timestamp::new(chrono::Utc::now().to_rfc3339()),
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
                let finish_time = Timestamp::new(chrono::Utc::now().to_rfc3339());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            }

            if let Err(e) = self.save_locked(session).await {
                let diag = library_internal_error("SAVE_FAILED", e.to_string());
                let finish_time = Timestamp::new(chrono::Utc::now().to_rfc3339());
                step_record.finish(StepOutcome::failed(diag.clone()), finish_time);
                let _ = self.step_log.record_finish(&step_record).await;
                return Err(diag);
            }

            self.emit_changed(session.document());
            session.document().version().get()
        } else {
            session.document().version().get()
        };

        let finish_time = Timestamp::new(chrono::Utc::now().to_rfc3339());
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
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let batch = CommandBatch::new(
            CommandBatchId::new(format!("batch-lib-rename-{nanos:x}-{count:x}")),
            context.clone(),
            session.document().version(),
            Timestamp::new(chrono::Utc::now().to_rfc3339()),
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
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let batch = CommandBatch::new(
            CommandBatchId::new(format!("batch-lib-tags-{nanos:x}-{count:x}")),
            context.clone(),
            session.document().version(),
            Timestamp::new(chrono::Utc::now().to_rfc3339()),
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
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let batch = CommandBatch::new(
            CommandBatchId::new(format!("batch-lib-remove-{nanos:x}-{count:x}")),
            context.clone(),
            session.document().version(),
            Timestamp::new(chrono::Utc::now().to_rfc3339()),
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

        Ok(Some(res.version().get()))
    }
}
