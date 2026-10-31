//! The six board kernel operations and their registration.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use mod_sdk::OperationSpec;
use reimagine_core::board::BoardDocument;
use reimagine_core::command::{CommandBatch, CommandStatus};
use reimagine_core::diagnostic::Diagnostic;
use reimagine_core::event::Timestamp;
use reimagine_core::model::CommandBatchId;
use serde_json::{Value, json};

use crate::board_service::BoardService;
use crate::error::AppHostError;
use crate::operation_service::{KernelCall, KernelOp, OperationService};

use super::board_caller_error;
use super::board_internal_error;
use super::commands::{
    build_commands, caller_base_version, touched_item_ids, validate_mutator_input,
};
use super::specs::{
    add_notes_spec, arrange_spec, mutator_spec, preview_spec, read_spec, remove_items_spec,
    update_notes_spec,
};

/// The built-in `board.read` kernel operation.
pub struct BoardReadOp {
    spec: OperationSpec,
    board_service: Arc<BoardService>,
}

impl BoardReadOp {
    pub fn new(board_service: Arc<BoardService>) -> Self {
        Self {
            spec: read_spec(),
            board_service,
        }
    }
}

#[async_trait]
impl KernelOp for BoardReadOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let board = self
            .board_service
            .snapshot(&call.project_id)
            .await
            .map_err(|error| {
                board_internal_error("board.read", "SNAPSHOT_FAILED", error.to_string())
            })?;
        Ok(snapshot_json(&board))
    }
}

/// The built-in `board.preview` kernel operation.
pub struct BoardPreviewOp {
    spec: OperationSpec,
    board_service: Arc<BoardService>,
}

impl BoardPreviewOp {
    pub fn new(board_service: Arc<BoardService>) -> Self {
        Self {
            spec: preview_spec(),
            board_service,
        }
    }
}

#[async_trait]
impl KernelOp for BoardPreviewOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        const OP: &str = "board.preview";
        let operation = call
            .caller_input
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| board_caller_error(OP, "INVALID_INPUT", "missing 'operation' string"))?;
        let input = call
            .caller_input
            .get("input")
            .ok_or_else(|| board_caller_error(OP, "INVALID_INPUT", "missing 'input' object"))?;

        let spec = mutator_spec(operation).ok_or_else(|| {
            board_caller_error(
                OP,
                "INVALID_INPUT",
                format!("`{operation}` is not a board mutator"),
            )
        })?;
        validate_mutator_input(OP, &spec, input)?;

        let board = self
            .board_service
            .snapshot(&call.project_id)
            .await
            .map_err(|error| board_internal_error(OP, "SNAPSHOT_FAILED", error.to_string()))?;
        let base_version = caller_base_version(input, OP)?.unwrap_or_else(|| board.version());
        let prepared = build_commands(operation, input, base_version, &board, OP)?;
        let batch = CommandBatch::new(
            next_preview_batch_id(),
            call.context.clone(),
            base_version,
            Timestamp::new(chrono::Utc::now().to_rfc3339()),
            prepared.commands,
        );
        let result = self
            .board_service
            .preview_batch(&call.project_id, batch)
            .await
            .map_err(|error| board_internal_error(OP, "PREVIEW_FAILED", error.to_string()))?;

        Ok(json!({
            "status": result.status(),
            "changes": result.changes(),
            "version": result.version().get(),
        }))
    }
}

/// One of the four board mutators. They differ only in their spec; the
/// shared builder turns the caller input into commands.
pub struct BoardMutatorOp {
    spec: OperationSpec,
    board_service: Arc<BoardService>,
}

impl BoardMutatorOp {
    pub fn new(board_service: Arc<BoardService>, spec: OperationSpec) -> Self {
        Self {
            spec,
            board_service,
        }
    }
}

#[async_trait]
impl KernelOp for BoardMutatorOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    fn records_own_activity(&self) -> bool {
        true
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let operation = self.spec.name.as_str();
        let board = self
            .board_service
            .snapshot(&call.project_id)
            .await
            .map_err(|error| {
                board_internal_error(operation, "SNAPSHOT_FAILED", error.to_string())
            })?;
        let base_version =
            caller_base_version(&call.caller_input, operation)?.unwrap_or_else(|| board.version());
        let prepared = build_commands(
            operation,
            &call.caller_input,
            base_version,
            &board,
            operation,
        )?;

        let result = self
            .board_service
            .apply_commands(
                &call.project_id,
                &call.context,
                Some(base_version),
                prepared.commands,
            )
            .await
            .map_err(|error| apply_error(operation, error))?;

        if result.status() == CommandStatus::Rejected {
            return Err(result.diagnostics().first().cloned().unwrap_or_else(|| {
                board_caller_error(operation, "REJECTED", "board batch was rejected")
            }));
        }

        let mut output = json!({
            "version": result.version().get(),
            "history_entry_id": result.history_entry_id().map(ToString::to_string),
            "board_item_ids": touched_item_ids(result.changes()),
        });
        if operation == "board.add_notes" {
            output["item_ids"] = json!(prepared.item_ids);
        }
        Ok(output)
    }
}

/// Register all board operations with an [`OperationService`].
pub fn register_board_ops(op_service: &OperationService, board_service: Arc<BoardService>) {
    op_service.register_kernel_op(Arc::new(BoardReadOp::new(Arc::clone(&board_service))));
    op_service.register_kernel_op(Arc::new(BoardPreviewOp::new(Arc::clone(&board_service))));
    for spec in [
        add_notes_spec(),
        update_notes_spec(),
        arrange_spec(),
        remove_items_spec(),
    ] {
        op_service.register_kernel_op(Arc::new(BoardMutatorOp::new(
            Arc::clone(&board_service),
            spec,
        )));
    }
}

fn snapshot_json(board: &BoardDocument) -> Value {
    let items: Vec<Value> = board
        .items()
        .values()
        .map(|item| {
            json!({
                "id": item.id().as_str(),
                "kind": item.kind(),
                "position": { "x": item.position().x, "y": item.position().y },
                "size": {
                    "width": item.size().width,
                    "height": item.size().height
                },
                "z": item.z(),
                "locked": item.is_locked(),
            })
        })
        .collect();
    json!({ "version": board.version().get(), "items": items })
}

fn apply_error(op_name: &str, error: AppHostError) -> Diagnostic {
    match error {
        AppHostError::BoardVersionConflict {
            base_version,
            current_version,
            ..
        } => board_caller_error(
            op_name,
            "VERSION_CONFLICT",
            format!(
                "board version conflict: the batch is based on version {base_version}, but the \
                 board is at version {current_version}; call board.read first and retry with the \
                 current version"
            ),
        ),
        other => board_internal_error(op_name, "APPLY_FAILED", other.to_string()),
    }
}

/// Generate a preview batch id without a random-number dependency.
fn next_preview_batch_id() -> CommandBatchId {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    CommandBatchId::new(format!("batch-board-preview-{nanos:x}-{count:x}"))
}
