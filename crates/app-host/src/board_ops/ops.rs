//! The six board kernel operations and their registration.

use std::sync::Arc;

use async_trait::async_trait;
use mod_sdk::OperationSpec;
use reimagine_core::board::BoardDocument;
use reimagine_core::command::CommandStatus;
use reimagine_core::diagnostic::Diagnostic;
use reimagine_core::model::DocumentVersion;
use serde_json::{Value, json};

use crate::board_service::BoardService;
use crate::error::AppHostError;
use crate::operation_service::{KernelCall, KernelOp, OperationService};

use super::board_caller_error;
use super::board_internal_error;
use super::commands::{diagnostics_json, parse, touched_item_ids, validate_mutator_input};
use super::specs::{BoardMutator, preview_spec, read_spec};

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

        let mutator = BoardMutator::from_name(operation).ok_or_else(|| {
            board_caller_error(
                OP,
                "INVALID_INPUT",
                format!("`{operation}` is not a board mutator"),
            )
        })?;
        validate_mutator_input(OP, &mutator.spec(), input)?;

        let plan = mutator.parse_body(input, OP)?;
        let base_version = plan.base_version();
        let result = self
            .board_service
            .preview_commands_with(
                &call.project_id,
                &call.context,
                base_version,
                move |document, base_version| plan.materialize(document, base_version),
            )
            .await
            .map_err(|error| batch_error(OP, "PREVIEW_FAILED", error))?;

        Ok(json!({
            "status": result.status(),
            "changes": result.changes(),
            "version": result.version().get(),
            "diagnostics": diagnostics_json(result.diagnostics()),
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
        let plan = parse(operation, &call.caller_input, operation)?;
        let base_version = plan.base_version();
        let result = self
            .board_service
            .apply_commands_with(
                &call.project_id,
                &call.context,
                base_version,
                move |document, base_version| plan.materialize(document, base_version),
            )
            .await
            .map_err(|error| batch_error(operation, "APPLY_FAILED", error))?;

        if result.status() == CommandStatus::Rejected {
            return Err(result.diagnostics().first().cloned().unwrap_or_else(|| {
                board_caller_error(operation, "REJECTED", "board batch was rejected")
            }));
        }

        let board_item_ids = touched_item_ids(result.changes());
        let mut output = json!({
            "version": result.version().get(),
            "history_entry_id": result.history_entry_id().map(ToString::to_string),
            "board_item_ids": board_item_ids.clone(),
        });
        if operation == "board.add_notes" {
            // Every command in an add_notes batch adds one note, so its
            // touched ids are exactly the generated note ids in input order.
            output["item_ids"] = json!(board_item_ids);
        }
        Ok(output)
    }
}

/// Register all board operations with an [`OperationService`].
pub fn register_board_ops(op_service: &OperationService, board_service: Arc<BoardService>) {
    op_service.register_kernel_op(Arc::new(BoardReadOp::new(Arc::clone(&board_service))));
    op_service.register_kernel_op(Arc::new(BoardPreviewOp::new(Arc::clone(&board_service))));
    for mutator in BoardMutator::ALL {
        op_service.register_kernel_op(Arc::new(BoardMutatorOp::new(
            Arc::clone(&board_service),
            mutator.spec(),
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

fn batch_error(op_name: &str, failure: &str, error: AppHostError) -> Diagnostic {
    match error {
        AppHostError::BoardVersionConflict {
            base_version,
            current_version,
            ..
        } => version_conflict_diagnostic(op_name, base_version, current_version),
        other => board_internal_error(op_name, failure, other.to_string()),
    }
}

fn version_conflict_diagnostic(
    op_name: &str,
    base_version: DocumentVersion,
    current_version: DocumentVersion,
) -> Diagnostic {
    board_caller_error(
        op_name,
        "VERSION_CONFLICT",
        format!(
            "board version conflict: the batch is based on version {base_version}, but the \
             board is at version {current_version}; call board.read first and retry with the \
             current version"
        ),
    )
}
