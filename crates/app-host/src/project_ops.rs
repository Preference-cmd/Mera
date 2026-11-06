//! Kernel operations for project metadata and workspace folders:
//! - `project.rename`
//! - `project.add_folder`
//! - `project.remove_folder`
//! - `project.list_folders`

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use mod_sdk::{OperationAnnotations, OperationSpec};
use reimagine_core::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
    DiagnosticTargetDomain,
};
use reimagine_core::event::Timestamp;
use reimagine_core::model::{DiagnosticId, WorkspaceFolderId};
use serde_json::{Value, json};

use crate::error::AppHostError;
use crate::operation_service::{KernelCall, KernelOp, OperationService};
use crate::project_service::ProjectService;

fn project_op_caller_error(op_name: &str, code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(
        DiagnosticId::new(format!("op:{op_name}:{code}")),
        DiagnosticCode::new(format!("PROJECT/CALLER_{code}")),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("project_ops"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new(format!("op.{op_name}"))),
    )
}

/// Longest project name `project.rename` accepts, in characters after trimming.
pub const PROJECT_NAME_MAX_CHARS: usize = 120;

/// The built-in `project.rename` kernel operation.
///
/// The name is trimmed before it is stored. An empty name, one longer than
/// [`PROJECT_NAME_MAX_CHARS`], or one containing control characters is a
/// caller error. A rename refreshes the project's `updated_at`.
pub struct ProjectRenameOp {
    spec: OperationSpec,
    project_service: Arc<ProjectService>,
}

impl ProjectRenameOp {
    pub fn new(project_service: Arc<ProjectService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "project.rename".to_owned(),
                description: "Rename the project".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "New display name; surrounding whitespace is trimmed, and the result must be non-empty, at most 120 characters and free of control characters"
                        }
                    },
                    "required": ["name"],
                    "additionalProperties": false,
                    "examples": [{ "name": "Spring campaign" }]
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "updated_at": { "type": "string" }
                    },
                    "required": ["name", "updated_at"],
                    "additionalProperties": false,
                    "examples": [{
                        "name": "Spring campaign",
                        "updated_at": "2026-01-01T00:00:00Z"
                    }]
                }),
                // Each call writes a fresh `updated_at`, like `library.rename`.
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: false,
                    idempotent: false,
                    deterministic: true,
                    open_world: false,
                },
            },
            project_service,
        }
    }
}

#[async_trait]
impl KernelOp for ProjectRenameOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        const OP: &str = "project.rename";
        let name = call
            .caller_input
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| project_op_caller_error(OP, "INVALID_INPUT", "missing 'name'"))?
            .trim();
        if name.is_empty() {
            return Err(project_op_caller_error(
                OP,
                "EMPTY_NAME",
                "project name must not be empty",
            ));
        }
        if name.chars().count() > PROJECT_NAME_MAX_CHARS {
            return Err(project_op_caller_error(
                OP,
                "NAME_TOO_LONG",
                format!("project name must be at most {PROJECT_NAME_MAX_CHARS} characters"),
            ));
        }
        if name.chars().any(char::is_control) {
            return Err(project_op_caller_error(
                OP,
                "INVALID_NAME",
                "project name must not contain control characters",
            ));
        }

        let to_diagnostic = |e: AppHostError| match e {
            AppHostError::Diagnostic(d) => *d,
            other => project_op_caller_error(OP, "RENAME_FAILED", other.to_string()),
        };
        let project = self
            .project_service
            .load_project(&call.project_id)
            .await
            .map_err(to_diagnostic)?;
        let mut metadata = project.metadata().clone();
        metadata.set_name(name);
        metadata.set_updated_at(Timestamp::new(chrono::Utc::now().to_rfc3339()));
        let project = self
            .project_service
            .update_project(&call.project_id, metadata)
            .await
            .map_err(to_diagnostic)?;

        Ok(json!({
            "name": project.metadata().name(),
            "updated_at": project.metadata().updated_at().as_str(),
        }))
    }
}

/// The built-in `project.add_folder` kernel operation.
pub struct ProjectAddFolderOp {
    spec: OperationSpec,
    project_service: Arc<ProjectService>,
}

impl ProjectAddFolderOp {
    pub fn new(project_service: Arc<ProjectService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "project.add_folder".to_owned(),
                description: "Add a workspace folder to the project".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Canonical absolute path to the workspace folder"
                        }
                    },
                    "required": ["path"],
                    "additionalProperties": false,
                    "examples": [{ "path": "/path/to/folder" }]
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "folder": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "path": { "type": "string" }
                            },
                            "required": ["id", "path"],
                            "additionalProperties": false
                        }
                    },
                    "required": ["folder"],
                    "additionalProperties": false,
                    "examples": [{
                        "folder": {
                            "id": "folder-1-1",
                            "path": "/path/to/folder"
                        }
                    }]
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: false,
                    idempotent: false,
                    deterministic: true,
                    open_world: false,
                },
            },
            project_service,
        }
    }
}

#[async_trait]
impl KernelOp for ProjectAddFolderOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let path_str = call
            .caller_input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                project_op_caller_error("project.add_folder", "INVALID_INPUT", "missing 'path'")
            })?;

        let folder = self
            .project_service
            .add_folder(&call.project_id, Path::new(path_str))
            .await
            .map_err(|e| match e {
                AppHostError::Diagnostic(d) => *d,
                other => project_op_caller_error(
                    "project.add_folder",
                    "ADD_FOLDER_FAILED",
                    other.to_string(),
                ),
            })?;

        Ok(json!({
            "folder": {
                "id": folder.id().as_str(),
                "path": folder.path()
            }
        }))
    }
}

/// The built-in `project.remove_folder` kernel operation.
pub struct ProjectRemoveFolderOp {
    spec: OperationSpec,
    project_service: Arc<ProjectService>,
}

impl ProjectRemoveFolderOp {
    pub fn new(project_service: Arc<ProjectService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "project.remove_folder".to_owned(),
                description: "Remove a workspace folder from the project".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "Identifier of the workspace folder to remove"
                        }
                    },
                    "required": ["id"],
                    "additionalProperties": false,
                    "examples": [{ "id": "folder-1-1" }]
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "removed": { "type": "string" }
                    },
                    "required": ["removed"],
                    "additionalProperties": false,
                    "examples": [{ "removed": "folder-1-1" }]
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: false,
                    idempotent: false,
                    deterministic: true,
                    open_world: false,
                },
            },
            project_service,
        }
    }
}

#[async_trait]
impl KernelOp for ProjectRemoveFolderOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let id_str = call
            .caller_input
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                project_op_caller_error("project.remove_folder", "INVALID_INPUT", "missing 'id'")
            })?;

        let folder_id = WorkspaceFolderId::try_new(id_str).map_err(|e| {
            project_op_caller_error("project.remove_folder", "INVALID_FOLDER_ID", e)
        })?;

        self.project_service
            .remove_folder(&call.project_id, &folder_id)
            .await
            .map_err(|e| match e {
                AppHostError::Diagnostic(d) => *d,
                other => project_op_caller_error(
                    "project.remove_folder",
                    "REMOVE_FOLDER_FAILED",
                    other.to_string(),
                ),
            })?;

        Ok(json!({ "removed": id_str }))
    }
}

/// The built-in `project.list_folders` kernel operation.
pub struct ProjectListFoldersOp {
    spec: OperationSpec,
    project_service: Arc<ProjectService>,
}

impl ProjectListFoldersOp {
    pub fn new(project_service: Arc<ProjectService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "project.list_folders".to_owned(),
                description: "List workspace folders configured for the project".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "folders": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "path": { "type": "string" }
                                },
                                "required": ["id", "path"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["folders"],
                    "additionalProperties": false,
                    "examples": [{
                        "folders": [{
                            "id": "folder-1-1",
                            "path": "/path/to/folder"
                        }]
                    }]
                }),
                annotations: OperationAnnotations {
                    read_only: true,
                    destructive: false,
                    idempotent: true,
                    deterministic: true,
                    open_world: false,
                },
            },
            project_service,
        }
    }
}

#[async_trait]
impl KernelOp for ProjectListFoldersOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        false
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let folders = self
            .project_service
            .folders(&call.project_id)
            .await
            .map_err(|e| match e {
                AppHostError::Diagnostic(d) => *d,
                other => project_op_caller_error(
                    "project.list_folders",
                    "LIST_FOLDERS_FAILED",
                    other.to_string(),
                ),
            })?;

        let folders_json: Vec<Value> = folders
            .iter()
            .map(|f| {
                json!({
                    "id": f.id().as_str(),
                    "path": f.path()
                })
            })
            .collect();

        Ok(json!({ "folders": folders_json }))
    }
}

/// Register all project operations with an [`OperationService`].
pub fn register_project_ops(op_service: &OperationService, project_service: Arc<ProjectService>) {
    op_service.register_kernel_op(Arc::new(ProjectRenameOp::new(Arc::clone(&project_service))));
    op_service.register_kernel_op(Arc::new(ProjectAddFolderOp::new(Arc::clone(
        &project_service,
    ))));
    op_service.register_kernel_op(Arc::new(ProjectRemoveFolderOp::new(Arc::clone(
        &project_service,
    ))));
    op_service.register_kernel_op(Arc::new(ProjectListFoldersOp::new(project_service)));
}
