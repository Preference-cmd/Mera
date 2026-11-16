//! Integration tests for `recipe.get`, `recipe.edit`, `StepInputSource`,
//! `Slot.stale_steps`, and collection atomic undo/redo.

#![allow(clippy::result_large_err)]

use std::path::PathBuf;
use std::sync::Arc;

use mera_app_host::{
    AssetStore, InvokeRequest, WorkspaceHost, WorkspaceHostBuilder, WorkspaceScope,
    agent_may_invoke,
};
use mera_core::command::{CommandActor, CommandActorKind, CommandProvenance, OperationContext};
use mera_core::event::Timestamp;
use mera_core::model::{AssetId, ProjectId};
use mera_core::project::ProjectMetadata;
use mera_core::task::CancelToken;
use mod_sdk::serde_json::{Value, json};
use mod_sdk::{
    EventBus, Invocation, Mod, ModContext, ModManifest, ModRegistry, OperationAnnotations,
    OperationSpec, async_trait,
};
use tempfile::TempDir;
use text_mod::TextMod;

/// A test-only non-deterministic mod (`deterministic: false`) that appends a
/// marker to a text document.
struct NonDetMod {
    manifest: ModManifest,
}

impl NonDetMod {
    fn new() -> Self {
        Self {
            manifest: ModManifest {
                id: "nondet".to_owned(),
                name: "NonDet".to_owned(),
                version: "0.1.0".to_owned(),
                api: "0.1".to_owned(),
                events: vec![],
                operations: vec![OperationSpec {
                    name: "nondet.polish".to_owned(),
                    description: "Non-deterministic text polish".to_owned(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "doc": {
                                "type": "string",
                                "format": "asset",
                                "contentMediaType": "text/*"
                            }
                        },
                        "required": ["doc"],
                        "additionalProperties": false
                    }),
                    output_schema: json!({
                        "type": "object",
                        "properties": {
                            "doc": {
                                "type": "string",
                                "format": "asset",
                                "contentMediaType": "text/markdown"
                            }
                        },
                        "required": ["doc"],
                        "additionalProperties": false
                    }),
                    annotations: OperationAnnotations {
                        read_only: false,
                        destructive: false,
                        idempotent: false,
                        deterministic: false,
                        open_world: false,
                    },
                }],
            },
        }
    }
}

#[async_trait]
impl Mod for NonDetMod {
    fn manifest(&self) -> &ModManifest {
        &self.manifest
    }

    async fn start(&self, _ctx: ModContext) -> Result<(), mod_sdk::diagnostic::Diagnostic> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), mod_sdk::diagnostic::Diagnostic> {
        Ok(())
    }

    async fn invoke(
        &self,
        invocation: Invocation,
    ) -> Result<Value, mod_sdk::diagnostic::Diagnostic> {
        let doc_path = invocation
            .input
            .get("doc")
            .and_then(Value::as_str)
            .expect("doc path");
        let content = tokio::fs::read_to_string(doc_path)
            .await
            .expect("read input doc");
        let out_path = invocation.output_dir.join("polished.md");
        tokio::fs::write(&out_path, format!("{content}\n\n[polished]"))
            .await
            .expect("write output doc");
        Ok(json!({ "doc": "polished.md" }))
    }
}

struct Fixture {
    host: WorkspaceHost,
    project: ProjectId,
    project_dir: PathBuf,
    _base: TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let base = tempfile::tempdir().expect("temp dir");
        let event_bus = EventBus::new();
        let text: Arc<dyn Mod> = Arc::new(TextMod::new());
        let nondet: Arc<dyn Mod> = Arc::new(NonDetMod::new());
        let registry = Arc::new(ModRegistry::start(vec![text, nondet], &event_bus).await);
        assert!(
            registry.list().iter().all(|m| m.health.is_available()),
            "all test mods must start"
        );

        let host = WorkspaceHostBuilder::new(WorkspaceScope::new("recipe-e2e"), base.path())
            .event_bus(event_bus)
            .mod_registry(registry)
            .build()
            .await
            .expect("build workspace host");

        let project = ProjectId::new("recipe-proj");
        let now = Timestamp::new("2026-11-14T00:00:00Z");
        host.project_service()
            .create_project(
                project.clone(),
                ProjectMetadata::new("Recipe Project", "", now.clone(), now),
            )
            .await
            .expect("create project");
        let project_dir = host.project_service().project_dir(&project);

        Self {
            host,
            project,
            project_dir,
            _base: base,
        }
    }

    fn human_context() -> OperationContext {
        OperationContext::new(
            CommandActor::new(CommandActorKind::Human).with_id("user-1"),
            CommandProvenance::Direct,
        )
    }

    async fn invoke(&self, name: &str, input: Value) -> Value {
        self.host
            .operation_service()
            .invoke(
                &self.project,
                InvokeRequest {
                    name: name.to_owned(),
                    input,
                    context: Self::human_context(),
                    cancel: CancelToken::new(),
                    run: None,
                    input_sources: Default::default(),
                },
            )
            .await
            .unwrap_or_else(|e| panic!("invoke `{name}` failed: {}", e.message()))
            .output
    }

    async fn invoke_err(&self, name: &str, input: Value) -> mera_core::diagnostic::Diagnostic {
        self.host
            .operation_service()
            .invoke(
                &self.project,
                InvokeRequest {
                    name: name.to_owned(),
                    input,
                    context: Self::human_context(),
                    cancel: CancelToken::new(),
                    run: None,
                    input_sources: Default::default(),
                },
            )
            .await
            .expect_err("expected operation to fail")
    }

    async fn import_text(&self, filename: &str, content: &str) -> String {
        let src = self._base.path().join(filename);
        tokio::fs::write(&src, content).await.expect("write source");
        let out = self
            .invoke("asset.import", json!({ "paths": [src.to_string_lossy()] }))
            .await;
        out["assets"][0].as_str().expect("asset_id").to_owned()
    }

    async fn create_collection(&self, name: &str, assets: &[&str]) -> String {
        let out = self
            .invoke(
                "collection.create",
                json!({ "name": name, "assets": assets }),
            )
            .await;
        out["collection_id"]
            .as_str()
            .expect("collection_id")
            .to_owned()
    }

    async fn run_workflow_on_collection(
        &self,
        name: &str,
        steps: Value,
        output: Value,
        collection_id: &str,
    ) {
        let wf = self
            .invoke(
                "dynamic_workflow.create",
                json!({
                    "name": name,
                    "workflow": {
                        "inputs": { "doc": { "type": "string", "format": "asset" } },
                        "params": { "type": "object" },
                        "steps": steps,
                        "output": output,
                    }
                }),
            )
            .await;
        let workflow_id = wf["dynamic_workflow_id"].as_str().expect("workflow id");
        let run = self
            .invoke(
                "dynamic_workflow.run",
                json!({
                    "dynamic_workflow_id": workflow_id,
                    "target": { "collection_id": collection_id },
                }),
            )
            .await;
        let run_id = run["run_id"].as_str().expect("run_id").to_owned();
        loop {
            let state = self
                .invoke("dynamic_workflow.get_run", json!({ "run_id": run_id }))
                .await;
            let status = state["state"].as_str().unwrap_or("");
            if status != "queued" && status != "running" {
                assert_eq!(status, "completed", "run failed: {state}");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    async fn read_asset_text(&self, asset_id: &str) -> String {
        let store = AssetStore::new(&self.project_dir);
        let id = AssetId::new(asset_id);
        let record = store
            .get_asset(&id)
            .await
            .expect("get asset")
            .expect("asset exists");
        let path = store.content_path(&record.hash);
        tokio::fs::read_to_string(path).await.expect("read asset")
    }
}

#[tokio::test]
async fn scenario_t_four_step_chain_replay_and_append_step() {
    let f = Fixture::new().await;
    let root = f
        .import_text("doc.md", "# Draft\r\n\r\nHello  world.")
        .await;
    let col = f.create_collection("Docs", &[&root]).await;

    // 1. Run Scenario T 4-step chain: normalize -> replace -> inspect -> template.
    f.run_workflow_on_collection(
        "scenario-t",
        json!([
            { "id": "norm", "op": "text.normalize", "input": { "doc": { "$from": "inputs.doc" } } },
            { "id": "rep", "op": "text.replace", "input": {
                "doc": { "$from": "steps.norm.doc" },
                "pairs": [{ "find": "Draft", "replace": "Alpha", "mode": "literal" }]
            }},
            { "id": "info", "op": "text.inspect", "input": { "doc": { "$from": "steps.rep.doc" } } },
            { "id": "tpl", "op": "text.template", "input": {
                "doc": { "$from": "steps.rep.doc" },
                "values": { "title": { "$from": "steps.info.title" } },
                "template": "---\ntitle: {{title}}\n---\n{{body}}"
            }}
        ]),
        json!({ "$from": "steps.tpl.doc" }),
        &col,
    )
    .await;

    let recipe = f
        .invoke(
            "recipe.get",
            json!({ "collection_id": col, "slot_id": "slot-1" }),
        )
        .await;
    assert_eq!(recipe["root_asset"]["id"], root);
    assert_eq!(recipe["stale"], false);
    let steps = recipe["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 4);
    assert_eq!(steps[0]["op"], "text.normalize");
    assert_eq!(steps[1]["op"], "text.replace");
    assert_eq!(steps[2]["op"], "text.inspect");
    assert_eq!(steps[2]["read_only"], true);
    assert_eq!(steps[2]["output"]["title"], "Alpha");
    assert_eq!(steps[3]["op"], "text.template");
    assert_eq!(
        steps[3]["input_sources"]["/values/title"]["step_id"],
        steps[2]["step_id"]
    );
    assert_eq!(
        steps[3]["input_sources"]["/values/title"]["output_pointer"],
        "/title"
    );

    let text_before = f
        .read_asset_text(recipe["current_asset"]["id"].as_str().unwrap())
        .await;
    assert!(text_before.contains("title: Alpha"), "got: {text_before}");

    // Edit step 2 (text.replace) to change "Alpha" -> "Beta".
    let replace_step_id = steps[1]["step_id"].as_str().unwrap().to_owned();
    let edit_out = f
        .invoke(
            "recipe.edit",
            json!({
                "collection_id": col,
                "slot_id": "slot-1",
                "step_id": replace_step_id,
                "input": {
                    "pairs": [{ "find": "Draft", "replace": "Beta", "mode": "literal" }]
                }
            }),
        )
        .await;
    assert_eq!(edit_out["stale"], false);
    assert_eq!(edit_out["replayed_steps"], 3); // replace + inspect + template
    assert_eq!(edit_out["stale_steps"], 0);

    let text_after = f
        .read_asset_text(edit_out["asset_id"].as_str().unwrap())
        .await;
    assert!(text_after.contains("title: Beta"), "got: {text_after}");
    assert!(text_after.contains("# Beta"), "got: {text_after}");

    // 2. Append a 1-step dynamic workflow (copyright footer) and edit replace again.
    f.run_workflow_on_collection(
        "append-footer",
        json!([
            { "id": "footer", "op": "text.template", "input": {
                "doc": { "$from": "inputs.doc" },
                "template": "---\ntitle: {{title}}\n---\n{{body}}\n\n(c) 2026"
            }}
        ]),
        json!({ "$from": "steps.footer.doc" }),
        &col,
    )
    .await;

    let recipe2 = f
        .invoke(
            "recipe.get",
            json!({ "collection_id": col, "slot_id": "slot-1" }),
        )
        .await;
    let steps2 = recipe2["steps"].as_array().unwrap();
    assert_eq!(steps2.len(), 5, "recipe2: {recipe2}");
    assert_eq!(steps2[4]["op"], "text.template");

    let replace_step_id2 = steps2[1]["step_id"].as_str().unwrap().to_owned();
    let edit_out2 = f
        .invoke(
            "recipe.edit",
            json!({
                "collection_id": col,
                "slot_id": "slot-1",
                "step_id": replace_step_id2,
                "input": {
                    "pairs": [{ "find": "Draft", "replace": "Gamma", "mode": "literal" }]
                }
            }),
        )
        .await;
    assert_eq!(edit_out2["replayed_steps"], 4); // replace -> inspect -> template -> footer
    let text_final = f
        .read_asset_text(edit_out2["asset_id"].as_str().unwrap())
        .await;
    assert!(text_final.contains("title: Gamma"), "got: {text_final}");
    assert!(text_final.contains("(c) 2026"), "got: {text_final}");
}

#[tokio::test]
async fn direct_op_invoke_and_set_slot_chaining() {
    let f = Fixture::new().await;
    let root = f.import_text("direct.md", "# Hello World").await;
    let col = f.create_collection("Direct", &[&root]).await;

    let rep1 = f
        .invoke(
            "text.replace",
            json!({
                "doc": root,
                "pairs": [{ "find": "Hello", "replace": "Hi", "mode": "literal" }]
            }),
        )
        .await;
    let ast1 = rep1["doc"].as_str().unwrap().to_owned();
    f.invoke(
        "collection.set_slot",
        json!({ "collection_id": col, "slot_id": "slot-1", "asset": ast1 }),
    )
    .await;

    let rep2 = f
        .invoke(
            "text.replace",
            json!({
                "doc": ast1,
                "pairs": [{ "find": "World", "replace": "Rust", "mode": "literal" }]
            }),
        )
        .await;
    let ast2 = rep2["doc"].as_str().unwrap().to_owned();
    f.invoke(
        "collection.set_slot",
        json!({ "collection_id": col, "slot_id": "slot-1", "asset": ast2 }),
    )
    .await;

    let recipe = f
        .invoke(
            "recipe.get",
            json!({ "collection_id": col, "slot_id": "slot-1" }),
        )
        .await;
    let steps = recipe["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(recipe["root_asset"]["id"], root);

    let edit_out = f
        .invoke(
            "recipe.edit",
            json!({
                "collection_id": col,
                "slot_id": "slot-1",
                "step_id": steps[0]["step_id"],
                "input": {
                    "pairs": [{ "find": "Hello", "replace": "Greetings", "mode": "literal" }]
                }
            }),
        )
        .await;
    assert_eq!(edit_out["replayed_steps"], 2);
    let final_text = f
        .read_asset_text(edit_out["asset_id"].as_str().unwrap())
        .await;
    assert_eq!(final_text, "# Greetings Rust");
}

#[tokio::test]
async fn nondeterministic_stale_truncation_and_atomic_undo_redo() {
    let f = Fixture::new().await;
    let root = f.import_text("nondet.md", "# Title\r\n\r\nBody").await;
    let col = f.create_collection("NonDet", &[&root]).await;

    f.run_workflow_on_collection(
        "nondet-chain",
        json!([
            { "id": "norm", "op": "text.normalize", "input": { "doc": { "$from": "inputs.doc" } } },
            { "id": "rep", "op": "text.replace", "input": {
                "doc": { "$from": "steps.norm.doc" },
                "pairs": [{ "find": "Title", "replace": "V1", "mode": "literal" }]
            }},
            { "id": "polish", "op": "nondet.polish", "input": { "doc": { "$from": "steps.rep.doc" } } },
            { "id": "footer", "op": "text.template", "input": {
                "doc": { "$from": "steps.polish.doc" },
                "template": "{{body}}\n--end--"
            }}
        ]),
        json!({ "$from": "steps.footer.doc" }),
        &col,
    )
    .await;

    let col_before = f
        .invoke("collection.get", json!({ "collection_id": col }))
        .await;
    assert_eq!(col_before["slots"][0]["stale"], false);
    let original_footer_asset = col_before["slots"][0]["asset_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let recipe_before = f
        .invoke(
            "recipe.get",
            json!({ "collection_id": col, "slot_id": "slot-1" }),
        )
        .await;
    let rep_step_id = recipe_before["steps"][1]["step_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Edit `replace` -> stops before `nondet.polish`.
    let edit_out = f
        .invoke(
            "recipe.edit",
            json!({
                "collection_id": col,
                "slot_id": "slot-1",
                "step_id": rep_step_id,
                "input": {
                    "pairs": [{ "find": "Title", "replace": "V2", "mode": "literal" }]
                }
            }),
        )
        .await;
    assert_eq!(edit_out["stale"], true);
    assert_eq!(edit_out["replayed_steps"], 1);
    assert_eq!(edit_out["stale_steps"], 2);

    let new_rep_asset = edit_out["asset_id"].as_str().unwrap().to_owned();
    let new_rep_text = f.read_asset_text(&new_rep_asset).await;
    assert!(new_rep_text.contains("# V2"), "got: {new_rep_text}");
    assert!(!new_rep_text.contains("[polished]"));

    let col_stale = f
        .invoke("collection.get", json!({ "collection_id": col }))
        .await;
    assert_eq!(col_stale["slots"][0]["stale"], true);
    assert_eq!(col_stale["slots"][0]["asset_id"], new_rep_asset);

    let recipe_stale = f
        .invoke(
            "recipe.get",
            json!({ "collection_id": col, "slot_id": "slot-1" }),
        )
        .await;
    assert_eq!(recipe_stale["stale"], true);
    let stale_steps = recipe_stale["steps"].as_array().unwrap();
    assert_eq!(stale_steps.len(), 4);
    assert_eq!(stale_steps[2]["op"], "nondet.polish");
    assert_eq!(stale_steps[2]["stale"], true);
    assert_eq!(stale_steps[2]["output"], Value::Null);
    assert_eq!(stale_steps[3]["op"], "text.template");
    assert_eq!(stale_steps[3]["stale"], true);
    assert_eq!(stale_steps[3]["output"], Value::Null);

    // Atomic undo restores original footer asset and stale: false.
    f.invoke("collection.undo", json!({ "collection_id": col }))
        .await;
    let col_undone = f
        .invoke("collection.get", json!({ "collection_id": col }))
        .await;
    assert_eq!(col_undone["slots"][0]["asset_id"], original_footer_asset);
    assert_eq!(col_undone["slots"][0]["stale"], false);

    // Atomic redo restores stale: true.
    f.invoke("collection.redo", json!({ "collection_id": col }))
        .await;
    let col_redone = f
        .invoke("collection.get", json!({ "collection_id": col }))
        .await;
    assert_eq!(col_redone["slots"][0]["asset_id"], new_rep_asset);
    assert_eq!(col_redone["slots"][0]["stale"], true);

    // collection.set_slot on a stale slot clears stale_steps.
    f.invoke(
        "collection.set_slot",
        json!({ "collection_id": col, "slot_id": "slot-1", "asset": original_footer_asset }),
    )
    .await;
    let col_cleared = f
        .invoke("collection.get", json!({ "collection_id": col }))
        .await;
    assert_eq!(col_cleared["slots"][0]["stale"], false);
}

#[tokio::test]
async fn array_output_step_split_is_boundary_and_rejected_by_recipe_edit() {
    let f = Fixture::new().await;
    let root = f
        .import_text("book.md", "## One\n\nFirst.\n\n## Two\n\nSecond.")
        .await;
    let split = f
        .invoke(
            "text.split",
            json!({ "doc": root, "by": { "kind": "heading", "level": 2 } }),
        )
        .await;
    let piece0 = split["docs"][0]["doc"].as_str().unwrap().to_owned();
    let col = f.create_collection("Pieces", &[&piece0]).await;

    let recipe = f
        .invoke(
            "recipe.get",
            json!({ "collection_id": col, "slot_id": "slot-1" }),
        )
        .await;
    assert_eq!(recipe["root_asset"]["id"], piece0);
    assert_eq!(recipe["steps"].as_array().unwrap().len(), 0);

    // Find text.split's step_id from the asset's origin.
    let store = AssetStore::new(&f.project_dir);
    let piece_record = store
        .get_asset(&AssetId::new(&piece0))
        .await
        .unwrap()
        .unwrap();

    let err = f
        .invoke_err(
            "recipe.edit",
            json!({
                "collection_id": col,
                "slot_id": "slot-1",
                "step_id": piece_record.origin.as_str(),
                "input": {}
            }),
        )
        .await;
    assert_eq!(
        err.code().as_str(),
        "RECIPE/CALLER_ARRAY_OUTPUT_UNSUPPORTED"
    );
}

#[test]
fn agent_policy_allows_recipe_operations() {
    assert!(agent_may_invoke("recipe.get"));
    assert!(agent_may_invoke("recipe.edit"));
}

#[tokio::test]
async fn recipe_edit_top_level_merge_drops_deleted_template_values_and_binding() {
    let f = Fixture::new().await;
    let root = f.import_text("tpl_del.md", "# BoundTitle\n\nBody").await;
    let col = f.create_collection("TplDelete", &[&root]).await;

    f.run_workflow_on_collection(
        "tpl-chain",
        json!([
            { "id": "info", "op": "text.inspect", "input": { "doc": { "$from": "inputs.doc" } } },
            { "id": "tpl", "op": "text.template", "input": {
                "doc": { "$from": "inputs.doc" },
                "values": {
                    "title": { "$from": "steps.info.title" },
                    "tag": "draft"
                },
                "template": "{{title}}|{{tag}}|{{body}}"
            }}
        ]),
        json!({ "$from": "steps.tpl.doc" }),
        &col,
    )
    .await;

    let recipe = f
        .invoke(
            "recipe.get",
            json!({ "collection_id": col, "slot_id": "slot-1" }),
        )
        .await;
    let tpl_step = &recipe["steps"].as_array().unwrap()[1];
    assert!(
        tpl_step["input_sources"].get("/values/title").is_some(),
        "initial template step must bind /values/title"
    );

    // Edit `text.template` replacing `values` with `{}` (deleting both `title` binding and `tag`).
    let edit_out = f
        .invoke(
            "recipe.edit",
            json!({
                "collection_id": col,
                "slot_id": "slot-1",
                "step_id": tpl_step["step_id"],
                "input": {
                    "values": {},
                    "template": "{{body}}"
                }
            }),
        )
        .await;
    let updated_text = f
        .read_asset_text(edit_out["asset_id"].as_str().unwrap())
        .await;
    assert_eq!(updated_text, "# BoundTitle\n\nBody");

    let recipe_after = f
        .invoke(
            "recipe.get",
            json!({ "collection_id": col, "slot_id": "slot-1" }),
        )
        .await;
    let steps_after = recipe_after["steps"].as_array().unwrap();
    assert_eq!(steps_after.len(), 1);
    let tpl_after = &steps_after[0];
    assert_eq!(tpl_after["input"]["values"], json!({}));
    assert!(
        tpl_after["input_sources"].get("/values/title").is_none(),
        "deleted /values/title must drop its input_sources binding"
    );
}
