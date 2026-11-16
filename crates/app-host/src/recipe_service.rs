#![allow(clippy::result_large_err)]
//! Per-slot recipe inspection (`recipe.get`) and downstream replay (`recipe.edit`).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use mera_config::AppPaths;
use mera_core::asset::AssetRecord;
use mera_core::collection::StaleStep;
use mera_core::command::{CommandActor, OperationContext};
use mera_core::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
    DiagnosticTargetDomain,
};
use mera_core::model::{AssetId, CollectionId, DiagnosticId, ProjectId, SlotId, StepId};
use mera_core::step::{StepInputSource, StepOutcome, StepRecord};
use mera_core::task::CancelToken;
use mod_sdk::OperationSpec;
use mod_sdk::schema::{find_asset_fields, has_array_asset_field};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::asset_store::AssetStore;
use crate::collection_service::{CollectionService, UpdateSlotError};
use crate::error::AppHostError;
use crate::operation_service::{InvokeRequest, OperationService};
use crate::step_log::StepLog;

fn recipe_caller_error(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(
        DiagnosticId::new(format!("op:recipe:{code}")),
        DiagnosticCode::new(format!("RECIPE/CALLER_{code}")),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("recipe_service"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new("op.recipe")),
    )
}

fn recipe_internal_error(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(
        DiagnosticId::new(format!("op:recipe:{code}")),
        DiagnosticCode::new(format!("RECIPE/INTERNAL_{code}")),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("recipe_service"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new("op.recipe")),
    )
}

/// Asset summary returned in `recipe.get`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipeAssetInfo {
    pub id: AssetId,
    pub media_type: String,
    pub size: u64,
}

impl From<&AssetRecord> for RecipeAssetInfo {
    fn from(record: &AssetRecord) -> Self {
        Self {
            id: record.id.clone(),
            media_type: record.media_type.clone(),
            size: record.size,
        }
    }
}

/// One step in a slot's reconstructed recipe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecipeStep {
    pub step_id: StepId,
    pub op: String,
    pub op_version: String,
    pub input: Value,
    pub input_sources: BTreeMap<String, StepInputSource>,
    pub output: Value,
    pub actor: CommandActor,
    pub read_only: bool,
    pub stale: bool,
}

/// Result of `recipe.get`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecipeSnapshot {
    pub collection_id: CollectionId,
    pub slot_id: SlotId,
    pub root_asset: RecipeAssetInfo,
    pub current_asset: RecipeAssetInfo,
    pub stale: bool,
    pub steps: Vec<RecipeStep>,
}

/// Result of `recipe.edit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipeEditOutcome {
    pub version: u64,
    pub asset_id: AssetId,
    pub stale: bool,
    pub replayed_steps: usize,
    pub stale_steps: usize,
}

struct LoadedRecipe {
    snapshot: RecipeSnapshot,
    root_origin: StepId,
    expected_asset: AssetId,
    expected_stale_steps: Vec<StaleStep>,
    op_specs: HashMap<String, OperationSpec>,
    step_by_id: HashMap<StepId, (usize, StepRecord)>,
}

/// Reconstructs per-slot operation recipes and replays downstream steps on edit.
pub struct RecipeService {
    paths: AppPaths,
    collections: Arc<CollectionService>,
    operations: Weak<OperationService>,
}

impl RecipeService {
    pub fn new(
        paths: AppPaths,
        collections: Arc<CollectionService>,
        operations: Weak<OperationService>,
    ) -> Self {
        Self {
            paths,
            collections,
            operations,
        }
    }

    fn project_dir(&self, project_id: &ProjectId) -> PathBuf {
        self.paths.project_dir(project_id.as_str())
    }

    fn operation_service(&self) -> Result<Arc<OperationService>, Diagnostic> {
        self.operations.upgrade().ok_or_else(|| {
            recipe_internal_error("SERVICE_GONE", "operation service is no longer available")
        })
    }

    /// Reconstruct the recipe for `(collection_id, slot_id)`.
    pub async fn get_recipe(
        &self,
        project_id: &ProjectId,
        collection_id: &CollectionId,
        slot_id: &SlotId,
    ) -> Result<RecipeSnapshot, Diagnostic> {
        let loaded = self.load_recipe(project_id, collection_id, slot_id).await?;
        Ok(loaded.snapshot)
    }

    /// Edit `target_step_id` with `caller_input`, replay deterministic downstream
    /// steps, truncate at any non-deterministic or open-world step, and
    /// atomically update the slot.
    #[allow(clippy::too_many_arguments)]
    pub async fn edit_recipe(
        &self,
        project_id: &ProjectId,
        collection_id: &CollectionId,
        slot_id: &SlotId,
        target_step_id: &StepId,
        caller_input: &Value,
        context: &OperationContext,
        cancel: &CancelToken,
    ) -> Result<RecipeEditOutcome, Diagnostic> {
        let caller_obj = caller_input.as_object().ok_or_else(|| {
            recipe_caller_error("INVALID_INPUT", "recipe.edit 'input' must be an object")
        })?;

        let loaded = self.load_recipe(project_id, collection_id, slot_id).await?;

        // Check array-output rejection before STEP_NOT_IN_RECIPE so targeting
        // the boundary split step returns RECIPE/CALLER_ARRAY_OUTPUT_UNSUPPORTED.
        if *target_step_id == loaded.root_origin
            && let Some((_, origin_record)) = loaded.step_by_id.get(target_step_id)
            && let Some(spec) = loaded.op_specs.get(&origin_record.op)
            && has_array_asset_field(&spec.output_schema)
        {
            return Err(recipe_caller_error(
                "ARRAY_OUTPUT_UNSUPPORTED",
                format!(
                    "step `{}` (`{}`) produces an array of assets and cannot be edited via recipe.edit",
                    target_step_id, origin_record.op
                ),
            ));
        }
        if let Some((_, target_record)) = loaded.step_by_id.get(target_step_id)
            && let Some(spec) = loaded.op_specs.get(&target_record.op)
            && has_array_asset_field(&spec.output_schema)
        {
            return Err(recipe_caller_error(
                "ARRAY_OUTPUT_UNSUPPORTED",
                format!(
                    "step `{}` (`{}`) produces an array of assets and cannot be edited via recipe.edit",
                    target_step_id, target_record.op
                ),
            ));
        }

        let Some(target_idx) = loaded
            .snapshot
            .steps
            .iter()
            .position(|s| &s.step_id == target_step_id)
        else {
            return Err(recipe_caller_error(
                "STEP_NOT_IN_RECIPE",
                format!(
                    "step `{}` is not in the recipe for slot `{}`",
                    target_step_id, slot_id
                ),
            ));
        };

        let target_step = &loaded.snapshot.steps[target_idx];
        if target_step.read_only {
            return Err(recipe_caller_error(
                "READ_ONLY_STEP",
                format!(
                    "step `{}` (`{}`) is read-only and cannot be edited",
                    target_step_id, target_step.op
                ),
            ));
        }

        let stale_step_ids: HashSet<StepId> = loaded
            .snapshot
            .steps
            .iter()
            .filter(|s| s.stale)
            .map(|s| s.step_id.clone())
            .collect();
        if has_stale_dependency(target_step, &loaded.snapshot.steps, &stale_step_ids) {
            return Err(recipe_caller_error(
                "UPSTREAM_STALE",
                format!(
                    "step `{}` depends on an earlier stale step that must be re-run first",
                    target_step_id
                ),
            ));
        }

        let operations = self.operation_service()?;

        // Seed step_outputs with all active steps before target_idx so both
        // target_idx and downstream steps can resolve upstream references.
        let mut step_outputs: HashMap<StepId, Value> = HashMap::new();
        for step in &loaded.snapshot.steps[..target_idx] {
            if !step.stale {
                step_outputs.insert(step.step_id.clone(), step.output.clone());
            }
        }

        // Merge caller input onto target_step.input at top-level properties so
        // replacing an object field (such as `values`) drops deleted subkeys.
        let mut merged_input = target_step.input.clone();
        if let Some(base_map) = merged_input.as_object_mut() {
            for (k, v) in caller_obj {
                base_map.insert(k.clone(), v.clone());
            }
        } else {
            merged_input = Value::Object(caller_obj.clone());
        }

        let asset_pointers: HashSet<String> = loaded
            .op_specs
            .get(&target_step.op)
            .map(|spec| {
                find_asset_fields(&spec.input_schema, &merged_input)
                    .into_iter()
                    .map(|f| f.pointer)
                    .collect()
            })
            .unwrap_or_default();

        let mut new_target_sources = BTreeMap::new();
        for (ptr, src) in &target_step.input_sources {
            let upstream_val = step_outputs
                .get(&src.step_id)
                .and_then(|out| out.pointer(&src.output_pointer));
            if asset_pointers.contains(ptr) {
                if let Some(val) = upstream_val {
                    set_json_pointer(&mut merged_input, ptr, val.clone());
                }
                new_target_sources.insert(ptr.clone(), src.clone());
            } else {
                let top_key_provided =
                    top_level_pointer_key(ptr).is_some_and(|key| caller_obj.contains_key(&key));
                if !top_key_provided {
                    if let Some(val) = upstream_val {
                        set_json_pointer(&mut merged_input, ptr, val.clone());
                    }
                    new_target_sources.insert(ptr.clone(), src.clone());
                } else if let (Some(merged_val), Some(up_val)) =
                    (merged_input.pointer(ptr), upstream_val)
                    && merged_val == up_val
                {
                    new_target_sources.insert(ptr.clone(), src.clone());
                }
            }
        }

        let target_result = operations
            .invoke(
                project_id,
                InvokeRequest {
                    name: target_step.op.clone(),
                    input: merged_input,
                    context: context.clone(),
                    cancel: cancel.clone(),
                    run: None,
                    input_sources: new_target_sources,
                },
            )
            .await?;

        let mut step_id_map: HashMap<StepId, StepId> = HashMap::new();
        step_id_map.insert(target_step.step_id.clone(), target_result.step.id.clone());
        step_outputs.insert(target_result.step.id.clone(), target_result.output.clone());

        let mut replayed_steps: usize = 1;
        let mut dirty_steps: HashSet<StepId> = HashSet::new();
        dirty_steps.insert(target_step.step_id.clone());
        let mut became_stale: HashSet<StepId> = HashSet::new();
        let mut new_stale_steps: Vec<StaleStep> = Vec::new();

        let mut last_single_asset = loaded
            .op_specs
            .get(&target_step.op)
            .and_then(|spec| extract_single_asset_output(spec, &target_result.output));

        for step in &loaded.snapshot.steps[(target_idx + 1)..] {
            let depends_on_dirty = step
                .input_sources
                .values()
                .any(|src| dirty_steps.contains(&src.step_id));

            if !depends_on_dirty && !step.stale {
                step_outputs.insert(step.step_id.clone(), step.output.clone());
                if let Some(spec) = loaded.op_specs.get(&step.op)
                    && let Some(asset) = extract_single_asset_output(spec, &step.output)
                {
                    last_single_asset = Some(asset);
                }
                continue;
            }

            let remapped_sources: BTreeMap<String, StepInputSource> = step
                .input_sources
                .iter()
                .map(|(ptr, src)| {
                    let mapped_id = step_id_map
                        .get(&src.step_id)
                        .cloned()
                        .unwrap_or_else(|| src.step_id.clone());
                    (
                        ptr.clone(),
                        StepInputSource {
                            step_id: mapped_id,
                            output_pointer: src.output_pointer.clone(),
                        },
                    )
                })
                .collect();

            let upstream_is_stale = step
                .input_sources
                .values()
                .any(|src| became_stale.contains(&src.step_id));

            let is_auto_replayable = loaded
                .op_specs
                .get(&step.op)
                .is_some_and(|spec| spec.annotations.deterministic && !spec.annotations.open_world);

            if upstream_is_stale || !is_auto_replayable {
                became_stale.insert(step.step_id.clone());
                dirty_steps.insert(step.step_id.clone());
                new_stale_steps.push(StaleStep {
                    step_id: step.step_id.clone(),
                    op: step.op.clone(),
                    op_version: step.op_version.clone(),
                    input: step.input.clone(),
                    input_sources: remapped_sources,
                    actor: step.actor.clone(),
                });
                continue;
            }

            let mut step_input = step.input.clone();
            for (ptr, src) in &remapped_sources {
                if let Some(upstream_out) = step_outputs.get(&src.step_id)
                    && let Some(val) = upstream_out.pointer(&src.output_pointer)
                {
                    set_json_pointer(&mut step_input, ptr, val.clone());
                }
            }

            let result = operations
                .invoke(
                    project_id,
                    InvokeRequest {
                        name: step.op.clone(),
                        input: step_input,
                        context: context.clone(),
                        cancel: cancel.clone(),
                        run: None,
                        input_sources: remapped_sources,
                    },
                )
                .await?;

            step_id_map.insert(step.step_id.clone(), result.step.id.clone());
            step_outputs.insert(result.step.id.clone(), result.output.clone());
            dirty_steps.insert(step.step_id.clone());
            replayed_steps += 1;

            if let Some(spec) = loaded.op_specs.get(&step.op)
                && let Some(asset) = extract_single_asset_output(spec, &result.output)
            {
                last_single_asset = Some(asset);
            }
        }

        let new_asset_id = last_single_asset.unwrap_or_else(|| loaded.expected_asset.clone());
        let stale = !new_stale_steps.is_empty();
        let stale_count = new_stale_steps.len();

        let cmd_result = self
            .collections
            .update_slot_if_unchanged(
                project_id,
                collection_id,
                context,
                slot_id.clone(),
                &loaded.expected_asset,
                &loaded.expected_stale_steps,
                new_asset_id.clone(),
                new_stale_steps,
            )
            .await
            .map_err(|err| match err {
                UpdateSlotError::UnknownSlot(id) => {
                    recipe_caller_error("UNKNOWN_SLOT", format!("slot `{id}` not found"))
                }
                UpdateSlotError::Conflict(msg) => recipe_caller_error("CONFLICT", msg),
                UpdateSlotError::Host(AppHostError::UnknownCollection { .. }) => {
                    recipe_caller_error(
                        "UNKNOWN_COLLECTION",
                        format!("collection `{collection_id}` not found"),
                    )
                }
                UpdateSlotError::Host(other) => {
                    recipe_internal_error("COLLECTION_UPDATE_FAILED", other.to_string())
                }
            })?;

        Ok(RecipeEditOutcome {
            version: cmd_result.version().get(),
            asset_id: new_asset_id,
            stale,
            replayed_steps,
            stale_steps: stale_count,
        })
    }

    async fn load_recipe(
        &self,
        project_id: &ProjectId,
        collection_id: &CollectionId,
        slot_id: &SlotId,
    ) -> Result<LoadedRecipe, Diagnostic> {
        let collection = self
            .collections
            .snapshot(project_id, collection_id)
            .await
            .map_err(|err| match err {
                AppHostError::UnknownCollection { .. } => recipe_caller_error(
                    "UNKNOWN_COLLECTION",
                    format!("collection `{collection_id}` not found"),
                ),
                other => recipe_internal_error("COLLECTION_SNAPSHOT_FAILED", other.to_string()),
            })?;

        let slot = collection
            .slot(slot_id)
            .ok_or_else(|| {
                recipe_caller_error(
                    "UNKNOWN_SLOT",
                    format!(
                        "slot `{}` not found in collection `{}`",
                        slot_id, collection_id
                    ),
                )
            })?
            .clone();

        let project_dir = self.project_dir(project_id);
        let asset_store = AssetStore::new(project_dir.clone());
        let step_log = StepLog::new(project_dir);

        let records = step_log.records().await?;
        let mut step_by_id: HashMap<StepId, (usize, StepRecord)> = HashMap::new();
        for (idx, record) in records.into_iter().enumerate() {
            step_by_id.insert(record.id.clone(), (idx, record));
        }

        let operations = self.operation_service()?;
        let op_specs: HashMap<String, OperationSpec> = operations
            .list(project_id)
            .await
            .into_iter()
            .map(|info| (info.spec.name.clone(), info.spec))
            .collect();

        let current_asset_record = asset_store.get_asset(&slot.asset).await?.ok_or_else(|| {
            recipe_internal_error(
                "MISSING_ASSET",
                format!("slot asset `{}` not found in asset store", slot.asset),
            )
        })?;

        let is_boundary = |step_id: &StepId| -> bool {
            let Some((_, record)) = step_by_id.get(step_id) else {
                return true;
            };
            if !matches!(record.outcome, StepOutcome::Succeeded { .. }) {
                return true;
            }
            if record.op == "asset.import" {
                return true;
            }
            if let Some(spec) = op_specs.get(&record.op)
                && has_array_asset_field(&spec.output_schema)
            {
                return true;
            }
            false
        };

        let mut active_ids: HashSet<StepId> = HashSet::new();
        let mut effective_sources: HashMap<StepId, BTreeMap<String, StepInputSource>> =
            HashMap::new();
        let mut root_asset_record: Option<AssetRecord> = None;

        if is_boundary(&current_asset_record.origin) {
            root_asset_record = Some(current_asset_record.clone());
        } else {
            let mut queue: VecDeque<StepId> = VecDeque::new();
            queue.push_back(current_asset_record.origin.clone());
            active_ids.insert(current_asset_record.origin.clone());

            while let Some(step_id) = queue.pop_front() {
                let Some((_, record)) = step_by_id.get(&step_id) else {
                    continue;
                };
                let mut sources = record.input_sources.clone();

                for src in record.input_sources.values() {
                    if !is_boundary(&src.step_id) && active_ids.insert(src.step_id.clone()) {
                        queue.push_back(src.step_id.clone());
                    }
                }

                if let Some(spec) = op_specs.get(&record.op) {
                    for field in find_asset_fields(&spec.input_schema, &record.input) {
                        let Some(asset_id_str) =
                            record.input.pointer(&field.pointer).and_then(Value::as_str)
                        else {
                            continue;
                        };
                        let Ok(asset_id) = AssetId::try_new(asset_id_str) else {
                            continue;
                        };
                        let Some(upstream_asset) = asset_store.get_asset(&asset_id).await? else {
                            continue;
                        };

                        if is_boundary(&upstream_asset.origin) {
                            if root_asset_record.is_none() {
                                root_asset_record = Some(upstream_asset);
                            }
                            continue;
                        }

                        if !sources.contains_key(&field.pointer)
                            && let Some((_, upstream_rec)) = step_by_id.get(&upstream_asset.origin)
                            && let StepOutcome::Succeeded { output } = &upstream_rec.outcome
                            && let Some(output_pointer) = find_asset_output_pointer(
                                op_specs.get(&upstream_rec.op),
                                output,
                                &asset_id,
                            )
                        {
                            sources.insert(
                                field.pointer.clone(),
                                StepInputSource {
                                    step_id: upstream_rec.id.clone(),
                                    output_pointer,
                                },
                            );
                        }

                        if active_ids.insert(upstream_asset.origin.clone()) {
                            queue.push_back(upstream_asset.origin.clone());
                        }
                    }
                }

                effective_sources.insert(step_id, sources);
            }
        }

        let root_asset_record = root_asset_record.unwrap_or_else(|| current_asset_record.clone());
        let ordered_active_ids = topo_sort_steps(&active_ids, &effective_sources, &step_by_id);

        let mut steps: Vec<RecipeStep> =
            Vec::with_capacity(ordered_active_ids.len() + slot.stale_steps.len());

        for step_id in ordered_active_ids {
            let Some((_, record)) = step_by_id.get(&step_id) else {
                continue;
            };
            let output = match &record.outcome {
                StepOutcome::Succeeded { output } => output.clone(),
                _ => Value::Null,
            };
            let read_only = op_specs
                .get(&record.op)
                .is_some_and(|spec| spec.annotations.read_only);
            let input_sources = effective_sources.remove(&step_id).unwrap_or_default();
            steps.push(RecipeStep {
                step_id: record.id.clone(),
                op: record.op.clone(),
                op_version: record.op_version.clone(),
                input: record.input.clone(),
                input_sources,
                output,
                actor: record.context.actor().clone(),
                read_only,
                stale: false,
            });
        }

        for stale in &slot.stale_steps {
            let read_only = op_specs
                .get(&stale.op)
                .is_some_and(|spec| spec.annotations.read_only);
            steps.push(RecipeStep {
                step_id: stale.step_id.clone(),
                op: stale.op.clone(),
                op_version: stale.op_version.clone(),
                input: stale.input.clone(),
                input_sources: stale.input_sources.clone(),
                output: Value::Null,
                actor: stale.actor.clone(),
                read_only,
                stale: true,
            });
        }

        let snapshot = RecipeSnapshot {
            collection_id: collection_id.clone(),
            slot_id: slot_id.clone(),
            root_asset: RecipeAssetInfo::from(&root_asset_record),
            current_asset: RecipeAssetInfo::from(&current_asset_record),
            stale: !slot.stale_steps.is_empty(),
            steps,
        };

        Ok(LoadedRecipe {
            snapshot,
            root_origin: root_asset_record.origin,
            expected_asset: slot.asset,
            expected_stale_steps: slot.stale_steps,
            op_specs,
            step_by_id,
        })
    }
}

fn has_stale_dependency(
    target: &RecipeStep,
    all_steps: &[RecipeStep],
    stale_ids: &HashSet<StepId>,
) -> bool {
    let by_id: HashMap<&StepId, &RecipeStep> = all_steps.iter().map(|s| (&s.step_id, s)).collect();
    let mut visited: HashSet<StepId> = HashSet::new();
    let mut queue: VecDeque<StepId> = target
        .input_sources
        .values()
        .map(|s| s.step_id.clone())
        .collect();
    while let Some(id) = queue.pop_front() {
        if stale_ids.contains(&id) {
            return true;
        }
        if visited.insert(id.clone())
            && let Some(step) = by_id.get(&id)
        {
            for src in step.input_sources.values() {
                queue.push_back(src.step_id.clone());
            }
        }
    }
    false
}

fn topo_sort_steps(
    active_ids: &HashSet<StepId>,
    effective_sources: &HashMap<StepId, BTreeMap<String, StepInputSource>>,
    step_by_id: &HashMap<StepId, (usize, StepRecord)>,
) -> Vec<StepId> {
    let mut in_degree: HashMap<StepId, usize> = HashMap::new();
    let mut dependents: HashMap<StepId, Vec<StepId>> = HashMap::new();

    for id in active_ids {
        in_degree.insert(id.clone(), 0);
    }

    for id in active_ids {
        let Some(sources) = effective_sources.get(id) else {
            continue;
        };
        let unique_parents: HashSet<&StepId> = sources
            .values()
            .map(|s| &s.step_id)
            .filter(|parent| active_ids.contains(*parent))
            .collect();
        for parent in unique_parents {
            *in_degree.entry(id.clone()).or_default() += 1;
            dependents
                .entry(parent.clone())
                .or_default()
                .push(id.clone());
        }
    }

    let order_key = |id: &StepId| -> (usize, String) {
        let idx = step_by_id.get(id).map(|(i, _)| *i).unwrap_or(usize::MAX);
        (idx, id.as_str().to_owned())
    };

    let mut ready: BinaryHeap<Reverse<(usize, String, StepId)>> = BinaryHeap::new();
    for (id, deg) in &in_degree {
        if *deg == 0 {
            let (idx, name) = order_key(id);
            ready.push(Reverse((idx, name, id.clone())));
        }
    }

    let mut ordered = Vec::with_capacity(active_ids.len());
    while let Some(Reverse((_, _, id))) = ready.pop() {
        ordered.push(id.clone());
        if let Some(children) = dependents.get(&id) {
            for child in children {
                if let Some(deg) = in_degree.get_mut(child) {
                    *deg -= 1;
                    if *deg == 0 {
                        let (idx, name) = order_key(child);
                        ready.push(Reverse((idx, name, child.clone())));
                    }
                }
            }
        }
    }
    ordered
}

fn find_asset_output_pointer(
    spec: Option<&OperationSpec>,
    output: &Value,
    asset_id: &AssetId,
) -> Option<String> {
    if let Some(spec) = spec {
        for field in find_asset_fields(&spec.output_schema, output) {
            if output.pointer(&field.pointer).and_then(Value::as_str) == Some(asset_id.as_str()) {
                return Some(field.pointer);
            }
        }
    }
    find_string_pointer(output, "", asset_id.as_str())
}

fn find_string_pointer(value: &Value, pointer: &str, needle: &str) -> Option<String> {
    match value {
        Value::String(s) if s == needle => Some(pointer.to_owned()),
        Value::Object(map) => {
            for (k, v) in map {
                let next = format!("{pointer}/{}", escape_pointer_segment(k));
                if let Some(found) = find_string_pointer(v, &next, needle) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                let next = format!("{pointer}/{i}");
                if let Some(found) = find_string_pointer(v, &next, needle) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_single_asset_output(spec: &OperationSpec, output: &Value) -> Option<AssetId> {
    if spec.annotations.read_only || has_array_asset_field(&spec.output_schema) {
        return None;
    }
    let fields = find_asset_fields(&spec.output_schema, output);
    if fields.len() != 1 {
        return None;
    }
    let raw = output.pointer(&fields[0].pointer).and_then(Value::as_str)?;
    AssetId::try_new(raw).ok()
}

fn top_level_pointer_key(pointer: &str) -> Option<String> {
    let rest = pointer.strip_prefix('/')?;
    let first = rest.split('/').next()?;
    Some(unescape_pointer_segment(first))
}

fn escape_pointer_segment(seg: &str) -> String {
    seg.replace('~', "~0").replace('/', "~1")
}

fn unescape_pointer_segment(seg: &str) -> String {
    seg.replace("~1", "/").replace("~0", "~")
}

fn set_json_pointer(root: &mut Value, pointer: &str, new_val: Value) {
    if pointer.is_empty() {
        *root = new_val;
        return;
    }
    let Some(rest) = pointer.strip_prefix('/') else {
        return;
    };
    let segments: Vec<String> = rest.split('/').map(unescape_pointer_segment).collect();
    set_segments(root, &segments, new_val);
}

fn set_segments(current: &mut Value, segments: &[String], new_val: Value) {
    let Some((head, tail)) = segments.split_first() else {
        *current = new_val;
        return;
    };
    if tail.is_empty() {
        match current {
            Value::Object(map) => {
                map.insert(head.clone(), new_val);
            }
            Value::Array(arr) => {
                if let Ok(idx) = head.parse::<usize>()
                    && idx < arr.len()
                {
                    arr[idx] = new_val;
                }
            }
            _ => {
                let mut map = Map::new();
                map.insert(head.clone(), new_val);
                *current = Value::Object(map);
            }
        }
        return;
    }

    match current {
        Value::Object(map) => {
            let entry = map
                .entry(head.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            set_segments(entry, tail, new_val);
        }
        Value::Array(arr) => {
            if let Ok(idx) = head.parse::<usize>()
                && idx < arr.len()
            {
                set_segments(&mut arr[idx], tail, new_val);
            }
        }
        _ => {}
    }
}

/// Format a [`RecipeSnapshot`] as the `recipe.get` JSON payload.
pub fn recipe_snapshot_to_json(snapshot: &RecipeSnapshot) -> Value {
    json!({
        "collection_id": snapshot.collection_id.as_str(),
        "slot_id": snapshot.slot_id.as_str(),
        "root_asset": {
            "id": snapshot.root_asset.id.as_str(),
            "media_type": snapshot.root_asset.media_type,
            "size": snapshot.root_asset.size,
        },
        "current_asset": {
            "id": snapshot.current_asset.id.as_str(),
            "media_type": snapshot.current_asset.media_type,
            "size": snapshot.current_asset.size,
        },
        "stale": snapshot.stale,
        "steps": snapshot.steps.iter().map(|s| json!({
            "step_id": s.step_id.as_str(),
            "op": s.op,
            "op_version": s.op_version,
            "input": s.input,
            "input_sources": s.input_sources,
            "output": s.output,
            "actor": s.actor,
            "read_only": s.read_only,
            "stale": s.stale,
        })).collect::<Vec<_>>(),
    })
}
