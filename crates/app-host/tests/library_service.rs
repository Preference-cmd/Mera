use std::fs;
use std::path::{Path, PathBuf};

use reimagine_app_host::{
    InvokeRequest, LibraryListFilter, StepLog, WorkspaceHostBuilder, WorkspaceScope,
};
use reimagine_core::command::{
    CommandActor, CommandActorKind, CommandProvenance, OperationContext,
};
use reimagine_core::event::EventBus;
use reimagine_core::model::ProjectId;
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
        .expect_err("agent must be denied library.add");
    assert_eq!(
        add_err.code().as_str(),
        "LIBRARY/CALLER_NOT_PERMITTED_TO_AGENT"
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
