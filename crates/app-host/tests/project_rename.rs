use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use reimagine_app_host::operation_service::InvokeRequest;
use reimagine_app_host::{WorkspaceHostBuilder, WorkspaceScope};
use reimagine_core::command::{
    CommandActor, CommandActorKind, CommandProvenance, OperationContext,
};
use reimagine_core::event::Timestamp;
use reimagine_core::model::ProjectId;
use reimagine_core::project::ProjectMetadata;
use reimagine_core::task::CancelToken;
use serde_json::{Value, json};

const CREATED_AT: &str = "2026-01-01T00:00:00Z";

fn temp_dir(prefix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("reimagine-project-rename-{prefix}-{nonce}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn rename_request(input: Value) -> InvokeRequest {
    InvokeRequest {
        name: "project.rename".to_string(),
        input,
        context: OperationContext::new(
            CommandActor::new(CommandActorKind::Human).with_id("user-1"),
            CommandProvenance::Direct,
        ),
        cancel: CancelToken::new(),
        run: None,
    }
}

async fn host_with_project(
    base: &Path,
    project_id: &ProjectId,
) -> reimagine_app_host::WorkspaceHost {
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-rename"), base)
        .build()
        .await
        .expect("build host");
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Old Name",
                "Rename target",
                Timestamp::new(CREATED_AT),
                Timestamp::new(CREATED_AT),
            ),
        )
        .await
        .expect("create project");
    host
}

#[tokio::test]
async fn rename_trims_persists_and_survives_a_host_rebuild() {
    let base = temp_dir("persist");
    let project_id = ProjectId::new("proj-rename");
    let host = host_with_project(&base, &project_id).await;

    let res = host
        .operation_service()
        .invoke(
            &project_id,
            rename_request(json!({ "name": "  New Name  " })),
        )
        .await
        .expect("rename op");
    assert_eq!(res.output["name"], "New Name");
    let updated_at = res.output["updated_at"].as_str().expect("updated_at");
    assert_ne!(updated_at, CREATED_AT);

    drop(host);
    let rebuilt = WorkspaceHostBuilder::new(WorkspaceScope::new("test-rename"), &base)
        .build()
        .await
        .expect("rebuild host");
    let project = rebuilt
        .project_service()
        .load_project(&project_id)
        .await
        .expect("load renamed project");
    assert_eq!(project.metadata().name(), "New Name");
    assert_eq!(project.metadata().description(), "Rename target");
    assert_eq!(project.metadata().created_at().as_str(), CREATED_AT);
    assert_eq!(project.metadata().updated_at().as_str(), updated_at);

    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn rename_rejects_empty_and_whitespace_names() {
    let base = temp_dir("empty");
    let project_id = ProjectId::new("proj-rename-empty");
    let host = host_with_project(&base, &project_id).await;

    for name in ["", "   \t\n"] {
        let err = host
            .operation_service()
            .invoke(&project_id, rename_request(json!({ "name": name })))
            .await
            .expect_err("empty name rejected");
        assert_eq!(err.code().as_str(), "PROJECT/CALLER_EMPTY_NAME");
    }

    let project = host
        .project_service()
        .load_project(&project_id)
        .await
        .expect("load project");
    assert_eq!(project.metadata().name(), "Old Name");
    assert_eq!(project.metadata().updated_at().as_str(), CREATED_AT);

    let _ = std::fs::remove_dir_all(&base);
}
