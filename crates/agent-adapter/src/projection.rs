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

use mera_app_host::{OperationInfo, agent_may_invoke};
use rmcp::model::{Tool, ToolAnnotations};
use serde_json::{Map, Value};

use crate::AdapterError;

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
/// Every operation name is validated before health and policy filtering, so
/// an invalid name fails the projection even when that operation would not be
/// listed. The mapped tool name must match `^[A-Za-z0-9_-]{1,64}$`, an
/// operation name may not contain `__`, and two operations may not map to the
/// same tool name; otherwise the call-time mapping would be ambiguous or
/// unusable. Operations that are unavailable or denied to agents by
/// [`agent_may_invoke`] are then filtered out.
pub fn project(infos: &[OperationInfo]) -> Result<Projection, AdapterError> {
    let mut tool_names: HashMap<String, &str> = HashMap::new();
    for info in infos {
        let operation = info.spec.name.as_str();
        if operation.contains("__") {
            return Err(AdapterError::Projection(format!(
                "operation `{operation}` contains `__`, which the tool-name mapping reserves"
            )));
        }
        let tool_name = operation.replace('.', "__");
        if !is_valid_tool_name(&tool_name) {
            return Err(AdapterError::Projection(format!(
                "operation `{operation}` maps to tool name `{tool_name}`, which must match \
                 ^[A-Za-z0-9_-]{{1,64}}$"
            )));
        }
        if let Some(previous) = tool_names.insert(tool_name.clone(), operation) {
            return Err(AdapterError::Projection(format!(
                "operations `{previous}` and `{operation}` collide on tool name `{tool_name}`"
            )));
        }
    }

    let mut tools = Vec::new();
    let mut operations = HashMap::new();

    for info in infos {
        let operation = info.spec.name.as_str();
        if !info.health.is_available() || !info.agent_permitted || !agent_may_invoke(operation) {
            continue;
        }
        let tool_name = operation.replace('.', "__");

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

/// Whether a mapped tool name satisfies the model-facing name grammar.
fn is_valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
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
    use mera_app_host::{OperationInfo, OperationProvider};
    use mera_core::diagnostic::{
        Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
        DiagnosticTargetDomain,
    };
    use mera_core::health::Health;
    use mera_core::model::DiagnosticId;
    use mod_sdk::{OperationAnnotations, OperationSpec};
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
            agent_permitted: true,
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
    use mera_core::health::Health;

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
    fn asset_import_and_list_are_exposed() {
        let projection = project(&[
            info("asset.import", Health::Available),
            info("asset.list", Health::Available),
        ])
        .expect("project");
        assert!(
            projection
                .tools()
                .iter()
                .any(|tool| tool.name == "asset__import")
        );
        assert_eq!(
            projection.operation_for("asset__import"),
            Some("asset.import")
        );
        assert_eq!(projection.operation_for("asset__list"), Some("asset.list"));
    }

    #[test]
    fn dynamic_workflow_resume_is_not_exposed() {
        let projection = project(&[
            info("dynamic_workflow.resume", Health::Available),
            info("dynamic_workflow.run", Health::Available),
        ])
        .expect("project");
        assert!(
            projection
                .tools()
                .iter()
                .all(|tool| tool.name != "dynamic_workflow__resume")
        );
        assert_eq!(projection.operation_for("dynamic_workflow__resume"), None);
        assert_eq!(
            projection.operation_for("dynamic_workflow__run"),
            Some("dynamic_workflow.run")
        );
    }

    #[test]
    fn library_remove_and_purge_are_not_exposed_while_add_and_list_are() {
        let projection = project(&[
            info("library.add", Health::Available),
            info("library.remove", Health::Available),
            info("library.purge", Health::Available),
            info("library.collect", Health::Available),
            info("library.list", Health::Available),
        ])
        .expect("project");
        assert!(
            projection
                .tools()
                .iter()
                .any(|tool| tool.name == "library__add")
        );
        assert!(
            projection
                .tools()
                .iter()
                .all(|tool| tool.name != "library__remove" && tool.name != "library__purge")
        );
        assert_eq!(
            projection.operation_for("library__add"),
            Some("library.add")
        );
        assert_eq!(projection.operation_for("library__remove"), None);
        assert_eq!(projection.operation_for("library__purge"), None);
        assert_eq!(
            projection.operation_for("library__collect"),
            Some("library.collect")
        );
        assert_eq!(
            projection.operation_for("library__list"),
            Some("library.list")
        );
    }

    #[test]
    fn project_folder_mutations_are_not_exposed_while_list_folders_is() {
        let projection = project(&[
            info("project.add_folder", Health::Available),
            info("project.remove_folder", Health::Available),
            info("project.list_folders", Health::Available),
        ])
        .expect("project");
        assert!(projection.tools().iter().all(
            |tool| tool.name != "project__add_folder" && tool.name != "project__remove_folder"
        ));
        assert!(
            projection
                .tools()
                .iter()
                .any(|tool| tool.name == "project__list_folders")
        );
        assert_eq!(projection.operation_for("project__add_folder"), None);
        assert_eq!(projection.operation_for("project__remove_folder"), None);
        assert_eq!(
            projection.operation_for("project__list_folders"),
            Some("project.list_folders")
        );
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
    fn invalid_tool_name_characters_are_rejected() {
        for name in ["board.add notes", "board.add/notes", "board.add:notes"] {
            let error = project(&[info(name, Health::Available)])
                .expect_err("an invalid tool name must be rejected");
            assert!(matches!(error, AdapterError::Projection(_)), "{name}");
            assert!(error.to_string().contains(name), "{error}");
        }
    }

    #[test]
    fn tool_names_longer_than_64_bytes_are_rejected() {
        let name = "a".repeat(65);
        let error = project(&[info(&name, Health::Available)])
            .expect_err("a 65-byte tool name must be rejected");
        assert!(matches!(error, AdapterError::Projection(_)));
        assert!(error.to_string().contains("64"));
    }

    #[test]
    fn hidden_invalid_names_are_validated_before_filtering() {
        let error = project(&[info("board__add_notes", unavailable())])
            .expect_err("a hidden name with __ must still be rejected");
        assert!(matches!(error, AdapterError::Projection(_)));
        assert!(error.to_string().contains("board__add_notes"));
    }

    #[test]
    fn hidden_duplicates_are_validated_before_filtering() {
        let error = project(&[
            info("board.add_notes", Health::Available),
            info("board.add_notes", unavailable()),
        ])
        .expect_err("a hidden duplicate must still be rejected");
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

    #[test]
    fn saved_dynamic_workflow_projects_cleanly() {
        let mut op = info("dynamic_workflow.dwf-1", Health::Available);
        op.provider = mera_app_host::OperationProvider::DynamicWorkflow {
            id: "dwf-1".to_owned(),
        };
        let mut denied_op = info("dynamic_workflow.dwf-2", Health::Available);
        denied_op.provider = mera_app_host::OperationProvider::DynamicWorkflow {
            id: "dwf-2".to_owned(),
        };
        denied_op.agent_permitted = false;

        let projection = project(&[op, denied_op]).expect("project saved dynamic workflow");
        assert_eq!(
            projection.operation_for("dynamic_workflow__dwf-1"),
            Some("dynamic_workflow.dwf-1")
        );
        assert_eq!(projection.operation_for("dynamic_workflow__dwf-2"), None);
    }
}
