use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use reimagine_app_host::operation_service::InvokeRequest;
use reimagine_app_host::step_log::StepLog;
use reimagine_app_host::{WorkspaceHostBuilder, WorkspaceScope};
use reimagine_core::command::{
    CommandActor, CommandActorKind, CommandProvenance, OperationContext,
};
use reimagine_core::event::Timestamp;
use reimagine_core::model::ProjectId;
use reimagine_core::project::ProjectMetadata;
use reimagine_core::task::CancelToken;
use serde_json::json;

fn temp_dir(prefix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("reimagine-agent-path-{prefix}-{nonce}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::canonicalize(dir).expect("canonicalize temp dir")
}

fn agent_context(id: &str) -> OperationContext {
    OperationContext::new(
        CommandActor::new(CommandActorKind::Agent).with_id(id),
        CommandProvenance::Direct,
    )
}

fn human_context(id: &str) -> OperationContext {
    OperationContext::new(
        CommandActor::new(CommandActorKind::Human).with_id(id),
        CommandProvenance::Direct,
    )
}

// 1. Folders: add/list/remove via ops; duplicate rejected; non-directory and missing path rejected;
// a folder inside the data root rejected; project.json without folders still loads; folder list persists across a host rebuild.
#[tokio::test]
async fn test_workspace_folders_ops_and_persistence() {
    let base = temp_dir("folders-ops");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-folders"), &base)
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-folders");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Folders Project",
                "Testing folders",
                Timestamp::new("2026-01-01T00:00:00Z"),
                Timestamp::new("2026-01-01T00:00:00Z"),
            ),
        )
        .await
        .expect("create project");

    let folder_a = base
        .parent()
        .unwrap()
        .join(format!("folder-a-{}", std::process::id()));
    std::fs::create_dir_all(&folder_a).expect("create folder a");
    let folder_a_canon = std::fs::canonicalize(&folder_a).expect("canonicalize folder a");

    // Add folder via project.add_folder op (human caller)
    let add_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "project.add_folder".to_string(),
                input: json!({ "path": folder_a.to_str().unwrap() }),
                context: human_context("user-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("add folder op");

    let folder_id = add_res.output["folder"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        add_res.output["folder"]["path"].as_str().unwrap(),
        folder_a_canon.to_str().unwrap()
    );

    // List folders via project.list_folders op
    let list_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "project.list_folders".to_string(),
                input: json!({}),
                context: human_context("user-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("list folders op");

    let folders = list_res.output["folders"].as_array().unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0]["id"].as_str().unwrap(), folder_id);
    assert_eq!(
        folders[0]["path"].as_str().unwrap(),
        folder_a_canon.to_str().unwrap()
    );

    // Duplicate folder rejected
    let dup_err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "project.add_folder".to_string(),
                input: json!({ "path": folder_a.to_str().unwrap() }),
                context: human_context("user-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("duplicate folder must be rejected");
    assert!(dup_err.code().as_str().contains("DUPLICATE_FOLDER"));

    // Missing path rejected
    let missing_path = folder_a.join("does_not_exist_xyz");
    let missing_err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "project.add_folder".to_string(),
                input: json!({ "path": missing_path.to_str().unwrap() }),
                context: human_context("user-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("missing path must be rejected");
    assert!(missing_err.code().as_str().contains("INVALID_PATH"));

    // Non-directory rejected
    let file_path = folder_a.join("not_a_dir.txt");
    std::fs::write(&file_path, "regular file").expect("write file");
    let file_err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "project.add_folder".to_string(),
                input: json!({ "path": file_path.to_str().unwrap() }),
                context: human_context("user-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("file path must be rejected");
    assert!(file_err.code().as_str().contains("NOT_A_DIRECTORY"));

    // Folder inside data root rejected
    let inside_data = host.config().paths().projects_dir().join("some_dir");
    std::fs::create_dir_all(&inside_data).expect("create inside data");
    let inside_err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "project.add_folder".to_string(),
                input: json!({ "path": inside_data.to_str().unwrap() }),
                context: human_context("user-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("inside data root must be rejected");
    assert!(inside_err.code().as_str().contains("INSIDE_DATA_ROOT"));

    // Remove folder via project.remove_folder op
    let remove_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "project.remove_folder".to_string(),
                input: json!({ "id": folder_id }),
                context: human_context("user-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("remove folder op");
    assert_eq!(remove_res.output["removed"].as_str().unwrap(), folder_id);

    // After removal, folder list is empty
    let list_after = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "project.list_folders".to_string(),
                input: json!({}),
                context: human_context("user-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("list after remove");
    assert_eq!(list_after.output["folders"].as_array().unwrap().len(), 0);

    // Add folder back for persistence test
    let add_again = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "project.add_folder".to_string(),
                input: json!({ "path": folder_a.to_str().unwrap() }),
                context: human_context("user-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("re-add folder");
    let new_folder_id = add_again.output["folder"]["id"].as_str().unwrap();

    // Verify folder list persists across a host rebuild
    drop(host);
    let rebuilt_host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-folders"), &base)
        .build()
        .await
        .expect("rebuild host");

    let loaded_folders = rebuilt_host
        .project_service()
        .folders(&project_id)
        .await
        .expect("read folders from rebuilt host");
    assert_eq!(loaded_folders.len(), 1);
    assert_eq!(loaded_folders[0].id().as_str(), new_folder_id);
    assert_eq!(loaded_folders[0].path(), folder_a_canon.to_str().unwrap());

    // Project without folders key still loads
    let legacy_project_file = rebuilt_host
        .project_service()
        .project_file(&ProjectId::new("legacy"));
    let legacy_dir = legacy_project_file.parent().unwrap();
    std::fs::create_dir_all(legacy_dir).expect("create legacy dir");
    std::fs::write(
        &legacy_project_file,
        r#"{
            "id": "legacy",
            "metadata": {
                "name": "Legacy Project",
                "description": "No folders key",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z"
            }
        }"#,
    )
    .expect("write legacy project");
    let legacy_loaded = rebuilt_host
        .project_service()
        .load_project(&ProjectId::new("legacy"))
        .await
        .expect("legacy project loads successfully");
    assert!(legacy_loaded.folders().is_empty());

    let _ = std::fs::remove_dir_all(&folder_a);
    let _ = std::fs::remove_dir_all(&base);
}

// 2. Agent library.add: inside folder succeeds (with agent actor in step record); outside -> PATH_OUTSIDE_WORKSPACE,
// nothing ingested, no step record; via symlink inside folder pointing outside -> rejected; inside data root -> rejected;
// no folders -> rejected.
#[tokio::test]
async fn test_agent_library_add_path_policy() {
    let base = temp_dir("agent-lib-add");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib-policy"), &base)
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-lib-agent");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Lib Agent Project",
                "Testing library.add agent policy",
                Timestamp::new("2026-01-01T00:00:00Z"),
                Timestamp::new("2026-01-01T00:00:00Z"),
            ),
        )
        .await
        .expect("create project");

    let folder = base
        .parent()
        .unwrap()
        .join(format!("agent-folder-{}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("create folder");
    let valid_file = folder.join("file_valid.png");
    std::fs::write(&valid_file, "valid content").expect("write valid file");

    let outside_folder = base
        .parent()
        .unwrap()
        .join(format!("agent-outside-{}", std::process::id()));
    std::fs::create_dir_all(&outside_folder).expect("create outside folder");
    let outside_file = outside_folder.join("file_outside.png");
    std::fs::write(&outside_file, "outside content").expect("write outside file");

    // Case 2e: No folders configured yet -> agent library.add rejected with PATH_OUTSIDE_WORKSPACE
    let no_folders_err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [valid_file.to_str().unwrap()] }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("must reject when project has no folders");
    assert_eq!(
        no_folders_err.code().as_str(),
        "LIBRARY/CALLER_PATH_OUTSIDE_WORKSPACE"
    );

    // Configure the workspace folder
    host.project_service()
        .add_folder(&project_id, &folder)
        .await
        .expect("add folder");

    // Case 2a: Inside folder succeeds; library step record actor is the agent
    let add_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [valid_file.to_str().unwrap()] }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("agent library.add inside folder must succeed");

    assert_eq!(add_res.output["items"].as_array().unwrap().len(), 1);

    // Verify library step record has actor = agent
    let lib_step_log = host.library_service().step_log();
    let records = lib_step_log.records().await.expect("library step records");
    let last_record = records.last().expect("must have step record");
    assert_eq!(last_record.context.actor().kind(), CommandActorKind::Agent);
    assert_eq!(last_record.context.actor().id(), Some("agent-1"));

    // Case 2b: Outside folder -> PATH_OUTSIDE_WORKSPACE, nothing ingested, no project step record written
    let count_before = host
        .library_service()
        .list(&Default::default())
        .await
        .expect("list")
        .len();
    let proj_records_before = StepLog::new(host.config().paths().project_dir(project_id.as_str()))
        .records()
        .await
        .expect("proj records")
        .len();

    let outside_err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [outside_file.to_str().unwrap()] }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("outside file must be rejected");
    assert_eq!(
        outside_err.code().as_str(),
        "LIBRARY/CALLER_PATH_OUTSIDE_WORKSPACE"
    );

    let count_after = host
        .library_service()
        .list(&Default::default())
        .await
        .expect("list")
        .len();
    assert_eq!(count_before, count_after, "nothing was ingested");

    let proj_records_after = StepLog::new(host.config().paths().project_dir(project_id.as_str()))
        .records()
        .await
        .expect("proj records")
        .len();
    assert_eq!(
        proj_records_before, proj_records_after,
        "no project step record written"
    );

    // Case 2c: Symlink inside folder pointing outside -> rejected
    let symlink_path = folder.join("symlink_outside.png");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside_file, &symlink_path).expect("create symlink");
    #[cfg(unix)]
    {
        let symlink_err = host
            .operation_service()
            .invoke(
                &project_id,
                InvokeRequest {
                    name: "library.add".to_string(),
                    input: json!({ "paths": [symlink_path.to_str().unwrap()] }),
                    context: agent_context("agent-1"),
                    cancel: CancelToken::new(),
                    run: None,
                },
            )
            .await
            .expect_err("symlink pointing outside must be rejected");
        assert_eq!(
            symlink_err.code().as_str(),
            "LIBRARY/CALLER_PATH_OUTSIDE_WORKSPACE"
        );
    }

    // Case 2d: Path inside data root even when a folder contains the data root -> rejected
    let data_file = host.config().paths().base_path().join("data_secret.txt");
    std::fs::write(&data_file, "data secret").expect("write data secret");
    let parent_of_base = host.config().paths().base_path().parent().unwrap();
    // Add parent_of_base directly to project folders in memory to simulate a folder containing data root
    let wide_folder_id = reimagine_core::model::WorkspaceFolderId::new("f-wide");
    let wide_folder = reimagine_core::project::WorkspaceFolder::new(
        wide_folder_id,
        std::fs::canonicalize(parent_of_base)
            .unwrap()
            .to_str()
            .unwrap(),
    );
    let mut proj = host
        .project_service()
        .load_project(&project_id)
        .await
        .unwrap();
    proj.add_folder(wide_folder);
    host.project_service()
        .update_project(&project_id, proj.metadata().clone())
        .await
        .unwrap();

    let data_root_err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [data_file.to_str().unwrap()] }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("data root file must be rejected");
    assert_eq!(
        data_root_err.code().as_str(),
        "LIBRARY/CALLER_PATH_OUTSIDE_WORKSPACE"
    );

    let _ = std::fs::remove_dir_all(&folder);
    let _ = std::fs::remove_dir_all(&outside_folder);
    let _ = std::fs::remove_dir_all(&base);
}

// 3. Agent asset.import: paths inside/outside cases; library_items succeeds with step actor = agent.
#[tokio::test]
async fn test_agent_asset_import_path_policy() {
    let base = temp_dir("agent-asset-import");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-asset-policy"), &base)
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-asset-agent");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Asset Agent Project",
                "Testing asset.import agent policy",
                Timestamp::new("2026-01-01T00:00:00Z"),
                Timestamp::new("2026-01-01T00:00:00Z"),
            ),
        )
        .await
        .expect("create project");

    let folder = base
        .parent()
        .unwrap()
        .join(format!("asset-folder-{}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("create folder");
    let valid_file = folder.join("valid_asset.png");
    std::fs::write(&valid_file, "asset content").expect("write asset");

    let outside_folder = base
        .parent()
        .unwrap()
        .join(format!("asset-outside-{}", std::process::id()));
    std::fs::create_dir_all(&outside_folder).expect("create outside folder");
    let outside_file = outside_folder.join("outside_asset.png");
    std::fs::write(&outside_file, "outside content").expect("write outside asset");

    host.project_service()
        .add_folder(&project_id, &folder)
        .await
        .expect("add folder");

    // Inside folder succeeds
    let import_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "paths": [valid_file.to_str().unwrap()] }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("import inside folder must succeed");

    assert_eq!(import_res.output["assets"].as_array().unwrap().len(), 1);

    // Check step record actor = agent
    let project_dir = host.config().paths().project_dir(project_id.as_str());
    let records = StepLog::new(project_dir.clone())
        .records()
        .await
        .expect("records");
    let last = records.last().expect("last record");
    assert_eq!(last.context.actor().kind(), CommandActorKind::Agent);
    assert_eq!(last.context.actor().id(), Some("agent-1"));

    // Outside folder fails with PATH_OUTSIDE_WORKSPACE
    let outside_err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "paths": [outside_file.to_str().unwrap()] }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("outside asset must be rejected");
    assert_eq!(
        outside_err.code().as_str(),
        "ASSET/CALLER_PATH_OUTSIDE_WORKSPACE"
    );

    // Symlink inside pointing outside fails
    let symlink_path = folder.join("symlink_outside.png");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside_file, &symlink_path).expect("create symlink");
    #[cfg(unix)]
    {
        let symlink_err = host
            .operation_service()
            .invoke(
                &project_id,
                InvokeRequest {
                    name: "asset.import".to_string(),
                    input: json!({ "paths": [symlink_path.to_str().unwrap()] }),
                    context: agent_context("agent-1"),
                    cancel: CancelToken::new(),
                    run: None,
                },
            )
            .await
            .expect_err("symlink pointing outside must be rejected");
        assert_eq!(
            symlink_err.code().as_str(),
            "ASSET/CALLER_PATH_OUTSIDE_WORKSPACE"
        );
    }

    // Agent asset.import { library_items } succeeds with step actor = agent
    let lib_add_res = host
        .library_service()
        .add(vec![valid_file.clone()], &human_context("user-1"))
        .await
        .expect("add to library");
    let lib_item_id = lib_add_res.items[0].id.as_str();

    let lib_import_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "library_items": [lib_item_id] }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("import library items by agent must succeed");
    assert_eq!(lib_import_res.output["assets"].as_array().unwrap().len(), 1);

    let records_after = StepLog::new(project_dir).records().await.expect("records");
    let last_after = records_after.last().expect("last record");
    assert_eq!(last_after.context.actor().kind(), CommandActorKind::Agent);
    assert_eq!(last_after.context.actor().id(), Some("agent-1"));

    let _ = std::fs::remove_dir_all(&folder);
    let _ = std::fs::remove_dir_all(&outside_folder);
    let _ = std::fs::remove_dir_all(&base);
}

// 4. Agent library.remove, library.purge, project.add_folder, project.remove_folder -> NOT_PERMITTED_TO_AGENT;
// agent library.list, project.list_folders succeed.
#[tokio::test]
async fn test_agent_denied_and_allowed_operations() {
    let base = temp_dir("agent-denied-allowed");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-denied-allowed"), &base)
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-denied-allowed");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Test Proj",
                "Testing allowed/denied",
                Timestamp::new("2026-01-01T00:00:00Z"),
                Timestamp::new("2026-01-01T00:00:00Z"),
            ),
        )
        .await
        .expect("create project");

    // library.remove -> NOT_PERMITTED_TO_AGENT
    let err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.remove".to_string(),
                input: json!({ "ids": ["asset-dummy"] }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("library.remove denied");
    assert_eq!(err.code().as_str(), "LIBRARY/CALLER_NOT_PERMITTED_TO_AGENT");

    // library.purge -> NOT_PERMITTED_TO_AGENT
    let err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.purge".to_string(),
                input: json!({}),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("library.purge denied");
    assert_eq!(err.code().as_str(), "LIBRARY/CALLER_NOT_PERMITTED_TO_AGENT");

    // project.add_folder -> NOT_PERMITTED_TO_AGENT
    let err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "project.add_folder".to_string(),
                input: json!({ "path": "/some/path" }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("project.add_folder denied");
    assert_eq!(err.code().as_str(), "PROJECT/CALLER_NOT_PERMITTED_TO_AGENT");

    // project.remove_folder -> NOT_PERMITTED_TO_AGENT
    let err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "project.remove_folder".to_string(),
                input: json!({ "id": "folder-1-1" }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("project.remove_folder denied");
    assert_eq!(err.code().as_str(), "PROJECT/CALLER_NOT_PERMITTED_TO_AGENT");

    // library.list succeeds
    let list_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.list".to_string(),
                input: json!({}),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("library.list succeeds");
    assert!(list_res.output["items"].is_array());

    // project.list_folders succeeds
    let folders_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "project.list_folders".to_string(),
                input: json!({}),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("project.list_folders succeeds");
    assert!(folders_res.output["folders"].is_array());

    let _ = std::fs::remove_dir_all(&base);
}

// 5. Human asset.import / library.add with a path outside any folder still succeed (unchanged).
#[tokio::test]
async fn test_human_asset_import_and_library_add_outside_folders() {
    let base = temp_dir("human-outside-folders");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-human-outside"), &base)
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-human-outside");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Human Outside Project",
                "Testing human caller without folders",
                Timestamp::new("2026-01-01T00:00:00Z"),
                Timestamp::new("2026-01-01T00:00:00Z"),
            ),
        )
        .await
        .expect("create project");

    let outside = base
        .parent()
        .unwrap()
        .join(format!("human-outside-{}", std::process::id()));
    std::fs::create_dir_all(&outside).expect("create outside dir");
    let file = outside.join("human_file.png");
    std::fs::write(&file, "human content").expect("write human file");

    // Human library.add outside folders succeeds
    let lib_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [file.to_str().unwrap()] }),
                context: human_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("human library.add outside folders succeeds");
    assert_eq!(lib_res.output["items"].as_array().unwrap().len(), 1);

    // Human asset.import outside folders succeeds
    let import_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "paths": [file.to_str().unwrap()] }),
                context: human_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("human asset.import outside folders succeeds");
    assert_eq!(import_res.output["assets"].as_array().unwrap().len(), 1);

    let _ = std::fs::remove_dir_all(&outside);
    let _ = std::fs::remove_dir_all(&base);
}

// 6. The input the op receives for an agent call holds the canonical path
// (e.g. pass <folder>/sub/../file.txt and assert the step record input or the op sees the canonical form).
#[tokio::test]
async fn test_agent_input_rewritten_to_canonical_path() {
    let base = temp_dir("agent-canonical-rewrite");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-canonical"), &base)
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-canonical");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Canonical Proj",
                "Testing canonical rewrite",
                Timestamp::new("2026-01-01T00:00:00Z"),
                Timestamp::new("2026-01-01T00:00:00Z"),
            ),
        )
        .await
        .expect("create project");

    let folder = base
        .parent()
        .unwrap()
        .join(format!("canon-folder-{}", std::process::id()));
    let sub = folder.join("sub");
    std::fs::create_dir_all(&sub).expect("create sub dir");
    let file = folder.join("target.png");
    std::fs::write(&file, "target content").expect("write target file");
    let canonical_file = std::fs::canonicalize(&file).expect("canonicalize file");

    host.project_service()
        .add_folder(&project_id, &folder)
        .await
        .expect("add folder");

    let uncanonical_input_path = format!("{}/sub/../target.png", folder.display());

    let res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "paths": [uncanonical_input_path] }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("import succeeds");
    assert_eq!(res.output["assets"].as_array().unwrap().len(), 1);

    // Verify step record input holds the canonical path
    let project_dir = host.config().paths().project_dir(project_id.as_str());
    let records = StepLog::new(project_dir).records().await.expect("records");
    let last = records.last().expect("last record");
    let recorded_path = last.input["paths"][0].as_str().expect("recorded path str");
    assert_eq!(recorded_path, canonical_file.to_str().unwrap());

    let _ = std::fs::remove_dir_all(&folder);
    let _ = std::fs::remove_dir_all(&base);
}
