use std::fs;
use std::path::PathBuf;

use mera_config::AppPaths;
use mera_core::model::ProjectId;
use tempfile::tempdir;

use super::approval::{SCRIPT_APPROVALS_FILENAME, ScriptApprovalStore, compute_execution_digest};
use super::runner::{ScriptService, path_to_relative_utf8, validate_output_file_in_out_dir};

#[test]
fn script_python_schema_satisfies_mod_sdk_marker_rules() {
    let spec = ScriptService::python_operation_spec();
    mod_sdk::schema::compile(&spec.input_schema).expect("input schema must compile");
    mod_sdk::schema::compile(&spec.output_schema).expect("output schema must compile");
    mod_sdk::schema::check_asset_marker_locations(&spec.input_schema)
        .expect("input asset markers must obey placement rules");
    mod_sdk::schema::check_asset_marker_locations(&spec.output_schema)
        .expect("output asset markers must obey placement rules");
    mod_sdk::schema::check_inline_text_marker_locations(&spec.input_schema)
        .expect("input inline-text markers must obey placement rules");
    mod_sdk::schema::check_inline_text_marker_locations(&spec.output_schema)
        .expect("output inline-text markers must obey placement rules");
    mod_sdk::schema::check_host_path_marker_locations(&spec.input_schema)
        .expect("input host-path markers must obey placement rules");
    assert!(!mod_sdk::schema::has_host_path_field(&spec.input_schema));
}

#[test]
fn compute_execution_digest_normalizes_crlf_and_tracks_one_byte_changes() {
    let d1 = compute_execution_digest(
        "print('hi')\n",
        &["numpy>=2.0".to_owned()],
        "3.12",
        false,
        Some("lock1"),
    );
    let d2 = compute_execution_digest(
        "print('hi')\r\n",
        &["numpy>=2.0".to_owned()],
        "3.12",
        false,
        Some("lock1"),
    );
    assert_eq!(d1, d2);

    let d_code = compute_execution_digest(
        "print('hi!')\n",
        &["numpy>=2.0".to_owned()],
        "3.12",
        false,
        Some("lock1"),
    );
    assert_ne!(d1, d_code);

    let d_dep = compute_execution_digest(
        "print('hi')\n",
        &["numpy>=2.1".to_owned()],
        "3.12",
        false,
        Some("lock1"),
    );
    assert_ne!(d1, d_dep);

    let d_py = compute_execution_digest(
        "print('hi')\n",
        &["numpy>=2.0".to_owned()],
        "3.13",
        false,
        Some("lock1"),
    );
    assert_ne!(d1, d_py);

    let d_build = compute_execution_digest(
        "print('hi')\n",
        &["numpy>=2.0".to_owned()],
        "3.12",
        true,
        Some("lock1"),
    );
    assert_ne!(d1, d_build);

    let d_lock = compute_execution_digest(
        "print('hi')\n",
        &["numpy>=2.0".to_owned()],
        "3.12",
        false,
        Some("lock2"),
    );
    assert_ne!(d1, d_lock);
}

#[tokio::test]
async fn script_approval_store_persists_lists_revokes_and_rejects_tampering() {
    let temp = tempdir().unwrap();
    let app_paths = AppPaths::new(temp.path());
    app_paths.ensure_all().await.unwrap();
    let project = ProjectId::new("default");
    fs::create_dir_all(app_paths.project_dir(project.as_str())).unwrap();

    let store = ScriptApprovalStore::new(app_paths.clone());
    let digest = compute_execution_digest("print(1)", &[], "3.12", false, None);
    assert!(
        !store
            .is_execution_approved(&project, &digest)
            .await
            .unwrap()
    );

    store.approve_execution(&project, &digest).await.unwrap();
    assert!(
        store
            .is_execution_approved(&project, &digest)
            .await
            .unwrap()
    );
    assert_eq!(store.list_approved(&project).await.unwrap().len(), 1);

    assert!(store.revoke_execution(&project, &digest).await.unwrap());
    assert!(
        !store
            .is_execution_approved(&project, &digest)
            .await
            .unwrap()
    );

    // Tampered approval file (mismatched key vs inner exec_digest) must be rejected.
    let approvals_path = app_paths
        .project_dir(project.as_str())
        .join(SCRIPT_APPROVALS_FILENAME);
    let other_digest = compute_execution_digest("print(2)", &[], "3.12", false, None);
    let tampered = serde_json::json!({
        "schema_version": 1,
        "approved": {
            digest: {
                "exec_digest": other_digest,
                "python": "3.12",
                "dependencies": [],
                "allow_source_build": false,
                "lock_sha256": null,
                "approved_at": "2026-11-24T00:00:00Z"
            }
        }
    });
    fs::write(&approvals_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
    let err = store
        .is_execution_approved(&project, &other_digest)
        .await
        .expect_err("tampered approval entry must fail");
    assert!(err.to_string().contains("tampered"));
}

#[test]
fn validate_output_file_in_out_dir_blocks_traversal_and_symlinks() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("_mera/out");
    let outside = temp.path().join("outside.txt");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(&outside, b"secret").unwrap();
    fs::write(out_dir.join("ok.txt"), b"ok").unwrap();
    let canon_out = fs::canonicalize(&out_dir).unwrap();

    let ok_path = validate_output_file_in_out_dir(&canon_out, &out_dir, "ok.txt").unwrap();
    assert!(ok_path.ends_with("ok.txt"));

    let err_parent = validate_output_file_in_out_dir(&canon_out, &out_dir, "../outside.txt")
        .expect_err("must reject parent traversal");
    assert_eq!(err_parent.code().as_str(), "SCRIPT/ESCAPE_OUTPUT_DIR");

    let err_abs = validate_output_file_in_out_dir(
        &canon_out,
        &out_dir,
        &PathBuf::from(&outside).to_string_lossy(),
    )
    .expect_err("must reject outside absolute path");
    assert_eq!(err_abs.code().as_str(), "SCRIPT/ESCAPE_OUTPUT_DIR");

    #[cfg(unix)]
    {
        let link = out_dir.join("escape_link.txt");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let err_link = validate_output_file_in_out_dir(&canon_out, &out_dir, "escape_link.txt")
            .expect_err("must reject symlink output");
        assert_eq!(err_link.code().as_str(), "SCRIPT/ESCAPE_OUTPUT_DIR");
    }
}

#[test]
fn path_to_relative_utf8_rejects_non_utf8_and_non_normal_components() {
    assert_eq!(
        path_to_relative_utf8(std::path::Path::new("_mera/out/img.png")).unwrap(),
        "_mera/out/img.png"
    );

    let err_parent = path_to_relative_utf8(std::path::Path::new("../out.png"))
        .expect_err("must reject parent component");
    assert_eq!(err_parent.code().as_str(), "SCRIPT/INVALID_OUTPUT");

    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let bad = std::path::Path::new(OsStr::from_bytes(b"_mera/out/\xff.bin"));
        let err_utf8 = path_to_relative_utf8(bad).expect_err("must reject non-UTF-8 path");
        assert_eq!(err_utf8.code().as_str(), "SCRIPT/INVALID_OUTPUT");
    }
}

#[tokio::test]
async fn script_service_rejects_relative_or_empty_resolved_input_asset_paths() {
    use crate::operation_service::KernelOp as _;
    use mera_core::command::{CommandActor, CommandActorKind, CommandProvenance, OperationContext};
    use mera_core::task::CancelToken;
    use mod_sdk::Invocation;

    let temp = tempdir().unwrap();
    let app_paths = AppPaths::new(temp.path().join("ws"));
    app_paths.ensure_all().await.unwrap();
    let mod_paths = crate::ModStorePaths::new(temp.path().join("app-data"));
    let service = std::sync::Arc::new(ScriptService::new(app_paths, mod_paths));
    let op = super::runner::ScriptPythonOp::new(service);

    for bad_asset in ["", "relative/file.txt"] {
        let call = crate::operation_service::KernelCall {
            invocation: Invocation::new(
                "script.python",
                serde_json::json!({
                    "code": "print('hi')",
                    "assets": [{ "name": "doc", "asset": bad_asset }],
                }),
                temp.path(),
                CancelToken::new(),
            ),
            project_id: ProjectId::new("default"),
            context: OperationContext::new(
                CommandActor::new(CommandActorKind::Human),
                CommandProvenance::Direct,
            ),
            caller_input: serde_json::json!({}),
        };
        let err = op
            .invoke(call)
            .await
            .expect_err("must reject non-absolute asset path");
        assert_eq!(err.code().as_str(), "SCRIPT/INVALID_INPUT");
    }
}
