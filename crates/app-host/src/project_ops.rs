//! Kernel operations for project workspace folders:
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
    op_service.register_kernel_op(Arc::new(ProjectAddFolderOp::new(Arc::clone(
        &project_service,
    ))));
    op_service.register_kernel_op(Arc::new(ProjectRemoveFolderOp::new(Arc::clone(
        &project_service,
    ))));
    op_service.register_kernel_op(Arc::new(ProjectListFoldersOp::new(project_service)));
}
