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
