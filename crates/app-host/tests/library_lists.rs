//! Read-only list operations behind the workbench library table:
//! `collection.list`, `dynamic_workflow.list` and `asset.list`.

use std::fs;
use std::path::Path;

use mera_app_host::{
    InvokeRequest, KernelCall, KernelOp, WorkspaceHost, WorkspaceHostBuilder, WorkspaceScope,
};
use mera_core::command::{CommandActor, CommandActorKind, CommandProvenance, OperationContext};
use mera_core::diagnostic::Diagnostic;
use mera_core::model::ProjectId;
use mera_core::project::ProjectMetadata;
use mera_core::task::CancelToken;
use mod_sdk::{OperationAnnotations, OperationSpec};
use serde_json::{Value, json};

fn temp_dir(prefix: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("mera-library-lists-{prefix}-{nonce}"))
}

fn test_context() -> OperationContext {
    OperationContext::new(
        CommandActor::new(CommandActorKind::Human).with_id("test-user"),
        CommandProvenance::Direct,
    )
}

async fn build_host(base: &Path) -> WorkspaceHost {
    WorkspaceHostBuilder::new(WorkspaceScope::new("test-library-lists"), base)
        .build()
        .await
        .expect("build host")
}

fn project_dir(host: &WorkspaceHost, project: &ProjectId) -> std::path::PathBuf {
    host.config().paths().project_dir(project.as_str())
}

async fn create_project(host: &WorkspaceHost, id: &str) -> ProjectId {
    let project_id = ProjectId::new(id);
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Library test",
                "test project",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .expect("create project");
    project_id
}

async fn invoke(host: &WorkspaceHost, project: &ProjectId, name: &str, input: Value) -> Value {
    host.operation_service()
        .invoke(
            project,
            InvokeRequest {
                name: name.to_owned(),
                input,
                context: test_context(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .unwrap_or_else(|error| panic!("{name} failed: {}", error.message()))
        .output
}

async fn import_files(
    host: &WorkspaceHost,
    project: &ProjectId,
    base: &Path,
    files: &[(&str, &str)],
) -> Vec<String> {
    let mut paths = Vec::new();
    for (name, content) in files {
        let path = base.join(format!("import-{name}"));
        fs::write(&path, content).expect("write import source");
        paths.push(path.to_string_lossy().to_string());
    }
    let output = invoke(host, project, "asset.import", json!({ "paths": paths })).await;
    output["assets"]
        .as_array()
        .expect("assets array")
        .iter()
        .map(|value| value.as_str().expect("asset id").to_owned())
        .collect()
}

async fn create_collection(
    host: &WorkspaceHost,
    project: &ProjectId,
    name: &str,
    assets: &[&str],
) -> String {
    let output = invoke(
        host,
        project,
        "collection.create",
        json!({ "name": name, "assets": assets }),
    )
    .await;
    output["collection_id"]
        .as_str()
        .expect("collection id")
        .to_owned()
}

async fn create_workflow(host: &WorkspaceHost, project: &ProjectId, name: &str) -> String {
    let output = invoke(
        host,
        project,
        "dynamic_workflow.create",
        json!({
            "name": name,
            "workflow": {
                "inputs": {},
                "params": { "type": "object" },
                "steps": [
                    { "id": "first", "op": "test.step", "input": {} },
                    { "id": "second", "op": "test.step", "input": {} }
                ],
                "output": { "$from": "steps.second.out" }
            }
        }),
    )
    .await;
    output["dynamic_workflow_id"]
        .as_str()
        .expect("dynamic workflow id")
        .to_owned()
}

#[tokio::test]
async fn list_operations_return_the_project_library() {
    let base = temp_dir("library");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-library").await;

    let assets = import_files(
        &host,
        &project,
        &base,
        &[("a.txt", "alpha"), ("b.txt", "beta")],
    )
    .await;
    assert_eq!(assets.len(), 2);

    let alpha_id = create_collection(&host, &project, "Alpha", &[assets[0].as_str()]).await;
    let beta_id = create_collection(
        &host,
        &project,
        "Beta",
        &[assets[0].as_str(), assets[1].as_str()],
    )
    .await;
    let workflow_id = create_workflow(&host, &project, "Flow One").await;

    let listed = invoke(&host, &project, "collection.list", json!({})).await;
    let collections = listed["collections"].as_array().expect("collections array");
    assert_eq!(collections.len(), 2);
    let alpha = collections
        .iter()
        .find(|collection| collection["name"] == json!("Alpha"))
        .expect("Alpha listed");
    assert_eq!(alpha["id"].as_str(), Some(alpha_id.as_str()));
    assert_eq!(alpha["version"].as_u64(), Some(0));
    assert_eq!(alpha["slot_count"].as_u64(), Some(1));
    let beta = collections
        .iter()
        .find(|collection| collection["name"] == json!("Beta"))
        .expect("Beta listed");
    assert_eq!(beta["id"].as_str(), Some(beta_id.as_str()));
    assert_eq!(beta["version"].as_u64(), Some(0));
    assert_eq!(beta["slot_count"].as_u64(), Some(2));

    let listed = invoke(&host, &project, "dynamic_workflow.list", json!({})).await;
    let workflows = listed["workflows"].as_array().expect("workflows array");
    assert_eq!(workflows.len(), 1);
    assert_eq!(workflows[0]["id"].as_str(), Some(workflow_id.as_str()));
    assert_eq!(workflows[0]["name"].as_str(), Some("Flow One"));
    assert_eq!(workflows[0]["version"].as_u64(), Some(1));
    assert_eq!(workflows[0]["step_count"].as_u64(), Some(2));

    let listed = invoke(&host, &project, "asset.list", json!({})).await;
    let records = listed["assets"].as_array().expect("assets array");
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[0]["id"].as_str(),
        Some(assets[1].as_str()),
        "assets are listed newest first"
    );
    assert_eq!(records[1]["id"].as_str(), Some(assets[0].as_str()));
    assert_eq!(records[0]["media_type"].as_str(), Some("text/plain"));
    assert_eq!(records[0]["size"].as_u64(), Some("beta".len() as u64));
    assert_eq!(records[1]["size"].as_u64(), Some("alpha".len() as u64));
    assert!(
        records[0]["origin"]
            .as_str()
            .is_some_and(|origin| !origin.is_empty())
    );

    // The three list operations are registered read-only, so invoking them
    // records no step and mutates no document.
    let infos = host.operation_service().list(&project).await;
    for name in ["collection.list", "dynamic_workflow.list", "asset.list"] {
        let info = infos
            .iter()
            .find(|info| info.spec.name == name)
            .unwrap_or_else(|| panic!("{name} is registered"));
        assert!(info.spec.annotations.read_only, "{name} must be read-only");
    }

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn list_operations_are_empty_for_unknown_and_empty_projects() {
    let base = temp_dir("empty");
    let host = build_host(&base).await;
    let empty = create_project(&host, "proj-empty").await;
    let unknown = ProjectId::new("proj-missing");

    for project in [&empty, &unknown] {
        for (name, field) in [
            ("collection.list", "collections"),
            ("dynamic_workflow.list", "workflows"),
            ("asset.list", "assets"),
        ] {
            let listed = invoke(&host, project, name, json!({})).await;
            assert_eq!(
                listed[field].as_array().map(Vec::len),
                Some(0),
                "{name} must list nothing for `{}`",
                project.as_str()
            );
        }
    }

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn list_operations_record_no_step() {
    let base = temp_dir("no-steps");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-no-steps").await;

    for name in ["collection.list", "dynamic_workflow.list", "asset.list"] {
        let _ = invoke(&host, &project, name, json!({})).await;
    }

    let steps_path = project_dir(&host, &project).join("steps.jsonl");
    let empty = match fs::metadata(&steps_path) {
        Ok(metadata) => metadata.len() == 0,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    };
    assert!(
        empty,
        "read-only list operations must not append to `{}`",
        steps_path.display()
    );

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn list_operations_return_records_after_a_host_reopen() {
    let base = temp_dir("reopen");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-reopen").await;

    let assets = import_files(&host, &project, &base, &[("a.txt", "alpha")]).await;
    let collection_id = create_collection(&host, &project, "Alpha", &[assets[0].as_str()]).await;
    let workflow_id = create_workflow(&host, &project, "Flow One").await;

    host.shutdown().await;
    drop(host);

    // A new host over the same root sees the persisted documents.
    let host = build_host(&base).await;

    let listed = invoke(&host, &project, "collection.list", json!({})).await;
    let collections = listed["collections"].as_array().expect("collections array");
    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0]["id"].as_str(), Some(collection_id.as_str()));
    assert_eq!(collections[0]["name"].as_str(), Some("Alpha"));
    assert_eq!(collections[0]["slot_count"].as_u64(), Some(1));

    let listed = invoke(&host, &project, "dynamic_workflow.list", json!({})).await;
    let workflows = listed["workflows"].as_array().expect("workflows array");
    assert_eq!(workflows.len(), 1);
    assert_eq!(workflows[0]["id"].as_str(), Some(workflow_id.as_str()));
    assert_eq!(workflows[0]["name"].as_str(), Some("Flow One"));
    assert_eq!(workflows[0]["step_count"].as_u64(), Some(2));

    let listed = invoke(&host, &project, "asset.list", json!({})).await;
    let records = listed["assets"].as_array().expect("assets array");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["id"].as_str(), Some(assets[0].as_str()));

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn asset_list_ignores_a_torn_last_line() {
    let base = temp_dir("torn");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-torn").await;

    let assets = import_files(&host, &project, &base, &[("a.txt", "alpha")]).await;
    assert_eq!(assets.len(), 1);

    let jsonl_path = host
        .config()
        .paths()
        .project_dir(project.as_str())
        .join("assets.jsonl");
    let mut contents = fs::read_to_string(&jsonl_path).expect("read assets.jsonl");
    contents.push_str(r#"{"id":"asset-torn","hash":"#);
    fs::write(&jsonl_path, contents).expect("append torn line");

    let listed = invoke(&host, &project, "asset.list", json!({})).await;
    let records = listed["assets"].as_array().expect("assets array");
    assert_eq!(
        records.len(),
        1,
        "a torn trailing line is not a completed record"
    );
    assert_eq!(records[0]["id"].as_str(), Some(assets[0].as_str()));

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn asset_import_after_a_torn_tail_keeps_both_records() {
    let base = temp_dir("torn-append");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-torn-append").await;

    let first = import_files(&host, &project, &base, &[("a.txt", "alpha")]).await;
    let jsonl_path = project_dir(&host, &project).join("assets.jsonl");
    let mut contents = fs::read_to_string(&jsonl_path).expect("read assets.jsonl");
    contents.push_str(r#"{"id":"asset-torn","hash":"#);
    fs::write(&jsonl_path, contents).expect("append torn line");

    // The next import must terminate the torn line before appending, so the
    // new record parses instead of merging with the partial one.
    let second = import_files(&host, &project, &base, &[("b.txt", "beta")]).await;
    assert_eq!(second.len(), 1);

    let listed = invoke(&host, &project, "asset.list", json!({})).await;
    let records = listed["assets"].as_array().expect("assets array");
    assert_eq!(
        records.len(),
        2,
        "both complete records are listed and the torn line is ignored"
    );
    assert_eq!(records[0]["id"].as_str(), Some(second[0].as_str()));
    assert_eq!(records[1]["id"].as_str(), Some(first[0].as_str()));

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn asset_list_skips_a_complete_but_invalid_line() {
    let base = temp_dir("invalid-line");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-invalid-line").await;

    let assets = import_files(&host, &project, &base, &[("a.txt", "alpha")]).await;
    let jsonl_path = project_dir(&host, &project).join("assets.jsonl");
    let mut contents = fs::read_to_string(&jsonl_path).expect("read assets.jsonl");
    // Syntactically valid JSON that is not an asset record, terminated by a
    // newline so it counts as a completed line.
    contents.push_str("{\"id\":\"asset-invalid\",\"media_type\":\"text/plain\"}\n");
    fs::write(&jsonl_path, contents).expect("append invalid record");

    let listed = invoke(&host, &project, "asset.list", json!({})).await;
    let records = listed["assets"].as_array().expect("assets array");
    assert_eq!(records.len(), 1, "an invalid completed line is ignored");
    assert_eq!(records[0]["id"].as_str(), Some(assets[0].as_str()));

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn list_operations_are_isolated_per_project() {
    let base = temp_dir("isolation");
    let host = build_host(&base).await;
    let alpha = create_project(&host, "proj-alpha").await;
    let beta = create_project(&host, "proj-beta").await;

    let alpha_assets = import_files(&host, &alpha, &base, &[("alpha.txt", "alpha")]).await;
    let beta_assets = import_files(&host, &beta, &base, &[("beta.txt", "beta")]).await;
    assert_eq!(alpha_assets.len(), 1);
    assert_eq!(beta_assets.len(), 1);
    assert_ne!(alpha_assets[0], beta_assets[0]);

    let alpha_collection =
        create_collection(&host, &alpha, "Alpha only", &[alpha_assets[0].as_str()]).await;
    let beta_collection =
        create_collection(&host, &beta, "Beta only", &[beta_assets[0].as_str()]).await;
    let alpha_workflow = create_workflow(&host, &alpha, "Alpha flow").await;
    let beta_workflow = create_workflow(&host, &beta, "Beta flow").await;

    let cases = [
        (
            &alpha,
            alpha_collection.as_str(),
            alpha_workflow.as_str(),
            alpha_assets[0].as_str(),
        ),
        (
            &beta,
            beta_collection.as_str(),
            beta_workflow.as_str(),
            beta_assets[0].as_str(),
        ),
    ];
    for (project, collection_id, workflow_id, asset_id) in cases {
        let listed = invoke(&host, project, "collection.list", json!({})).await;
        let collections = listed["collections"].as_array().expect("collections array");
        assert_eq!(
            collections.len(),
            1,
            "only `{}`'s collection is listed",
            project.as_str()
        );
        assert_eq!(collections[0]["id"].as_str(), Some(collection_id));

        let listed = invoke(&host, project, "dynamic_workflow.list", json!({})).await;
        let workflows = listed["workflows"].as_array().expect("workflows array");
        assert_eq!(
            workflows.len(),
            1,
            "only `{}`'s workflow is listed",
            project.as_str()
        );
        assert_eq!(workflows[0]["id"].as_str(), Some(workflow_id));

        let listed = invoke(&host, project, "asset.list", json!({})).await;
        let records = listed["assets"].as_array().expect("assets array");
        assert_eq!(
            records.len(),
            1,
            "only `{}`'s assets are listed",
            project.as_str()
        );
        assert_eq!(records[0]["id"].as_str(), Some(asset_id));
    }

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn list_operations_skip_malformed_documents() {
    let base = temp_dir("malformed");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-malformed").await;

    let assets = import_files(&host, &project, &base, &[("a.txt", "alpha")]).await;
    let collection_id = create_collection(&host, &project, "Alpha", &[assets[0].as_str()]).await;
    let workflow_id = create_workflow(&host, &project, "Flow One").await;

    // A malformed document is logged as a warning and skipped; one bad file
    // must not hide the rest of the library or fail the list operation.
    let project_root = project_dir(&host, &project);
    let malformed_collection = project_root.join("collections").join("broken.json");
    fs::create_dir_all(malformed_collection.parent().expect("collection dir"))
        .expect("create collections dir");
    fs::write(&malformed_collection, "{ not a collection document")
        .expect("write malformed collection");
    let malformed_workflow = project_root.join("dynamic_workflows").join("broken.json");
    fs::create_dir_all(malformed_workflow.parent().expect("workflow dir"))
        .expect("create dynamic workflows dir");
    fs::write(&malformed_workflow, "{ not a dynamic workflow document")
        .expect("write malformed workflow");

    let listed = invoke(&host, &project, "collection.list", json!({})).await;
    let collections = listed["collections"].as_array().expect("collections array");
    assert_eq!(collections.len(), 1, "the valid collection is still listed");
    assert_eq!(collections[0]["id"].as_str(), Some(collection_id.as_str()));

    let listed = invoke(&host, &project, "dynamic_workflow.list", json!({})).await;
    let workflows = listed["workflows"].as_array().expect("workflows array");
    assert_eq!(workflows.len(), 1, "the valid workflow is still listed");
    assert_eq!(workflows[0]["id"].as_str(), Some(workflow_id.as_str()));

    let _ = fs::remove_dir_all(&base);
}

struct TestProduceOp {
    spec: OperationSpec,
}

impl TestProduceOp {
    fn new() -> Self {
        Self {
            spec: OperationSpec {
                name: "sample.transform".to_owned(),
                description: "Produce two assets for asset.list name tests".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "doc": { "type": "string", "format": "asset" } },
                    "required": ["doc"],
                    "additionalProperties": false
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "first": { "type": "string", "format": "asset", "media_type": "text/plain" },
                        "second": { "type": "string", "format": "asset", "media_type": "text/plain" }
                    },
                    "required": ["first", "second"],
                    "additionalProperties": false
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: false,
                    idempotent: false,
                    deterministic: true,
                    open_world: false,
                },
            },
        }
    }
}

#[async_trait::async_trait]
impl KernelOp for TestProduceOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let first = call.invocation.output_dir.join("out1.txt");
        let second = call.invocation.output_dir.join("out2.txt");
        fs::write(&first, "out 1").expect("write out1");
        fs::write(&second, "out 2").expect("write out2");
        Ok(json!({ "first": "out1.txt", "second": "out2.txt" }))
    }
}

#[tokio::test]
async fn asset_list_derives_names_from_paths_library_and_operation_outputs() {
    let base = temp_dir("asset-names");
    let host = build_host(&base).await;
    host.operation_service()
        .register_kernel_op(std::sync::Arc::new(TestProduceOp::new()));
    let project = create_project(&host, "proj-asset-names").await;

    // 1. Path import uses the original filename
    let path_assets = import_files(
        &host,
        &project,
        &base,
        &[("hello_world.txt", "hello world")],
    )
    .await;
    assert_eq!(path_assets.len(), 1);

    // 2. Operation output uses "<op> #<N>"
    let op_out = invoke(
        &host,
        &project,
        "sample.transform",
        json!({ "doc": path_assets[0] }),
    )
    .await;
    let op_asset_1 = op_out["first"].as_str().expect("first asset").to_owned();
    let op_asset_2 = op_out["second"].as_str().expect("second asset").to_owned();

    // 3. Global library import preserves the library entry's name at import time,
    // even if the library entry is renamed afterwards.
    let lib_src = base.join("prompts_master.txt");
    fs::write(&lib_src, "prompt v1").expect("write lib source");
    let lib_added = invoke(
        &host,
        &project,
        "library.add",
        json!({ "paths": [lib_src.to_string_lossy()] }),
    )
    .await;
    let lib_id = lib_added["items"][0]["id"]
        .as_str()
        .expect("library item id")
        .to_owned();
    let lib_import = invoke(
        &host,
        &project,
        "asset.import",
        json!({ "library_items": [lib_id] }),
    )
    .await;
    let lib_asset_id = lib_import["assets"][0]
        .as_str()
        .expect("imported asset id")
        .to_owned();

    let _ = invoke(
        &host,
        &project,
        "library.rename",
        json!({ "id": lib_id, "name": "renamed_later.txt" }),
    )
    .await;

    let listed = invoke(&host, &project, "asset.list", json!({})).await;
    let assets = listed["assets"].as_array().expect("assets array");
    assert_eq!(assets.len(), 4);

    let find_name = |id: &str| -> String {
        assets
            .iter()
            .find(|a| a["id"].as_str() == Some(id))
            .and_then(|a| a["name"].as_str())
            .expect("asset entry with name")
            .to_owned()
    };

    assert_eq!(find_name(&path_assets[0]), "import-hello_world.txt");
    assert_eq!(find_name(&op_asset_1), "sample.transform #1");
    assert_eq!(find_name(&op_asset_2), "sample.transform #2");
    assert_eq!(find_name(&lib_asset_id), "prompts_master.txt");

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn library_place_handles_single_and_multi_item_and_single_step_undo() {
    let base = temp_dir("library-place");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-library-place").await;

    let p1 = base.join("single_brief.txt");
    let p2 = base.join("shot_a.txt");
    let p3 = base.join("shot_b.txt");
    fs::write(&p1, "brief").expect("write p1");
    fs::write(&p2, "shot a").expect("write p2");
    fs::write(&p3, "shot b").expect("write p3");

    let added = invoke(
        &host,
        &project,
        "library.add",
        json!({
            "paths": [
                p1.to_string_lossy(),
                p2.to_string_lossy(),
                p3.to_string_lossy(),
            ]
        }),
    )
    .await;
    let items = added["items"].as_array().expect("added items");
    let id1 = items[0]["id"].as_str().unwrap().to_owned();
    let id2 = items[1]["id"].as_str().unwrap().to_owned();
    let id3 = items[2]["id"].as_str().unwrap().to_owned();

    // 1. Placing a single library item puts an asset_ref card centered at position (240x160).
    let single_out = invoke(
        &host,
        &project,
        "library.place",
        json!({
            "library_items": [id1],
            "position": { "x": 500, "y": 400 }
        }),
    )
    .await;
    assert_eq!(single_out["collection_id"], Value::Null);
    let single_board_item = single_out["board_item_id"].as_str().unwrap().to_owned();
    let single_asset_id = single_out["assets"][0].as_str().unwrap().to_owned();
    assert!(single_out["history_entry_id"].as_str().is_some());

    let board = host.board_service().snapshot(&project).await.unwrap();
    let placed_single = board
        .items()
        .get(&mera_core::model::BoardItemId::new(&single_board_item))
        .expect("single placed item on board");
    assert_eq!(placed_single.position().x, 500 - 120);
    assert_eq!(placed_single.position().y, 400 - 80);
    assert!(matches!(
        placed_single.kind(),
        mera_core::board::BoardItemKind::AssetRef { artifact_id } if artifact_id.as_str() == single_asset_id
    ));

    // Activity panel recorded a single "activity.library.place" record tied to the board history
    // (after the preceding `library.add` record).
    let act_out = invoke(&host, &project, "activity.list", json!({ "limit": 10 })).await;
    let records = act_out["records"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["summaryKey"], json!("activity.library.place"));
    assert_eq!(records[0]["summaryParams"]["count"], json!(1));
    assert_eq!(
        records[0]["summaryParams"]["name"],
        json!("single_brief.txt")
    );
    assert_eq!(records[0]["targets"], json!([single_board_item]));
    assert_eq!(records[0]["historyEntryId"], single_out["history_entry_id"]);

    // 2. Placing multiple library items via an Agent actor creates a collection card centered at position (300x200).
    let agent_ctx = OperationContext::new(
        CommandActor::new(CommandActorKind::Agent).with_id("agent-placer"),
        CommandProvenance::Direct,
    );
    let multi_out = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "library.place".to_owned(),
                input: json!({
                    "library_items": [id2, id3],
                    "position": { "x": 300, "y": 250 },
                    "name": "Morning shots"
                }),
                context: agent_ctx,
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect("agent library.place succeeds")
        .output;

    let col_id = multi_out["collection_id"]
        .as_str()
        .expect("collection_id")
        .to_owned();
    let multi_board_item = multi_out["board_item_id"].as_str().unwrap().to_owned();
    let multi_entry = multi_out["history_entry_id"].as_str().unwrap().to_owned();

    let board = host.board_service().snapshot(&project).await.unwrap();
    let placed_col = board
        .items()
        .get(&mera_core::model::BoardItemId::new(&multi_board_item))
        .expect("collection card placed on board");
    assert_eq!(placed_col.position().x, 300 - 150);
    assert_eq!(placed_col.position().y, 250 - 100);
    assert!(matches!(
        placed_col.kind(),
        mera_core::board::BoardItemKind::Collection { collection_id } if collection_id.as_str() == col_id
    ));

    // Single-step board undo removes the placed collection card while keeping imported project assets.
    let undone = host
        .board_service()
        .undo(
            &project,
            CommandActor::new(CommandActorKind::Human),
            Some(&mera_core::model::HistoryEntryId::new(multi_entry)),
        )
        .await
        .unwrap()
        .expect("undo applied");
    assert!(undone.history_entry_id().is_some());

    let board_after_undo = host.board_service().snapshot(&project).await.unwrap();
    assert!(
        !board_after_undo
            .items()
            .contains_key(&mera_core::model::BoardItemId::new(&multi_board_item)),
        "board undo removed placed card"
    );

    let listed_assets = invoke(&host, &project, "asset.list", json!({})).await;
    let project_assets = listed_assets["assets"].as_array().unwrap();
    assert_eq!(
        project_assets.len(),
        3,
        "imported assets stay in project contents"
    );
    assert!(project_assets.iter().any(|a| {
        a["name"].as_str() == Some("single_brief.txt") && a["library_item"].as_str() == Some(&id1)
    }));

    let _ = fs::remove_dir_all(&base);
}
