//! Board operation contract tests: atomic batch construction against the
//! locked version, rejected-preview diagnostics, batch bounds, the size floor,
//! preview/execution equivalence, no-op semantics and exact-integer coercion.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use reimagine_app_host::{
    AppHostError, AppHostErrorCode, InvokeRequest, WorkspaceHost, WorkspaceHostBuilder,
    WorkspaceScope,
};
use reimagine_core::board::{BoardCommand, BoardItemPosition};
use reimagine_core::command::{
    CommandActor, CommandActorKind, CommandProvenance, OperationContext,
};
use reimagine_core::diagnostic::Diagnostic;
use reimagine_core::model::{BoardItemId, DocumentVersion, ProjectId};
use reimagine_core::project::ProjectMetadata;
use reimagine_core::task::CancelToken;
use serde_json::{Value, json};

fn temp_dir(prefix: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("reimagine-board-contract-{prefix}-{nonce}"))
}

async fn build_host(base: &Path) -> WorkspaceHost {
    WorkspaceHostBuilder::new(WorkspaceScope::new("test-board-contract"), base)
        .build()
        .await
        .expect("build host")
}

async fn create_project(host: &WorkspaceHost, id: &str) -> ProjectId {
    let project_id = ProjectId::new(id);
    host.project_service()
        .create_project(
            project_id.clone(),
            ProjectMetadata::new(
                "Board contract",
                "board operation contract tests",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .expect("create project");
    project_id
}

fn agent_context() -> OperationContext {
    OperationContext::new(
        CommandActor::new(CommandActorKind::Agent).with_id("test-connection"),
        CommandProvenance::Direct,
    )
}

async fn invoke(
    host: &WorkspaceHost,
    project: &ProjectId,
    name: &str,
    input: Value,
) -> Result<Value, Diagnostic> {
    host.operation_service()
        .invoke(
            project,
            InvokeRequest {
                name: name.to_owned(),
                input,
                context: agent_context(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .map(|result| result.output)
}

async fn activity_count(host: &WorkspaceHost, project: &ProjectId) -> usize {
    let output = invoke(host, project, "activity.list", json!({ "limit": 50 }))
        .await
        .expect("activity.list");
    output["records"].as_array().expect("records array").len()
}

async fn add_note(host: &WorkspaceHost, project: &ProjectId, content: &str) -> String {
    let output = invoke(
        host,
        project,
        "board.add_notes",
        json!({ "notes": [{ "content": content }] }),
    )
    .await
    .expect("add note");
    output["item_ids"][0]
        .as_str()
        .expect("generated note id")
        .to_owned()
}

async fn board_item(host: &WorkspaceHost, project: &ProjectId, id: &str) -> Value {
    let board = host
        .board_service()
        .snapshot(project)
        .await
        .expect("snapshot");
    let item = board
        .item(&BoardItemId::new(id.to_owned()))
        .expect("board item");
    let content = match item.kind() {
        reimagine_core::board::BoardItemKind::Note { content } => Some(content.clone()),
        _ => None,
    };
    json!({
        "position": { "x": item.position().x, "y": item.position().y },
        "size": { "width": item.size().width, "height": item.size().height },
        "z": item.z(),
        "content": content,
    })
}

#[tokio::test]
async fn omitted_base_version_is_built_under_the_lock() {
    let base = temp_dir("atomic");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-atomic").await;

    let item = BoardItemId::new(add_note(&host, &project, "target").await);

    // The builder runs with the document and version the batch will use; after
    // one applied batch that is the new version, not a snapshot taken earlier.
    let observed = Arc::new(AtomicU64::new(u64::MAX));
    let applied = host
        .board_service()
        .apply_commands_with(&project, &agent_context(), None, {
            let observed = Arc::clone(&observed);
            let item = item.clone();
            move |document, base_version| {
                assert_eq!(
                    document.version(),
                    base_version,
                    "the builder must see the document of the chosen base version"
                );
                observed.store(base_version.get(), Ordering::SeqCst);
                vec![BoardCommand::MoveItem {
                    item_id: item,
                    position: BoardItemPosition::new(7, 7),
                }]
            }
        })
        .await
        .expect("apply on the locked version");
    assert_eq!(applied.version(), DocumentVersion::new(2));
    assert_eq!(
        observed.load(Ordering::SeqCst),
        1,
        "an omitted base version must be read under the lock"
    );

    // A supplied version that is no longer current conflicts before the
    // builder runs; the panic would fail the test if it did.
    let stale = host
        .board_service()
        .apply_commands_with(
            &project,
            &agent_context(),
            Some(DocumentVersion::new(0)),
            |_document, _base_version| panic!("stale base version must not build"),
        )
        .await
        .expect_err("stale base version must conflict");
    assert_eq!(stale.code(), AppHostErrorCode::Conflict);
    match &stale {
        AppHostError::BoardVersionConflict {
            base_version,
            current_version,
            ..
        } => {
            assert_eq!(base_version.get(), 0);
            assert_eq!(current_version.get(), 2);
        }
        other => panic!("expected BoardVersionConflict, got {other:?}"),
    }

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn rejected_preview_reports_the_core_diagnostics() {
    let base = temp_dir("preview-rejected");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-preview-rejected").await;

    let item = BoardItemId::new(add_note(&host, &project, "locked").await);
    host.board_service()
        .apply_commands(
            &project,
            &agent_context(),
            None,
            vec![BoardCommand::LockItem {
                item_id: item.clone(),
                locked: true,
            }],
        )
        .await
        .expect("lock item");

    // A valid preview whose batch the board rejects is a successful call with
    // a Rejected status and the reason, not an error.
    let preview = invoke(
        &host,
        &project,
        "board.preview",
        json!({
            "operation": "board.update_notes",
            "input": { "updates": [{ "item_id": item.as_str(), "content": "blocked" }] }
        }),
    )
    .await
    .expect("a rejected batch still previews");
    assert_eq!(preview["status"], json!("Rejected"));
    assert_eq!(preview["changes"], json!([]));
    assert_eq!(preview["version"], json!(2));
    let diagnostics = preview["diagnostics"].as_array().expect("diagnostics");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["code"], json!("CORE/BOARD_ITEM_LOCKED"));
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .expect("message")
            .contains("locked"),
        "the diagnostic explains the lock: {diagnostics:?}"
    );

    // A successful preview carries an empty diagnostics array.
    let applied = invoke(
        &host,
        &project,
        "board.preview",
        json!({ "operation": "board.add_notes", "input": { "notes": [{ "content": "ok" }] } }),
    )
    .await
    .expect("preview add_notes");
    assert_eq!(applied["status"], json!("Applied"));
    assert_eq!(applied["diagnostics"], json!([]));

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn preview_caller_errors_are_rejected_with_diagnostics() {
    let base = temp_dir("preview-caller");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-preview-caller").await;

    let item = add_note(&host, &project, "target").await;

    // An unknown preview operation, including a real non-mutator operation
    // name, is a caller error.
    for operation in ["board.not_a_mutator", "board.read"] {
        let error = invoke(
            &host,
            &project,
            "board.preview",
            json!({ "operation": operation, "input": {} }),
        )
        .await
        .expect_err("unknown preview operation must be rejected");
        assert_eq!(
            error.code().as_str(),
            "BOARD/CALLER_INVALID_INPUT",
            "{operation} must name a mutator"
        );
        assert!(error.message().contains(operation));
    }

    // Invalid nested input is a caller error.
    let error = invoke(
        &host,
        &project,
        "board.preview",
        json!({ "operation": "board.add_notes", "input": { "notes": [] } }),
    )
    .await
    .expect_err("empty notes must be rejected");
    assert_eq!(error.code().as_str(), "BOARD/CALLER_INVALID_INPUT");

    // A stale explicit base version is a version conflict, not a rejection.
    let error = invoke(
        &host,
        &project,
        "board.preview",
        json!({
            "operation": "board.update_notes",
            "input": {
                "updates": [{ "item_id": item, "content": "stale" }],
                "base_version": 0
            }
        }),
    )
    .await
    .expect_err("stale preview base version must be rejected");
    assert_eq!(error.code().as_str(), "BOARD/CALLER_VERSION_CONFLICT");
    assert!(
        error.message().contains("board.read"),
        "the diagnostic must tell the caller to re-read: {}",
        error.message()
    );

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn batch_arrays_reject_zero_and_over_limit_inputs() {
    let base = temp_dir("batch-bounds");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-batch-bounds").await;

    let sample = json!({ "item_id": "item-x", "content": "x", "z": 0 });
    let cases = [
        ("board.add_notes", "notes", sample.clone()),
        ("board.update_notes", "updates", sample.clone()),
        ("board.arrange", "items", sample.clone()),
        ("board.remove_items", "item_ids", json!("item-x")),
    ];
    for (operation, key, entry) in cases {
        let batch = |entries: Vec<Value>| {
            let mut input = serde_json::Map::new();
            input.insert(key.to_owned(), Value::Array(entries));
            Value::Object(input)
        };

        let error = invoke(&host, &project, operation, batch(Vec::new()))
            .await
            .expect_err("an empty batch must be rejected");
        assert_eq!(
            error.code().as_str(),
            "BOARD/CALLER_INVALID_INPUT",
            "{operation} must reject an empty '{key}' array"
        );

        let error = invoke(&host, &project, operation, batch(vec![entry; 101]))
            .await
            .expect_err("a batch over the limit must be rejected");
        assert_eq!(
            error.code().as_str(),
            "BOARD/CALLER_INVALID_INPUT",
            "{operation} must reject more than 100 '{key}' entries"
        );
    }

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn arrange_enforces_the_item_size_floor() {
    let base = temp_dir("size-floor");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-size-floor").await;

    let item = add_note(&host, &project, "resize").await;
    for size in [
        json!({ "width": 119, "height": 80 }),
        json!({ "width": 120, "height": 79 }),
    ] {
        let error = invoke(
            &host,
            &project,
            "board.arrange",
            json!({ "items": [{ "item_id": item, "size": size }] }),
        )
        .await
        .expect_err("sizes below the floor must be rejected");
        assert_eq!(error.code().as_str(), "BOARD/CALLER_INVALID_INPUT");
    }

    invoke(
        &host,
        &project,
        "board.arrange",
        json!({ "items": [{ "item_id": item, "size": { "width": 120, "height": 80 } }] }),
    )
    .await
    .expect("the floor itself is accepted");

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn preview_and_execution_agree_for_update_notes_and_arrange() {
    let base = temp_dir("equivalence");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-equivalence").await;

    let item = add_note(&host, &project, "alpha").await;

    // update_notes: the previewed change is applied verbatim.
    let update_input = json!({ "updates": [{ "item_id": item, "content": "beta" }] });
    let preview = invoke(
        &host,
        &project,
        "board.preview",
        json!({ "operation": "board.update_notes", "input": update_input.clone() }),
    )
    .await
    .expect("preview update_notes");
    assert_eq!(preview["status"], json!("Applied"));
    assert_eq!(preview["version"], json!(2));
    assert_eq!(preview["changes"][0]["type"], json!("note_updated"));
    assert_eq!(preview["changes"][0]["before"], json!("alpha"));
    assert_eq!(preview["changes"][0]["after"], json!("beta"));

    let executed = invoke(&host, &project, "board.update_notes", update_input)
        .await
        .expect("execute update_notes");
    assert_eq!(executed["version"], preview["version"]);
    assert_eq!(executed["board_item_ids"], json!([item]));
    assert_eq!(
        board_item(&host, &project, &item).await["content"],
        preview["changes"][0]["after"]
    );

    // arrange: every previewed before/after pair is what execution produces.
    let arrange_input = json!({
        "items": [{
            "item_id": item,
            "position": { "x": 40, "y": 40 },
            "size": { "width": 300, "height": 200 },
            "z": 2
        }]
    });
    let preview = invoke(
        &host,
        &project,
        "board.preview",
        json!({ "operation": "board.arrange", "input": arrange_input.clone() }),
    )
    .await
    .expect("preview arrange");
    assert_eq!(preview["status"], json!("Applied"));
    assert_eq!(preview["version"], json!(3));
    let changes = preview["changes"].as_array().expect("changes");
    assert_eq!(changes.len(), 3);
    assert_eq!(changes[0]["type"], json!("item_moved"));
    assert_eq!(changes[0]["before"], json!({ "x": 0, "y": 0 }));
    assert_eq!(changes[0]["after"], json!({ "x": 40, "y": 40 }));
    assert_eq!(changes[1]["type"], json!("item_resized"));
    assert_eq!(changes[1]["after"], json!({ "width": 300, "height": 200 }));
    assert_eq!(changes[2]["type"], json!("item_z_changed"));
    assert_eq!(changes[2]["after"], json!(2));

    let executed = invoke(&host, &project, "board.arrange", arrange_input)
        .await
        .expect("execute arrange");
    assert_eq!(executed["version"], preview["version"]);
    assert_eq!(
        executed["board_item_ids"],
        json!([item]),
        "the touched ids match the previewed changes without duplicates"
    );
    let applied = board_item(&host, &project, &item).await;
    assert_eq!(applied["position"], changes[0]["after"]);
    assert_eq!(applied["size"], changes[1]["after"]);
    assert_eq!(applied["z"], changes[2]["after"]);

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn noop_update_leaves_no_history_entry_and_no_activity() {
    let base = temp_dir("noop");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-noop").await;

    let item = add_note(&host, &project, "same").await;
    assert_eq!(activity_count(&host, &project).await, 1);

    let output = invoke(
        &host,
        &project,
        "board.update_notes",
        json!({ "updates": [{ "item_id": item, "content": "same" }] }),
    )
    .await
    .expect("a no-op update succeeds");
    assert_eq!(output["version"], json!(1));
    assert_eq!(output["history_entry_id"], json!(null));
    assert_eq!(output["board_item_ids"], json!([]));
    assert_eq!(
        activity_count(&host, &project).await,
        1,
        "a no-op batch appends no activity record"
    );

    // The add is still the only history entry: undo restores the empty board.
    let undone = host
        .board_service()
        .undo(&project, agent_context().actor().clone(), None)
        .await
        .expect("undo")
        .expect("the add entry is on the undo stack");
    assert_eq!(undone.version(), DocumentVersion::new(2));
    let board = host
        .board_service()
        .snapshot(&project)
        .await
        .expect("snapshot");
    assert!(
        board.items().is_empty(),
        "a no-op batch must not push a history entry"
    );

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn integral_float_inputs_are_accepted_and_fractions_rejected() {
    let base = temp_dir("floats");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-floats").await;

    let output = invoke(
        &host,
        &project,
        "board.add_notes",
        json!({
            "notes": [{
                "content": "float",
                "position": { "x": 120.0, "y": 80.0 },
                "size": { "width": 240.0, "height": 160.0 }
            }]
        }),
    )
    .await
    .expect("integral floats are integers");
    let item = output["item_ids"][0].as_str().expect("id").to_owned();
    let applied = board_item(&host, &project, &item).await;
    assert_eq!(applied["position"], json!({ "x": 120, "y": 80 }));
    assert_eq!(applied["size"], json!({ "width": 240, "height": 160 }));

    invoke(
        &host,
        &project,
        "board.arrange",
        json!({
            "items": [{
                "item_id": item,
                "position": { "x": 40.0, "y": 40.0 },
                "size": { "width": 300.0, "height": 200.0 },
                "z": 2.0
            }]
        }),
    )
    .await
    .expect("integral floats in arrange");
    let applied = board_item(&host, &project, &item).await;
    assert_eq!(applied["position"], json!({ "x": 40, "y": 40 }));
    assert_eq!(applied["size"], json!({ "width": 300, "height": 200 }));
    assert_eq!(applied["z"], json!(2));

    // An integral float base version selects the current version.
    let updated = invoke(
        &host,
        &project,
        "board.update_notes",
        json!({
            "updates": [{ "item_id": item, "content": "edited" }],
            "base_version": 2.0
        }),
    )
    .await
    .expect("integral float base version");
    assert_eq!(updated["version"], json!(3));

    // Truly fractional values stay caller errors.
    for (operation, input) in [
        (
            "board.add_notes",
            json!({ "notes": [{ "content": "x", "position": { "x": 1.5, "y": 0 } }] }),
        ),
        (
            "board.add_notes",
            json!({ "notes": [{ "content": "x", "size": { "width": 120.5, "height": 80 } }] }),
        ),
        (
            "board.arrange",
            json!({ "items": [{ "item_id": item, "z": 1.5 }] }),
        ),
        (
            "board.update_notes",
            json!({
                "updates": [{ "item_id": item, "content": "x" }],
                "base_version": 1.5
            }),
        ),
    ] {
        let error = invoke(&host, &project, operation, input)
            .await
            .expect_err("fractional values must be rejected");
        assert_eq!(
            error.code().as_str(),
            "BOARD/CALLER_INVALID_INPUT",
            "{operation} must reject a fractional value"
        );
    }

    let _ = fs::remove_dir_all(&base);
}
