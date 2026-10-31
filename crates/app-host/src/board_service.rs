//! Board service: load/save the persisted project canvas document and
//! run board commands through `DocumentSession`.
//!
//! Every board is 1:1 with a project and lives at
//! `projects/{project_id}/board.json`. Successful command application
//! saves the document atomically and emits one `board.changed` event on
//! the shared workspace document event bus.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use reimagine_config::AppPaths;
use reimagine_core::board::{
    BoardChange, BoardCommand, BoardDocument, BoardItemKind, BoardItemPosition, BoardItemSize,
};
use reimagine_core::command::{
    CommandActor, CommandBatch, CommandResult, CommandStatus, OperationContext,
};
use reimagine_core::document::DocumentSession;
use reimagine_core::event::{Event, EventBus, Timestamp};
use reimagine_core::model::{
    BoardId, BoardItemId, CommandBatchId, DocumentVersion, HistoryEntryId, ProjectId,
};
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;

use crate::activity::{ActivityKind, ActivityService};
use crate::document_store::{read_document, write_document};
use crate::{AppHostError, AppHostResult};

pub struct BoardService {
    paths: AppPaths,
    sessions: RwLock<BTreeMap<ProjectId, Arc<AsyncMutex<DocumentSession<BoardDocument>>>>>,
    event_bus: EventBus,
    activity: Option<Arc<ActivityService>>,
}

impl std::fmt::Debug for BoardService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let session_count = self
            .sessions
            .read()
            .map(|sessions| sessions.len())
            .unwrap_or_default();
        f.debug_struct("BoardService")
            .field("paths", &self.paths)
            .field("session_count", &session_count)
            .field("activity", &self.activity.is_some())
            .finish()
    }
}

impl BoardService {
    pub fn new(paths: AppPaths) -> Self {
        Self::with_event_bus(paths, EventBus::new())
    }

    pub fn with_event_bus(paths: AppPaths, event_bus: EventBus) -> Self {
        Self::with_event_bus_and_activity(paths, event_bus, None)
    }

    pub fn with_event_bus_and_activity(
        paths: AppPaths,
        event_bus: EventBus,
        activity: Option<Arc<ActivityService>>,
    ) -> Self {
        Self {
            paths,
            sessions: RwLock::new(BTreeMap::new()),
            event_bus,
            activity,
        }
    }

    pub fn board_path(&self, project_id: &ProjectId) -> std::path::PathBuf {
        self.paths
            .project_dir(project_id.as_str())
            .join("board.json")
    }

    /// Load the board for `project_id`. The document is created by
    /// project creation (`ensure_board_file`); asking for a project
    /// without a board file yields [`AppHostError::UnknownBoard`]. The
    /// returned document is the current in-memory snapshot shared by
    /// this service.
    pub async fn load_or_create(&self, project_id: &ProjectId) -> AppHostResult<BoardDocument> {
        let cached = self
            .sessions
            .read()
            .expect("board session registry poisoned")
            .get(project_id)
            .cloned();
        if let Some(session) = cached {
            return Ok(session.lock().await.document().clone());
        }

        let path = self.board_path(project_id);
        let board: BoardDocument =
            read_document(&path, "board")
                .await?
                .ok_or_else(|| AppHostError::UnknownBoard {
                    project_id: project_id.clone(),
                })?;
        let session = Arc::new(AsyncMutex::new(DocumentSession::new(board)));

        // Double-check: another task may have created the session while
        // we read from disk. No registry guard is held across the
        // awaited session lock below.
        let winner = {
            let mut sessions = self
                .sessions
                .write()
                .expect("board session registry poisoned");
            if let Some(existing) = sessions.get(project_id) {
                Some(existing.clone())
            } else {
                sessions.insert(project_id.clone(), Arc::clone(&session));
                None
            }
        };
        match winner {
            Some(existing) => Ok(existing.lock().await.document().clone()),
            None => Ok(session.lock().await.document().clone()),
        }
    }

    /// Current snapshot for a project. Unknown projects yield
    /// [`AppHostError::UnknownBoard`].
    pub async fn snapshot(&self, project_id: &ProjectId) -> AppHostResult<BoardDocument> {
        self.session_for(project_id).await?;
        self.load_or_create(project_id).await
    }

    /// Preview a command batch without mutating the board.
    pub async fn preview_batch(
        &self,
        project_id: &ProjectId,
        batch: CommandBatch<BoardCommand>,
    ) -> AppHostResult<CommandResult<BoardChange>> {
        let session = self.session_for(project_id).await?;
        Ok(session.lock().await.preview_batch(&(), batch))
    }

    /// Apply a command batch, persist the board, and emit
    /// `board.changed` when the batch was applied. An applied batch also
    /// appends one `activity.appended` record.
    pub async fn apply_batch(
        &self,
        project_id: &ProjectId,
        batch: CommandBatch<BoardCommand>,
    ) -> AppHostResult<CommandResult<BoardChange>> {
        let session = self.session_for(project_id).await?;
        let mut guard = session.lock().await;
        self.apply_locked(project_id, &mut guard, batch).await
    }

    /// Build and apply one batch under the session lock.
    ///
    /// `build` runs with the current document and the version the batch is
    /// based on: `base_version` when the caller supplied one, otherwise the
    /// version read under the lock. A supplied version that no longer matches
    /// is rejected with [`AppHostError::BoardVersionConflict`] before `build`
    /// runs, so a caller that omits the version never builds against a
    /// snapshot a concurrent edit has already replaced.
    ///
    /// An applied batch persists the board, emits `board.changed` and appends
    /// exactly one activity record through the same path as
    /// [`Self::apply_batch`].
    pub async fn apply_commands_with<F>(
        &self,
        project_id: &ProjectId,
        context: &OperationContext,
        base_version: Option<DocumentVersion>,
        build: F,
    ) -> AppHostResult<CommandResult<BoardChange>>
    where
        F: FnOnce(&BoardDocument, DocumentVersion) -> Vec<BoardCommand> + Send,
    {
        let session = self.session_for(project_id).await?;
        let mut guard = session.lock().await;
        let batch = Self::batch_under_lock(
            project_id,
            context,
            base_version,
            next_batch_id(""),
            &guard,
            build,
        )?;
        self.apply_locked(project_id, &mut guard, batch).await
    }

    /// Apply caller-built commands as one batch under the session lock.
    ///
    /// A thin wrapper over [`Self::apply_commands_with`]: the commands are
    /// already built, so `base_version` only selects the version they apply
    /// to. When it is `None`, the current version is read while the lock is
    /// held, so concurrent callers without an opinion serialize instead of
    /// conflicting. An explicit version that no longer matches is rejected
    /// with [`AppHostError::BoardVersionConflict`] before anything is applied,
    /// persisted, emitted or recorded.
    ///
    /// An applied batch persists the board, emits `board.changed` and appends
    /// exactly one activity record through the same path as
    /// [`Self::apply_batch`].
    pub async fn apply_commands(
        &self,
        project_id: &ProjectId,
        context: &OperationContext,
        base_version: Option<DocumentVersion>,
        commands: Vec<BoardCommand>,
    ) -> AppHostResult<CommandResult<BoardChange>> {
        self.apply_commands_with(
            project_id,
            context,
            base_version,
            move |_document, _base_version| commands,
        )
        .await
    }

    /// Build and preview one batch under the session lock, without mutating
    /// the board.
    ///
    /// `build` sees the same document and chosen version as
    /// [`Self::apply_commands_with`], and a stale explicit `base_version` is
    /// rejected with [`AppHostError::BoardVersionConflict`] the same way. A
    /// batch the board would reject comes back as a rejected result whose
    /// diagnostics explain why.
    pub async fn preview_commands_with<F>(
        &self,
        project_id: &ProjectId,
        context: &OperationContext,
        base_version: Option<DocumentVersion>,
        build: F,
    ) -> AppHostResult<CommandResult<BoardChange>>
    where
        F: FnOnce(&BoardDocument, DocumentVersion) -> Vec<BoardCommand> + Send,
    {
        let session = self.session_for(project_id).await?;
        let guard = session.lock().await;
        let batch = Self::batch_under_lock(
            project_id,
            context,
            base_version,
            next_batch_id("preview-"),
            &guard,
            build,
        )?;
        Ok(guard.preview_batch(&(), batch))
    }

    /// Choose the base version under the session lock and build the batch.
    /// Shared by the apply and preview entry points so they cannot disagree
    /// about which version a caller-supplied or omitted base selects.
    fn batch_under_lock<F>(
        project_id: &ProjectId,
        context: &OperationContext,
        base_version: Option<DocumentVersion>,
        batch_id: CommandBatchId,
        session: &DocumentSession<BoardDocument>,
        build: F,
    ) -> AppHostResult<CommandBatch<BoardCommand>>
    where
        F: FnOnce(&BoardDocument, DocumentVersion) -> Vec<BoardCommand>,
    {
        let current_version = session.version();
        let base_version = match base_version {
            Some(base_version) if base_version != current_version => {
                return Err(AppHostError::BoardVersionConflict {
                    project_id: project_id.clone(),
                    base_version,
                    current_version,
                });
            }
            Some(base_version) => base_version,
            None => current_version,
        };
        let commands = build(session.document(), base_version);
        Ok(CommandBatch::new(
            batch_id,
            context.clone(),
            base_version,
            Timestamp::new(chrono::Utc::now().to_rfc3339()),
            commands,
        ))
    }

    /// Apply, persist, emit and record one batch while the session lock is
    /// held. Both batch entry points share this sequence.
    async fn apply_locked(
        &self,
        project_id: &ProjectId,
        session: &mut DocumentSession<BoardDocument>,
        batch: CommandBatch<BoardCommand>,
    ) -> AppHostResult<CommandResult<BoardChange>> {
        let actor = batch.context().actor().clone();
        let (summary_key, summary) = apply_summary(batch.commands());
        let command_count = batch.commands().len();
        let result = session.apply_batch(&(), batch);
        if result.status() == CommandStatus::Applied {
            self.save_locked(project_id, session).await?;
            self.emit_changed(session);
            self.record_board_activity(
                project_id,
                ActivityKind::Applied,
                actor,
                summary_key,
                summary,
                json!({ "count": command_count }),
                result.changes(),
                result.history_entry_id().cloned(),
            )
            .await;
        }
        Ok(result)
    }

    /// Append a single item to the board under session lock.
    ///
    /// Reads the current document version while holding the session lock so
    /// concurrent additions on the same board do not conflict on version. An
    /// applied result names the session history entry for the append through
    /// [`CommandResult::history_entry_id`], so callers that wrap this append
    /// in an operation can report the entry their activity record must undo.
    pub async fn append_item(
        &self,
        project_id: &ProjectId,
        context: &OperationContext,
        item_id: BoardItemId,
        kind: BoardItemKind,
        position: BoardItemPosition,
        size: BoardItemSize,
    ) -> AppHostResult<CommandResult<BoardChange>> {
        let session = self.session_for(project_id).await?;
        let mut guard = session.lock().await;
        let base_version = guard.document().version();
        let batch_id = CommandBatchId::new(format!("batch-add-item-{}", item_id.as_str()));
        let batch = CommandBatch::new(
            batch_id,
            context.clone(),
            base_version,
            Timestamp::new(chrono::Utc::now().to_rfc3339()),
            vec![BoardCommand::AddItem {
                item_id,
                kind,
                position,
                size,
            }],
        );
        let result = guard.apply_batch(&(), batch);
        if result.status() == CommandStatus::Applied {
            self.save_locked(project_id, &guard).await?;
            self.emit_changed(&guard);
        }
        Ok(result)
    }

    /// Undo the last applied batch, persisting the restored document.
    ///
    /// `actor` is the operator asking for the undo; it is recorded in the
    /// activity log so a future agent path can pass an agent actor instead of
    /// the UI's human actor. When `expected` names the entry the caller
    /// believes is on top, a mismatch rejects the request with
    /// [`AppHostError::BoardHistoryMismatch`] before the board changes;
    /// `None` keeps the plain "undo the current top" behavior.
    pub async fn undo(
        &self,
        project_id: &ProjectId,
        actor: CommandActor,
        expected: Option<&HistoryEntryId>,
    ) -> AppHostResult<Option<CommandResult<BoardChange>>> {
        let session = self.session_for(project_id).await?;
        let mut guard = session.lock().await;
        let current_undo = guard.undo_entry_id().cloned();
        let current_redo = guard.redo_entry_id().cloned();
        if let Some(expected) = expected
            && current_undo.as_ref() != Some(expected)
        {
            return Err(AppHostError::BoardHistoryMismatch {
                project_id: project_id.clone(),
                action: "undo",
                expected_entry_id: expected.clone(),
                undo_entry_id: current_undo,
                redo_entry_id: current_redo,
            });
        }
        let Some(result) = guard.undo() else {
            return Ok(None);
        };
        self.save_locked(project_id, &guard).await?;
        self.emit_changed(&guard);
        self.record_board_activity(
            project_id,
            ActivityKind::Undone,
            actor,
            "activity.board.undo",
            "Undid the last board change",
            json!({}),
            result.changes(),
            result.history_entry_id().cloned(),
        )
        .await;
        Ok(Some(result))
    }

    /// Redo the last undone batch, persisting the restored document.
    ///
    /// `actor` is recorded in the activity log, as for [`Self::undo`], and
    /// `expected` guards the redo stack's top the same way.
    pub async fn redo(
        &self,
        project_id: &ProjectId,
        actor: CommandActor,
        expected: Option<&HistoryEntryId>,
    ) -> AppHostResult<Option<CommandResult<BoardChange>>> {
        let session = self.session_for(project_id).await?;
        let mut guard = session.lock().await;
        let current_undo = guard.undo_entry_id().cloned();
        let current_redo = guard.redo_entry_id().cloned();
        if let Some(expected) = expected
            && current_redo.as_ref() != Some(expected)
        {
            return Err(AppHostError::BoardHistoryMismatch {
                project_id: project_id.clone(),
                action: "redo",
                expected_entry_id: expected.clone(),
                undo_entry_id: current_undo,
                redo_entry_id: current_redo,
            });
        }
        let Some(result) = guard.redo() else {
            return Ok(None);
        };
        self.save_locked(project_id, &guard).await?;
        self.emit_changed(&guard);
        self.record_board_activity(
            project_id,
            ActivityKind::Redone,
            actor,
            "activity.board.redo",
            "Redid the last board change",
            json!({}),
            result.changes(),
            result.history_entry_id().cloned(),
        )
        .await;
        Ok(Some(result))
    }

    /// Drop the in-memory session for a project (project deletion).
    pub fn remove_project(&self, project_id: &ProjectId) {
        self.sessions
            .write()
            .expect("board session registry poisoned")
            .remove(project_id);
    }

    async fn session_for(
        &self,
        project_id: &ProjectId,
    ) -> AppHostResult<Arc<AsyncMutex<DocumentSession<BoardDocument>>>> {
        self.load_or_create(project_id).await?;
        self.sessions
            .read()
            .expect("board session registry poisoned")
            .get(project_id)
            .cloned()
            .ok_or_else(|| AppHostError::UnknownBoard {
                project_id: project_id.clone(),
            })
    }

    fn emit_changed(&self, session: &DocumentSession<BoardDocument>) {
        let board = session.document();
        self.event_bus.publish(Event::Document {
            kind: "board.changed".to_owned(),
            project_id: board.project_id().to_string(),
            document_id: board.id().to_string(),
            version: board.version().get(),
        });
    }

    /// Append an activity record, never failing the board operation: a broken
    /// activity log must not undo an already persisted board change.
    #[allow(clippy::too_many_arguments)]
    async fn record_board_activity(
        &self,
        project_id: &ProjectId,
        kind: ActivityKind,
        actor: CommandActor,
        summary_key: impl Into<String>,
        summary: impl Into<String>,
        summary_params: serde_json::Value,
        changes: &[BoardChange],
        history_entry_id: Option<HistoryEntryId>,
    ) {
        let Some(activity) = &self.activity else {
            return;
        };

        // One entry per touched item, in change order.
        let mut targets: Vec<String> = Vec::new();
        for change in changes {
            let item_id = change.item_id().to_string();
            if !targets.contains(&item_id) {
                targets.push(item_id);
            }
        }

        if let Err(error) = activity
            .record(
                project_id,
                kind,
                actor,
                summary,
                summary_key,
                summary_params,
                targets,
                history_entry_id,
            )
            .await
        {
            tracing::warn!(
                project_id = %project_id,
                ?kind,
                %error,
                "failed to append board activity record"
            );
        }
    }

    async fn save_locked(
        &self,
        project_id: &ProjectId,
        session: &DocumentSession<BoardDocument>,
    ) -> AppHostResult<()> {
        let path = self.board_path(project_id);
        write_document(&path, session.document(), "board").await
    }
}

/// Generate a batch id without a random-number dependency: a nanosecond
/// timestamp plus a process-wide counter, the same scheme the other services
/// use for generated ids. `prefix` distinguishes batch kinds; preview batches
/// never enter history, so their id only correlates one preview call's
/// diagnostics.
fn next_batch_id(prefix: &str) -> CommandBatchId {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    CommandBatchId::new(format!("batch-board-{prefix}{nanos:x}-{count:x}"))
}

/// Summary key and English fallback for an applied board batch.
fn apply_summary(commands: &[BoardCommand]) -> (&'static str, String) {
    let count = commands.len();
    let all_adds = commands
        .iter()
        .all(|command| matches!(command, BoardCommand::AddItem { .. }));
    let all_notes = commands.iter().all(|command| {
        matches!(
            command,
            BoardCommand::AddItem {
                kind: BoardItemKind::Note { .. },
                ..
            }
        )
    });

    if all_adds && all_notes {
        (
            "activity.board.addNote",
            match count {
                1 => "Added a note".to_owned(),
                _ => format!("Added {count} notes"),
            },
        )
    } else if all_adds {
        (
            "activity.board.addItem",
            match count {
                1 => "Added an item".to_owned(),
                _ => format!("Added {count} items"),
            },
        )
    } else {
        (
            "activity.board.apply",
            match count {
                1 => "Applied a board change".to_owned(),
                _ => format!("Applied {count} board changes"),
            },
        )
    }
}

fn default_board(project_id: &ProjectId) -> BoardDocument {
    BoardDocument::new(
        BoardId::new(format!("board-{project_id}")),
        project_id.clone(),
        DocumentVersion::new(0),
    )
}

/// Ensure the board file exists on disk without loading a session.
/// Used by project creation so a fresh project always has its canvas
/// document from the first write.
pub(crate) async fn ensure_board_file(
    paths: &AppPaths,
    project_id: &ProjectId,
) -> AppHostResult<()> {
    let path = paths.project_dir(project_id.as_str()).join("board.json");
    if path.exists() {
        return Ok(());
    }
    write_document(&path, &default_board(project_id), "board").await
}
