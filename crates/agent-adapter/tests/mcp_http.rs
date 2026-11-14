//! End-to-end MCP tests: a real kernel-only workspace, the adapter on
//! 127.0.0.1:0, and the rmcp client over Streamable HTTP.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use reimagine_agent_adapter::{
    AgentAdapter, ConnectionBinding, ConnectionRegistry, ConnectionScope, HostContext,
};
use reimagine_app_host::{InvokeRequest, WorkspaceHost, WorkspaceHostBuilder, WorkspaceScope};
use reimagine_core::command::{
    CommandActor, CommandActorKind, CommandProvenance, OperationContext,
};
use reimagine_core::model::ProjectId;
use reimagine_core::project::ProjectMetadata;
use reimagine_core::task::CancelToken;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use serde_json::{Value, json};

const TOKEN_ID: &str = "mcp-test";
const TOKEN_LABEL: &str = "MCP test";

struct TestHost {
    workspace: RwLock<Arc<WorkspaceHost>>,
    active: RwLock<ProjectId>,
}

impl TestHost {
    fn new(workspace: WorkspaceHost) -> Self {
        Self {
            workspace: RwLock::new(Arc::new(workspace)),
            active: RwLock::new(ProjectId::new("default")),
        }
    }

    fn replace_workspace(&self, workspace: WorkspaceHost) {
        *self.workspace.write().expect("workspace lock") = Arc::new(workspace);
    }

    fn set_active(&self, project: ProjectId) {
        *self.active.write().expect("active lock") = project;
    }
}

impl HostContext for TestHost {
    fn workspace(&self) -> Arc<WorkspaceHost> {
        Arc::clone(&self.workspace.read().expect("workspace lock"))
    }

    fn active_project(&self) -> ProjectId {
        self.active.read().expect("active lock").clone()
    }
}

struct Harness {
    _temp: tempfile::TempDir,
    base: PathBuf,
    host: Arc<TestHost>,
    registry: Arc<ConnectionRegistry>,
    adapter: AgentAdapter,
}

impl Harness {
    async fn start(scope: ConnectionScope) -> (Self, String) {
        let temp = tempfile::tempdir().expect("temp dir");
        let base = temp.path().to_path_buf();
        let workspace = build_host(&base).await;
        let host = Arc::new(TestHost::new(workspace));
        let registry = Arc::new(ConnectionRegistry::new());
        let token = registry.issue(ConnectionBinding {
            id: TOKEN_ID.to_owned(),
            label: TOKEN_LABEL.to_owned(),
            scope,
        });
        let adapter = AgentAdapter::start(
            "127.0.0.1:0".parse().expect("loopback address"),
            host.clone(),
            registry.clone(),
        )
        .await
        .expect("start adapter");
        let harness = Self {
            _temp: temp,
            base,
            host,
            registry,
            adapter,
        };
        (harness, token)
    }

    fn url(&self) -> String {
        format!("http://{}/mcp", self.adapter.local_addr())
    }

    fn board_path(&self, project: &str) -> PathBuf {
        self.base.join("projects").join(project).join("board.json")
    }

    async fn connect(&self, token: &str) -> RunningService<rmcp::RoleClient, ()> {
        let transport = StreamableHttpClientTransport::with_client(
            reqwest013::Client::new(),
            StreamableHttpClientTransportConfig::with_uri(self.url()).auth_header(token),
        );
        ().serve(transport).await.expect("MCP initialize")
    }

    /// Stop the server and wait for its task, so no listener outlives a test.
    async fn stop(self) {
        self.adapter.shutdown().await;
    }
}

async fn build_host(base: &Path) -> WorkspaceHost {
    WorkspaceHostBuilder::new(WorkspaceScope::new("test-agent-adapter"), base)
        .build()
        .await
        .expect("build workspace")
}

async fn create_project(host: &WorkspaceHost, id: &str) {
    host.project_service()
        .create_project(
            ProjectId::new(id),
            ProjectMetadata::new(
                "Adapter project",
                "agent adapter test project",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .expect("create project");
}

fn args(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().expect("object arguments").clone()
}

fn note_contents(board: &Value) -> Vec<String> {
    // board.json stores items as an id-keyed object; board.read returns an array.
    let items: Vec<&Value> = match &board["items"] {
        Value::Array(items) => items.iter().collect(),
        Value::Object(items) => items.values().collect(),
        other => panic!("unexpected items: {other}"),
    };
    items
        .into_iter()
        .filter(|item| item["kind"]["type"] == "note")
        .map(|item| {
            item["kind"]["content"]
                .as_str()
                .expect("note content")
                .to_owned()
        })
        .collect()
}

fn read_board(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).expect("board file")).expect("board json")
}

async fn activity_records(host: &WorkspaceHost, project: &ProjectId) -> Vec<Value> {
    let result = host
        .operation_service()
        .invoke(
            project,
            InvokeRequest {
                name: "activity.list".to_owned(),
                input: json!({ "limit": 50 }),
                context: OperationContext::new(
                    CommandActor::new(CommandActorKind::Human),
                    CommandProvenance::Direct,
                ),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect("activity.list");
    result.output["records"]
        .as_array()
        .expect("records")
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_list_projects_board_operations_with_asset_import_and_library_add() {
    let (harness, token) =
        Harness::start(ConnectionScope::Project(ProjectId::new("default"))).await;
    let client = harness.connect(&token).await;
    let tools = client.list_tools(None).await.expect("tools/list").tools;

    let add = tools
        .iter()
        .find(|tool| tool.name == "board__add_notes")
        .expect("board__add_notes is listed");
    assert!(
        tools.iter().any(|tool| tool.name == "board__preview"),
        "board__preview is listed"
    );
    assert!(
        tools.iter().any(|tool| tool.name == "asset__import"),
        "asset__import is listed"
    );
    assert!(
        tools.iter().any(|tool| tool.name == "library__add"),
        "library__add is listed"
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool.name == "project__list_folders"),
        "project__list_folders is listed"
    );
    assert!(
        tools.iter().all(|tool| tool.name != "project__add_folder"),
        "project__add_folder is never listed"
    );
    assert!(
        tools
            .iter()
            .all(|tool| tool.name != "project__remove_folder"),
        "project__remove_folder is never listed"
    );
    assert!(
        tools.iter().all(|tool| tool.name != "library__remove"),
        "library__remove is never listed"
    );
    assert!(
        tools.iter().all(|tool| tool.name != "library__purge"),
        "library__purge is never listed"
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool.name == "dynamic_workflow__run"),
        "dynamic_workflow__run is listed"
    );
    assert!(
        tools
            .iter()
            .all(|tool| tool.name != "dynamic_workflow__resume"),
        "dynamic_workflow__resume is never listed"
    );

    let infos = harness
        .host
        .workspace()
        .operation_service()
        .list(&ProjectId::new("default"))
        .await;
    let spec = &infos
        .iter()
        .find(|info| info.spec.name == "board.add_notes")
        .expect("board.add_notes is registered")
        .spec;
    assert_eq!(add.description.as_deref(), Some(spec.description.as_str()));
    assert_eq!(
        Value::Object((*add.input_schema).clone()),
        spec.input_schema,
        "the projected input schema is the operation's schema"
    );
    assert_eq!(
        Value::Object((**add.output_schema.as_ref().expect("output schema")).clone(),),
        spec.output_schema,
        "the projected output schema is the operation's schema"
    );
    let hints = add.annotations.as_ref().expect("annotations");
    assert_eq!(hints.read_only_hint, Some(false));
    assert_eq!(hints.destructive_hint, Some(false));
    assert_eq!(hints.idempotent_hint, Some(false));
    assert_eq!(hints.open_world_hint, Some(false));

    client.cancel().await.expect("close client");
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn call_add_notes_writes_the_board_and_one_agent_activity_record() {
    let (harness, token) =
        Harness::start(ConnectionScope::Project(ProjectId::new("default"))).await;
    let client = harness.connect(&token).await;

    let result = client
        .call_tool(
            CallToolRequestParams::new("board__add_notes").with_arguments(args(json!({
                "notes": [
                    { "content": "First note" },
                    { "content": "Second note", "position": { "x": 500, "y": 0 } }
                ]
            }))),
        )
        .await
        .expect("tools/call");
    assert_eq!(result.is_error, Some(false));

    let structured = result
        .structured_content
        .as_ref()
        .expect("structured content")
        .clone();
    let text = result.content[0]
        .as_text()
        .expect("text content")
        .text
        .clone();
    assert_eq!(
        serde_json::from_str::<Value>(&text).expect("text content is JSON"),
        structured,
        "the text form carries the same structured result"
    );
    assert_eq!(
        structured["item_ids"].as_array().expect("item ids").len(),
        2
    );
    assert_eq!(structured["version"], json!(1));
    assert!(
        structured["history_entry_id"].is_string(),
        "the batch names its history entry"
    );

    let contents = note_contents(&read_board(&harness.board_path("default")));
    assert_eq!(contents, vec!["First note", "Second note"]);

    let records = activity_records(&harness.host.workspace(), &ProjectId::new("default")).await;
    assert_eq!(records.len(), 1, "exactly the board record is written");
    assert_eq!(records[0]["actor"]["kind"], json!("Agent"));
    assert_eq!(records[0]["actor"]["id"], json!(TOKEN_ID));
    assert_eq!(records[0]["actor"]["label"], json!(TOKEN_LABEL));

    client.cancel().await.expect("close client");
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn active_project_binding_follows_host_changes() {
    let (harness, token) = Harness::start(ConnectionScope::ActiveProject).await;
    {
        let workspace = harness.host.workspace();
        create_project(&workspace, "p1").await;
        create_project(&workspace, "p2").await;
    }
    let client = harness.connect(&token).await;

    harness.host.set_active(ProjectId::new("p1"));
    client
        .call_tool(
            CallToolRequestParams::new("board__add_notes")
                .with_arguments(args(json!({ "notes": [{ "content": "alpha" }] }))),
        )
        .await
        .expect("add to p1");

    harness.host.set_active(ProjectId::new("p2"));
    client
        .call_tool(
            CallToolRequestParams::new("board__add_notes")
                .with_arguments(args(json!({ "notes": [{ "content": "beta" }] }))),
        )
        .await
        .expect("add to p2");

    assert_eq!(
        note_contents(&read_board(&harness.board_path("p1"))),
        vec!["alpha"]
    );
    assert_eq!(
        note_contents(&read_board(&harness.board_path("p2"))),
        vec!["beta"]
    );

    client.cancel().await.expect("close client");
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn excluded_tool_returns_a_tool_error() {
    let (harness, token) =
        Harness::start(ConnectionScope::Project(ProjectId::new("default"))).await;
    let client = harness.connect(&token).await;

    let result = client
        .call_tool(
            CallToolRequestParams::new("dynamic_workflow__resume").with_arguments(args(json!({
                "run_id": "test-run"
            }))),
        )
        .await
        .expect("tools/call");
    assert_eq!(result.is_error, Some(true));
    let text = result.content[0]
        .as_text()
        .expect("text error")
        .text
        .clone();
    assert!(
        text.contains("not exposed"),
        "unexpected error text: {text}"
    );

    client.cancel().await.expect("close client");
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_asset_import_outside_folders_returns_a_tool_error_and_does_not_import() {
    let (harness, token) =
        Harness::start(ConnectionScope::Project(ProjectId::new("default"))).await;
    let client = harness.connect(&token).await;
    let source = harness.base.join("candidate.txt");
    fs::write(&source, "not imported").expect("write source");

    let result = client
        .call_tool(
            CallToolRequestParams::new("asset__import").with_arguments(args(json!({
                "paths": [source.to_string_lossy()]
            }))),
        )
        .await
        .expect("tools/call");
    assert_eq!(result.is_error, Some(true));
    let text = result.content[0]
        .as_text()
        .expect("text error")
        .text
        .clone();
    assert!(
        text.contains("outside workspace"),
        "unexpected error text: {text}"
    );
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .expect("structured error")["code"],
        json!("ASSET/CALLER_PATH_OUTSIDE_WORKSPACE")
    );

    let listed = harness
        .host
        .workspace()
        .operation_service()
        .invoke(
            &ProjectId::new("default"),
            InvokeRequest {
                name: "asset.list".to_owned(),
                input: json!({}),
                context: OperationContext::new(
                    CommandActor::new(CommandActorKind::Human),
                    CommandProvenance::Direct,
                ),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect("asset.list");
    assert_eq!(listed.output["assets"], json!([]), "nothing was imported");

    client.cancel().await.expect("close client");
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_item_returns_the_core_diagnostic_as_a_tool_error() {
    let (harness, token) =
        Harness::start(ConnectionScope::Project(ProjectId::new("default"))).await;
    let client = harness.connect(&token).await;

    let result = client
        .call_tool(
            CallToolRequestParams::new("board__remove_items").with_arguments(args(json!({
                "item_ids": ["missing-item"]
            }))),
        )
        .await
        .expect("tools/call");
    assert_eq!(result.is_error, Some(true));
    let text = result.content[0]
        .as_text()
        .expect("text error")
        .text
        .clone();
    assert!(
        text.contains("CORE/BOARD_ITEM_MISSING"),
        "unexpected text: {text}"
    );
    assert!(
        text.contains("board item does not exist"),
        "unexpected text: {text}"
    );
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .expect("structured error")["code"],
        json!("CORE/BOARD_ITEM_MISSING")
    );

    client.cancel().await.expect("close client");
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_missing_tokens_wrong_tokens_and_foreign_origins_or_hosts() {
    let (harness, token) =
        Harness::start(ConnectionScope::Project(ProjectId::new("default"))).await;
    let http = reqwest013::Client::new();
    let url = harness.url();

    let missing = http.post(&url).send().await.expect("no token request");
    assert_eq!(missing.status(), reqwest013::StatusCode::UNAUTHORIZED);
    assert_eq!(
        missing.headers()["www-authenticate"],
        "Bearer",
        "401 advertises the scheme"
    );

    let wrong = http
        .post(&url)
        .header("Authorization", format!("Bearer {}", "0".repeat(64)))
        .send()
        .await
        .expect("wrong token request");
    assert_eq!(wrong.status(), reqwest013::StatusCode::UNAUTHORIZED);

    let foreign_origin = http
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Origin", "http://evil.example")
        .body("{}")
        .send()
        .await
        .expect("foreign origin request");
    assert_eq!(foreign_origin.status(), reqwest013::StatusCode::FORBIDDEN);

    let foreign_host = http
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header(reqwest013::header::HOST, "evil.example")
        .body("{}")
        .send()
        .await
        .expect("foreign host request");
    assert_eq!(foreign_host.status(), reqwest013::StatusCode::FORBIDDEN);

    assert!(harness.registry.resolve(&token).is_some());
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuilt_workspace_sees_persisted_board_state() {
    let (harness, token) =
        Harness::start(ConnectionScope::Project(ProjectId::new("default"))).await;
    let client = harness.connect(&token).await;

    client
        .call_tool(
            CallToolRequestParams::new("board__add_notes")
                .with_arguments(args(json!({ "notes": [{ "content": "persisted" }] }))),
        )
        .await
        .expect("add note");

    let rebuilt = build_host(&harness.base).await;
    harness.host.replace_workspace(rebuilt);

    let read = client
        .call_tool(CallToolRequestParams::new("board__read").with_arguments(args(json!({}))))
        .await
        .expect("board__read");
    assert_eq!(read.is_error, Some(false));
    let structured = read.structured_content.expect("snapshot");
    assert_eq!(note_contents(&structured), vec!["persisted"]);

    client.cancel().await.expect("close client");
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fixed_project_binding_ignores_the_active_project() {
    let (harness, token) =
        Harness::start(ConnectionScope::Project(ProjectId::new("default"))).await;
    create_project(&harness.host.workspace(), "other").await;
    harness.host.set_active(ProjectId::new("other"));
    let client = harness.connect(&token).await;

    client
        .call_tool(
            CallToolRequestParams::new("board__add_notes")
                .with_arguments(args(json!({ "notes": [{ "content": "fixed scope" }] }))),
        )
        .await
        .expect("add note");

    assert_eq!(
        note_contents(&read_board(&harness.board_path("default"))),
        vec!["fixed scope"]
    );
    assert!(
        note_contents(&read_board(&harness.board_path("other"))).is_empty(),
        "the active project's board stayed empty"
    );

    client.cancel().await.expect("close client");
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_tool_returns_a_tool_error() {
    let (harness, token) =
        Harness::start(ConnectionScope::Project(ProjectId::new("default"))).await;
    let client = harness.connect(&token).await;

    let result = client
        .call_tool(CallToolRequestParams::new("nope__nope"))
        .await
        .expect("tools/call");
    assert_eq!(result.is_error, Some(true));
    let text = result.content[0]
        .as_text()
        .expect("text error")
        .text
        .clone();
    assert!(
        text.contains("not exposed"),
        "unexpected error text: {text}"
    );

    client.cancel().await.expect("close client");
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_token_is_rejected() {
    let (harness, token) =
        Harness::start(ConnectionScope::Project(ProjectId::new("default"))).await;
    assert!(harness.registry.revoke(&token));

    let response = reqwest013::Client::new()
        .post(harness.url())
        .header(reqwest013::header::AUTHORIZATION, format!("Bearer {token}"))
        .body("{}")
        .send()
        .await
        .expect("revoked token request");
    assert_eq!(response.status(), reqwest013::StatusCode::UNAUTHORIZED);

    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn initialize_is_stateless_and_returns_no_session_id() {
    let (harness, token) =
        Harness::start(ConnectionScope::Project(ProjectId::new("default"))).await;

    let response = reqwest013::Client::new()
        .post(harness.url())
        .header(reqwest013::header::AUTHORIZATION, format!("Bearer {token}"))
        .header(reqwest013::header::CONTENT_TYPE, "application/json")
        .header(
            reqwest013::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2026-07-28",
                    "capabilities": {},
                    "clientInfo": { "name": "raw-initialize-test", "version": "0.0.0" }
                }
            })
            .to_string(),
        )
        .send()
        .await
        .expect("raw initialize request");

    assert_eq!(response.status(), reqwest013::StatusCode::OK);
    assert!(
        response.headers().get("mcp-session-id").is_none(),
        "stateless mode must not issue a session id"
    );

    harness.stop().await;
}
