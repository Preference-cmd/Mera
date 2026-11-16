//! Canonical policy for operations an agent caller may not invoke or may invoke
//! only within constrained workspace boundaries.
//!
//! The policy is enforced in `OperationService::invoke`, which is the one path
//! every operation call takes, including a dynamic workflow step that carries
//! its caller's context. Surfaces that list operations (such as an operator
//! adapter) read `agent_may_invoke`, so what they expose and what a call may
//! reach cannot drift apart.
//!
//! # Denied Operations Invariant
//!
//! The following operations are strictly denied to agent callers by name:
//! - `library.remove`: modifies the shared global library; because the library is
//!   shared by every project, only a human removes items from it.
//! - `library.purge`: permanently drops removed entries, deletes unreferenced
//!   content files, and clears library undo history; because it is irreversible
//!   and affects every project, only a human may invoke it.
//! - `dynamic_workflow.resume`: executes the resumed run's remaining steps with
//!   the `OperationContext` stored on the run, which could allow an agent to borrow
//!   the human actor that started it and reach denied operations. It stays human-only
//!   until a run records the resuming caller separately.
//! - `project.add_folder`: adds a host directory to a project's workspace folders;
//!   the user, not the agent, sets the agent's file boundary.
//! - `project.remove_folder`: removes a workspace folder from a project; only the
//!   user may modify the agent's file boundary.
//!
//! # Workspace Path Invariant
//!
//! Operations that accept host file paths (`format: "host-path"`, such as `asset.import`
//! and `library.add`) are subject to argument-level policy checks:
//! - Every path must be absolute, exist and resolve to a canonical path.
//! - The canonical path must be under at least one of the call's project workspace folders.
//! - The canonical path must NOT be under the canonical application data root, even if a
//!   workspace folder contains the application data root.
//! - If the project has no workspace folders configured, the call is rejected with
//!   `PATH_OUTSIDE_WORKSPACE`.
//! - Input paths are rewritten to their canonical paths so subsequent operations execute
//!   against the exact path validated by the policy.
//!
//! # Known Residual
//!
//! A directory component swapped for a symlink between this policy check and the subsequent
//! filesystem read/copy could theoretically escape; the OS sandbox around the agent process
//! is the ultimate boundary.

use std::path::Path;

use mera_core::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
    DiagnosticTargetDomain,
};
use mera_core::model::DiagnosticId;
use mera_core::project::WorkspaceFolder;
use mod_sdk::OperationSpec;
use serde_json::Value;

/// Operations that an agent caller may never invoke by name.
pub const AGENT_DENIED_OPERATIONS: &[&str] = &[
    "dynamic_workflow.resume",
    "library.purge",
    "library.remove",
    "project.add_folder",
    "project.remove_folder",
];

/// Whether an agent caller may invoke `operation` by name.
///
/// Only agent callers are restricted; human, importer, and system callers keep
/// their behavior.
pub fn agent_may_invoke(operation: &str) -> bool {
    !AGENT_DENIED_OPERATIONS.contains(&operation)
}

fn op_caller_error(op_name: &str, code: &str, message: impl Into<String>) -> Diagnostic {
    let namespace = op_name.split('.').next().unwrap_or("kernel");
    Diagnostic::new(
        DiagnosticId::new(format!("op:{op_name}:{code}")),
        DiagnosticCode::new(format!("{}/CALLER_{code}", namespace.to_uppercase())),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("operation_policy"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new(format!("op.{op_name}"))),
    )
}

/// Enforce argument-level policy on an operation call from an agent actor.
///
/// 1. Rejects operations on [`AGENT_DENIED_OPERATIONS`] with `NOT_PERMITTED_TO_AGENT`.
/// 2. Discovers all `host-path` fields marked in the operation input schema.
/// 3. Canonicalizes each path, verifying it exists, is under at least one workspace folder,
///    and does not reside within the application data root.
/// 4. Rewrites each `host-path` field in `input` to its canonical path.
#[allow(clippy::result_large_err)]
pub fn check_agent_call(
    spec: &OperationSpec,
    mut input: Value,
    folders: &[WorkspaceFolder],
    data_root: &Path,
) -> Result<Value, Diagnostic> {
    if !agent_may_invoke(&spec.name) {
        return Err(op_caller_error(
            &spec.name,
            "NOT_PERMITTED_TO_AGENT",
            format!(
                "operation `{}` is not permitted for agent callers",
                spec.name
            ),
        ));
    }

    let fields = mod_sdk::schema::find_host_path_fields(&spec.input_schema, &input);
    if fields.is_empty() {
        return Ok(input);
    }

    let canonical_data_root =
        std::fs::canonicalize(data_root).unwrap_or_else(|_| data_root.to_path_buf());

    for field in &fields {
        let Some(val) = input.pointer_mut(&field.pointer) else {
            continue;
        };
        let Some(raw_path) = val.as_str() else {
            continue;
        };

        // A relative path would resolve against the host process's working
        // directory, which the agent neither sees nor controls.
        if !Path::new(raw_path).is_absolute() {
            return Err(op_caller_error(
                &spec.name,
                "INVALID_PATH",
                format!("host path `{raw_path}` must be absolute"),
            ));
        }

        let canonical = match std::fs::canonicalize(raw_path) {
            Ok(c) => c,
            Err(e) => {
                return Err(op_caller_error(
                    &spec.name,
                    "INVALID_PATH",
                    format!("host path `{raw_path}` could not be resolved: {e}"),
                ));
            }
        };

        let canonical_str = match canonical.to_str() {
            Some(s) => s,
            None => {
                return Err(op_caller_error(
                    &spec.name,
                    "INVALID_PATH",
                    format!("canonical path for `{raw_path}` is not valid UTF-8"),
                ));
            }
        };

        if folders.is_empty() {
            return Err(op_caller_error(
                &spec.name,
                "PATH_OUTSIDE_WORKSPACE",
                format!(
                    "path `{raw_path}` is outside workspace: the project has no workspace folders"
                ),
            ));
        }

        let is_under_folder = folders
            .iter()
            .any(|folder| canonical.starts_with(Path::new(folder.path())));

        if !is_under_folder {
            return Err(op_caller_error(
                &spec.name,
                "PATH_OUTSIDE_WORKSPACE",
                format!(
                    "path `{raw_path}` (canonical: `{}`) is outside project workspace folders",
                    canonical.display()
                ),
            ));
        }

        if canonical.starts_with(&canonical_data_root) {
            return Err(op_caller_error(
                &spec.name,
                "PATH_OUTSIDE_WORKSPACE",
                format!(
                    "path `{raw_path}` (canonical: `{}`) resolves inside application data root `{}`",
                    canonical.display(),
                    canonical_data_root.display()
                ),
            ));
        }

        *val = Value::String(canonical_str.to_owned());
    }

    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mera_core::model::WorkspaceFolderId;
    use mod_sdk::OperationAnnotations;
    use serde_json::json;

    #[test]
    fn denied_operations_are_rejected_by_name() {
        assert!(!agent_may_invoke("library.remove"));
        assert!(!agent_may_invoke("library.purge"));
        assert!(!agent_may_invoke("dynamic_workflow.resume"));
        assert!(!agent_may_invoke("project.add_folder"));
        assert!(!agent_may_invoke("project.remove_folder"));
    }

    #[test]
    fn allowed_operations_by_name() {
        assert!(agent_may_invoke("asset.import"));
        assert!(agent_may_invoke("asset.list"));
        assert!(agent_may_invoke("board.add_notes"));
        assert!(agent_may_invoke("dynamic_workflow.run"));
        assert!(agent_may_invoke("library.add"));
        assert!(agent_may_invoke("library.collect"));
        assert!(agent_may_invoke("library.list"));
        assert!(agent_may_invoke("library.rename"));
        assert!(agent_may_invoke("library.tag"));
        assert!(agent_may_invoke("project.list_folders"));
        assert!(agent_may_invoke("project.rename"));
    }

    #[test]
    fn check_agent_call_rejects_name_denied_operation() {
        let spec = OperationSpec {
            name: "project.add_folder".to_owned(),
            description: "test".to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            annotations: OperationAnnotations {
                read_only: false,
                destructive: false,
                idempotent: false,
                deterministic: true,
                open_world: false,
            },
        };
        let res = check_agent_call(&spec, json!({}), &[], Path::new("/data"));
        let err = res.expect_err("must reject name-denied op");
        assert_eq!(err.code().as_str(), "PROJECT/CALLER_NOT_PERMITTED_TO_AGENT");
    }

    #[test]
    fn check_agent_call_validates_and_rewrites_host_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let folder_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&folder_dir).expect("create folder");
        let data_root = temp.path().join("data_root");
        std::fs::create_dir_all(&data_root).expect("create data root");

        let file = folder_dir.join("test.txt");
        std::fs::write(&file, "hello").expect("write file");
        std::fs::create_dir_all(folder_dir.join("sub")).expect("create sub dir");

        let spec = OperationSpec {
            name: "asset.import".to_owned(),
            description: "test".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string", "format": "host-path" }
                    }
                }
            }),
            output_schema: json!({ "type": "object" }),
            annotations: OperationAnnotations {
                read_only: false,
                destructive: false,
                idempotent: false,
                deterministic: true,
                open_world: false,
            },
        };

        let canonical_folder = std::fs::canonicalize(&folder_dir).expect("canonicalize folder");
        let folder = WorkspaceFolder::new(
            WorkspaceFolderId::new("f-1"),
            canonical_folder.to_str().unwrap(),
        );

        // 1. Success case: inside folder
        let uncanonical_path = format!("{}/./sub/../test.txt", folder_dir.display());
        let input = json!({ "paths": [uncanonical_path] });
        let rewritten = check_agent_call(&spec, input, std::slice::from_ref(&folder), &data_root)
            .expect("should allow valid path");
        let canonical_file = std::fs::canonicalize(&file).expect("canonicalize file");
        assert_eq!(
            rewritten["paths"][0].as_str().unwrap(),
            canonical_file.to_str().unwrap()
        );

        // A relative path is rejected even when it would resolve inside.
        let err_relative = check_agent_call(
            &spec,
            json!({ "paths": ["test.txt"] }),
            std::slice::from_ref(&folder),
            &data_root,
        )
        .expect_err("should reject a relative path");
        assert_eq!(err_relative.code().as_str(), "ASSET/CALLER_INVALID_PATH");

        // 2. Failure case: no folders configured
        let err_no_folders = check_agent_call(
            &spec,
            json!({ "paths": [file.to_str().unwrap()] }),
            &[],
            &data_root,
        )
        .expect_err("should reject when no folders");
        assert_eq!(
            err_no_folders.code().as_str(),
            "ASSET/CALLER_PATH_OUTSIDE_WORKSPACE"
        );

        // 3. Failure case: outside folders
        let outside_file = temp.path().join("outside.txt");
        std::fs::write(&outside_file, "outside").expect("write outside");
        let err_outside = check_agent_call(
            &spec,
            json!({ "paths": [outside_file.to_str().unwrap()] }),
            std::slice::from_ref(&folder),
            &data_root,
        )
        .expect_err("should reject outside file");
        assert_eq!(
            err_outside.code().as_str(),
            "ASSET/CALLER_PATH_OUTSIDE_WORKSPACE"
        );

        // 4. Failure case: file inside data root even if a folder includes it
        let data_file = data_root.join("secret.txt");
        std::fs::write(&data_file, "secret").expect("write secret");
        let wide_folder = WorkspaceFolder::new(
            WorkspaceFolderId::new("f-wide"),
            std::fs::canonicalize(temp.path())
                .unwrap()
                .to_str()
                .unwrap(),
        );
        let err_data_root = check_agent_call(
            &spec,
            json!({ "paths": [data_file.to_str().unwrap()] }),
            &[wide_folder],
            &data_root,
        )
        .expect_err("should reject path in data root");
        assert_eq!(
            err_data_root.code().as_str(),
            "ASSET/CALLER_PATH_OUTSIDE_WORKSPACE"
        );
    }
}
