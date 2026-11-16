//! Automated 30-document Scenario T end-to-end integration tests.
//!
//! Spawns the real `text-mod` subprocess via [`ProcessMod`] and drives the
//! full `WorkspaceHost` pipeline (asset store, collections, board, dynamic
//! workflows, recipes, event bus, and operation registry).

#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use mera_app_host::{
    AssetStore, InvokeRequest, InvokeResult, OperationProvider, WorkspaceHost,
    WorkspaceHostBuilder, WorkspaceScope,
};
use mera_core::board::BoardItemKind;
use mera_core::command::{CommandActor, CommandActorKind, CommandProvenance, OperationContext};
use mera_core::dynamic_workflow::{RunDocument, RunState, SlotState};
use mera_core::event::Event;
use mera_core::model::{AssetId, ProjectId, RunId};
use mera_core::project::ProjectMetadata;
use mera_core::task::CancelToken;
use mod_sdk::serde_json::{self, Value, json};
use mod_sdk::{EventBus, Mod, ModRegistry, ProcessMod, ProcessModSpec};
use tempfile::TempDir;

const PROJECT: &str = "scenario-t-e2e";

struct Fixture {
    host: WorkspaceHost,
    project: ProjectId,
    project_dir: PathBuf,
    base: TempDir,
}

impl Fixture {
    async fn new(scope: &str) -> Self {
        let base = tempfile::tempdir().expect("temp dir");
        let event_bus = EventBus::new();
        let process_mod = ProcessMod::spawn(ProcessModSpec::new(env!("CARGO_BIN_EXE_text-mod")))
            .await
            .expect("spawn the real text-mod");
        let module: Arc<dyn Mod> = Arc::new(process_mod);
        let registry = Arc::new(ModRegistry::start(vec![module], &event_bus).await);
        assert!(
            registry
                .list()
                .first()
                .is_some_and(|entry| entry.health.is_available()),
            "the text mod must start"
        );

        let host = WorkspaceHostBuilder::new(WorkspaceScope::new(scope), base.path())
            .event_bus(event_bus)
            .mod_registry(Arc::clone(&registry))
            .build()
            .await
            .expect("build the workspace host");

        let project = ProjectId::new(PROJECT);
        host.project_service()
            .create_project(
                project.clone(),
                ProjectMetadata::new(
                    "Scenario T",
                    "30-document end-to-end integration fixture",
                    "2026-11-14T00:00:00Z".into(),
                    "2026-11-14T00:00:00Z".into(),
                ),
            )
            .await
            .expect("create the project");
        let project_dir = host.config().paths().project_dir(project.as_str());

        Self {
            host,
            project,
            project_dir,
            base,
        }
    }

    fn human_context() -> OperationContext {
        OperationContext::new(
            CommandActor::new(CommandActorKind::Human).with_id("editor-human"),
            CommandProvenance::Direct,
        )
    }

    fn agent_context() -> OperationContext {
        OperationContext::new(
            CommandActor::new(CommandActorKind::Agent).with_id("assistant-agent"),
            CommandProvenance::Direct,
        )
    }

    async fn invoke_as(&self, name: &str, input: Value, context: OperationContext) -> InvokeResult {
        self.host
            .operation_service()
            .invoke(
                &self.project,
                InvokeRequest {
                    name: name.to_owned(),
                    input,
                    context,
                    cancel: CancelToken::new(),
                    run: None,
                    input_sources: BTreeMap::new(),
                },
            )
            .await
            .unwrap_or_else(|error| panic!("{name} failed: {error:?}"))
    }

    async fn invoke(&self, name: &str, input: Value) -> Value {
        self.invoke_as(name, input, Self::human_context())
            .await
            .output
    }

    async fn import_bytes(&self, filename: &str, bytes: &[u8]) -> (PathBuf, String) {
        let path = self.base.path().join(filename);
        std::fs::write(&path, bytes).expect("write fixture file");
        let output = self
            .invoke("asset.import", json!({ "paths": [path.to_string_lossy()] }))
            .await;
        let asset_id = output["assets"]
            .as_array()
            .and_then(|assets| assets.first())
            .and_then(Value::as_str)
            .expect("imported asset id")
            .to_owned();
        (path, asset_id)
    }

    fn store(&self) -> AssetStore {
        AssetStore::new(&self.project_dir)
    }

    async fn asset_bytes(&self, asset_id: &str) -> Vec<u8> {
        let id = AssetId::try_new(asset_id).expect("valid asset id");
        let record = self
            .store()
            .get_asset(&id)
            .await
            .expect("read asset record")
            .expect("asset exists");
        std::fs::read(self.store().content_path(&record.hash)).expect("read asset bytes")
    }

    async fn asset_text(&self, asset_id: &str) -> String {
        String::from_utf8(self.asset_bytes(asset_id).await).expect("asset is valid UTF-8")
    }

    async fn asset_hash(&self, asset_id: &str) -> String {
        let id = AssetId::try_new(asset_id).expect("valid asset id");
        self.store()
            .get_asset(&id)
            .await
            .expect("read asset record")
            .expect("asset exists")
            .hash
    }

    async fn wait_for_run_polling(&self, run_id: &RunId, mut on_tick: impl FnMut()) -> RunDocument {
        for _ in 0..1000 {
            on_tick();
            let out = self
                .invoke(
                    "dynamic_workflow.get_run",
                    json!({ "run_id": run_id.as_str() }),
                )
                .await;
            let doc: RunDocument = serde_json::from_value(out).expect("parse run document");
            if !matches!(doc.state, RunState::Queued | RunState::Running) {
                on_tick();
                return doc;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        panic!("run `{run_id}` did not settle in time");
    }

    async fn wait_for_run(&self, run_id: &RunId) -> RunDocument {
        self.wait_for_run_polling(run_id, || {}).await
    }

    async fn create_editorial_workflow(&self) -> String {
        let created = self
            .invoke(
                "dynamic_workflow.create",
                json!({
                    "name": "editorial_pipeline",
                    "description": "Normalize, replace glossary terms, inspect title, and render front matter.",
                    "workflow": {
                        "inputs": {
                            "doc": { "type": "string", "format": "asset", "contentMediaType": "text/*" }
                        },
                        "params": {
                            "type": "object",
                            "properties": {
                                "glossary": { "type": "array" },
                                "author": { "type": "string" },
                                "date": { "type": "string" }
                            },
                            "required": ["glossary", "author", "date"]
                        },
                        "steps": [
                            {
                                "id": "norm",
                                "op": "text.normalize",
                                "input": {
                                    "doc": { "$from": "inputs.doc" },
                                    "punctuation": "halfwidth",
                                    "whitespace": true,
                                    "newline": true,
                                    "unicode": "NFC"
                                }
                            },
                            {
                                "id": "terms",
                                "op": "text.replace",
                                "input": {
                                    "doc": { "$from": "steps.norm.doc" },
                                    "pairs": { "$from": "params.glossary" }
                                }
                            },
                            {
                                "id": "info",
                                "op": "text.inspect",
                                "input": {
                                    "doc": { "$from": "steps.terms.doc" }
                                }
                            },
                            {
                                "id": "front",
                                "op": "text.template",
                                "input": {
                                    "doc": { "$from": "steps.terms.doc" },
                                    "template": "---\ntitle: {{title}}\nauthor: {{author}}\ndate: {{date}}\n---\n\n{{body}}",
                                    "values": {
                                        "title": { "$from": "steps.info.title" },
                                        "author": { "$from": "params.author" },
                                        "date": { "$from": "params.date" }
                                    }
                                }
                            }
                        ],
                        "output": { "$from": "steps.front.doc" }
                    }
                }),
            )
            .await;
        created["dynamic_workflow_id"]
            .as_str()
            .expect("dynamic_workflow_id")
            .to_owned()
    }
}

fn generate_interview_markdown(index_1_based: usize) -> String {
    let ai_mention = if index_1_based.is_multiple_of(3) || index_1_based == 12 {
        "AI and LLM Systems"
    } else {
        "Creative Tooling"
    };
    format!(
        "# Interview {index_1_based:02}： {ai_mention}   Session\r\n\r\n\
         Q（Host）： How  does  AI  and  LLM  workflow  automation  help  studio  teams？\r\n\r\n\
         A（Guest {index_1_based:02}）： We  combine  local  models  with  AI  pipelines， keeping  every  LLM  step  inspectable。\r\n"
    )
}

fn default_glossary() -> Value {
    json!([
        { "find": "AI", "replace": "Artificial Intelligence", "mode": "word" },
        { "find": "LLM", "replace": "Large Language Model", "mode": "word" }
    ])
}

#[tokio::test]
async fn thirty_slot_batch_run_publishes_slot_events_and_isolates_invalid_utf8_failure() {
    let f = Fixture::new("scenario-t-batch-isolation").await;

    let mut asset_ids = Vec::with_capacity(30);
    let mut original_hashes = Vec::with_capacity(30);
    let mut original_bytes = Vec::with_capacity(30);

    for idx in 1..=30 {
        let filename = format!("interview_{idx:02}.md");
        let bytes: Vec<u8> = if idx == 18 {
            b"# Broken\nInvalid bytes: \xff\xfe\xfd\n".to_vec()
        } else {
            generate_interview_markdown(idx).into_bytes()
        };
        let (_path, asset_id) = f.import_bytes(&filename, &bytes).await;
        original_hashes.push(f.asset_hash(&asset_id).await);
        original_bytes.push(bytes);
        asset_ids.push(asset_id);
    }

    let created_col = f
        .invoke(
            "collection.create",
            json!({ "name": "Interviews Batch", "assets": asset_ids }),
        )
        .await;
    let col_id = created_col["collection_id"]
        .as_str()
        .expect("collection_id")
        .to_owned();
    let board_item_id = created_col["board_item_id"]
        .as_str()
        .expect("board_item_id")
        .to_owned();

    let board = f
        .host
        .board_service()
        .snapshot(&f.project)
        .await
        .expect("board snapshot");
    let (_, board_item) = board
        .items()
        .iter()
        .find(|(id, _)| id.as_str() == board_item_id)
        .expect("collection board item");
    assert!(
        matches!(
            board_item.kind(),
            BoardItemKind::Collection { collection_id } if collection_id.as_str() == col_id
        ),
        "board must record the collection item"
    );

    let mut events_rx = f.host.event_bus().subscribe();
    let workflow_id = f.create_editorial_workflow().await;
    let run_out = f
        .invoke_as(
            "dynamic_workflow.run",
            json!({
                "dynamic_workflow_id": workflow_id,
                "target": { "collection_id": col_id },
                "params": {
                    "glossary": default_glossary(),
                    "author": "Studio Desk",
                    "date": "2026-11-14"
                }
            }),
            Fixture::agent_context(),
        )
        .await
        .output;
    let run_id = RunId::new(run_out["run_id"].as_str().expect("run_id"));

    let mut captured = Vec::new();
    let run_doc = f
        .wait_for_run_polling(&run_id, || {
            while let Ok(event) = events_rx.try_recv() {
                captured.push(event);
            }
        })
        .await;

    assert_eq!(run_doc.state, RunState::Failed);
    assert_eq!(run_doc.items.len(), 30);

    let succeeded: Vec<_> = run_doc
        .items
        .iter()
        .filter(|item| item.state == SlotState::Succeeded)
        .collect();
    let failed: Vec<_> = run_doc
        .items
        .iter()
        .filter(|item| item.state == SlotState::Failed)
        .collect();
    assert_eq!(succeeded.len(), 29);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].slot_id.as_ref().unwrap().as_str(), "slot-18");

    let diag = failed[0]
        .diagnostic
        .as_ref()
        .expect("failed slot must carry a diagnostic");
    assert_eq!(diag.code().as_str(), "MOD/CALLER_INVALID_UTF8");
    assert!(
        diag.message().to_ascii_lowercase().contains("utf-8"),
        "expected invalid UTF-8 message, got {}",
        diag.message()
    );

    let slot_changed_count = captured
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::Document { kind, document_id, .. }
                    if kind == "dynamic_workflow.slot_changed" && document_id == run_id.as_str()
            )
        })
        .count();
    assert_eq!(
        slot_changed_count, 60,
        "expected 2 slot_changed events (running + terminal) for each of the 30 slots"
    );

    let col = f
        .invoke("collection.get", json!({ "collection_id": col_id }))
        .await;
    let slots = col["slots"].as_array().expect("slots");
    assert_eq!(slots.len(), 30);

    for (idx, slot) in slots.iter().enumerate() {
        let current_asset = slot["asset_id"].as_str().expect("slot asset_id");
        if idx == 17 {
            assert_eq!(
                current_asset, asset_ids[17],
                "failed slot-18 must keep pointing to its original asset"
            );
        } else {
            assert_ne!(current_asset, asset_ids[idx]);
            let text = f.asset_text(current_asset).await;
            assert!(
                text.starts_with("---\ntitle: "),
                "slot {idx} missing front matter: {text}"
            );
            assert!(text.contains("author: Studio Desk\n"));
            assert!(text.contains("date: 2026-11-14\n"));
            assert!(!text.contains('\r'));
            assert!(!text.contains('，'));
        }

        // Original imported asset remains byte-for-byte unchanged in AssetStore.
        assert_eq!(f.asset_hash(&asset_ids[idx]).await, original_hashes[idx]);
        assert_eq!(f.asset_bytes(&asset_ids[idx]).await, original_bytes[idx]);
    }

    f.host.shutdown().await;
}

#[tokio::test]
async fn cancel_stops_in_flight_thirty_slot_text_mod_batch_run() {
    let f = Fixture::new("scenario-t-cancel").await;

    let repeated_para =
        "AI and LLM tooling accelerates editorial QA across studio teams. ".repeat(1600);
    let mut asset_ids = Vec::with_capacity(30);
    for idx in 1..=30 {
        let content = format!("# Interview {idx:02}: AI and LLM Scale\n\n{repeated_para}\n");
        let (_path, asset_id) = f
            .import_bytes(&format!("large_{idx:02}.md"), content.as_bytes())
            .await;
        asset_ids.push(asset_id);
    }

    let created_col = f
        .invoke(
            "collection.create",
            json!({ "name": "Large Cancel Batch", "assets": asset_ids }),
        )
        .await;
    let col_id = created_col["collection_id"]
        .as_str()
        .expect("collection_id")
        .to_owned();

    let workflow_id = f.create_editorial_workflow().await;
    let mut events_rx = f.host.event_bus().subscribe();

    let run_out = f
        .invoke(
            "dynamic_workflow.run",
            json!({
                "dynamic_workflow_id": workflow_id,
                "target": { "collection_id": col_id },
                "params": {
                    "glossary": [
                        { "find": "\\bAI\\b", "replace": "Artificial Intelligence", "mode": "regex" },
                        { "find": "LLM", "replace": "Large Language Model", "mode": "word" },
                        { "find": "editorial", "replace": "curated", "mode": "word" },
                        { "find": "studio", "replace": "production", "mode": "word" }
                    ],
                    "author": "Studio Desk",
                    "date": "2026-11-14"
                }
            }),
        )
        .await;
    let run_id = RunId::new(run_out["run_id"].as_str().expect("run_id"));

    // Wait until the first slot event arrives, proving execution in text-mod is active.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .expect("timed out waiting for first slot_changed event");
        let event = tokio::time::timeout(remaining, events_rx.recv())
            .await
            .expect("timed out receiving event")
            .expect("event bus open");
        if matches!(
            event,
            Event::Document { ref kind, ref document_id, .. }
                if kind == "dynamic_workflow.slot_changed" && document_id == run_id.as_str()
        ) {
            break;
        }
    }

    let cancel_out = f
        .invoke(
            "dynamic_workflow.cancel",
            json!({ "run_id": run_id.as_str() }),
        )
        .await;
    assert_eq!(cancel_out["state"], "cancelled");

    let run_doc = f.wait_for_run(&run_id).await;
    assert_eq!(run_doc.state, RunState::Cancelled);
    let succeeded_count = run_doc
        .items
        .iter()
        .filter(|item| item.state == SlotState::Succeeded)
        .count();
    assert!(
        succeeded_count < 30,
        "expected cancellation to stop remaining slots, got {succeeded_count}/30 succeeded"
    );

    f.host.shutdown().await;
}

#[tokio::test]
async fn full_scenario_t_walkthrough_edit_append_freeze_reuse_split_and_join() {
    let f = Fixture::new("scenario-t-walkthrough").await;

    // Step 1: Import 30 valid interview transcripts (01..30).
    let mut raw_assets = Vec::with_capacity(30);
    let mut raw_contents = Vec::with_capacity(30);
    for idx in 1..=30 {
        let content = generate_interview_markdown(idx);
        let (_path, asset_id) = f
            .import_bytes(&format!("interview_{idx:02}.md"), content.as_bytes())
            .await;
        raw_contents.push(content);
        raw_assets.push(asset_id);
    }

    let main_col_out = f
        .invoke(
            "collection.create",
            json!({ "name": "Curated Interviews", "assets": raw_assets }),
        )
        .await;
    let main_col_id = main_col_out["collection_id"]
        .as_str()
        .expect("main collection_id")
        .to_owned();

    // Step 2: 3-slot trial collection first, then full 30-slot batch as Agent actor.
    let trial_col_out = f
        .invoke(
            "collection.create",
            json!({ "name": "Trial 3", "assets": &raw_assets[..3] }),
        )
        .await;
    let trial_col_id = trial_col_out["collection_id"]
        .as_str()
        .expect("trial collection_id")
        .to_owned();

    let workflow_id = f.create_editorial_workflow().await;
    let batch_params = json!({
        "glossary": default_glossary(),
        "author": "Studio Desk",
        "date": "2026-11-14"
    });

    let trial_run_out = f
        .invoke_as(
            "dynamic_workflow.run",
            json!({
                "dynamic_workflow_id": workflow_id,
                "target": { "collection_id": trial_col_id },
                "params": batch_params
            }),
            Fixture::agent_context(),
        )
        .await
        .output;
    let trial_run = f
        .wait_for_run(&RunId::new(trial_run_out["run_id"].as_str().unwrap()))
        .await;
    assert_eq!(trial_run.state, RunState::Completed);

    let full_run_out = f
        .invoke_as(
            "dynamic_workflow.run",
            json!({
                "dynamic_workflow_id": workflow_id,
                "target": { "collection_id": main_col_id },
                "params": batch_params
            }),
            Fixture::agent_context(),
        )
        .await
        .output;
    let full_run = f
        .wait_for_run(&RunId::new(full_run_out["run_id"].as_str().unwrap()))
        .await;
    assert_eq!(full_run.state, RunState::Completed);
    assert!(
        full_run
            .items
            .iter()
            .all(|item| item.state == SlotState::Succeeded)
    );

    // Step 3: recipe.edit on slot-12 as Human actor + undo/redo.
    let recipe_before = f
        .invoke(
            "recipe.get",
            json!({ "collection_id": main_col_id, "slot_id": "slot-12" }),
        )
        .await;
    let steps_before = recipe_before["steps"].as_array().expect("recipe steps");
    assert_eq!(steps_before.len(), 4);
    for step in steps_before {
        assert_eq!(step["actor"]["kind"], "Agent");
    }
    let pre_edit_asset = recipe_before["current_asset"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let pre_edit_text = f.asset_text(&pre_edit_asset).await;
    assert!(pre_edit_text.contains(
        "title: Interview 12: Artificial Intelligence and Large Language Model Systems Session"
    ));

    let replace_step_id = steps_before[1]["step_id"].as_str().unwrap().to_owned();
    let edit1 = f
        .invoke_as(
            "recipe.edit",
            json!({
                "collection_id": main_col_id,
                "slot_id": "slot-12",
                "step_id": replace_step_id,
                "input": {
                    "pairs": [
                        { "find": "LLM", "replace": "Large Language Model", "mode": "word" }
                    ]
                }
            }),
            Fixture::human_context(),
        )
        .await
        .output;
    assert_eq!(edit1["stale"], false);
    assert_eq!(edit1["replayed_steps"], 3);
    let edited_asset_1 = edit1["asset_id"].as_str().unwrap().to_owned();
    let edited_text_1 = f.asset_text(&edited_asset_1).await;
    assert!(
        edited_text_1.contains("title: Interview 12: AI and Large Language Model Systems Session")
    );
    assert!(edited_text_1.contains("with AI pipelines"));
    assert!(edited_text_1.contains("every Large Language Model step"));

    let recipe_after_edit1 = f
        .invoke(
            "recipe.get",
            json!({ "collection_id": main_col_id, "slot_id": "slot-12" }),
        )
        .await;
    let steps_after_edit1 = recipe_after_edit1["steps"].as_array().unwrap();
    assert_eq!(steps_after_edit1[0]["actor"]["kind"], "Agent");
    assert_eq!(steps_after_edit1[1]["actor"]["kind"], "Human");
    assert_eq!(steps_after_edit1[2]["actor"]["kind"], "Human");
    assert_eq!(steps_after_edit1[3]["actor"]["kind"], "Human");

    // Undo reverts slot-12 to pre_edit_asset, redo restores edited_asset_1.
    f.invoke("collection.undo", json!({ "collection_id": main_col_id }))
        .await;
    let col_undone = f
        .invoke("collection.get", json!({ "collection_id": main_col_id }))
        .await;
    assert_eq!(col_undone["slots"][11]["asset_id"], pre_edit_asset);

    f.invoke("collection.redo", json!({ "collection_id": main_col_id }))
        .await;
    let col_redone = f
        .invoke("collection.get", json!({ "collection_id": main_col_id }))
        .await;
    assert_eq!(col_redone["slots"][11]["asset_id"], edited_asset_1);

    // Step 4: Append 1-step copyright workflow across all 30 slots, then edit slot-12 again.
    let copyright_wf = f
        .invoke(
            "dynamic_workflow.create",
            json!({
                "name": "append_copyright",
                "workflow": {
                    "inputs": { "doc": { "type": "string", "format": "asset" } },
                    "params": {
                        "type": "object",
                        "properties": { "year": { "type": "string" } },
                        "required": ["year"]
                    },
                    "steps": [{
                        "id": "copyright",
                        "op": "text.template",
                        "input": {
                            "doc": { "$from": "inputs.doc" },
                            "template": "---\ntitle: {{title}}\nauthor: {{author}}\ndate: {{date}}\n---\n\n{{body}}\n\n---\nCopyright {{year}}",
                            "values": {
                                "year": { "$from": "params.year" }
                            }
                        }
                    }],
                    "output": { "$from": "steps.copyright.doc" }
                }
            }),
        )
        .await;
    let copyright_wf_id = copyright_wf["dynamic_workflow_id"].as_str().unwrap();
    let copyright_run_out = f
        .invoke(
            "dynamic_workflow.run",
            json!({
                "dynamic_workflow_id": copyright_wf_id,
                "target": { "collection_id": main_col_id },
                "params": { "year": "2026 Studio" }
            }),
        )
        .await;
    let copyright_run = f
        .wait_for_run(&RunId::new(copyright_run_out["run_id"].as_str().unwrap()))
        .await;
    assert_eq!(copyright_run.state, RunState::Completed);

    let col_after_copyright = f
        .invoke("collection.get", json!({ "collection_id": main_col_id }))
        .await;
    let slot12_after_copyright = col_after_copyright["slots"][11]["asset_id"]
        .as_str()
        .unwrap();
    let slot12_copyright_text = f.asset_text(slot12_after_copyright).await;
    assert!(
        slot12_copyright_text
            .contains("title: Interview 12: AI and Large Language Model Systems Session")
    );
    assert!(slot12_copyright_text.ends_with("\n\n---\nCopyright 2026 Studio"));

    // Second recipe.edit on slot-12's text.replace step: replays replace -> inspect -> front -> copyright.
    let recipe_5 = f
        .invoke(
            "recipe.get",
            json!({ "collection_id": main_col_id, "slot_id": "slot-12" }),
        )
        .await;
    let steps_5 = recipe_5["steps"].as_array().unwrap();
    assert_eq!(steps_5.len(), 5);
    let replace_step_id_2 = steps_5[1]["step_id"].as_str().unwrap().to_owned();

    let edit2 = f
        .invoke(
            "recipe.edit",
            json!({
                "collection_id": main_col_id,
                "slot_id": "slot-12",
                "step_id": replace_step_id_2,
                "input": {
                    "pairs": [
                        { "find": "LLM", "replace": "Foundation Model", "mode": "word" }
                    ]
                }
            }),
        )
        .await;
    assert_eq!(edit2["stale"], false);
    assert_eq!(edit2["replayed_steps"], 4);

    let slot12_final_asset = edit2["asset_id"].as_str().unwrap().to_owned();
    let slot12_final_bytes = f.asset_bytes(&slot12_final_asset).await;
    let slot12_final_text = String::from_utf8(slot12_final_bytes.clone()).unwrap();
    assert!(
        slot12_final_text.contains("title: Interview 12: AI and Foundation Model Systems Session")
    );
    assert!(slot12_final_text.contains("author: Studio Desk\n"));
    assert!(slot12_final_text.contains("date: 2026-11-14\n"));
    assert!(slot12_final_text.contains("every Foundation Model step"));
    assert!(slot12_final_text.ends_with("\n\n---\nCopyright 2026 Studio"));

    // Step 5: Freeze slot-12's recipe, verify in op_list, and reuse byte-identically.
    let frozen = f
        .invoke(
            "dynamic_workflow.from_recipe",
            json!({
                "collection_id": main_col_id,
                "slot_id": "slot-12",
                "name": "interview_pipeline",
                "save": true
            }),
        )
        .await;
    let frozen_id = frozen["dynamic_workflow_id"].as_str().unwrap().to_owned();
    let frozen_op = format!("dynamic_workflow.{frozen_id}");

    let ops = f.host.operation_service().list(&f.project).await;
    assert!(
        ops.iter().any(|info| {
            info.spec.name == frozen_op
                && info.provider
                    == OperationProvider::DynamicWorkflow {
                        id: frozen_id.clone(),
                    }
        }),
        "frozen workflow operation `{frozen_op}` must appear in op_list"
    );

    let (_reimport_path, reimported_12) = f
        .import_bytes("interview_12_reimport.md", raw_contents[11].as_bytes())
        .await;
    let direct_out = f.invoke(&frozen_op, json!({ "doc": reimported_12 })).await;
    let direct_asset = direct_out["output"].as_str().unwrap();
    assert_eq!(f.asset_bytes(direct_asset).await, slot12_final_bytes);

    let reuse_col_out = f
        .invoke(
            "collection.create",
            json!({ "name": "Reuse Collection", "assets": [reimported_12] }),
        )
        .await;
    let reuse_col_id = reuse_col_out["collection_id"].as_str().unwrap();
    let wrapper_wf = f
        .invoke(
            "dynamic_workflow.create",
            json!({
                "name": "reuse_wrapper",
                "workflow": {
                    "inputs": { "doc": { "type": "string", "format": "asset" } },
                    "params": { "type": "object" },
                    "steps": [{
                        "id": "frozen",
                        "op": frozen_op,
                        "input": { "doc": { "$from": "inputs.doc" } }
                    }],
                    "output": { "$from": "steps.frozen.output" }
                }
            }),
        )
        .await;
    let wrapper_id = wrapper_wf["dynamic_workflow_id"].as_str().unwrap();
    let reuse_run_out = f
        .invoke(
            "dynamic_workflow.run",
            json!({
                "dynamic_workflow_id": wrapper_id,
                "target": { "collection_id": reuse_col_id }
            }),
        )
        .await;
    let reuse_run = f
        .wait_for_run(&RunId::new(reuse_run_out["run_id"].as_str().unwrap()))
        .await;
    assert_eq!(reuse_run.state, RunState::Completed);

    let reuse_col = f
        .invoke("collection.get", json!({ "collection_id": reuse_col_id }))
        .await;
    let reuse_asset = reuse_col["slots"][0]["asset_id"].as_str().unwrap();
    assert_eq!(f.asset_bytes(reuse_asset).await, slot12_final_bytes);

    // Step 6: text.split -> collection.create -> text.join anthology.
    let long_interview = "\
## Chapter 1: Origins\n\nWe started with local tools.\n\n\
## Chapter 2: Architecture\n\nEvery operation is recorded.\n\n\
## Chapter 3: Future\n\nHuman and agent workflows stay unified.\n";
    let (_long_path, long_asset) = f
        .import_bytes("long_interview.md", long_interview.as_bytes())
        .await;
    let split_res = f
        .invoke_as(
            "text.split",
            json!({
                "doc": long_asset,
                "by": { "kind": "heading", "level": 2 }
            }),
            Fixture::human_context(),
        )
        .await;
    let split_docs = split_res.output["docs"].as_array().expect("split docs");
    assert_eq!(split_docs.len(), 3);

    let mut chapter_assets = Vec::with_capacity(3);
    for doc in split_docs {
        let asset_id = doc["doc"].as_str().unwrap().to_owned();
        let record = f
            .store()
            .get_asset(&AssetId::try_new(&asset_id).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.origin, split_res.step.id);
        chapter_assets.push(asset_id);
    }
    let chapter_col = f
        .invoke(
            "collection.create",
            json!({ "name": "Chapters", "assets": chapter_assets }),
        )
        .await;
    assert!(chapter_col["collection_id"].as_str().is_some());

    // Join all 30 curated interviews from main_col_id in exact slot order.
    let main_col = f
        .invoke("collection.get", json!({ "collection_id": main_col_id }))
        .await;
    let main_slots = main_col["slots"].as_array().unwrap();
    assert_eq!(main_slots.len(), 30);

    let join_docs: Vec<Value> = main_slots
        .iter()
        .map(|slot| json!({ "doc": slot["asset_id"].as_str().unwrap() }))
        .collect();
    let joined = f
        .invoke(
            "text.join",
            json!({ "docs": join_docs, "separator": "\n\n===\n\n" }),
        )
        .await;
    let anthology_asset = joined["doc"].as_str().unwrap();
    let anthology = f.asset_text(anthology_asset).await;

    let mut expected_parts = Vec::with_capacity(30);
    for (idx, slot) in main_slots.iter().enumerate() {
        let part = f.asset_text(slot["asset_id"].as_str().unwrap()).await;
        assert!(part.contains(&format!("Interview {:02}:", idx + 1)));
        assert!(part.ends_with("\n\n---\nCopyright 2026 Studio"));
        expected_parts.push(part);
    }
    assert_eq!(anthology, expected_parts.join("\n\n===\n\n"));
    assert!(anthology.contains("title: Interview 12: AI and Foundation Model Systems Session"));

    f.host.shutdown().await;
}
