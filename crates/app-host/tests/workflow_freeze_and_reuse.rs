//! Integration tests for `dynamic_workflow.from_recipe` and exposing saved
//! dynamic workflows as `dynamic_workflow.<id>` operations.

#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use mod_sdk::serde_json::{Value, json};
use mod_sdk::{EventBus, Mod, ModRegistry};
use reimagine_app_host::{
    AssetStore, InvokeRequest, OperationProvider, WorkspaceHost, WorkspaceHostBuilder,
    WorkspaceScope,
};
use reimagine_core::command::{
    CommandActor, CommandActorKind, CommandProvenance, OperationContext,
};
use reimagine_core::dynamic_workflow::{
    DynamicWorkflowDocument, RunDocument, RunState, WorkflowSpec,
};
use reimagine_core::event::Timestamp;
use reimagine_core::health::Health;
use reimagine_core::model::{AssetId, CollectionId, DynamicWorkflowId, ProjectId, RunId, SlotId};
use reimagine_core::project::ProjectMetadata;
use reimagine_core::step::StepInputSource;
use reimagine_core::task::CancelToken;
use tempfile::TempDir;
use text_mod::TextMod;

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
        let registry = Arc::new(ModRegistry::start(vec![text], &event_bus).await);
        assert!(
            registry.list().iter().all(|m| m.health.is_available()),
            "all test mods must start"
        );

        let host = WorkspaceHostBuilder::new(WorkspaceScope::new("freeze-reuse-e2e"), base.path())
            .event_bus(event_bus)
            .mod_registry(registry)
            .build()
            .await
            .expect("build workspace host");

        let project = ProjectId::new("proj-a");
        let now = Timestamp::new("2026-11-14T00:00:00Z");
        host.project_service()
            .create_project(
                project.clone(),
                ProjectMetadata::new("Project A", "", now.clone(), now),
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

    fn agent_context() -> OperationContext {
        OperationContext::new(
            CommandActor::new(CommandActorKind::Agent).with_id("agent-1"),
            CommandProvenance::Direct,
        )
    }

    async fn invoke(&self, name: &str, input: Value) -> Value {
        self.invoke_with_sources(name, input, BTreeMap::new())
            .await
            .output
    }

    async fn invoke_with_sources(
        &self,
        name: &str,
        input: Value,
        input_sources: BTreeMap<String, StepInputSource>,
    ) -> reimagine_app_host::InvokeResult {
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
                    input_sources,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("invoke `{name}` failed: {}", e.message()))
    }

    async fn invoke_err(
        &self,
        name: &str,
        input: Value,
        context: OperationContext,
    ) -> reimagine_core::diagnostic::Diagnostic {
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
            .expect_err("expected operation invocation to fail")
    }

    async fn import_markdown(&self, filename: &str, content: &str) -> AssetId {
        let file_path = self._base.path().join(filename);
        tokio::fs::write(&file_path, content)
            .await
            .expect("write markdown file");
        let out = self
            .invoke("asset.import", json!({ "paths": [file_path] }))
            .await;
        AssetId::new(out["assets"][0].as_str().expect("asset id"))
    }

    async fn read_asset_bytes(&self, asset_id: &AssetId) -> Vec<u8> {
        let store = AssetStore::new(self.project_dir.clone());
        let record = store
            .get_asset(asset_id)
            .await
            .expect("get asset")
            .expect("asset exists");
        tokio::fs::read(store.content_path(&record.hash))
            .await
            .expect("read asset bytes")
    }

    async fn wait_for_run(&self, run_id: &RunId) -> RunDocument {
        for _ in 0..100 {
            let out = self
                .invoke(
                    "dynamic_workflow.get_run",
                    json!({ "run_id": run_id.as_str() }),
                )
                .await;
            let doc: RunDocument = serde_json::from_value(out).expect("parse run");
            if !matches!(doc.state, RunState::Queued | RunState::Running) {
                return doc;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("run `{run_id}` did not settle in time");
    }
}

#[tokio::test]
async fn scenario_t_diamond_dag_freeze_edit_and_byte_identical_reuse() {
    let f = Fixture::new().await;
    let md_content = "#   Hello   World  \r\n\r\nAI is great.\r\n";
    let root_asset = f.import_markdown("draft.md", md_content).await;

    let col_out = f
        .invoke(
            "collection.create",
            json!({
                "name": "Editorial Batch",
                "assets": [root_asset.as_str()]
            }),
        )
        .await;
    let collection_id = CollectionId::new(col_out["collection_id"].as_str().unwrap());
    let slot_id = SlotId::new("slot-1");

    // Step 1: text.normalize
    let s1 = f
        .invoke_with_sources(
            "text.normalize",
            json!({ "doc": root_asset.as_str() }),
            BTreeMap::new(),
        )
        .await;
    let s1_doc = s1.output["doc"].as_str().unwrap().to_owned();

    // Step 2: text.replace consuming s1
    let s2 = f
        .invoke_with_sources(
            "text.replace",
            json!({
                "doc": s1_doc,
                "pairs": [{ "find": "AI", "replace": "A.I.", "mode": "literal" }]
            }),
            BTreeMap::from([(
                "/doc".to_owned(),
                StepInputSource {
                    step_id: s1.step.id.clone(),
                    output_pointer: "/doc".to_owned(),
                },
            )]),
        )
        .await;
    let s2_doc = s2.output["doc"].as_str().unwrap().to_owned();

    // Step 3: text.inspect consuming s2 (diamond side-branch)
    let s3 = f
        .invoke_with_sources(
            "text.inspect",
            json!({ "doc": s2_doc }),
            BTreeMap::from([(
                "/doc".to_owned(),
                StepInputSource {
                    step_id: s2.step.id.clone(),
                    output_pointer: "/doc".to_owned(),
                },
            )]),
        )
        .await;
    let s3_title = s3.output["title"].as_str().unwrap().to_owned();

    // Step 4: text.template consuming s2.doc AND s3.title (diamond merge)
    let s4 = f
        .invoke_with_sources(
            "text.template",
            json!({
                "doc": s2_doc,
                "template": "# {{title}}\n\n{{body}}",
                "values": { "title": s3_title }
            }),
            BTreeMap::from([
                (
                    "/doc".to_owned(),
                    StepInputSource {
                        step_id: s2.step.id.clone(),
                        output_pointer: "/doc".to_owned(),
                    },
                ),
                (
                    "/values/title".to_owned(),
                    StepInputSource {
                        step_id: s3.step.id.clone(),
                        output_pointer: "/title".to_owned(),
                    },
                ),
            ]),
        )
        .await;
    let s4_doc = s4.output["doc"].as_str().unwrap().to_owned();

    // Set slot to s4_doc
    f.invoke(
        "collection.set_slot",
        json!({
            "collection_id": collection_id.as_str(),
            "slot_id": slot_id.as_str(),
            "asset": s4_doc
        }),
    )
    .await;

    // Edit text.replace via recipe.edit
    f.invoke(
        "recipe.edit",
        json!({
            "collection_id": collection_id.as_str(),
            "slot_id": slot_id.as_str(),
            "step_id": s2.step.id.as_str(),
            "input": {
                "pairs": [{ "find": "AI", "replace": "Artificial Intelligence", "mode": "literal" }]
            }
        }),
    )
    .await;

    // Append a copyright step on top of the edited slot
    let current_col = f
        .invoke(
            "collection.get",
            json!({ "collection_id": collection_id.as_str() }),
        )
        .await;
    let edited_asset = current_col["slots"][0]["asset_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let recipe_after_edit = f
        .invoke(
            "recipe.get",
            json!({
                "collection_id": collection_id.as_str(),
                "slot_id": slot_id.as_str()
            }),
        )
        .await;
    let last_step_id = recipe_after_edit["steps"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["step_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let s5 = f
        .invoke_with_sources(
            "text.template",
            json!({
                "doc": edited_asset,
                "template": "{{body}}\n\nCopyright 2026"
            }),
            BTreeMap::from([(
                "/doc".to_owned(),
                StepInputSource {
                    step_id: reimagine_core::model::StepId::new(last_step_id),
                    output_pointer: "/doc".to_owned(),
                },
            )]),
        )
        .await;
    let final_slot_asset = AssetId::new(s5.output["doc"].as_str().unwrap());
    f.invoke(
        "collection.set_slot",
        json!({
            "collection_id": collection_id.as_str(),
            "slot_id": slot_id.as_str(),
            "asset": final_slot_asset.as_str()
        }),
    )
    .await;

    let expected_bytes = f.read_asset_bytes(&final_slot_asset).await;

    // Freeze the slot's recipe
    let frozen = f
        .invoke(
            "dynamic_workflow.from_recipe",
            json!({
                "collection_id": collection_id.as_str(),
                "slot_id": slot_id.as_str(),
                "description": "Normalize, replace, inspect, template, copyright"
            }),
        )
        .await;
    let dwf_id = frozen["dynamic_workflow_id"].as_str().unwrap().to_owned();
    assert_eq!(frozen["name"], "Editorial Batch (slot-1)");

    // Re-import the exact same original markdown file
    let reimported = f.import_markdown("draft_copy.md", md_content).await;

    // 1. Reuse via direct op_invoke("dynamic_workflow.<id>", { "doc": reimported })
    let op_name = format!("dynamic_workflow.{dwf_id}");
    let direct_out = f
        .invoke(&op_name, json!({ "doc": reimported.as_str() }))
        .await;
    let direct_asset = AssetId::new(direct_out["output"].as_str().unwrap());
    assert_eq!(f.read_asset_bytes(&direct_asset).await, expected_bytes);

    // 2. Reuse via dynamic_workflow.run on a new collection
    let col2 = f
        .invoke(
            "collection.create",
            json!({
                "name": "Reuse Batch",
                "assets": [reimported.as_str()]
            }),
        )
        .await;
    let col2_id = col2["collection_id"].as_str().unwrap();
    let run_out = f
        .invoke(
            "dynamic_workflow.run",
            json!({
                "dynamic_workflow_id": dwf_id,
                "target": { "collection_id": col2_id }
            }),
        )
        .await;
    let run_id = RunId::new(run_out["run_id"].as_str().unwrap());
    let run_doc = f.wait_for_run(&run_id).await;
    assert_eq!(run_doc.state, RunState::Completed);

    let col2_get = f
        .invoke("collection.get", json!({ "collection_id": col2_id }))
        .await;
    let col2_asset = AssetId::new(col2_get["slots"][0]["asset_id"].as_str().unwrap());
    assert_eq!(f.read_asset_bytes(&col2_asset).await, expected_bytes);
}

#[tokio::test]
async fn op_list_project_scoping_and_mcp_projection() {
    let f = Fixture::new().await;
    let other_proj = ProjectId::new("proj-b");
    let now = Timestamp::new("2026-11-14T00:00:00Z");
    f.host
        .project_service()
        .create_project(
            other_proj.clone(),
            ProjectMetadata::new("Project B", "", now.clone(), now),
        )
        .await
        .expect("create other project");

    let created = f
        .invoke(
            "dynamic_workflow.create",
            json!({
                "name": "Normalize Doc",
                "description": "Normalizes text",
                "workflow": {
                    "inputs": { "doc": { "type": "string", "format": "asset", "contentMediaType": "text/plain" } },
                    "params": { "type": "object" },
                    "steps": [{
                        "id": "s1",
                        "op": "text.normalize",
                        "input": { "doc": { "$from": "inputs.doc" } }
                    }],
                    "output": { "$from": "steps.s1.doc" }
                }
            }),
        )
        .await;
    let dwf_id = created["dynamic_workflow_id"].as_str().unwrap();
    let expected_op = format!("dynamic_workflow.{dwf_id}");

    let ops_a = f.host.operation_service().list(&f.project).await;
    let op_info = ops_a
        .iter()
        .find(|info| info.spec.name == expected_op)
        .expect("saved workflow must appear in project A op_list");
    assert_eq!(
        op_info.provider,
        OperationProvider::DynamicWorkflow {
            id: dwf_id.to_owned()
        }
    );
    assert_eq!(op_info.health, Health::Available);
    assert_eq!(op_info.spec.description, "Normalizes text");

    // Must NOT appear in project B
    let ops_b = f.host.operation_service().list(&other_proj).await;
    assert!(
        ops_b.iter().all(|info| info.spec.name != expected_op),
        "saved workflow must not leak into another project"
    );

    // Operation name is MCP-projection-safe
    assert!(!op_info.spec.name.contains("__"));
    assert!(op_info.spec.name.replace('.', "__").len() <= 64);
}

#[tokio::test]
async fn nested_workflow_composition_and_recipe_replay() {
    let f = Fixture::new().await;
    let inner = f
        .invoke(
            "dynamic_workflow.create",
            json!({
                "name": "Inner Normalize",
                "workflow": {
                    "inputs": { "doc": { "type": "string", "format": "asset" } },
                    "params": { "type": "object" },
                    "steps": [{
                        "id": "s1",
                        "op": "text.normalize",
                        "input": { "doc": { "$from": "inputs.doc" } }
                    }],
                    "output": { "$from": "steps.s1.doc" }
                }
            }),
        )
        .await;
    let inner_id = inner["dynamic_workflow_id"].as_str().unwrap();
    let inner_op = format!("dynamic_workflow.{inner_id}");

    let outer = f
        .invoke(
            "dynamic_workflow.create",
            json!({
                "name": "Outer Compose",
                "workflow": {
                    "inputs": { "doc": { "type": "string", "format": "asset" } },
                    "params": { "type": "object" },
                    "steps": [
                        {
                            "id": "step1",
                            "op": inner_op,
                            "input": { "doc": { "$from": "inputs.doc" } }
                        },
                        {
                            "id": "step2",
                            "op": "text.template",
                            "input": {
                                "doc": { "$from": "steps.step1.output" },
                                "template": "{{body}} [outer]"
                            }
                        }
                    ],
                    "output": { "$from": "steps.step2.doc" }
                }
            }),
        )
        .await;
    let outer_id = outer["dynamic_workflow_id"].as_str().unwrap();
    let outer_op = format!("dynamic_workflow.{outer_id}");

    let asset = f.import_markdown("nested.md", "Hello   world\r\n").await;

    // 1. Direct op_invoke
    let direct = f.invoke(&outer_op, json!({ "doc": asset.as_str() })).await;
    let direct_asset = AssetId::new(direct["output"].as_str().unwrap());
    assert_eq!(
        String::from_utf8(f.read_asset_bytes(&direct_asset).await).unwrap(),
        "Hello world\n [outer]"
    );

    // 2. Run on a collection + recipe.get + recipe.edit
    let col = f
        .invoke(
            "collection.create",
            json!({ "name": "Nested Col", "assets": [asset.as_str()] }),
        )
        .await;
    let col_id = col["collection_id"].as_str().unwrap();
    let run_out = f
        .invoke(
            "dynamic_workflow.run",
            json!({
                "dynamic_workflow_id": outer_id,
                "target": { "collection_id": col_id }
            }),
        )
        .await;
    let run_doc = f
        .wait_for_run(&RunId::new(run_out["run_id"].as_str().unwrap()))
        .await;
    assert_eq!(run_doc.state, RunState::Completed);

    let recipe = f
        .invoke(
            "recipe.get",
            json!({ "collection_id": col_id, "slot_id": "slot-1" }),
        )
        .await;
    let steps = recipe["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["op"], inner_op);
    assert_eq!(steps[1]["op"], "text.template");

    let template_step_id = steps[1]["step_id"].as_str().unwrap();
    f.invoke(
        "recipe.edit",
        json!({
            "collection_id": col_id,
            "slot_id": "slot-1",
            "step_id": template_step_id,
            "input": { "template": "{{body}} [edited]" }
        }),
    )
    .await;

    let col_after = f
        .invoke("collection.get", json!({ "collection_id": col_id }))
        .await;
    let edited_asset = AssetId::new(col_after["slots"][0]["asset_id"].as_str().unwrap());
    assert_eq!(
        String::from_utf8(f.read_asset_bytes(&edited_asset).await).unwrap(),
        "Hello world\n [edited]"
    );
}

#[tokio::test]
async fn cycle_detection_marks_unavailable_and_rejects_invoke() {
    let f = Fixture::new().await;
    let wf_dir = f.project_dir.join("dynamic_workflows");
    tokio::fs::create_dir_all(&wf_dir).await.unwrap();

    let make_doc = |id: &str, target_op: &str| {
        let spec: WorkflowSpec = serde_json::from_value(json!({
            "inputs": { "doc": { "type": "string", "format": "asset" } },
            "params": { "type": "object" },
            "steps": [{
                "id": "s1",
                "op": target_op,
                "input": { "doc": { "$from": "inputs.doc" } }
            }],
            "output": { "$from": "steps.s1.output" }
        }))
        .unwrap();
        DynamicWorkflowDocument::new(DynamicWorkflowId::new(id), f.project.clone(), id, spec)
    };

    let doc_a = make_doc("dwf-a", "dynamic_workflow.dwf-b");
    let doc_b = make_doc("dwf-b", "dynamic_workflow.dwf-a");
    let doc_self = make_doc("dwf-self", "dynamic_workflow.dwf-self");

    tokio::fs::write(
        wf_dir.join("dwf-a.json"),
        serde_json::to_vec_pretty(&doc_a).unwrap(),
    )
    .await
    .unwrap();
    tokio::fs::write(
        wf_dir.join("dwf-b.json"),
        serde_json::to_vec_pretty(&doc_b).unwrap(),
    )
    .await
    .unwrap();
    tokio::fs::write(
        wf_dir.join("dwf-self.json"),
        serde_json::to_vec_pretty(&doc_self).unwrap(),
    )
    .await
    .unwrap();

    let ops = f.host.operation_service().list(&f.project).await;
    for name in [
        "dynamic_workflow.dwf-a",
        "dynamic_workflow.dwf-b",
        "dynamic_workflow.dwf-self",
    ] {
        let info = ops.iter().find(|i| i.spec.name == name).expect("listed");
        match &info.health {
            Health::Unavailable(diag) => {
                assert_eq!(
                    diag.code().as_str(),
                    "DYNAMIC_WORKFLOW/CALLER_CYCLIC_WORKFLOW"
                );
            }
            Health::Available => panic!("expected `{name}` to be Unavailable due to cycle"),
        }
    }

    let asset = f.import_markdown("cycle.md", "hi").await;
    let err = f
        .invoke_err(
            "dynamic_workflow.dwf-a",
            json!({ "doc": asset.as_str() }),
            Fixture::human_context(),
        )
        .await;
    assert_eq!(
        err.code().as_str(),
        "DYNAMIC_WORKFLOW/CALLER_CYCLIC_WORKFLOW"
    );
}

#[tokio::test]
async fn preview_dynamic_workflow_writes_no_steps_or_assets() {
    let f = Fixture::new().await;
    let asset = f.import_markdown("prev.md", "Hello   world\r\n").await;

    let created = f
        .invoke(
            "dynamic_workflow.create",
            json!({
                "name": "Previewable",
                "workflow": {
                    "inputs": { "doc": { "type": "string", "format": "asset" } },
                    "params": { "type": "object" },
                    "steps": [
                        {
                            "id": "s1",
                            "op": "text.normalize",
                            "input": { "doc": { "$from": "inputs.doc" } }
                        },
                        {
                            "id": "s2",
                            "op": "text.template",
                            "input": {
                                "doc": { "$from": "steps.s1.doc" },
                                "template": "{{body}}!"
                            }
                        }
                    ],
                    "output": { "$from": "steps.s2.doc" }
                }
            }),
        )
        .await;
    let dwf_id = created["dynamic_workflow_id"].as_str().unwrap();
    let op_name = format!("dynamic_workflow.{dwf_id}");

    let steps_before = tokio::fs::read_to_string(f.project_dir.join("steps.jsonl"))
        .await
        .unwrap_or_default();
    let assets_before = f.invoke("asset.list", json!({})).await["assets"]
        .as_array()
        .unwrap()
        .len();

    let preview_out = f
        .host
        .operation_service()
        .preview(
            &f.project,
            &op_name,
            json!({ "doc": asset.as_str() }),
            "prev-key-1",
            CancelToken::new(),
        )
        .await
        .expect("preview should succeed");

    let uri = preview_out["output"].as_str().unwrap();
    assert!(
        uri.starts_with("reimagine-asset://preview/prev-key-1/"),
        "unexpected preview URI: {uri}"
    );
    let rel = uri
        .strip_prefix("reimagine-asset://preview/prev-key-1/")
        .unwrap();
    let file_path = f
        .host
        .operation_service()
        .resolve_preview_file("prev-key-1", rel)
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(file_path).await.unwrap(),
        "Hello world\n!"
    );

    let steps_after = tokio::fs::read_to_string(f.project_dir.join("steps.jsonl"))
        .await
        .unwrap_or_default();
    let assets_after = f.invoke("asset.list", json!({})).await["assets"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(steps_after, steps_before);
    assert_eq!(assets_after, assets_before);
}

#[tokio::test]
async fn edge_cases_stale_empty_save_false_and_agent_policy() {
    let f = Fixture::new().await;
    let asset = f.import_markdown("edge.md", "hello").await;
    let col = f
        .invoke(
            "collection.create",
            json!({ "name": "Edge Col", "assets": [asset.as_str()] }),
        )
        .await;
    let col_id = col["collection_id"].as_str().unwrap();

    // 1. Untouched 0-step slot -> EMPTY_RECIPE
    let err_empty = f
        .invoke_err(
            "dynamic_workflow.from_recipe",
            json!({ "collection_id": col_id, "slot_id": "slot-1" }),
            Fixture::human_context(),
        )
        .await;
    assert_eq!(
        err_empty.code().as_str(),
        "DYNAMIC_WORKFLOW/CALLER_EMPTY_RECIPE"
    );

    // 2. save: false returns workflow without persisting
    let s1 = f
        .invoke_with_sources(
            "text.normalize",
            json!({ "doc": asset.as_str() }),
            BTreeMap::new(),
        )
        .await;
    let s1_doc = s1.output["doc"].as_str().unwrap();
    f.invoke(
        "collection.set_slot",
        json!({ "collection_id": col_id, "slot_id": "slot-1", "asset": s1_doc }),
    )
    .await;

    let count_before = f.invoke("dynamic_workflow.list", json!({})).await["workflows"]
        .as_array()
        .unwrap()
        .len();
    let unsaved = f
        .invoke(
            "dynamic_workflow.from_recipe",
            json!({ "collection_id": col_id, "slot_id": "slot-1", "save": false }),
        )
        .await;
    assert!(unsaved.get("dynamic_workflow_id").is_none());
    assert!(unsaved.get("workflow").is_some());
    let count_after = f.invoke("dynamic_workflow.list", json!({})).await["workflows"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(count_before, count_after);

    // 3. Stale slot -> STALE_RECIPE
    f.host
        .collection_service()
        .update_slot_if_unchanged(
            &f.project,
            &CollectionId::new(col_id),
            &Fixture::human_context(),
            SlotId::new("slot-1"),
            &AssetId::new(s1_doc),
            &[],
            AssetId::new(s1_doc),
            vec![reimagine_core::collection::StaleStep {
                step_id: s1.step.id.clone(),
                op: "text.normalize".to_owned(),
                op_version: "0.1.0".to_owned(),
                input: json!({ "doc": asset.as_str() }),
                input_sources: BTreeMap::new(),
                actor: Fixture::human_context().actor().clone(),
            }],
        )
        .await
        .unwrap();
    let err_stale = f
        .invoke_err(
            "dynamic_workflow.from_recipe",
            json!({ "collection_id": col_id, "slot_id": "slot-1" }),
            Fixture::human_context(),
        )
        .await;
    assert_eq!(
        err_stale.code().as_str(),
        "DYNAMIC_WORKFLOW/CALLER_STALE_RECIPE"
    );

    // 4. Agent invoking workflow containing library.purge -> NOT_PERMITTED_TO_AGENT before step 1 runs
    let bad_wf = f
        .invoke(
            "dynamic_workflow.create",
            json!({
                "name": "Agent Denied",
                "workflow": {
                    "inputs": {},
                    "params": { "type": "object" },
                    "steps": [{
                        "id": "s1",
                        "op": "library.purge",
                        "input": {}
                    }],
                    "output": { "$from": "steps.s1.purged" }
                }
            }),
        )
        .await;
    let bad_op = format!(
        "dynamic_workflow.{}",
        bad_wf["dynamic_workflow_id"].as_str().unwrap()
    );
    let ops = f.host.operation_service().list(&f.project).await;
    let bad_info = ops
        .iter()
        .find(|info| info.spec.name == bad_op)
        .expect("bad workflow listed");
    assert!(!bad_info.agent_permitted);

    let err_agent = f
        .invoke_err(&bad_op, json!({}), Fixture::agent_context())
        .await;
    assert_eq!(
        err_agent.code().as_str(),
        "DYNAMIC_WORKFLOW/CALLER_NOT_PERMITTED_TO_AGENT"
    );
}
