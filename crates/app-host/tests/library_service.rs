use std::fs;
use std::path::{Path, PathBuf};

use reimagine_app_host::{
    AssetStore, InvokeRequest, LibraryListFilter, StepLog, WorkspaceHostBuilder, WorkspaceScope,
};
use reimagine_core::command::{
    CommandActor, CommandActorKind, CommandProvenance, OperationContext,
};
use reimagine_core::event::EventBus;
use reimagine_core::model::{AssetId, ProjectId};
use reimagine_core::project::ProjectMetadata;
use reimagine_core::task::CancelToken;
use serde_json::json;

fn temp_dir(prefix: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("reimagine-app-host-lib-{prefix}-{nonce}"))
}

fn test_context(actor_id: &str) -> OperationContext {
    OperationContext::new(
        CommandActor::new(CommandActorKind::Human).with_id(actor_id),
        CommandProvenance::Direct,
    )
}

fn agent_context(actor_id: &str) -> OperationContext {
    OperationContext::new(
        CommandActor::new(CommandActorKind::Agent).with_id(actor_id),
        CommandProvenance::Direct,
    )
}

fn count_files_in_dir(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    let mut count = 0;
    for entry in walkdir(dir) {
        if entry.is_file() {
            count += 1;
        }
    }
    count
}

fn walkdir(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walkdir(&path));
            } else {
                files.push(path);
            }
        }
    }
    files
}

#[tokio::test]
async fn library_add_deduplication_and_provenance() {
    let base = temp_dir("lib-add");
    let event_bus = EventBus::new();
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib"), &base)
        .event_bus(event_bus.clone())
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-lib-1");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Proj",
                "desc",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .expect("create project");

    // Prepare 3 test files where file1 and file2 have identical content
    let files_dir = temp_dir("source-files");
    fs::create_dir_all(&files_dir).unwrap();
    let file1 = files_dir.join("photo1.png");
    let file2 = files_dir.join("photo2_duplicate.png");
    let file3 = files_dir.join("doc.txt");

    fs::write(&file1, b"identical content for file 1 and 2").unwrap();
    fs::write(&file2, b"identical content for file 1 and 2").unwrap();
    fs::write(&file3, b"unique content for doc").unwrap();

    let req = InvokeRequest {
        name: "library.add".to_string(),
        input: json!({
            "paths": [
                file1.to_str().unwrap(),
                file2.to_str().unwrap(),
                file3.to_str().unwrap(),
            ]
        }),
        context: test_context("human-user-1"),
        cancel: CancelToken::new(),
        run: None,
    };

    let res = host
        .operation_service()
        .invoke(&project_id, req)
        .await
        .expect("invoke library.add");

    // 1. library.add of 3 files where 2 have identical content -> 2 items
    let items = res.output["items"].as_array().expect("items array");
    assert_eq!(
        items.len(),
        2,
        "3 files with 2 identical must yield 2 items"
    );

    // 2. library/assets/ holds exactly 2 content files
    let library_assets_dir = host.base_path().join("library/assets");
    let content_files_count = count_files_in_dir(&library_assets_dir);
    assert_eq!(
        content_files_count, 2,
        "library/assets/ must hold exactly 2 content files"
    );

    // 3. library.list returns 2 items named after the source files
    let list_req = InvokeRequest {
        name: "library.list".to_string(),
        input: json!({}),
        context: test_context("human-user-1"),
        cancel: CancelToken::new(),
        run: None,
    };
    let list_res = host
        .operation_service()
        .invoke(&project_id, list_req)
        .await
        .expect("invoke library.list");
    let list_items = list_res.output["items"].as_array().expect("items array");
    assert_eq!(list_items.len(), 2);
    let names: Vec<&str> = list_items
        .iter()
        .map(|it| it["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"photo1.png"));
    assert!(names.contains(&"doc.txt"));

    // 4. library/steps.jsonl has a finished library.add record carrying the actor
    let lib_step_log = StepLog::new(host.base_path().join("library"));
    let lib_steps = lib_step_log.records().await.expect("read library steps");
    assert_eq!(lib_steps.len(), 1);
    assert_eq!(lib_steps[0].op, "library.add");
    assert_eq!(lib_steps[0].context.actor().id(), Some("human-user-1"));

    // 5. the calling project's steps.jsonl also has the library.add step
    let proj_step_log = StepLog::new(host.config().paths().project_dir(project_id.as_str()));
    let proj_steps = proj_step_log.records().await.expect("read project steps");
    assert!(
        proj_steps
            .iter()
            .any(|s| s.op == "library.add" && s.context.actor().id() == Some("human-user-1")),
        "calling project's steps.jsonl must also record the library.add step"
    );
}

#[tokio::test]
async fn project_listing_does_not_include_the_library() {
    let base = temp_dir("lib-proj-list");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib"), &base)
        .build()
        .await
        .expect("build host");

    // Add a file to library to ensure library directory and index exist
    let files_dir = temp_dir("source-files-proj");
    fs::create_dir_all(&files_dir).unwrap();
    let file = files_dir.join("sample.txt");
    fs::write(&file, b"sample text").unwrap();

    let project_id = ProjectId::new("proj-real");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Real Proj",
                "desc",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .expect("create project");

    let req = InvokeRequest {
        name: "library.add".to_string(),
        input: json!({ "paths": [file.to_str().unwrap()] }),
        context: test_context("human-1"),
        cancel: CancelToken::new(),
        run: None,
    };
    host.operation_service()
        .invoke(&project_id, req)
        .await
        .expect("library.add");

    // Verify library directory exists
    assert!(host.base_path().join("library").exists());

    // Project listing must not include library
    let projects = host
        .project_service()
        .list_projects()
        .await
        .expect("list projects");

    assert!(projects.iter().all(|p| p.id().as_str() != "library"));
    assert!(projects.iter().any(|p| p.id().as_str() == "proj-real"));
}

#[tokio::test]
async fn library_rename_tag_remove_undo_redo_and_filter() {
    let base = temp_dir("lib-crud");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib"), &base)
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-crud");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Crud",
                "desc",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .expect("create project");

    // Add 2 files
    let files_dir = temp_dir("source-crud");
    fs::create_dir_all(&files_dir).unwrap();
    let file1 = files_dir.join("image.png");
    let file2 = files_dir.join("notes.txt");
    fs::write(&file1, b"fake png data").unwrap();
    fs::write(&file2, b"text data").unwrap();

    let add_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [file1.to_str().unwrap(), file2.to_str().unwrap()] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("add");

    let items = add_res.output["items"].as_array().unwrap();
    let id1 = items[0]["id"].as_str().unwrap().to_string();
    let id2 = items[1]["id"].as_str().unwrap().to_string();

    // 1. Rename
    let rename_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.rename".to_string(),
                input: json!({ "id": id1, "name": "renamed_image.png" }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("rename");
    assert!(rename_res.output["version"].as_u64().unwrap() > 0);

    // Verify name changed in list
    let list = host
        .library_service()
        .list(&LibraryListFilter::default())
        .await
        .expect("list");
    let item1 = list.iter().find(|i| i.id.as_str() == id1).unwrap();
    assert_eq!(item1.name, "renamed_image.png");

    // Undo rename
    host.operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.undo".to_string(),
                input: json!({}),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("undo");
    let list = host
        .library_service()
        .list(&LibraryListFilter::default())
        .await
        .expect("list");
    assert_eq!(
        list.iter().find(|i| i.id.as_str() == id1).unwrap().name,
        "image.png"
    );

    // Redo rename
    host.operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.redo".to_string(),
                input: json!({}),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("redo");
    let list = host
        .library_service()
        .list(&LibraryListFilter::default())
        .await
        .expect("list");
    assert_eq!(
        list.iter().find(|i| i.id.as_str() == id1).unwrap().name,
        "renamed_image.png"
    );

    // 2. Tag
    host.operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.tag".to_string(),
                input: json!({ "id": id1, "tags": ["landscape", "nature"] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("tag");

    let list = host
        .library_service()
        .list(&LibraryListFilter {
            tag: Some("nature".to_string()),
            ..Default::default()
        })
        .await
        .expect("list with tag");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id.as_str(), id1);

    // Undo tag
    host.operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.undo".to_string(),
                input: json!({}),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("undo tag");
    let list = host
        .library_service()
        .list(&LibraryListFilter {
            tag: Some("nature".to_string()),
            ..Default::default()
        })
        .await
        .expect("list with tag after undo");
    assert_eq!(list.len(), 0);

    // Redo tag
    host.operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.redo".to_string(),
                input: json!({}),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("redo tag");
    let list = host
        .library_service()
        .list(&LibraryListFilter {
            tag: Some("nature".to_string()),
            ..Default::default()
        })
        .await
        .expect("list with tag after redo");
    assert_eq!(list.len(), 1);

    // 3. Remove
    host.operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.remove".to_string(),
                input: json!({ "ids": [id1] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("remove");

    // library.list hides removed items unless include_removed is true
    let list_default = host
        .library_service()
        .list(&LibraryListFilter::default())
        .await
        .expect("list default");
    assert_eq!(list_default.len(), 1);
    assert_eq!(list_default[0].id.as_str(), id2);

    let list_all = host
        .library_service()
        .list(&LibraryListFilter {
            include_removed: true,
            ..Default::default()
        })
        .await
        .expect("list all");
    assert_eq!(list_all.len(), 2);
    let removed_item = list_all.iter().find(|i| i.id.as_str() == id1).unwrap();
    assert!(removed_item.removed);

    // Undo remove restores active status
    host.operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.undo".to_string(),
                input: json!({}),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("undo remove");

    let list_after_undo = host
        .library_service()
        .list(&LibraryListFilter::default())
        .await
        .expect("list after undo remove");
    assert_eq!(list_after_undo.len(), 2);

    // Redo remove re-hides
    host.operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.redo".to_string(),
                input: json!({}),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("redo remove");

    let list_after_redo = host
        .library_service()
        .list(&LibraryListFilter::default())
        .await
        .expect("list after redo remove");
    assert_eq!(list_after_redo.len(), 1);
}

#[tokio::test]
async fn agent_actor_policy_enforcement() {
    let base = temp_dir("lib-agent");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib"), &base)
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-agent");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "AgentProj",
                "desc",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .expect("create project");

    let files_dir = temp_dir("source-agent");
    fs::create_dir_all(&files_dir).unwrap();
    let file = files_dir.join("test.txt");
    fs::write(&file, b"content").unwrap();

    // 1. Agent call to library.add is rejected with NOT_PERMITTED_TO_AGENT
    let add_err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [file.to_str().unwrap()] }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("agent must be denied library.add without workspace folders");
    assert_eq!(
        add_err.code().as_str(),
        "LIBRARY/CALLER_PATH_OUTSIDE_WORKSPACE"
    );

    // 2. Agent call to library.remove is rejected with NOT_PERMITTED_TO_AGENT
    let remove_err = host
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
        .expect_err("agent must be denied library.remove");
    assert_eq!(
        remove_err.code().as_str(),
        "LIBRARY/CALLER_NOT_PERMITTED_TO_AGENT"
    );

    // 3. Agent call to library.list succeeds
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
        .expect("agent library.list must succeed");
    assert!(list_res.output["items"].is_array());
}

#[tokio::test]
async fn input_validation_empty_paths_symlink_missing_file() {
    let base = temp_dir("lib-validation");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib"), &base)
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-val");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "ValProj",
                "desc",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .expect("create project");

    let files_dir = temp_dir("source-val");
    fs::create_dir_all(&files_dir).unwrap();
    let valid_file = files_dir.join("valid.txt");
    fs::write(&valid_file, b"valid").unwrap();

    let missing_file = files_dir.join("nonexistent.txt");

    let symlink_file = files_dir.join("symlink.txt");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&valid_file, &symlink_file).unwrap();

    // 1. Empty paths rejected by schema validation
    let empty_err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("empty paths must be rejected");
    assert_eq!(empty_err.code().as_str(), "LIBRARY/CALLER_INVALID_INPUT");

    // Also direct call to library_service.add with empty paths rejected
    let service_empty_err = host
        .library_service()
        .add(vec![], &test_context("human-1"))
        .await
        .expect_err("service add empty paths must be rejected");
    assert_eq!(
        service_empty_err.code().as_str(),
        "LIBRARY/CALLER_EMPTY_PATHS"
    );

    // 2. Missing file rejected
    let missing_err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [missing_file.to_str().unwrap()] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("missing file must be rejected");
    assert_eq!(missing_err.code().as_str(), "LIBRARY/CALLER_FILE_NOT_FOUND");

    // 3. Symlink rejected
    #[cfg(unix)]
    {
        let symlink_err = host
            .operation_service()
            .invoke(
                &project_id,
                InvokeRequest {
                    name: "library.add".to_string(),
                    input: json!({ "paths": [symlink_file.to_str().unwrap()] }),
                    context: test_context("human-1"),
                    cancel: CancelToken::new(),
                    run: None,
                },
            )
            .await
            .expect_err("symlink must be rejected");
        assert_eq!(
            symlink_err.code().as_str(),
            "LIBRARY/CALLER_SYMLINK_REJECTED"
        );
    }

    // 4. Empty remove rejected
    host.operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.remove".to_string(),
                input: json!({ "ids": [] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("empty ids must be rejected");

    // Verify NO entries were added to the library
    let list = host
        .library_service()
        .list(&LibraryListFilter::default())
        .await
        .expect("list");
    assert_eq!(
        list.len(),
        0,
        "no entries must be added on validation failures"
    );
}

#[tokio::test]
async fn test_import_library_items_into_project() {
    let base = temp_dir("lib-import-test");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib"), &base)
        .build()
        .await
        .expect("build host");

    let project_a = ProjectId::new("proj-a");
    host.project_service()
        .create_project(
            project_a.clone(),
            ProjectMetadata::new(
                "Project A",
                "desc",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .expect("create project");

    let files_dir = temp_dir("source-import");
    fs::create_dir_all(&files_dir).unwrap();
    let file1 = files_dir.join("photo.png");
    let file2 = files_dir.join("readme.txt");
    fs::write(&file1, b"png content 12345").unwrap();
    fs::write(&file2, b"txt content 67890").unwrap();

    let add_res = host
        .operation_service()
        .invoke(
            &project_a,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [file1.to_str().unwrap(), file2.to_str().unwrap()] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("library.add");

    let add_items = add_res.output["items"].as_array().unwrap();
    assert_eq!(add_items.len(), 2);
    let lib_id1 = add_items[0]["id"].as_str().unwrap().to_string();
    let lib_id2 = add_items[1]["id"].as_str().unwrap().to_string();

    let lib_list = host
        .library_service()
        .list(&LibraryListFilter::default())
        .await
        .unwrap();
    let item1 = lib_list.iter().find(|i| i.id.as_str() == lib_id1).unwrap();
    let item2 = lib_list.iter().find(|i| i.id.as_str() == lib_id2).unwrap();

    let import_res = host
        .operation_service()
        .invoke(
            &project_a,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "library_items": [&lib_id1, &lib_id2] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("import library items into project A");

    let assets = import_res.output["assets"]
        .as_array()
        .expect("assets array");
    assert_eq!(assets.len(), 2);
    let a_id1 = AssetId::new(assets[0].as_str().unwrap());
    let a_id2 = AssetId::new(assets[1].as_str().unwrap());

    let lib_sources = import_res.output["library_sources"]
        .as_array()
        .expect("library_sources array");
    assert_eq!(lib_sources.len(), 2);
    assert_eq!(lib_sources[0]["item"], lib_id1);
    assert_eq!(lib_sources[0]["hash"], item1.hash);
    assert_eq!(lib_sources[1]["item"], lib_id2);
    assert_eq!(lib_sources[1]["hash"], item2.hash);

    let project_store = AssetStore::new(host.config().paths().project_dir(project_a.as_str()));
    let rec1 = project_store
        .get_asset(&a_id1)
        .await
        .unwrap()
        .expect("rec1");
    let rec2 = project_store
        .get_asset(&a_id2)
        .await
        .unwrap()
        .expect("rec2");

    assert_eq!(rec1.hash, item1.hash);
    assert_eq!(rec1.media_type, item1.media_type);
    assert_eq!(rec2.hash, item2.hash);
    assert_eq!(rec2.media_type, item2.media_type);

    let step_log = StepLog::new(host.config().paths().project_dir(project_a.as_str()));
    let steps = step_log.records().await.unwrap();
    let import_step = steps
        .iter()
        .find(|s| s.op == "asset.import")
        .expect("import step");
    assert_eq!(rec1.origin, import_step.id);
    assert_eq!(rec2.origin, import_step.id);
    assert!(import_step.input.get("library_items").is_some());
    match &import_step.outcome {
        reimagine_core::step::StepOutcome::Succeeded { output } => {
            assert!(output.get("library_sources").is_some());
        }
        _ => panic!("import step did not succeed"),
    }
}

#[tokio::test]
async fn test_asset_import_rejections() {
    let base = temp_dir("lib-import-rejections");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib"), &base)
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-rej");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Proj",
                "desc",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .expect("create project");

    let files_dir = temp_dir("source-rej");
    fs::create_dir_all(&files_dir).unwrap();
    let file1 = files_dir.join("file1.txt");
    let file2 = files_dir.join("file2.txt");
    fs::write(&file1, b"one").unwrap();
    fs::write(&file2, b"two").unwrap();

    let add_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [file1.to_str().unwrap(), file2.to_str().unwrap()] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("add");

    let add_items = add_res.output["items"].as_array().unwrap();
    let valid_id = add_items[0]["id"].as_str().unwrap().to_string();
    let to_remove_id = add_items[1]["id"].as_str().unwrap().to_string();

    host.operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.remove".to_string(),
                input: json!({ "ids": [&to_remove_id] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("remove");

    // 1. Both paths and library_items
    let err_both = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({
                    "paths": [file1.to_str().unwrap()],
                    "library_items": [&valid_id]
                }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("both must be rejected");
    assert_eq!(err_both.code().as_str(), "ASSET/CALLER_INVALID_INPUT");

    // 2. Neither
    let err_neither = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({}),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("neither must be rejected");
    assert_eq!(err_neither.code().as_str(), "ASSET/CALLER_INVALID_INPUT");

    // 3. Unknown id
    let err_unknown = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "library_items": ["asset-unknown-999"] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("unknown id must be rejected");
    assert_eq!(err_unknown.code().as_str(), "ASSET/CALLER_ENTRY_NOT_FOUND");

    // 4. Removed id
    let err_removed = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "library_items": [&to_remove_id] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("removed id must be rejected");
    assert_eq!(err_removed.code().as_str(), "ASSET/CALLER_ENTRY_REMOVED");

    // 5. Duplicate id
    let err_duplicate = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "library_items": [&valid_id, &valid_id] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("duplicate id must be rejected");
    assert_eq!(err_duplicate.code().as_str(), "ASSET/CALLER_DUPLICATE_ITEM");

    // Verify nothing was ingested into project
    let project_store = AssetStore::new(host.config().paths().project_dir(project_id.as_str()));
    let project_assets = project_store.list().await.unwrap();
    assert!(
        project_assets.is_empty(),
        "nothing must be ingested on rejection"
    );
}

#[tokio::test]
async fn test_asset_independence() {
    let base = temp_dir("lib-independence");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib"), &base)
        .build()
        .await
        .expect("build host");

    let project_a = ProjectId::new("proj-ind-a");
    host.project_service()
        .create_project(
            project_a.clone(),
            ProjectMetadata::new(
                "Proj A",
                "desc",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .expect("create project");

    let files_dir = temp_dir("source-ind");
    fs::create_dir_all(&files_dir).unwrap();
    let file = files_dir.join("data.txt");
    fs::write(&file, b"independent bytes 10101").unwrap();

    let add_res = host
        .operation_service()
        .invoke(
            &project_a,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [file.to_str().unwrap()] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("add");

    let lib_item_id = add_res.output["items"][0]["id"].as_str().unwrap();

    let import_res = host
        .operation_service()
        .invoke(
            &project_a,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "library_items": [lib_item_id] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("import");

    let proj_asset_id = import_res.output["assets"][0].as_str().unwrap();
    let project_store = AssetStore::new(host.config().paths().project_dir(project_a.as_str()));
    let rec = project_store
        .get_asset(&AssetId::new(proj_asset_id))
        .await
        .unwrap()
        .unwrap();
    let content_path = project_store.content_path(&rec.hash);
    assert_eq!(fs::read(&content_path).unwrap(), b"independent bytes 10101");

    host.operation_service()
        .invoke(
            &project_a,
            InvokeRequest {
                name: "library.remove".to_string(),
                input: json!({ "ids": [lib_item_id] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("remove");

    host.operation_service()
        .invoke(
            &project_a,
            InvokeRequest {
                name: "library.purge".to_string(),
                input: json!({}),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("purge");

    assert_eq!(fs::read(&content_path).unwrap(), b"independent bytes 10101");

    host.delete_project(&project_a)
        .await
        .expect("delete project A");

    let file2 = files_dir.join("another.txt");
    fs::write(&file2, b"another file").unwrap();
    let add_res2 = host
        .library_service()
        .add(vec![file2], &test_context("human-1"))
        .await
        .expect("library add after delete project");
    assert_eq!(add_res2.items.len(), 1);
}

#[tokio::test]
async fn test_library_collect() {
    let base = temp_dir("lib-collect-test");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib"), &base)
        .build()
        .await
        .expect("build host");

    let project_a = ProjectId::new("proj-col-a");
    let project_b = ProjectId::new("proj-col-b");
    host.project_service()
        .create_project(
            project_a.clone(),
            ProjectMetadata::new(
                "Proj A",
                "desc",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .unwrap();
    host.project_service()
        .create_project(
            project_b.clone(),
            ProjectMetadata::new(
                "Proj B",
                "desc",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .unwrap();

    let files_dir = temp_dir("source-collect");
    fs::create_dir_all(&files_dir).unwrap();
    let file = files_dir.join("photo.png");
    fs::write(&file, b"sample png data for collect").unwrap();

    let import_res = host
        .operation_service()
        .invoke(
            &project_a,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "paths": [file.to_str().unwrap()] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("import into A");

    let proj_a_asset_id = import_res.output["assets"][0].as_str().unwrap();

    let collect_res = host
        .operation_service()
        .invoke(
            &project_a,
            InvokeRequest {
                name: "library.collect".to_string(),
                input: json!({
                    "items": [{ "asset": proj_a_asset_id }]
                }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("collect into library");

    let collected_items = collect_res.output["items"].as_array().unwrap();
    assert_eq!(collected_items.len(), 1);
    let collected_lib_id = collected_items[0]["id"].as_str().unwrap();
    let collected_name = collected_items[0]["name"].as_str().unwrap();
    assert!(!collected_items[0]["deduplicated"].as_bool().unwrap());

    assert!(collected_name.starts_with("collected-"));
    assert!(collected_name.ends_with(".png"));

    let lib_list = host
        .library_service()
        .list(&LibraryListFilter::default())
        .await
        .unwrap();
    let lib_entry = lib_list
        .iter()
        .find(|i| i.id.as_str() == collected_lib_id)
        .unwrap();
    assert_eq!(
        lib_entry.source,
        reimagine_core::library::LibrarySource::Project {
            project_id: project_a.clone(),
            asset_id: AssetId::new(proj_a_asset_id),
        }
    );

    let import_b_res = host
        .operation_service()
        .invoke(
            &project_b,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "library_items": [collected_lib_id] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("import into B");

    let proj_b_asset_id = import_b_res.output["assets"][0].as_str().unwrap();
    let store_b = AssetStore::new(host.config().paths().project_dir(project_b.as_str()));
    let rec_b = store_b
        .get_asset(&AssetId::new(proj_b_asset_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rec_b.hash, lib_entry.hash);

    let collect_again_res = host
        .operation_service()
        .invoke(
            &project_a,
            InvokeRequest {
                name: "library.collect".to_string(),
                input: json!({
                    "items": [{ "asset": proj_a_asset_id }]
                }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("collect again");

    let again_items = collect_again_res.output["items"].as_array().unwrap();
    assert_eq!(again_items.len(), 1);
    assert_eq!(again_items[0]["id"], collected_lib_id);
    assert!(again_items[0]["deduplicated"].as_bool().unwrap());
}

#[tokio::test]
async fn test_library_purge() {
    let base = temp_dir("lib-purge-test");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib"), &base)
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-purge");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Proj",
                "desc",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .unwrap();

    let files_dir = temp_dir("source-purge");
    fs::create_dir_all(&files_dir).unwrap();
    let file_a = files_dir.join("file_a.txt");
    let file_b = files_dir.join("file_b.txt");
    let file_c = files_dir.join("file_c.txt");
    fs::write(&file_a, b"content a").unwrap();
    fs::write(&file_b, b"content b").unwrap();
    fs::write(&file_c, b"content c").unwrap();

    let add_ab = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [file_a.to_str().unwrap(), file_b.to_str().unwrap()] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .unwrap();

    let id_a = add_ab.output["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let id_b = add_ab.output["items"][1]["id"]
        .as_str()
        .unwrap()
        .to_string();

    host.operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.remove".to_string(),
                input: json!({ "ids": [&id_b] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .unwrap();

    host.operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.add".to_string(),
                input: json!({ "paths": [file_c.to_str().unwrap()] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .unwrap();

    host.operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.undo".to_string(),
                input: json!({}),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .unwrap();

    let lib_store = host.library_service().asset_store();
    let rec_a = lib_store
        .get_asset(&AssetId::new(&id_a))
        .await
        .unwrap()
        .unwrap();
    let rec_b = lib_store
        .get_asset(&AssetId::new(&id_b))
        .await
        .unwrap()
        .unwrap();
    let path_a = lib_store.content_path(&rec_a.hash);
    let path_b = lib_store.content_path(&rec_b.hash);

    assert!(path_a.is_file());
    assert!(path_b.is_file());

    let purge_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.purge".to_string(),
                input: json!({}),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("purge");

    assert_eq!(purge_res.output["purged_entries"], 1);
    assert!(purge_res.output["deleted_files"].as_u64().unwrap() >= 2);

    assert!(path_a.is_file());
    assert!(!path_b.is_file());

    let undo_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.undo".to_string(),
                input: json!({}),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(undo_res.output["applied"], false);

    let purge2_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.purge".to_string(),
                input: json!({}),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(purge2_res.output["purged_entries"], 0);
    assert_eq!(purge2_res.output["deleted_files"], 0);
    assert_eq!(purge2_res.output["freed_bytes"], 0);
}

#[tokio::test]
async fn test_policy_purge_collect_import() {
    let base = temp_dir("lib-policy-test");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib"), &base)
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-policy");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Proj",
                "desc",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .unwrap();

    let files_dir = temp_dir("source-policy");
    fs::create_dir_all(&files_dir).unwrap();
    let file = files_dir.join("doc.txt");
    fs::write(&file, b"policy doc").unwrap();

    let import_err = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "paths": [file.to_str().unwrap()] }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect_err("asset.import denied to agents");
    assert_eq!(
        import_err.code().as_str(),
        "ASSET/CALLER_PATH_OUTSIDE_WORKSPACE"
    );

    let import_human = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "asset.import".to_string(),
                input: json!({ "paths": [file.to_str().unwrap()] }),
                context: test_context("human-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .unwrap();
    let asset_id = import_human.output["assets"][0].as_str().unwrap();

    let collect_res = host
        .operation_service()
        .invoke(
            &project_id,
            InvokeRequest {
                name: "library.collect".to_string(),
                input: json!({
                    "items": [{ "asset": asset_id }]
                }),
                context: agent_context("agent-1"),
                cancel: CancelToken::new(),
                run: None,
            },
        )
        .await
        .expect("library.collect allowed to agents");
    assert_eq!(collect_res.output["items"].as_array().unwrap().len(), 1);

    let purge_err = host
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
        .expect_err("library.purge denied to agents");
    assert_eq!(
        purge_err.code().as_str(),
        "LIBRARY/CALLER_NOT_PERMITTED_TO_AGENT"
    );
}

#[tokio::test]
async fn test_library_purge_keeps_content_shared_with_an_active_entry() {
    let base = temp_dir("lib-purge-shared");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib"), &base)
        .build()
        .await
        .expect("build host");

    let project_id = ProjectId::new("proj-purge-shared");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Proj",
                "desc",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .unwrap();

    let files_dir = temp_dir("source-purge-shared");
    fs::create_dir_all(&files_dir).unwrap();
    let first = files_dir.join("first.txt");
    let second = files_dir.join("second.txt");
    fs::write(&first, b"shared content").unwrap();
    fs::write(&second, b"shared content").unwrap();

    let invoke = |name: &str, input: serde_json::Value| {
        let host = &host;
        let project_id = &project_id;
        let name = name.to_string();
        async move {
            host.operation_service()
                .invoke(
                    project_id,
                    InvokeRequest {
                        name,
                        input,
                        context: test_context("human-1"),
                        cancel: CancelToken::new(),
                        run: None,
                    },
                )
                .await
                .unwrap()
        }
    };

    // Add, remove, then add the same bytes again: add only dedups against
    // active entries, so the second add creates a new entry with the same hash.
    let added = invoke("library.add", json!({ "paths": [first.to_str().unwrap()] })).await;
    let removed_id = added.output["items"][0]["id"].as_str().unwrap().to_string();
    invoke("library.remove", json!({ "ids": [&removed_id] })).await;
    let readded = invoke(
        "library.add",
        json!({ "paths": [second.to_str().unwrap()] }),
    )
    .await;
    let active_id = readded.output["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(removed_id, active_id);

    let lib_store = host.library_service().asset_store();
    let removed_rec = lib_store
        .get_asset(&AssetId::new(&removed_id))
        .await
        .unwrap()
        .unwrap();
    let active_rec = lib_store
        .get_asset(&AssetId::new(&active_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(removed_rec.hash, active_rec.hash);
    let content = lib_store.content_path(&active_rec.hash);

    let purged = invoke("library.purge", json!({})).await;
    assert_eq!(purged.output["purged_entries"], 1);
    assert_eq!(purged.output["deleted_files"], 0);

    assert!(
        content.is_file(),
        "content of the active entry must survive"
    );
    let doc = host.library_service().snapshot().await.unwrap();
    assert!(!doc.entries().contains_key(&AssetId::new(&removed_id)));
    assert!(doc.entries().contains_key(&AssetId::new(&active_id)));
}
