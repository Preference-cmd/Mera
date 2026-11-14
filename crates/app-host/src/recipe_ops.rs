#![allow(clippy::result_large_err)]
//! Kernel operations for slot recipes:
//! - `recipe.get`
//! - `recipe.edit`

use std::sync::Arc;

use async_trait::async_trait;
use mod_sdk::{OperationAnnotations, OperationSpec};
use reimagine_core::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
    DiagnosticTargetDomain,
};
use reimagine_core::model::{CollectionId, DiagnosticId, SlotId, StepId};
use serde_json::{Value, json};

use crate::operation_service::{KernelCall, KernelOp, OperationService};
use crate::recipe_service::{RecipeService, recipe_snapshot_to_json};

fn recipe_caller_error(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(
        DiagnosticId::new(format!("op:recipe:{code}")),
        DiagnosticCode::new(format!("RECIPE/CALLER_{code}")),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("recipe_ops"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new("op.recipe")),
    )
}

/// The built-in `recipe.get` kernel operation.
pub struct RecipeGetOp {
    spec: OperationSpec,
    recipe_service: Arc<RecipeService>,
}

impl RecipeGetOp {
    pub fn new(recipe_service: Arc<RecipeService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "recipe.get".to_owned(),
                description: "Inspect the operation recipe and stale steps for a collection slot"
                    .to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "collection_id": { "type": "string" },
                        "slot_id": { "type": "string" }
                    },
                    "required": ["collection_id", "slot_id"],
                    "additionalProperties": false,
                    "examples": [{ "collection_id": "col-1", "slot_id": "slot-1" }]
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "collection_id": { "type": "string" },
                        "slot_id": { "type": "string" },
                        "root_asset": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "media_type": { "type": "string" },
                                "size": { "type": "integer", "minimum": 0 }
                            },
                            "required": ["id", "media_type", "size"],
                            "additionalProperties": false
                        },
                        "current_asset": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "media_type": { "type": "string" },
                                "size": { "type": "integer", "minimum": 0 }
                            },
                            "required": ["id", "media_type", "size"],
                            "additionalProperties": false
                        },
                        "stale": { "type": "boolean" },
                        "steps": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "step_id": { "type": "string" },
                                    "op": { "type": "string" },
                                    "op_version": { "type": "string" },
                                    "input": { "type": "object" },
                                    "input_sources": { "type": "object" },
                                    "output": { "type": ["object", "null"] },
                                    "actor": { "type": "object" },
                                    "read_only": { "type": "boolean" },
                                    "stale": { "type": "boolean" }
                                },
                                "required": [
                                    "step_id",
                                    "op",
                                    "op_version",
                                    "input",
                                    "input_sources",
                                    "output",
                                    "actor",
                                    "read_only",
                                    "stale"
                                ],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": [
                        "collection_id",
                        "slot_id",
                        "root_asset",
                        "current_asset",
                        "stale",
                        "steps"
                    ],
                    "additionalProperties": false
                }),
                annotations: OperationAnnotations {
                    read_only: true,
                    destructive: false,
                    idempotent: true,
                    deterministic: false,
                    open_world: false,
                },
            },
            recipe_service,
        }
    }
}

#[async_trait]
impl KernelOp for RecipeGetOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let col_str = call
            .caller_input
            .get("collection_id")
            .and_then(Value::as_str)
            .ok_or_else(|| recipe_caller_error("INVALID_INPUT", "missing 'collection_id'"))?;
        let slot_str = call
            .caller_input
            .get("slot_id")
            .and_then(Value::as_str)
            .ok_or_else(|| recipe_caller_error("INVALID_INPUT", "missing 'slot_id'"))?;

        let collection_id = CollectionId::try_new(col_str)
            .map_err(|e| recipe_caller_error("INVALID_COLLECTION_ID", e))?;
        let slot_id =
            SlotId::try_new(slot_str).map_err(|e| recipe_caller_error("INVALID_SLOT_ID", e))?;

        let snapshot = self
            .recipe_service
            .get_recipe(&call.project_id, &collection_id, &slot_id)
            .await?;

        Ok(recipe_snapshot_to_json(&snapshot))
    }
}

/// The built-in `recipe.edit` kernel operation.
pub struct RecipeEditOp {
    spec: OperationSpec,
    recipe_service: Arc<RecipeService>,
}

impl RecipeEditOp {
    pub fn new(recipe_service: Arc<RecipeService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "recipe.edit".to_owned(),
                description: "Edit a step in a slot's recipe and replay downstream steps"
                    .to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "collection_id": { "type": "string" },
                        "slot_id": { "type": "string" },
                        "step_id": { "type": "string" },
                        "input": { "type": "object" }
                    },
                    "required": ["collection_id", "slot_id", "step_id", "input"],
                    "additionalProperties": false
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "version": { "type": "integer", "minimum": 0 },
                        "asset_id": { "type": "string" },
                        "stale": { "type": "boolean" },
                        "replayed_steps": { "type": "integer", "minimum": 0 },
                        "stale_steps": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["version", "asset_id", "stale", "replayed_steps", "stale_steps"],
                    "additionalProperties": false
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: false,
                    idempotent: false,
                    deterministic: false,
                    open_world: false,
                },
            },
            recipe_service,
        }
    }
}

#[async_trait]
impl KernelOp for RecipeEditOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let col_str = call
            .caller_input
            .get("collection_id")
            .and_then(Value::as_str)
            .ok_or_else(|| recipe_caller_error("INVALID_INPUT", "missing 'collection_id'"))?;
        let slot_str = call
            .caller_input
            .get("slot_id")
            .and_then(Value::as_str)
            .ok_or_else(|| recipe_caller_error("INVALID_INPUT", "missing 'slot_id'"))?;
        let step_str = call
            .caller_input
            .get("step_id")
            .and_then(Value::as_str)
            .ok_or_else(|| recipe_caller_error("INVALID_INPUT", "missing 'step_id'"))?;
        let input = call
            .caller_input
            .get("input")
            .filter(|v| v.is_object())
            .ok_or_else(|| recipe_caller_error("INVALID_INPUT", "missing 'input' object"))?;

        let collection_id = CollectionId::try_new(col_str)
            .map_err(|e| recipe_caller_error("INVALID_COLLECTION_ID", e))?;
        let slot_id =
            SlotId::try_new(slot_str).map_err(|e| recipe_caller_error("INVALID_SLOT_ID", e))?;
        let step_id =
            StepId::try_new(step_str).map_err(|e| recipe_caller_error("INVALID_STEP_ID", e))?;

        let outcome = self
            .recipe_service
            .edit_recipe(
                &call.project_id,
                &collection_id,
                &slot_id,
                &step_id,
                input,
                &call.context,
                &call.invocation.cancel,
            )
            .await?;

        Ok(json!({
            "version": outcome.version,
            "asset_id": outcome.asset_id.as_str(),
            "stale": outcome.stale,
            "replayed_steps": outcome.replayed_steps,
            "stale_steps": outcome.stale_steps,
        }))
    }
}

/// Register the built-in recipe kernel operations with `op_service`.
pub fn register_recipe_ops(op_service: &OperationService, recipe_service: Arc<RecipeService>) {
    op_service.register_kernel_op(Arc::new(RecipeGetOp::new(Arc::clone(&recipe_service))));
    op_service.register_kernel_op(Arc::new(RecipeEditOp::new(recipe_service)));
}
