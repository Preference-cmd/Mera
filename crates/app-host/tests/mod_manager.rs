//! End-to-end tests for [`mera_app_host::ModManagerService`],
//! archive validation, staging self-check, per-operation Agent visibility,
//! upgrades, conflicts, hot-swapping, and startup recovery.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use mera_app_host::{
    InstallModRequest, InvokeRequest, ModInstallSource, ModManagerError, ModManagerService,
    ModStorePaths, ModVisibilityTable, WorkspaceHost, WorkspaceHostBuilder, WorkspaceScope,
};
use mera_core::command::{CommandActor, CommandActorKind, CommandProvenance, OperationContext};
use mera_core::health::Health;
use mera_core::model::ProjectId;
use mera_core::task::CancelToken;
use mod_sdk::package::{EntrypointSpec, ModPackageManifest, ModPermissions};
use mod_sdk::{ModManifest, OperationAnnotations, OperationSpec};
use serde_json::json;

fn fixture_binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_app-host-mod-fixture"))
}

fn manifest_v1(version: &str) -> ModManifest {
    ModManifest {
        id: "ext_demo".to_owned(),
        name: "External Demo".to_owned(),
        version: version.to_owned(),
        api: "0.1".to_owned(),
        operations: vec![OperationSpec {
            name: "ext_demo.echo".to_owned(),
            description: "Echo input text".to_owned(),
            input_schema: json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": { "type": "string" }
                }
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "echo": { "type": "string" }
                }
            }),
            annotations: OperationAnnotations {
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
                deterministic: true,
            },
        }],
        events: Vec::new(),
    }
}

fn manifest_v2() -> ModManifest {
    let mut m = manifest_v1("0.2.0");
    m.operations.push(OperationSpec {
        name: "ext_demo.read_file".to_owned(),
        description: "Inspect a workspace file path".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string", "format": "host-path" }
            }
        }),
        output_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            }
        }),
        annotations: OperationAnnotations {
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
            deterministic: true,
        },
    });
    m
}

fn manifest_fail_start() -> ModManifest {
    let mut m = manifest_v1("0.1.0");
    m.id = "ext_fail".to_owned();
    m.name = "Broken Mod".to_owned();
    m.operations[0].name = "ext_fail.ping".to_owned();
    m
}

fn package_meta(
    manifest: &ModManifest,
    bin_rel_path: &str,
    permissions: ModPermissions,
) -> ModPackageManifest {
    ModPackageManifest {
        schema_version: 1,
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        target: "noarch".to_owned(),
        api: manifest.api.clone(),
        author: Some("Community Author".to_owned()),
        homepage: None,
        license: Some("MIT".to_owned()),
        entrypoint: EntrypointSpec::Binary {
            path: bin_rel_path.to_owned(),
        },
        permissions,
    }
}

fn build_rmod_archive(
    package: &ModPackageManifest,
    manifest: &ModManifest,
    bin_rel_path: &str,
    bin_bytes: &[u8],
) -> Vec<u8> {
    let pkg_json = serde_json::to_vec_pretty(package).unwrap();
    let man_json = serde_json::to_vec_pretty(manifest).unwrap();
    let entries: Vec<(&str, &[u8], u32)> = vec![
        ("mod-package.json", &pkg_json, 0o644),
        ("manifest.json", &man_json, 0o644),
        (bin_rel_path, bin_bytes, 0o755),
    ];

    let mut out = Vec::new();
    {
        let gz = flate2::write::GzEncoder::new(&mut out, flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (path, data, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            tar.append_data(&mut header, path, data).unwrap();
        }
        tar.finish().unwrap();
    }
    out
}

fn human_ctx() -> OperationContext {
    OperationContext::new(
        CommandActor::new(CommandActorKind::Human),
        CommandProvenance::Direct,
    )
}

fn agent_ctx() -> OperationContext {
    OperationContext::new(
        CommandActor::new(CommandActorKind::Agent).with_id("agent-1"),
        CommandProvenance::Direct,
    )
}

async fn setup_workspace(base: &Path, app_data: &Path) -> WorkspaceHost {
    WorkspaceHostBuilder::new(WorkspaceScope::new("mod-mgr-test"), base)
        .app_data_root(app_data)
        .build()
        .await
        .expect("workspace build")
}

#[tokio::test]
async fn third_party_install_self_check_agent_isolation_and_upgrade_flow() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("workspace");
    let app_data = temp.path().join("app-data");
    let bin_bytes = std::fs::read(fixture_binary()).unwrap();
    let host = setup_workspace(&base, &app_data).await;
    let project = ProjectId::new("default");

    let exe_suffix = std::env::consts::EXE_SUFFIX;
    let bin_v1_rel = format!("bin/fixture-mod-v1{exe_suffix}");
    let man_v1 = manifest_v1("0.1.0");
    let pkg_v1 = package_meta(
        &man_v1,
        &bin_v1_rel,
        ModPermissions {
            recommended_agent_operations: vec!["ext_demo.echo".to_owned()],
            ..ModPermissions::default()
        },
    );
    let archive_v1 = build_rmod_archive(&pkg_v1, &man_v1, &bin_v1_rel, &bin_bytes);
    let source = ModInstallSource::ThirdPartyUrl {
        source_url: "https://example.com/ext-demo/index.json".to_owned(),
    };

    // 1. Preview metadata without extracting or executing binaries.
    let preview = host
        .mod_manager()
        .preview_archive_bytes(&archive_v1, source.clone())
        .expect("preview v1");
    assert!(preview.requires_explicit_consent);
    assert!(!preview.upgrade.is_upgrade);
    assert!(!host.mod_manager().paths().staging_root().exists());

    // 2. Reject third-party installation without explicit consent.
    let err = host
        .mod_manager()
        .install_archive_bytes(
            &archive_v1,
            source.clone(),
            InstallModRequest::default().enable(true),
        )
        .await
        .expect_err("unconsented install must fail");
    assert!(matches!(err, ModManagerError::ConsentRequired { .. }));

    // 3. Reject archive whose live `initialize` manifest disagrees with `manifest.json`, cleaning staging.
    let mismatch_rel = format!("bin/fixture-mod-mismatch{exe_suffix}");
    let mismatch_pkg = package_meta(&man_v1, &mismatch_rel, ModPermissions::default());
    let mismatch_archive = build_rmod_archive(&mismatch_pkg, &man_v1, &mismatch_rel, &bin_bytes);
    let err = host
        .mod_manager()
        .install_archive_bytes(
            &mismatch_archive,
            source.clone(),
            InstallModRequest::default().with_consent().enable(true),
        )
        .await
        .expect_err("manifest mismatch must be rejected");
    assert!(matches!(err, ModManagerError::ManifestMismatch { .. }));
    let staging_leftovers: Vec<_> = std::fs::read_dir(host.mod_manager().paths().staging_root())
        .into_iter()
        .flatten()
        .collect();
    assert!(
        staging_leftovers.is_empty(),
        "staging must be cleaned up on rejection"
    );

    // 4. Reject bad-init binary cleanly.
    let bad_init_rel = format!("bin/fixture-mod-bad-init{exe_suffix}");
    let bad_init_pkg = package_meta(&man_v1, &bad_init_rel, ModPermissions::default());
    let bad_init_archive = build_rmod_archive(&bad_init_pkg, &man_v1, &bad_init_rel, &bin_bytes);
    let err = host
        .mod_manager()
        .install_archive_bytes(
            &bad_init_archive,
            source.clone(),
            InstallModRequest::default().with_consent().enable(true),
        )
        .await
        .expect_err("crashing init binary must be rejected");
    assert!(matches!(err, ModManagerError::SelfCheckFailed { .. }));

    // 4b. Reject path-traversal `version` before staging or touching package storage.
    let mut evil_man = manifest_v1("../../../../tmp/evil");
    evil_man.version = "../../../../tmp/evil".to_owned();
    let evil_pkg = package_meta(&evil_man, &bin_v1_rel, ModPermissions::default());
    let evil_archive = build_rmod_archive(&evil_pkg, &evil_man, &bin_v1_rel, &bin_bytes);
    let err = host
        .mod_manager()
        .install_archive_bytes(
            &evil_archive,
            source.clone(),
            InstallModRequest::default().with_consent().enable(true),
        )
        .await
        .expect_err("path-traversal version must be rejected");
    match &err {
        ModManagerError::Diagnostic(diag) => {
            assert_eq!(diag.code().as_str(), "MOD/CALLER_INVALID_VERSION");
        }
        other => panic!("expected Diagnostic(MOD/CALLER_INVALID_VERSION), got {other:?}"),
    }
    let staging_leftovers: Vec<_> = std::fs::read_dir(host.mod_manager().paths().staging_root())
        .into_iter()
        .flatten()
        .collect();
    assert!(
        staging_leftovers.is_empty(),
        "staging must remain empty after version validation failure"
    );

    // 5. Install v1 cleanly WITHOUT checking any operation for the Agent.
    // Even though `recommended_agent_operations` lists `ext_demo.echo`, it MUST stay hidden from Agent!
    let info = host
        .mod_manager()
        .install_archive_bytes(
            &archive_v1,
            source.clone(),
            InstallModRequest::default().with_consent().enable(true),
        )
        .await
        .expect("install v1");
    assert!(info.health.is_available());
    assert!(info.record.agent_visible_operations.is_empty());

    let ops = host.operation_service().list(&project).await;
    let demo_op = ops
        .iter()
        .find(|o| o.spec.name == "ext_demo.echo")
        .expect("ext_demo.echo registered in op_list");
    assert!(demo_op.health.is_available());
    assert!(
        !demo_op.agent_permitted,
        "newly installed third-party mod op must NOT be agent_permitted implicitly"
    );

    // Human can invoke it right away.
    let human_res = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "ext_demo.echo".to_owned(),
                input: json!({ "text": "hello" }),
                context: human_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect("human invoke succeeds");
    assert_eq!(human_res.output, json!({ "echo": "0.1.0:hello" }));

    // Agent cannot invoke directly OR indirectly via a dynamic workflow.
    let agent_err = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "ext_demo.echo".to_owned(),
                input: json!({ "text": "hello" }),
                context: agent_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect_err("agent direct invoke must fail");
    assert_eq!(
        agent_err.code().as_str(),
        "EXT_DEMO/CALLER_NOT_PERMITTED_TO_AGENT"
    );

    let created_dw = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "dynamic_workflow.create".to_owned(),
                input: json!({
                    "name": "Wrap Demo Echo",
                    "workflow": {
                        "inputs": {},
                        "params": { "type": "object" },
                        "steps": [{
                            "id": "s1",
                            "op": "ext_demo.echo",
                            "input": { "text": "via-dw" }
                        }],
                        "output": { "$from": "steps.s1.echo" }
                    }
                }),
                context: human_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect("create wrapping dynamic workflow");
    let dw_op_name = format!(
        "dynamic_workflow.{}",
        created_dw.output["dynamic_workflow_id"].as_str().unwrap()
    );
    let dw_err = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: dw_op_name.clone(),
                input: json!({}),
                context: agent_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect_err("agent dynamic workflow wrapping hidden mod op must fail");
    assert_eq!(
        dw_err.code().as_str(),
        "DYNAMIC_WORKFLOW/CALLER_NOT_PERMITTED_TO_AGENT"
    );

    // 6. Explicitly check `ext_demo.echo` for Agent visibility.
    let mut visible = BTreeSet::new();
    visible.insert("ext_demo.echo".to_owned());
    let updated = host
        .mod_manager()
        .set_agent_visible_operations("ext_demo", visible)
        .await
        .expect("enable agent visibility for ext_demo.echo");
    assert!(
        updated
            .record
            .agent_visible_operations
            .contains("ext_demo.echo")
    );

    let ops = host.operation_service().list(&project).await;
    assert!(
        ops.iter()
            .find(|o| o.spec.name == "ext_demo.echo")
            .unwrap()
            .agent_permitted
    );
    let agent_res = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "ext_demo.echo".to_owned(),
                input: json!({ "text": "agent-ok" }),
                context: agent_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect("agent invoke succeeds after explicit opt-in");
    assert_eq!(agent_res.output, json!({ "echo": "0.1.0:agent-ok" }));

    // 7. Reject cross-origin overwrite from a different source.
    let other_source = ModInstallSource::ThirdPartyUrl {
        source_url: "https://other.example/index.json".to_owned(),
    };
    let conflict = host
        .mod_manager()
        .install_archive_bytes(
            &archive_v1,
            other_source,
            InstallModRequest::default().with_consent().enable(true),
        )
        .await
        .expect_err("cross-origin overwrite must fail");
    assert!(matches!(conflict, ModManagerError::SourceConflict { .. }));

    // 8. Same-origin upgrade v0.1.0 -> v0.2.0 expands permissions (`host_paths: true`)
    // and introduces `ext_demo.read_file`:
    //   - blocked without permission consent
    //   - once confirmed, previously checked `ext_demo.echo` stays Agent-visible
    //     while newly added `ext_demo.read_file` stays HIDDEN from Agent!
    let bin_v2_rel = format!("bin/fixture-mod-v2{exe_suffix}");
    let man_v2 = manifest_v2();
    let pkg_v2 = package_meta(
        &man_v2,
        &bin_v2_rel,
        ModPermissions {
            host_paths: true,
            ..ModPermissions::default()
        },
    );
    let archive_v2 = build_rmod_archive(&pkg_v2, &man_v2, &bin_v2_rel, &bin_bytes);

    let err = host
        .mod_manager()
        .install_archive_bytes(
            &archive_v2,
            source.clone(),
            InstallModRequest::default().with_consent().enable(true),
        )
        .await
        .expect_err("permission expansion upgrade without consent must fail");
    assert!(matches!(
        err,
        ModManagerError::PermissionExpansionRequiresConsent { .. }
    ));

    let upgraded = host
        .mod_manager()
        .install_archive_bytes(
            &archive_v2,
            source,
            InstallModRequest::default()
                .with_consent()
                .consent_permission_expansion(true)
                .enable(true),
        )
        .await
        .expect("consented upgrade v2");
    assert_eq!(upgraded.record.manifest.version, "0.2.0");
    assert!(
        upgraded
            .record
            .agent_visible_operations
            .contains("ext_demo.echo")
    );
    assert!(
        !upgraded
            .record
            .agent_visible_operations
            .contains("ext_demo.read_file"),
        "newly added operation on upgrade must stay hidden from Agent until checked"
    );

    let ops = host.operation_service().list(&project).await;
    assert!(
        ops.iter()
            .find(|o| o.spec.name == "ext_demo.echo")
            .unwrap()
            .agent_permitted
    );
    assert!(
        !ops.iter()
            .find(|o| o.spec.name == "ext_demo.read_file")
            .unwrap()
            .agent_permitted
    );

    // Hot-swapped binary now runs v0.2.0.
    let v2_res = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "ext_demo.echo".to_owned(),
                input: json!({ "text": "after-upgrade" }),
                context: agent_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect("v2 active");
    assert_eq!(v2_res.output, json!({ "echo": "0.2.0:after-upgrade" }));

    // 9. Disable stops the mod and removes it from op_list; uninstall deletes it cleanly.
    host.mod_manager()
        .set_enabled("ext_demo", false)
        .await
        .expect("disable ext_demo");
    assert!(
        host.operation_service()
            .list(&project)
            .await
            .iter()
            .all(|o| o.spec.name != "ext_demo.echo")
    );

    host.mod_manager()
        .set_enabled("ext_demo", true)
        .await
        .expect("re-enable ext_demo");
    assert!(
        host.operation_service()
            .list(&project)
            .await
            .iter()
            .any(|o| o.spec.name == "ext_demo.echo")
    );

    host.mod_manager()
        .uninstall("ext_demo", true)
        .await
        .expect("uninstall ext_demo");
    assert!(host.mod_manager().list_installed().is_empty());

    host.shutdown().await;
}

#[tokio::test]
async fn startup_rehydrates_mods_cleans_staging_and_isolates_broken_mod() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("workspace");
    let app_data = temp.path().join("app-data");
    let bin_bytes = std::fs::read(fixture_binary()).unwrap();
    let exe_suffix = std::env::consts::EXE_SUFFIX;

    // Install valid `ext_demo` via host #1.
    let host1 = setup_workspace(&base, &app_data).await;
    let bin_v1_rel = format!("bin/fixture-mod-v1{exe_suffix}");
    let man_v1 = manifest_v1("0.1.0");
    let pkg_v1 = package_meta(&man_v1, &bin_v1_rel, ModPermissions::default());
    let archive_v1 = build_rmod_archive(&pkg_v1, &man_v1, &bin_v1_rel, &bin_bytes);

    host1
        .mod_manager()
        .install_archive_bytes(
            &archive_v1,
            ModInstallSource::LocalArchive {
                filename: "ext_demo-0.1.0.rmod.tar.gz".to_owned(),
            },
            InstallModRequest::default()
                .with_consent()
                .enable(true)
                .agent_visible_operations(["ext_demo.echo".to_owned()].into_iter().collect()),
        )
        .await
        .expect("install valid mod");
    let local_upgrade_err = host1
        .mod_manager()
        .install_archive_bytes(
            &archive_v1,
            ModInstallSource::LocalArchive {
                filename: "ext_demo-0.1.0-reinstall.rmod.tar.gz".to_owned(),
            },
            InstallModRequest::default().enable(true),
        )
        .await
        .expect_err("local archive upgrade without explicit consent must fail");
    assert!(matches!(
        local_upgrade_err,
        ModManagerError::ConsentRequired { .. }
    ));
    host1.shutdown().await;

    // Seed leftover staging garbage + a broken mod record whose binary fails `mod/start`.
    let stale_dir = ModStorePaths::new(&app_data)
        .staging_root()
        .join("stale-leftover");
    std::fs::create_dir_all(&stale_dir).unwrap();
    std::fs::write(stale_dir.join("part"), b"partial").unwrap();

    let broken_dir = app_data
        .join("mods")
        .join("packages")
        .join("ext_fail")
        .join("0.1.0");
    let broken_bin = broken_dir.join(format!("bin/fixture-mod-fail-start{exe_suffix}"));
    std::fs::create_dir_all(broken_bin.parent().unwrap()).unwrap();
    std::fs::write(&broken_bin, &bin_bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&broken_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let fail_manifest = manifest_fail_start();
    let fail_pkg = package_meta(
        &fail_manifest,
        &format!("bin/fixture-mod-fail-start{exe_suffix}"),
        ModPermissions::default(),
    );
    let store = mera_app_host::ModInventoryStore::new(ModStorePaths::new(&app_data));
    let mut inv = store.load().unwrap();
    inv.mods.insert(
        "ext_fail".to_owned(),
        mera_app_host::InstalledModRecord {
            manifest: fail_manifest,
            package: fail_pkg,
            source: ModInstallSource::LocalArchive {
                filename: "broken.rmod.tar.gz".to_owned(),
            },
            package_sha256: None,
            install_dir: broken_dir,
            enabled: true,
            installed_at: "2026-11-14T00:00:00Z".to_owned(),
            granted_permissions: ModPermissions::default(),
            agent_visible_operations: BTreeSet::new(),
        },
    );
    store.save(&inv).unwrap();

    // Boot fresh host #2 and verify startup recovery & isolation.
    let host2 = setup_workspace(&base, &app_data).await;
    host2.mod_manager().load_enabled_mods_at_startup().await;

    assert!(
        !stale_dir.exists(),
        "stale staging dir must be purged at startup"
    );

    let project = ProjectId::new("default");
    let ops = host2.operation_service().list(&project).await;
    let demo = ops.iter().find(|o| o.spec.name == "ext_demo.echo").unwrap();
    assert!(demo.health.is_available());
    assert!(demo.agent_permitted);

    let broken = ops.iter().find(|o| o.spec.name == "ext_fail.ping").unwrap();
    assert!(
        matches!(broken.health, Health::Unavailable(_)),
        "broken mod must surface Health::Unavailable without blocking healthy mods"
    );
    assert!(!broken.agent_permitted);

    // Check bundled conflict guard and local directory mount cleanup behavior.
    host2.mod_manager().register_bundled_mod_id("ext_demo");
    host2.mod_manager().register_bundled_mod_id("text");
    host2.shutdown().await;
}

#[tokio::test]
async fn local_directory_install_and_uninstall_preserves_source_tree() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("workspace");
    let app_data = temp.path().join("app-data");
    let local_mod_dir = temp.path().join("dev-mod");
    let exe_suffix = std::env::consts::EXE_SUFFIX;
    let bin_rel = format!("bin/fixture-mod-v1{exe_suffix}");

    std::fs::create_dir_all(local_mod_dir.join("bin")).unwrap();
    std::fs::copy(fixture_binary(), local_mod_dir.join(&bin_rel)).unwrap();
    let manifest = manifest_v1("0.1.0");
    let package = package_meta(&manifest, &bin_rel, ModPermissions::default());
    std::fs::write(
        local_mod_dir.join("mod-package.json"),
        serde_json::to_vec_pretty(&package).unwrap(),
    )
    .unwrap();
    std::fs::write(
        local_mod_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let host = setup_workspace(&base, &app_data).await;
    host.mod_manager().register_bundled_mod_id("text");

    let err = host
        .mod_manager()
        .install_local_directory(&local_mod_dir, InstallModRequest::default().enable(true))
        .await
        .expect_err("local directory mount requires consent");
    assert!(matches!(err, ModManagerError::ConsentRequired { .. }));

    let info = host
        .mod_manager()
        .install_local_directory(
            &local_mod_dir,
            InstallModRequest::default().with_consent().enable(true),
        )
        .await
        .expect("mount local mod dir");
    assert!(info.health.is_available());

    host.mod_manager()
        .uninstall("ext_demo", true)
        .await
        .expect("uninstall local dir");
    assert!(
        local_mod_dir.join("manifest.json").is_file(),
        "uninstalling a mounted local dev dir must not delete user files"
    );

    // Ensure standalone ModManagerService::new + ModVisibilityTable compiles cleanly.
    let _vis = Arc::new(ModVisibilityTable::new());
    let _ = ModManagerService::new;

    host.shutdown().await;
}

#[tokio::test]
async fn workspace_builder_honors_explicit_mods_root_outside_app_data_root() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("workspace");
    let app_data = temp.path().join("app-data");
    let external_mods = temp.path().join("dot-mera").join("mods");
    let exe_suffix = std::env::consts::EXE_SUFFIX;
    let bin_rel = format!("bin/fixture-mod-v1{exe_suffix}");

    let manifest = manifest_v1("0.1.0");
    let package = package_meta(&manifest, &bin_rel, ModPermissions::default());
    let bin_bytes = std::fs::read(fixture_binary()).unwrap();
    let archive_bytes = build_rmod_archive(&package, &manifest, &bin_rel, &bin_bytes);

    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-explicit-mods-root"), &base)
        .app_data_root(&app_data)
        .mods_root(&external_mods)
        .build()
        .await
        .unwrap();

    assert_eq!(host.mod_manager().paths().mods_root(), external_mods);

    let info = host
        .mod_manager()
        .install_archive_bytes(
            &archive_bytes,
            ModInstallSource::LocalArchive {
                filename: "ext_demo.rmod.tar.gz".to_owned(),
            },
            InstallModRequest::default().with_consent().enable(true),
        )
        .await
        .unwrap();
    assert!(info.health.is_available());
    assert!(external_mods.join("inventory.json").is_file());
    assert!(
        external_mods
            .join("packages")
            .join("ext_demo")
            .join("0.1.0")
            .is_dir()
    );
    assert!(
        !app_data.join("mods").exists(),
        "explicit mods_root must not create <app_data_root>/mods"
    );
    host.shutdown().await;

    let host2 = WorkspaceHostBuilder::new(WorkspaceScope::new("test-explicit-mods-root"), &base)
        .app_data_root(&app_data)
        .mods_root(&external_mods)
        .build()
        .await
        .unwrap();
    let rehydrated = host2.mod_manager().load_enabled_mods_at_startup().await;
    assert_eq!(rehydrated.len(), 1);
    assert!(rehydrated[0].health.is_available());
    host2.shutdown().await;
}

#[tokio::test]
async fn reload_local_mod_and_dto_projections_enforce_consent_and_visibility() {
    use mera_app_host::dto::{ModEntryDto, ModInstallPreviewDto, ModSourceDto};

    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("workspace");
    let app_data = temp.path().join("app-data");
    let local_mod_dir = temp.path().join("dev-mod");
    let exe_suffix = std::env::consts::EXE_SUFFIX;
    let bin_v1_rel = format!("bin/fixture-mod-v1{exe_suffix}");
    let bin_v2_rel = format!("bin/fixture-mod-v2{exe_suffix}");

    std::fs::create_dir_all(local_mod_dir.join("bin")).unwrap();
    std::fs::copy(fixture_binary(), local_mod_dir.join(&bin_v1_rel)).unwrap();
    std::fs::copy(fixture_binary(), local_mod_dir.join(&bin_v2_rel)).unwrap();

    let man_v1 = manifest_v1("0.1.0");
    let pkg_v1 = package_meta(&man_v1, &bin_v1_rel, ModPermissions::default());
    std::fs::write(
        local_mod_dir.join("mod-package.json"),
        serde_json::to_vec_pretty(&pkg_v1).unwrap(),
    )
    .unwrap();
    std::fs::write(
        local_mod_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&man_v1).unwrap(),
    )
    .unwrap();

    let host = setup_workspace(&base, &app_data).await;
    let preview_v1 = host
        .mod_manager()
        .preview_local_directory(&local_mod_dir)
        .expect("preview local v1");
    let preview_v1_dto = ModInstallPreviewDto::from_trust_summary(preview_v1, None);
    assert!(!preview_v1_dto.is_upgrade);
    assert!(preview_v1_dto.requires_explicit_consent);
    assert!(preview_v1_dto.source_conflict.is_none());

    let info_v1 = host
        .mod_manager()
        .install_local_directory(
            &local_mod_dir,
            InstallModRequest::default()
                .with_consent()
                .enable(true)
                .agent_visible_operations(["ext_demo.echo".to_owned()].into_iter().collect()),
        )
        .await
        .expect("install local v1");
    let dto_v1 = ModEntryDto::from_installed(info_v1);
    assert!(!dto_v1.read_only_bundled);
    assert_eq!(
        dto_v1.agent_visible_operations,
        vec!["ext_demo.echo".to_owned()]
    );

    // Update directory in place to v2 with a new host-path operation -> permission expansion.
    let man_v2 = manifest_v2();
    let pkg_v2 = package_meta(
        &man_v2,
        &bin_v2_rel,
        ModPermissions {
            host_paths: true,
            recommended_agent_operations: vec!["ext_demo.read_file".to_owned()],
            ..Default::default()
        },
    );
    std::fs::write(
        local_mod_dir.join("mod-package.json"),
        serde_json::to_vec_pretty(&pkg_v2).unwrap(),
    )
    .unwrap();
    std::fs::write(
        local_mod_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&man_v2).unwrap(),
    )
    .unwrap();

    // Previewing the directory again must canonicalize the path and avoid a false source conflict.
    let preview_v2 = host
        .mod_manager()
        .preview_local_directory(&local_mod_dir)
        .expect("preview local v2");
    let existing = host.mod_manager().installed_record("ext_demo");
    let preview_v2_dto = ModInstallPreviewDto::from_trust_summary(preview_v2, existing.as_ref());
    assert!(preview_v2_dto.is_upgrade);
    assert!(preview_v2_dto.source_conflict.is_none());
    assert!(preview_v2_dto.requires_permission_consent);
    assert_eq!(
        preview_v2_dto.existing_agent_visible_operations,
        vec!["ext_demo.echo".to_owned()]
    );
    assert_eq!(
        preview_v2_dto.newly_added_operations,
        vec!["ext_demo.read_file".to_owned()]
    );

    // Reload without permission consent is blocked.
    let err = host
        .mod_manager()
        .reload_local_mod("ext_demo", false)
        .await
        .expect_err("permission expansion must block silent reload");
    assert!(matches!(
        err,
        ModManagerError::PermissionExpansionRequiresConsent { .. }
    ));

    // Reload with permission consent succeeds, preserves ext_demo.echo, and leaves read_file hidden.
    let reloaded = host
        .mod_manager()
        .reload_local_mod("ext_demo", true)
        .await
        .expect("reload local v2 with consent");
    let dto_v2 = ModEntryDto::from_installed(reloaded);
    assert_eq!(dto_v2.version, "0.2.0");
    assert!(matches!(dto_v2.source, ModSourceDto::LocalDirectory { .. }));
    assert_eq!(
        dto_v2.agent_visible_operations,
        vec!["ext_demo.echo".to_owned()]
    );
    assert!(
        dto_v2
            .operations
            .iter()
            .find(|op| op.name == "ext_demo.read_file")
            .unwrap()
            .recommended_for_agent
    );
    assert!(
        dto_v2
            .operations
            .iter()
            .find(|op| op.name == "ext_demo.read_file")
            .unwrap()
            .uses_host_path
    );

    host.shutdown().await;
}

async fn spawn_test_http_server(
    routes: std::collections::HashMap<String, (u16, Vec<u8>)>,
) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");
    let routes = Arc::new(routes);

    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let routes = Arc::clone(&routes);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let Ok(n) = stream.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let (status, body) = routes
                    .get(path)
                    .cloned()
                    .unwrap_or_else(|| (404, b"not found".to_vec()));
                let reason = match status {
                    200 => "OK",
                    404 => "Not Found",
                    _ => "Error",
                };
                let header = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(&body).await;
                let _ = stream.shutdown().await;
            });
        }
    });

    (base_url, handle)
}

fn sign_tuf_payload<T: serde::Serialize>(
    signing_key: &ed25519_dalek::SigningKey,
    signed: &T,
) -> String {
    use ed25519_dalek::Signer;
    let value = serde_json::to_value(signed).unwrap();
    let bytes = serde_json::to_vec(&value).unwrap();
    hex::encode(signing_key.sign(&bytes).to_bytes())
}

fn build_signed_tuf_bundle(
    archive_bytes: &[u8],
    manifest: &ModManifest,
    package: &ModPackageManifest,
    timestamp_version: u64,
) -> (
    mera_package_installer::tuf::RootMetadata,
    std::collections::HashMap<String, (u16, Vec<u8>)>,
) {
    use mera_package_installer::tuf::*;
    use sha2::{Digest, Sha256};

    let seed = [42u8; 32];
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let pub_hex = hex::encode(signing_key.verifying_key().to_bytes());
    let key_id = hex::encode(Sha256::digest(
        serde_json::to_vec(&serde_json::json!({
            "keytype": "ed25519",
            "scheme": "ed25519",
            "keyval": { "public": pub_hex }
        }))
        .unwrap(),
    ));
    let key = TufKey {
        key_type: "ed25519".to_owned(),
        scheme: "ed25519".to_owned(),
        keyval: TufKeyVal { public: pub_hex },
    };
    let role_cfg = RoleConfig {
        keyids: vec![key_id.clone()],
        threshold: 1,
    };

    let root_signed = RootSigned {
        kind: "root".to_owned(),
        version: 1,
        expires: "2999-01-01T00:00:00Z".to_owned(),
        keys: std::collections::HashMap::from([(key_id.clone(), key)]),
        roles: std::collections::HashMap::from([
            ("root".to_owned(), role_cfg.clone()),
            ("timestamp".to_owned(), role_cfg.clone()),
            ("snapshot".to_owned(), role_cfg.clone()),
            ("targets".to_owned(), role_cfg),
        ]),
    };
    let root = RootMetadata {
        signatures: vec![SignatureEntry {
            keyid: key_id.clone(),
            sig: sign_tuf_payload(&signing_key, &root_signed),
        }],
        signed: root_signed,
    };

    let archive_sha = hex::encode(Sha256::digest(archive_bytes));
    let archive_name = format!("{}-{}.rmod.tar.gz", manifest.id, manifest.version);
    let targets_signed = TargetsSigned {
        kind: "targets".to_owned(),
        version: 1,
        expires: "2999-01-01T00:00:00Z".to_owned(),
        targets: std::collections::HashMap::from([(
            archive_name.clone(),
            TargetDesc {
                length: archive_bytes.len() as u64,
                hashes: std::collections::HashMap::from([("sha256".to_owned(), archive_sha)]),
                custom: Some(json!({
                    "id": manifest.id,
                    "name": manifest.name,
                    "version": manifest.version,
                    "description": "Official TUF test mod",
                    "api": manifest.api,
                    "target": package.target,
                    "author": package.author,
                    "homepage": package.homepage,
                    "license": package.license,
                    "permissions": package.permissions,
                    "capabilities": ["echo"],
                })),
            },
        )]),
    };
    let targets = TargetsMetadata {
        signatures: vec![SignatureEntry {
            keyid: key_id.clone(),
            sig: sign_tuf_payload(&signing_key, &targets_signed),
        }],
        signed: targets_signed,
    };
    let targets_bytes = serde_json::to_vec(&targets).unwrap();

    let snapshot_signed = SnapshotSigned {
        kind: "snapshot".to_owned(),
        version: 1,
        expires: "2999-01-01T00:00:00Z".to_owned(),
        meta: std::collections::HashMap::from([(
            "targets.json".to_owned(),
            TargetsMetaEntry {
                version: 1,
                length: Some(targets_bytes.len() as u64),
                hashes: Some(std::collections::HashMap::from([(
                    "sha256".to_owned(),
                    hex::encode(Sha256::digest(&targets_bytes)),
                )])),
            },
        )]),
    };
    let snapshot = SnapshotMetadata {
        signatures: vec![SignatureEntry {
            keyid: key_id.clone(),
            sig: sign_tuf_payload(&signing_key, &snapshot_signed),
        }],
        signed: snapshot_signed,
    };
    let snapshot_bytes = serde_json::to_vec(&snapshot).unwrap();

    let timestamp_signed = TimestampSigned {
        kind: "timestamp".to_owned(),
        version: timestamp_version,
        expires: "2999-01-01T00:00:00Z".to_owned(),
        meta: std::collections::HashMap::from([(
            "snapshot.json".to_owned(),
            SnapshotMetaEntry {
                version: 1,
                length: Some(snapshot_bytes.len() as u64),
                hashes: Some(std::collections::HashMap::from([(
                    "sha256".to_owned(),
                    hex::encode(Sha256::digest(&snapshot_bytes)),
                )])),
            },
        )]),
    };
    let timestamp = TimestampMetadata {
        signatures: vec![SignatureEntry {
            keyid: key_id,
            sig: sign_tuf_payload(&signing_key, &timestamp_signed),
        }],
        signed: timestamp_signed,
    };
    let timestamp_bytes = serde_json::to_vec(&timestamp).unwrap();

    let routes = std::collections::HashMap::from([
        ("/timestamp.json".to_owned(), (200, timestamp_bytes)),
        ("/1.snapshot.json".to_owned(), (200, snapshot_bytes)),
        ("/1.targets.json".to_owned(), (200, targets_bytes)),
        (format!("/{archive_name}"), (200, archive_bytes.to_vec())),
    ]);
    (root, routes)
}

async fn wait_for_download_status(
    events: &mut mera_core::event::EventReceiver,
    task_id: &str,
    expected_status: &str,
) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let ev = tokio::time::timeout_at(deadline, events.recv())
            .await
            .unwrap_or_else(|_| {
                panic!("download event timeout waiting for `{expected_status}` on task `{task_id}`")
            })
            .expect("event bus closed");
        if let mera_core::event::Event::Mod { kind, payload } = ev
            && kind == "mod.download_progress"
            && payload["taskId"] == task_id
        {
            let status = payload["status"].as_str().unwrap_or_default();
            if status == expected_status {
                return;
            }
            if matches!(status, "failed" | "cancelled") {
                panic!(
                    "download task `{task_id}` entered terminal status `{status}` while waiting for `{expected_status}`: {payload}"
                );
            }
        }
    }
}

#[tokio::test]
async fn official_tuf_catalog_browse_download_install_and_rollback_guard() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("workspace");
    let app_data = temp.path().join("app-data");
    let exe_suffix = std::env::consts::EXE_SUFFIX;
    let bin_rel = format!("bin/fixture-mod-v1{exe_suffix}");
    let man = manifest_v1("0.1.0");
    let pkg = package_meta(&man, &bin_rel, ModPermissions::default());
    let bin_bytes = std::fs::read(fixture_binary()).unwrap();
    let archive_bytes = build_rmod_archive(&pkg, &man, &bin_rel, &bin_bytes);

    let (root, routes) = build_signed_tuf_bundle(&archive_bytes, &man, &pkg, 2);
    let (base_url, server_handle) = spawn_test_http_server(routes).await;

    let host = setup_workspace(&base, &app_data).await;
    host.mod_manager().set_official_catalog_policy(
        mera_app_host::mod_manager::OfficialCatalogPolicy {
            discovery_url: format!("{base_url}/"),
            trusted_root: root.clone(),
        },
    );

    let (items, outcomes) = host
        .mod_manager()
        .browse_remote_catalog(Some("official"), true)
        .await
        .unwrap();
    assert!(outcomes[0].ok, "official fetch must succeed: {outcomes:?}");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "ext_demo");
    assert!(items[0].compatible);

    let mut events = host.event_bus().subscribe();
    let task_id = host
        .mod_manager()
        .start_remote_download("official", "ext_demo", None)
        .await
        .expect("start official download");

    wait_for_download_status(&mut events, &task_id, "installed").await;

    let installed = host.mod_manager().list_installed();
    assert_eq!(installed.len(), 1);
    assert!(installed[0].health.is_available());
    assert!(
        installed[0].record.agent_visible_operations.is_empty(),
        "official mod operations must default hidden from Agent per D2"
    );

    let project = ProjectId::new("default");
    let agent_err = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "ext_demo.echo".to_owned(),
                input: json!({ "text": "blocked" }),
                context: agent_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect_err("default-hidden official op must reject Agent");
    assert_eq!(
        agent_err.code().as_str(),
        "EXT_DEMO/CALLER_NOT_PERMITTED_TO_AGENT"
    );

    // Rollback attack: serve timestamp version 1 (< stored 2) -> must fail closed.
    server_handle.abort();
    let (_, stale_routes) = build_signed_tuf_bundle(&archive_bytes, &man, &pkg, 1);
    let (stale_url, stale_handle) = spawn_test_http_server(stale_routes).await;
    host.mod_manager().set_official_catalog_policy(
        mera_app_host::mod_manager::OfficialCatalogPolicy {
            discovery_url: format!("{stale_url}/"),
            trusted_root: root,
        },
    );
    let (_, stale_outcomes) = host
        .mod_manager()
        .browse_remote_catalog(Some("official"), true)
        .await
        .unwrap();
    assert!(!stale_outcomes[0].ok, "TUF rollback must be rejected");

    stale_handle.abort();
    host.shutdown().await;
}

#[tokio::test]
async fn third_party_signed_index_holds_for_consent_supports_cancel_and_installs() {
    use ed25519_dalek::{Signer, SigningKey};
    use sha2::{Digest, Sha256};

    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("workspace");
    let app_data = temp.path().join("app-data");
    let exe_suffix = std::env::consts::EXE_SUFFIX;
    let bin_rel = format!("bin/fixture-mod-v1{exe_suffix}");
    let man = manifest_v1("0.1.0");
    let pkg = package_meta(&man, &bin_rel, ModPermissions::default());
    let bin_bytes = std::fs::read(fixture_binary()).unwrap();
    let archive_bytes = build_rmod_archive(&pkg, &man, &bin_rel, &bin_bytes);
    let archive_sha = hex::encode(Sha256::digest(&archive_bytes));

    let signing_key = SigningKey::from_bytes(&[9u8; 32]);
    let pub_hex = hex::encode(signing_key.verifying_key().to_bytes());
    let entries = vec![mera_app_host::mod_manager::ThirdPartyIndexEntry {
        id: "ext_demo".to_owned(),
        name: "External Demo".to_owned(),
        version: "0.1.0".to_owned(),
        description: Some("Community demo mod".to_owned()),
        api: "0.1".to_owned(),
        author: Some("Community".to_owned()),
        homepage: None,
        license: Some("MIT".to_owned()),
        permissions: pkg.permissions.clone(),
        capabilities: vec!["echo".to_owned()],
        artifacts: vec![mera_app_host::mod_manager::ModArtifactEntry {
            target: "noarch".to_owned(),
            url: "ext_demo-0.1.0.rmod.tar.gz".to_owned(),
            sha256: archive_sha,
            size_bytes: Some(archive_bytes.len() as u64),
        }],
    }];
    let signable = json!({
        "schema_version": 1,
        "name": "Signed Community",
        "homepage": null,
        "mods": entries,
    });
    let canonical = serde_json::to_vec(&serde_json::to_value(&signable).unwrap()).unwrap();
    let sig_hex = hex::encode(signing_key.sign(&canonical).to_bytes());
    let index_bytes = serde_json::to_vec(&json!({
        "schema_version": 1,
        "name": "Signed Community",
        "homepage": null,
        "mods": entries,
        "signature": sig_hex,
    }))
    .unwrap();

    let routes = std::collections::HashMap::from([
        ("/index.json".to_owned(), (200, index_bytes)),
        (
            "/ext_demo-0.1.0.rmod.tar.gz".to_owned(),
            (200, archive_bytes),
        ),
    ]);
    let (base_url, server_handle) = spawn_test_http_server(routes).await;

    let host = setup_workspace(&base, &app_data).await;
    assert!(
        host.mod_manager()
            .add_third_party_source("Bad", "http://example.com/index.json", None)
            .is_err(),
        "non-loopback http:// must be rejected"
    );

    let source = host
        .mod_manager()
        .add_third_party_source(
            "Signed Community",
            &format!("{base_url}/index.json"),
            Some(&pub_hex),
        )
        .expect("add signed third-party source");

    let (items, outcomes) = host
        .mod_manager()
        .browse_remote_catalog(Some(&source.id), true)
        .await
        .unwrap();
    assert!(outcomes[0].ok);
    assert_eq!(items.len(), 1);

    let mut events = host.event_bus().subscribe();
    let task_cancel = host
        .mod_manager()
        .start_remote_download(&source.id, "ext_demo", None)
        .await
        .unwrap();

    wait_for_download_status(&mut events, &task_cancel, "awaiting_consent").await;

    // Must not touch inventory or packages directory before explicit consent.
    assert!(host.mod_manager().list_installed().is_empty());
    assert!(
        !host
            .mod_manager()
            .paths()
            .mod_package_root("ext_demo")
            .exists()
    );

    let cancelled = host
        .mod_manager()
        .cancel_remote_download(&task_cancel)
        .unwrap();
    assert_eq!(
        cancelled.status,
        mera_app_host::mod_manager::ModDownloadStatus::Cancelled
    );
    assert!(host.mod_manager().list_installed().is_empty());

    // Download again and approve with explicit Agent visibility on `ext_demo.echo`.
    let task_ok = host
        .mod_manager()
        .start_remote_download(&source.id, "ext_demo", None)
        .await
        .unwrap();
    wait_for_download_status(&mut events, &task_ok, "awaiting_consent").await;

    let installed = host
        .mod_manager()
        .confirm_downloaded_install(
            &task_ok,
            true,
            ["ext_demo.echo".to_owned()].into_iter().collect(),
        )
        .await
        .expect("confirm third-party download");
    assert!(installed.health.is_available());
    assert!(
        installed
            .record
            .agent_visible_operations
            .contains("ext_demo.echo")
    );

    let res = host
        .operation_service()
        .invoke(
            &ProjectId::new("default"),
            InvokeRequest {
                name: "ext_demo.echo".to_owned(),
                input: json!({ "text": "remote-ok" }),
                context: agent_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect("agent invocation succeeds after explicit check");
    assert_eq!(res.output["echo"], "0.1.0:remote-ok");

    server_handle.abort();
    host.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn portable_uv_bootstrap_and_uv_entrypoint_mod_run_inside_sandbox() {
    use sha2::{Digest, Sha256};

    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("workspace");
    let app_data = temp.path().join("app-data");
    let bin_bytes = std::fs::read(fixture_binary()).unwrap();

    // Build a portable `uv` archive whose `uv` binary handles `uv run --no-project <script>`
    // by executing the target binary named inside `<script>`.
    let uv_shim = b"#!/bin/sh\nwhile [ \"$1\" = \"run\" ] || [ \"$1\" = \"--no-project\" ]; do shift; done\nif [ \"$1\" = \"--python\" ]; then shift 2; fi\nSCRIPT=\"$1\"\nshift\nTARGET=$(cat \"$SCRIPT\")\nexec \"$TARGET\" \"$@\"\n";
    let mut uv_tar = Vec::new();
    {
        let gz = flate2::write::GzEncoder::new(&mut uv_tar, flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        let mut header = tar::Header::new_gnu();
        header.set_size(uv_shim.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, "uv", &uv_shim[..]).unwrap();
        tar.finish().unwrap();
    }
    let uv_sha = hex::encode(Sha256::digest(&uv_tar));

    let routes = std::collections::HashMap::from([("/uv.tar.gz".to_owned(), (200, uv_tar))]);
    let (base_url, server_handle) = spawn_test_http_server(routes).await;

    let host = setup_workspace(&base, &app_data).await;
    let status = host
        .mod_manager()
        .bootstrap_uv(Some(&format!("{base_url}/uv.tar.gz")), Some(&uv_sha))
        .await
        .expect("bootstrap portable uv");
    assert!(status.available);
    assert_eq!(
        status.source,
        mera_app_host::mod_manager::UvSource::ManagedPortable
    );

    // Mount a local mod directory using `EntrypointSpec::Uv` pointing into a script
    // whose content names the bundled fixture executable inside the package directory.
    let uv_mod_dir = temp.path().join("uv-mod");
    std::fs::create_dir_all(uv_mod_dir.join("runtime")).unwrap();
    let target_bin = uv_mod_dir.join("runtime").join("fixture-mod-v1");
    std::fs::write(&target_bin, &bin_bytes).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&target_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let canonical_target_bin = std::fs::canonicalize(&target_bin).unwrap();
    std::fs::write(
        uv_mod_dir.join("runtime").join("entry.sh"),
        format!("{}\n", canonical_target_bin.display()),
    )
    .unwrap();

    let man = manifest_v1("0.1.0");
    let pkg = ModPackageManifest {
        schema_version: 1,
        id: man.id.clone(),
        version: man.version.clone(),
        target: "noarch".to_owned(),
        api: man.api.clone(),
        author: Some("Community".to_owned()),
        homepage: None,
        license: Some("MIT".to_owned()),
        entrypoint: EntrypointSpec::Uv {
            script: "runtime/entry.sh".to_owned(),
            python: Some("3.12".to_owned()),
            requirements: None,
            args: Vec::new(),
        },
        permissions: ModPermissions::default(),
    };
    std::fs::write(
        uv_mod_dir.join("mod-package.json"),
        serde_json::to_vec_pretty(&pkg).unwrap(),
    )
    .unwrap();
    std::fs::write(
        uv_mod_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&man).unwrap(),
    )
    .unwrap();

    let installed = host
        .mod_manager()
        .install_local_directory(
            &uv_mod_dir,
            InstallModRequest::default()
                .with_consent()
                .enable(true)
                .agent_visible_operations(["ext_demo.echo".to_owned()].into_iter().collect()),
        )
        .await
        .expect("install uv entrypoint mod");
    assert!(installed.health.is_available());

    let res = host
        .operation_service()
        .invoke(
            &ProjectId::new("default"),
            InvokeRequest {
                name: "ext_demo.echo".to_owned(),
                input: json!({ "text": "uv-sandbox-ok" }),
                context: agent_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect("uv mod invocation succeeds under sandbox");
    assert_eq!(res.output["echo"], "0.1.0:uv-sandbox-ok");

    server_handle.abort();
    host.shutdown().await;
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn seatbelt_blocks_mod_writes_outside_allowed_paths() {
    let temp = tempfile::tempdir().unwrap();
    let pkg_dir = temp.path().join("pkg");
    let data_dir = temp.path().join("data");
    let forbidden_dir = temp.path().join("forbidden");
    std::fs::create_dir_all(&pkg_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&forbidden_dir).unwrap();

    let allowed_file = data_dir.join("allowed.txt");
    let forbidden_file = forbidden_dir.join("escape.txt");
    let policy = mera_sandbox::SandboxPolicy::mod_restricted(pkg_dir, data_dir);
    let script = format!(
        "echo ok > '{}' && echo bad > '{}'",
        allowed_file.display(),
        forbidden_file.display()
    );
    let launch = mera_sandbox::resolve_sandbox_launch(
        Path::new("/bin/sh"),
        &["-c".into(), script.into()],
        &policy,
    )
    .expect("resolve seatbelt launch");
    assert_eq!(launch.backend, mera_sandbox::SandboxBackend::Seatbelt);

    let mut spec =
        mod_sdk::ProcessModSpec::new("/bin/sh").with_sandbox(mod_sdk::ProcessSandboxLaunch {
            backend: launch.backend.as_str().to_owned(),
            os_enforced: launch.backend.is_os_enforced(),
            program: launch.program,
            args: launch.args,
            env: launch.env,
        });
    spec.args = vec!["-c".into(), "true".into()];

    let err = mod_sdk::ProcessMod::spawn(spec)
        .await
        .expect_err("seatbelt must reject writing outside allowed paths");
    assert_eq!(std::fs::read_to_string(&allowed_file).unwrap(), "ok\n");
    assert!(!forbidden_file.exists(), "forbidden file created: {err:?}");
}
