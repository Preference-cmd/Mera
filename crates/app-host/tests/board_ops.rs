//! Board kernel operations: one undoable batch and one board activity record
//! per mutator call, preview/execution equivalence, diagnostics, the automatic
//! note layout and the inline-text cap.

use std::fs;
use std::path::{Path, PathBuf};

use mera_app_host::{
    AppHostError, AppHostErrorCode, InvokeRequest, MAX_INLINE_TEXT_BYTES, WorkspaceHost,
    WorkspaceHostBuilder, WorkspaceScope,
};
use mera_core::board::{BoardCommand, BoardItemPosition, BoardItemSize};
use mera_core::command::{CommandActor, CommandActorKind, CommandProvenance, OperationContext};
use mera_core::diagnostic::Diagnostic;
use mera_core::model::{BoardItemId, DocumentVersion, ProjectId};
use mera_core::project::ProjectMetadata;
use mera_core::task::CancelToken;
use serde_json::{Value, json};

fn temp_dir(prefix: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("mera-board-ops-{prefix}-{nonce}"))
}

async fn build_host(base: &Path) -> WorkspaceHost {
    WorkspaceHostBuilder::new(WorkspaceScope::new("test-board-ops"), base)
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
                "Board ops",
                "board operation test project",
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ),
        )
        .await
        .expect("create project");
    project_id
}

fn agent_actor() -> CommandActor {
    CommandActor::new(CommandActorKind::Agent)
        .with_id("test-connection")
        .with_label("Board test")
}

fn agent_context() -> OperationContext {
    OperationContext::new(agent_actor(), CommandProvenance::Direct)
}

fn human_context() -> OperationContext {
    OperationContext::new(
        CommandActor::new(CommandActorKind::Human).with_id("human-1"),
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

async fn activity_records(host: &WorkspaceHost, project: &ProjectId) -> Vec<Value> {
    let output = invoke(host, project, "activity.list", json!({ "limit": 50 }))
        .await
        .expect("activity.list");
    output["records"].as_array().expect("records array").clone()
}

fn string_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|entry| entry.as_str().expect("string entry").to_owned())
        .collect()
}

fn item_ids(output: &Value, field: &str) -> Vec<String> {
    string_array(&output[field])
}

fn assert_board_activity(
    record: &Value,
    summary_key: &str,
    summary: &str,
    targets: &[String],
    history_entry_id: &str,
) {
    assert_eq!(record["kind"], json!("applied"));
    assert_eq!(record["summaryKey"], json!(summary_key));
    assert_eq!(record["summary"], json!(summary));
    assert_eq!(record["actor"]["kind"], json!("Agent"));
    assert_eq!(record["actor"]["id"], json!("test-connection"));
    assert_eq!(record["targets"], json!(targets));
    assert_eq!(record["historyEntryId"], json!(history_entry_id));
}

fn board_json(host: &WorkspaceHost, project: &ProjectId) -> Value {
    let path = host.board_service().board_path(project);
    let contents = fs::read_to_string(path).expect("read board.json");
    serde_json::from_str(&contents).expect("parse board.json")
}

fn project_dir(host: &WorkspaceHost, project: &ProjectId) -> PathBuf {
    host.config().paths().project_dir(project.as_str())
}

#[tokio::test]
async fn every_mutator_writes_one_board_activity_and_one_history_entry() {
    let base = temp_dir("mutators");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-mutators").await;

    // add_notes: one batch, one undo entry, one board activity record.
    let added = invoke(
        &host,
        &project,
        "board.add_notes",
        json!({ "notes": [{ "content": "one" }, { "content": "two" }] }),
    )
    .await
    .expect("add notes");
    let ids = item_ids(&added, "item_ids");
    assert_eq!(ids.len(), 2);
    assert_eq!(item_ids(&added, "board_item_ids"), ids);
    assert_eq!(added["version"], json!(1));
    let add_entry = added["history_entry_id"]
        .as_str()
        .expect("add_notes names its history entry")
        .to_owned();

    let on_disk = board_json(&host, &project);
    assert_eq!(on_disk["version"], json!(1));
    assert!(on_disk["items"].get(&ids[0]).is_some());
    assert!(on_disk["items"].get(&ids[1]).is_some());

    let records = activity_records(&host, &project).await;
    assert_eq!(records.len(), 1, "add_notes appends exactly one record");
    assert_board_activity(
        &records[0],
        "activity.board.addNote",
        "Added 2 notes",
        &ids,
        &add_entry,
    );

    // update_notes.
    let updated = invoke(
        &host,
        &project,
        "board.update_notes",
        json!({
            "updates": [
                { "item_id": ids[0], "content": "one edited" },
                { "item_id": ids[1], "content": "two edited" }
            ]
        }),
    )
    .await
    .expect("update notes");
    assert_eq!(updated["version"], json!(2));
    assert_eq!(updated["board_item_ids"], json!(ids));
    let update_entry = updated["history_entry_id"]
        .as_str()
        .expect("update_notes names its history entry")
        .to_owned();

    let records = activity_records(&host, &project).await;
    assert_eq!(
        records.len(),
        2,
        "update_notes appends exactly one record, not a generic op record too"
    );
    assert_board_activity(
        &records[0],
        "activity.board.apply",
        "Applied 2 board changes",
        &ids,
        &update_entry,
    );
    let on_disk = board_json(&host, &project);
    assert_eq!(on_disk["version"], json!(2));
    assert_eq!(
        on_disk["items"][&ids[0]]["kind"]["content"],
        json!("one edited")
    );

    // arrange: move, resize and restack in one batch.
    let arranged = invoke(
        &host,
        &project,
        "board.arrange",
        json!({
            "items": [
                { "item_id": ids[0], "position": { "x": 10, "y": 10 } },
                { "item_id": ids[1], "size": { "width": 300, "height": 200 } },
                { "item_id": ids[0], "z": 3 }
            ]
        }),
    )
    .await
    .expect("arrange");
    assert_eq!(arranged["version"], json!(3));
    assert_eq!(
        arranged["board_item_ids"],
        json!(ids),
        "targets keep first-touch order without duplicates"
    );
    let arrange_entry = arranged["history_entry_id"]
        .as_str()
        .expect("arrange names its history entry")
        .to_owned();

    let records = activity_records(&host, &project).await;
    assert_eq!(records.len(), 3);
    assert_board_activity(
        &records[0],
        "activity.board.apply",
        "Applied 3 board changes",
        &ids,
        &arrange_entry,
    );
    let on_disk = board_json(&host, &project);
    assert_eq!(on_disk["version"], json!(3));
    assert_eq!(
        on_disk["items"][&ids[0]]["position"],
        json!({ "x": 10, "y": 10 })
    );
    assert_eq!(on_disk["items"][&ids[0]]["z"], json!(3));
    assert_eq!(
        on_disk["items"][&ids[1]]["size"],
        json!({ "width": 300, "height": 200 })
    );

    // remove_items.
    let removed = invoke(
        &host,
        &project,
        "board.remove_items",
        json!({ "item_ids": [ids[1]] }),
    )
    .await
    .expect("remove item");
    assert_eq!(removed["version"], json!(4));
    assert_eq!(removed["board_item_ids"], json!([ids[1]]));
    let remove_entry = removed["history_entry_id"]
        .as_str()
        .expect("remove_items names its history entry")
        .to_owned();

    let records = activity_records(&host, &project).await;
    assert_eq!(records.len(), 4);
    assert_board_activity(
        &records[0],
        "activity.board.apply",
        "Applied a board change",
        &[ids[1].clone()],
        &remove_entry,
    );
    let on_disk = board_json(&host, &project);
    assert_eq!(on_disk["version"], json!(4));
    assert!(on_disk["items"].get(&ids[0]).is_some());
    assert!(
        on_disk["items"].get(&ids[1]).is_none(),
        "remove_items persists the removal"
    );
    assert!(
        records
            .iter()
            .all(|record| record["summaryKey"] != json!("activity.op")),
        "a board mutator must not also produce a generic op activity record: {records:?}"
    );

    // The mutator's step record still lands even though the generic activity
    // record is skipped.
    let steps = fs::read_to_string(project_dir(&host, &project).join("steps.jsonl"))
        .expect("read steps.jsonl");
    for op in [
        "board.add_notes",
        "board.update_notes",
        "board.arrange",
        "board.remove_items",
    ] {
        assert!(
            steps.contains(&format!("\"op\":\"{op}\"")),
            "{op} must write its step record: {steps}"
        );
    }
    assert!(steps.contains("\"id\":\"test-connection\""));

    // Every mutator batch entered the board history: undoing in reverse order
    // names the entry each operation reported and finally empties the board.
    for (expected_entry, expected_version) in [
        (remove_entry, 5),
        (arrange_entry, 6),
        (update_entry, 7),
        (add_entry, 8),
    ] {
        let undone = host
            .board_service()
            .undo(&project, agent_actor(), None)
            .await
            .expect("undo succeeds")
            .expect("the mutator batch is on the undo stack");
        assert_eq!(
            undone.history_entry_id().map(ToString::to_string),
            Some(expected_entry)
        );
        assert_eq!(
            board_json(&host, &project)["version"],
            json!(expected_version)
        );
    }

    let on_disk = board_json(&host, &project);
    assert_eq!(on_disk["version"], json!(8));
    assert!(
        on_disk["items"].as_object().expect("items map").is_empty(),
        "undoing all four batches restores the empty board"
    );

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn preview_does_not_mutate_and_matches_the_following_execution() {
    let base = temp_dir("preview");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-preview").await;

    let input = json!({ "notes": [{ "content": "alpha" }, { "content": "beta" }] });
    let preview = invoke(
        &host,
        &project,
        "board.preview",
        json!({ "operation": "board.add_notes", "input": input.clone() }),
    )
    .await
    .expect("preview add_notes");

    assert_eq!(preview["status"], json!("Applied"));
    assert_eq!(preview["version"], json!(1));
    let changes = preview["changes"].as_array().expect("changes array");
    assert_eq!(changes.len(), 2);
    let preview_ids: Vec<String> = changes
        .iter()
        .map(|change| {
            assert_eq!(change["type"], json!("item_added"));
            change["item_id"]
                .as_str()
                .expect("change item id")
                .to_owned()
        })
        .collect();
    let preview_positions: Vec<(i64, i64)> = changes
        .iter()
        .map(|change| {
            (
                change["position"]["x"].as_i64().expect("x"),
                change["position"]["y"].as_i64().expect("y"),
            )
        })
        .collect();
    assert_eq!(
        preview_positions,
        vec![(0, 0), (0, 200)],
        "an empty board starts at the origin and stacks downward"
    );

    // The preview itself changed nothing.
    let board = host
        .board_service()
        .snapshot(&project)
        .await
        .expect("snapshot");
    assert_eq!(board.version(), DocumentVersion::new(0));
    assert!(board.items().is_empty());
    assert!(
        activity_records(&host, &project).await.is_empty(),
        "a preview appends no activity"
    );

    // Execution builds identical commands for the same input and base version.
    let executed = invoke(&host, &project, "board.add_notes", input)
        .await
        .expect("execute add_notes");
    assert_eq!(item_ids(&executed, "item_ids"), preview_ids);
    assert_eq!(executed["version"], preview["version"]);
    assert_eq!(executed["board_item_ids"], json!(preview_ids));

    let board = host
        .board_service()
        .snapshot(&project)
        .await
        .expect("snapshot");
    assert_eq!(board.version(), DocumentVersion::new(1));
    let executed_positions: Vec<(i64, i64)> = preview_ids
        .iter()
        .map(|id| {
            let item = board
                .item(&BoardItemId::new(id.clone()))
                .expect("previewed item exists");
            (item.position().x, item.position().y)
        })
        .collect();
    assert_eq!(executed_positions, preview_positions);

    // The same equivalence holds for a destructive operation.
    let remove_input = json!({ "item_ids": [preview_ids[0]] });
    let preview = invoke(
        &host,
        &project,
        "board.preview",
        json!({ "operation": "board.remove_items", "input": remove_input.clone() }),
    )
    .await
    .expect("preview remove_items");
    assert_eq!(preview["status"], json!("Applied"));
    assert_eq!(preview["version"], json!(2));
    assert_eq!(preview["changes"][0]["type"], json!("item_removed"));
    assert_eq!(preview["changes"][0]["item_id"], json!(preview_ids[0]));

    let removed = invoke(&host, &project, "board.remove_items", remove_input)
        .await
        .expect("execute remove_items");
    assert_eq!(removed["version"], preview["version"]);
    assert_eq!(removed["board_item_ids"], json!([preview_ids[0]]));

    // An explicit base version names the same generated ids as the implicit
    // current version, so a preview made before reading the version still
    // matches the execution.
    let preview = invoke(
        &host,
        &project,
        "board.preview",
        json!({
            "operation": "board.add_notes",
            "input": { "notes": [{ "content": "gamma" }], "base_version": 2 }
        }),
    )
    .await
    .expect("preview with an explicit base version");
    let executed = invoke(
        &host,
        &project,
        "board.add_notes",
        json!({ "notes": [{ "content": "gamma" }] }),
    )
    .await
    .expect("execute without a base version");
    assert_eq!(
        executed["item_ids"][0], preview["changes"][0]["item_id"],
        "the base version is a seed component, not part of the id payload"
    );
    assert_eq!(executed["version"], preview["version"]);

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn version_conflicts_locked_items_missing_items_and_invalid_input_are_diagnostics() {
    let base = temp_dir("diagnostics");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-diagnostics").await;

    let added = invoke(
        &host,
        &project,
        "board.add_notes",
        json!({ "notes": [{ "content": "target" }] }),
    )
    .await
    .expect("add note");
    let item = BoardItemId::new(item_ids(&added, "item_ids")[0].clone());

    // An explicit stale base version is a caller diagnostic telling the agent
    // to read first.
    let error = invoke(
        &host,
        &project,
        "board.update_notes",
        json!({
            "updates": [{ "item_id": item.as_str(), "content": "stale" }],
            "base_version": 0
        }),
    )
    .await
    .expect_err("stale base version must be rejected");
    assert_eq!(error.code().as_str(), "BOARD/CALLER_VERSION_CONFLICT");
    assert!(
        error.message().contains("board.read"),
        "the diagnostic must tell the caller to call board.read: {}",
        error.message()
    );

    // The typed service error carries the versions and classifies as a
    // conflict for the IPC payload.
    let stale = host
        .board_service()
        .apply_commands(
            &project,
            &agent_context(),
            Some(DocumentVersion::new(0)),
            vec![BoardCommand::MoveItem {
                item_id: item.clone(),
                position: BoardItemPosition::new(7, 7),
            }],
        )
        .await
        .expect_err("stale base version must not apply");
    assert_eq!(stale.code(), AppHostErrorCode::Conflict);
    match &stale {
        AppHostError::BoardVersionConflict {
            base_version,
            current_version,
            ..
        } => {
            assert_eq!(base_version.get(), 0);
            assert_eq!(current_version.get(), 1);
        }
        other => panic!("expected BoardVersionConflict, got {other:?}"),
    }
    let details = stale.details().expect("conflict details");
    assert_eq!(details["base_version"], json!("0"));
    assert_eq!(details["current_version"], json!("1"));

    // Without a base version the service applies on the version it reads
    // under the lock.
    let applied = host
        .board_service()
        .apply_commands(
            &project,
            &agent_context(),
            None,
            vec![BoardCommand::MoveItem {
                item_id: item.clone(),
                position: BoardItemPosition::new(7, 7),
            }],
        )
        .await
        .expect("apply on the current version");
    assert_eq!(applied.version(), DocumentVersion::new(2));

    // Locked items cannot be edited; the first core diagnostic carries why.
    host.board_service()
        .apply_commands(
            &project,
            &human_context(),
            None,
            vec![BoardCommand::LockItem {
                item_id: item.clone(),
                locked: true,
            }],
        )
        .await
        .expect("lock item");
    let error = invoke(
        &host,
        &project,
        "board.update_notes",
        json!({ "updates": [{ "item_id": item.as_str(), "content": "locked" }] }),
    )
    .await
    .expect_err("locked item must be rejected");
    assert_eq!(error.code().as_str(), "CORE/BOARD_ITEM_LOCKED");

    // Missing items report the core diagnostic too.
    let error = invoke(
        &host,
        &project,
        "board.update_notes",
        json!({ "updates": [{ "item_id": "item-does-not-exist", "content": "x" }] }),
    )
    .await
    .expect_err("missing item must be rejected");
    assert_eq!(error.code().as_str(), "CORE/BOARD_ITEM_MISSING");

    let error = invoke(
        &host,
        &project,
        "board.remove_items",
        json!({ "item_ids": ["item-does-not-exist"] }),
    )
    .await
    .expect_err("missing item removal must be rejected");
    assert_eq!(error.code().as_str(), "CORE/BOARD_ITEM_MISSING");

    // Schema violations are caller input errors.
    let error = invoke(
        &host,
        &project,
        "board.arrange",
        json!({ "items": [{ "item_id": item.as_str() }] }),
    )
    .await
    .expect_err("an arranged item must set at least one field");
    assert_eq!(error.code().as_str(), "BOARD/CALLER_INVALID_INPUT");

    let error = invoke(&host, &project, "board.add_notes", json!({ "notes": [] }))
        .await
        .expect_err("an empty notes array must be rejected");
    assert_eq!(error.code().as_str(), "BOARD/CALLER_INVALID_INPUT");

    let _ = fs::remove_dir_all(&base);
}

fn rect(position: &BoardItemPosition, size: &BoardItemSize) -> (i64, i64, i64, i64) {
    (
        position.x,
        position.y,
        position.x.saturating_add(size.width as i64),
        position.y.saturating_add(size.height as i64),
    )
}

fn overlaps(a: (i64, i64, i64, i64), b: (i64, i64, i64, i64)) -> bool {
    a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3
}

async fn assert_no_overlaps(host: &WorkspaceHost, project: &ProjectId) {
    let board = host
        .board_service()
        .snapshot(project)
        .await
        .expect("snapshot");
    let items: Vec<_> = board.items().values().collect();
    for (index, item) in items.iter().enumerate() {
        for other in &items[index + 1..] {
            assert!(
                !overlaps(
                    rect(item.position(), item.size()),
                    rect(other.position(), other.size())
                ),
                "items `{}` and `{}` overlap",
                item.id(),
                other.id()
            );
        }
    }
}

#[tokio::test]
async fn automatic_layout_avoids_existing_items_and_is_deterministic() {
    let base = temp_dir("layout");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-layout").await;

    invoke(
        &host,
        &project,
        "board.add_notes",
        json!({
            "notes": [{
                "content": "existing",
                "position": { "x": 100, "y": 50 },
                "size": { "width": 200, "height": 120 }
            }]
        }),
    )
    .await
    .expect("add existing note");

    let input = json!({ "notes": [{ "content": "a" }, { "content": "b" }] });
    let first_preview = invoke(
        &host,
        &project,
        "board.preview",
        json!({ "operation": "board.add_notes", "input": input.clone() }),
    )
    .await
    .expect("first preview");
    let second_preview = invoke(
        &host,
        &project,
        "board.preview",
        json!({ "operation": "board.add_notes", "input": input.clone() }),
    )
    .await
    .expect("second preview");
    assert_eq!(
        first_preview, second_preview,
        "previewing the same input at the same version is deterministic"
    );

    let executed = invoke(&host, &project, "board.add_notes", input)
        .await
        .expect("add automatic notes");
    assert_eq!(
        executed["item_ids"][0], first_preview["changes"][0]["item_id"],
        "execution reuses the previewed ids"
    );

    let board = host
        .board_service()
        .snapshot(&project)
        .await
        .expect("snapshot");
    let first = board
        .item(&BoardItemId::new(
            executed["item_ids"][0].as_str().expect("id").to_owned(),
        ))
        .expect("first automatic note");
    let second = board
        .item(&BoardItemId::new(
            executed["item_ids"][1].as_str().expect("id").to_owned(),
        ))
        .expect("second automatic note");
    assert_eq!(*first.position(), BoardItemPosition::new(340, 50));
    assert_eq!(*second.position(), BoardItemPosition::new(340, 250));
    assert_no_overlaps(&host, &project).await;

    // A mixed batch: the automatic note moves to the right of the explicitly
    // positioned note in the same batch, so the two cannot overlap.
    let mixed = invoke(
        &host,
        &project,
        "board.add_notes",
        json!({
            "notes": [
                { "content": "explicit", "position": { "x": 500, "y": 500 } },
                { "content": "automatic" }
            ]
        }),
    )
    .await
    .expect("add mixed notes");
    assert_eq!(mixed["item_ids"].as_array().expect("ids").len(), 2);
    let automatic_id = BoardItemId::new(mixed["item_ids"][1].as_str().expect("id").to_owned());
    let board = host
        .board_service()
        .snapshot(&project)
        .await
        .expect("snapshot");
    assert_eq!(board.items().len(), 5);
    let automatic = board.item(&automatic_id).expect("automatic note");
    assert_eq!(
        automatic.position().x,
        780,
        "the column sits 40px right of the widest item"
    );
    assert_eq!(
        automatic.position().y,
        50,
        "the column starts at the bbox top"
    );
    assert_no_overlaps(&host, &project).await;

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn oversized_note_content_is_rejected_by_the_inline_text_cap() {
    let base = temp_dir("inline-cap");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-inline-cap").await;

    let oversized = "a".repeat(MAX_INLINE_TEXT_BYTES + 1);
    let error = invoke(
        &host,
        &project,
        "board.add_notes",
        json!({ "notes": [{ "content": oversized }] }),
    )
    .await
    .expect_err("oversized note content must be rejected");
    assert_eq!(error.code().as_str(), "BOARD/CALLER_INLINE_TEXT_TOO_LARGE");
    assert!(
        error.message().contains(&MAX_INLINE_TEXT_BYTES.to_string()),
        "the diagnostic names the cap: {}",
        error.message()
    );

    let board = host
        .board_service()
        .snapshot(&project)
        .await
        .expect("snapshot");
    assert_eq!(board.version(), DocumentVersion::new(0));
    assert!(board.items().is_empty());
    assert!(
        activity_records(&host, &project).await.is_empty(),
        "a rejected call appends no activity"
    );
    assert!(
        !project_dir(&host, &project).join("steps.jsonl").exists(),
        "a rejected call writes no step record"
    );

    // The preview path applies the same cap to the nested mutator input.
    let error = invoke(
        &host,
        &project,
        "board.preview",
        json!({
            "operation": "board.add_notes",
            "input": { "notes": [{ "content": "a".repeat(MAX_INLINE_TEXT_BYTES + 1) }] }
        }),
    )
    .await
    .expect_err("oversized preview input must be rejected");
    assert_eq!(error.code().as_str(), "BOARD/CALLER_INLINE_TEXT_TOO_LARGE");

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn generic_op_preview_still_refuses_the_board_mutators() {
    let base = temp_dir("op-preview");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-op-preview").await;

    for (index, name) in [
        "board.add_notes",
        "board.update_notes",
        "board.arrange",
        "board.remove_items",
    ]
    .into_iter()
    .enumerate()
    {
        let error = host
            .operation_service()
            .preview(
                &project,
                name,
                json!({}),
                &format!("board-op-{index}"),
                CancelToken::new(),
            )
            .await
            .expect_err("op_preview must refuse a document mutator");
        assert_eq!(
            error.code().as_str(),
            "BOARD/CALLER_DOCUMENT_CHANGE_PREVIEW_REFUSED",
            "{name} must be refused"
        );
    }

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn op_list_publishes_the_four_preview_mutators() {
    let base = temp_dir("op-list");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-op-list").await;

    let infos = host.operation_service().list(&project).await;
    let preview = infos
        .iter()
        .find(|info| info.spec.name == "board.preview")
        .expect("op_list exposes board.preview");
    let operation = &preview.spec.input_schema["properties"]["operation"];
    assert_eq!(operation["type"], json!("string"));
    assert_eq!(
        operation["enum"],
        json!([
            "board.add_notes",
            "board.update_notes",
            "board.arrange",
            "board.remove_items"
        ]),
        "the preview operation enum names exactly the four board mutators"
    );

    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn board_read_returns_the_version_and_item_fields() {
    let base = temp_dir("read");
    let host = build_host(&base).await;
    let project = create_project(&host, "proj-read").await;

    let added = invoke(
        &host,
        &project,
        "board.add_notes",
        json!({
            "notes": [{
                "content": "hello",
                "position": { "x": 12, "y": 34 },
                "size": { "width": 240, "height": 160 }
            }]
        }),
    )
    .await
    .expect("add note");
    let item_id = item_ids(&added, "item_ids")[0].clone();

    let read = invoke(&host, &project, "board.read", json!({}))
        .await
        .expect("board.read");
    assert_eq!(read["version"], json!(1));
    let items = read["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item["id"], json!(item_id));
    assert_eq!(item["kind"]["type"], json!("note"));
    assert_eq!(item["kind"]["content"], json!("hello"));
    assert_eq!(item["position"], json!({ "x": 12, "y": 34 }));
    assert_eq!(item["size"], json!({ "width": 240, "height": 160 }));
    assert_eq!(item["z"], json!(0));
    assert_eq!(item["locked"], json!(false));

    // Reading is read-only: no activity record and no step record.
    assert_eq!(activity_records(&host, &project).await.len(), 1);
    let steps =
        fs::read_to_string(project_dir(&host, &project).join("steps.jsonl")).expect("steps.jsonl");
    assert!(!steps.contains("board.read"));

    let _ = fs::remove_dir_all(&base);
}
