//! One projection policy from the operation table to MCP tools.
//!
//! `tools/list` and `tools/call` both go through [`project`], so a tool that
//! was listed is exactly the set that can be called, even if the table or an
//! operation's health changes between the two requests.
//!
//! Model APIs restrict tool names to `^[a-zA-Z0-9_-]{1,64}$`, so the
//! operation separator `.` is projected to `__`. Operation names may not
//! contain `__` themselves, which keeps the mapping reversible.

use std::collections::HashMap;
use std::sync::Arc;

use reimagine_app_host::OperationInfo;
use rmcp::model::{Tool, ToolAnnotations};
use serde_json::{Map, Value};

use crate::AdapterError;

/// The one operation the adapter never exposes.
///
/// It reads files from arbitrary host paths; until the agent's file boundary
/// is decided, agents get board and asset operations but not this one.
pub const EXCLUDED_OPERATION: &str = "asset.import";

/// A projected tool list plus the call-time lookup derived from the same pass.
#[derive(Debug, Clone)]
pub struct Projection {
    tools: Vec<Tool>,
    operations: HashMap<String, String>,
}

impl Projection {
    /// Every tool to advertise.
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    /// The operation behind a tool name, present exactly when it was listed.
    pub fn operation_for(&self, tool: &str) -> Option<&str> {
        self.operations.get(tool).map(String::as_str)
    }
}

/// Project the available part of the operation table.
///
/// Filters out unavailable operations and [`EXCLUDED_OPERATION`], and rejects
/// an operation whose name contains `__` or whose tool name is a duplicate,
/// because either would make the call-time mapping ambiguous.
pub fn project(infos: &[OperationInfo]) -> Result<Projection, AdapterError> {
    let mut tools = Vec::new();
    let mut operations = HashMap::new();

    for info in infos {
        let operation = info.spec.name.as_str();
        if !info.health.is_available() || operation == EXCLUDED_OPERATION {
            continue;
        }
        if operation.contains("__") {
            return Err(AdapterError::Projection(format!(
                "operation `{operation}` contains `__`, which the tool-name mapping reserves"
            )));
        }
        let tool_name = operation.replace('.', "__");
        if operations.contains_key(&tool_name) {
            return Err(AdapterError::Projection(format!(
                "operations collide on tool name `{tool_name}`"
            )));
        }

        let input_schema: Arc<Map<String, Value>> =
            Arc::new(as_object(&info.spec.input_schema, operation, "input")?);
        let output_schema = Arc::new(as_object(&info.spec.output_schema, operation, "output")?);

        let mut annotations = ToolAnnotations::new();
        annotations.read_only_hint = Some(info.spec.annotations.read_only);
        annotations.destructive_hint = Some(info.spec.annotations.destructive);
        annotations.idempotent_hint = Some(info.spec.annotations.idempotent);
        annotations.open_world_hint = Some(info.spec.annotations.open_world);

        let tool = Tool::new(
            tool_name.clone(),
            info.spec.description.clone(),
            input_schema,
        )
        .with_raw_output_schema(output_schema)
        .with_annotations(annotations);

        operations.insert(tool_name, operation.to_owned());
        tools.push(tool);
    }

    Ok(Projection { tools, operations })
}

fn as_object(
    schema: &Value,
    operation: &str,
    direction: &str,
) -> Result<Map<String, Value>, AdapterError> {
    schema.as_object().cloned().ok_or_else(|| {
        AdapterError::Projection(format!(
            "operation `{operation}` has a non-object {direction} schema"
        ))
    })
}

/// Test-only OperationInfo constructors shared by this module's tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use mod_sdk::{OperationAnnotations, OperationSpec};
    use reimagine_app_host::{OperationInfo, OperationProvider};
    use reimagine_core::diagnostic::{
        Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
        DiagnosticTargetDomain,
    };
    use reimagine_core::health::Health;
    use reimagine_core::model::DiagnosticId;
    use serde_json::{Value, json};

    pub(crate) fn info(name: &str, health: Health) -> OperationInfo {
        OperationInfo {
            spec: OperationSpec {
                name: name.to_owned(),
                description: format!("Operation {name}"),
                input_schema: json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "additionalProperties": false
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": { "ok": { "type": "boolean" } },
                    "additionalProperties": false
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: true,
                    idempotent: false,
                    deterministic: true,
                    open_world: false,
                },
            },
            provider: OperationProvider::Kernel,
            health,
        }
    }

    pub(crate) fn unavailable() -> Health {
        Health::Unavailable(Diagnostic::new(
            DiagnosticId::new("test:unavailable"),
            DiagnosticCode::new("TEST/UNAVAILABLE"),
            DiagnosticSeverity::Error,
            DiagnosticSourceName::new("test"),
            "operation is down",
            DiagnosticTarget::new(DiagnosticTargetDomain::new("op.test")),
        ))
    }

    pub(crate) fn value(schema: &serde_json::Map<String, Value>) -> Value {
        Value::Object(schema.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{info, unavailable, value};
    use super::*;
    use reimagine_core::health::Health;

    #[test]
    fn names_roundtrip_and_schemas_and_hints_are_copied() {
        let projection = project(&[
            info("board.add_notes", Health::Available),
            info("board.preview", Health::Available),
        ])
        .expect("project");

        let add = projection
            .tools()
            .iter()
            .find(|tool| tool.name == "board__add_notes")
            .expect("projected name");
        assert_eq!(
            projection.operation_for("board__add_notes"),
            Some("board.add_notes")
        );
        assert_eq!(
            projection.operation_for("board__preview"),
            Some("board.preview")
        );
        assert_eq!(projection.operation_for("board.add_notes"), None);
        assert_eq!(
            add.description.as_deref(),
            Some("Operation board.add_notes")
        );
        assert_eq!(
            value(add.input_schema.as_ref()),
            info("board.add_notes", Health::Available).spec.input_schema
        );
        assert_eq!(
            value(add.output_schema.as_ref().expect("output schema")),
            info("board.add_notes", Health::Available)
                .spec
                .output_schema
        );
        let hints = add.annotations.as_ref().expect("annotations");
        assert_eq!(hints.read_only_hint, Some(false));
        assert_eq!(hints.destructive_hint, Some(true));
        assert_eq!(hints.idempotent_hint, Some(false));
        assert_eq!(hints.open_world_hint, Some(false));
    }

    #[test]
    fn asset_import_is_not_exposed() {
        let projection = project(&[
            info("asset.import", Health::Available),
            info("asset.list", Health::Available),
        ])
        .expect("project");
        assert!(
            projection
                .tools()
                .iter()
                .all(|tool| tool.name != "asset__import")
        );
        assert_eq!(projection.operation_for("asset__import"), None);
        assert_eq!(projection.operation_for("asset__list"), Some("asset.list"));
    }

    #[test]
    fn unavailable_operations_are_not_exposed() {
        let projection = project(&[
            info("board.read", Health::Available),
            info("text.replace", unavailable()),
        ])
        .expect("project");
        assert_eq!(projection.operation_for("board__read"), Some("board.read"));
        assert_eq!(projection.operation_for("text__replace"), None);
    }

    #[test]
    fn reserved_separator_is_rejected() {
        let error = project(&[info("board__add_notes", Health::Available)])
            .expect_err("a name with __ must be rejected");
        assert!(matches!(error, AdapterError::Projection(_)));
        assert!(error.to_string().contains("board__add_notes"));
    }

    #[test]
    fn duplicate_tool_names_are_rejected() {
        let error = project(&[
            info("board.add_notes", Health::Available),
            info("board.add_notes", Health::Available),
        ])
        .expect_err("duplicate tool names must be rejected");
        assert!(matches!(error, AdapterError::Projection(_)));
        assert!(error.to_string().contains("board__add_notes"));
    }

    #[test]
    fn non_object_schemas_are_rejected() {
        let mut op = info("board.add_notes", Health::Available);
        op.spec.input_schema = serde_json::json!(true);
        let error = project(&[op]).expect_err("non-object schema must be rejected");
        assert!(error.to_string().contains("non-object input schema"));
    }
}
