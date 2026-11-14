//! Saved dynamic workflows exposed as `dynamic_workflow.<id>` operations.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mod_sdk::schema::{
    check_asset_marker_locations, check_host_path_marker_locations,
    check_inline_text_marker_locations, find_asset_fields, has_asset_field,
};
use mod_sdk::{Invocation, OperationAnnotations, OperationSpec};
use reimagine_core::command::{
    CommandActor, CommandActorKind, CommandProvenance, OperationContext,
};
use reimagine_core::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
    DiagnosticTargetDomain,
};
use reimagine_core::dynamic_workflow::{
    Binding, BindingScope, DynamicWorkflowDocument, resolve_value,
};
use reimagine_core::health::Health;
use reimagine_core::model::{AssetId, DiagnosticId, DynamicWorkflowId, ProjectId, StepId};
use reimagine_core::step::StepInputSource;
use reimagine_core::task::CancelToken;
use serde_json::{Map, Value, json};

use crate::asset_store::matches_media_type_pattern;
use crate::operation_policy::agent_may_invoke;

use super::{
    FoundOperation, InvokeRequest, KernelCall, KernelOp, MAX_INLINE_TEXT_BYTES, OperationInfo,
    OperationProvider, OperationService, inline_text_bytes, op_caller_error, op_internal_error,
    set_json_pointer, validate_relative_path,
};

#[derive(Clone)]
pub(super) struct ResolvedDynamicWorkflow {
    pub(super) document: DynamicWorkflowDocument,
    pub(super) spec: OperationSpec,
    pub(super) health: Health,
    pub(super) changes_documents: bool,
    pub(super) transitive_ops: Vec<String>,
}

struct BaseOpInfo {
    spec: OperationSpec,
    health: Health,
    changes_documents: bool,
}

fn dynamic_workflow_caller_error(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(
        DiagnosticId::new(format!("op:dynamic_workflow:{code}")),
        DiagnosticCode::new(format!("DYNAMIC_WORKFLOW/CALLER_{code}")),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("operation_service"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new("op.dynamic_workflow")),
    )
}

pub(super) fn is_mcp_safe_dynamic_workflow_op(
    op_name: &str,
    kernel_ops: &HashMap<String, Arc<dyn KernelOp>>,
) -> bool {
    if kernel_ops.contains_key(op_name) || op_name.contains("__") {
        return false;
    }
    let mapped = op_name.replace('.', "__");
    !mapped.is_empty()
        && mapped.len() <= 64
        && mapped
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn params_are_required(params: &Value) -> bool {
    let has_required = params
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|arr| !arr.is_empty());
    let has_min_props = params
        .get("minProperties")
        .and_then(Value::as_u64)
        .is_some_and(|n| n > 0);
    has_required || has_min_props
}

fn walk_schema<'a>(
    mut schema: &'a Value,
    segments: impl Iterator<Item = &'a str>,
) -> Option<&'a Value> {
    for seg in segments {
        if let Some(prop) = schema
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|props| props.get(seg))
        {
            schema = prop;
        } else if seg.parse::<usize>().is_ok()
            && let Some(items) = schema.get("items").filter(|v| v.is_object())
        {
            schema = items;
        } else {
            return None;
        }
    }
    schema.is_object().then_some(schema)
}

fn synthesize_output_schema(
    document: &DynamicWorkflowDocument,
    step_output_schemas: &HashMap<String, &Value>,
) -> Value {
    let fallback = || {
        json!({
            "type": "object",
            "properties": {
                "output": { "type": "string", "format": "asset" }
            },
            "required": ["output"],
            "additionalProperties": false
        })
    };

    let Ok(binding) = Binding::parse(&document.workflow.output) else {
        return fallback();
    };

    let resolved = match binding.root() {
        "inputs" => document
            .workflow
            .inputs
            .get(binding.name())
            .and_then(|s| walk_schema(s, binding.from().split('.').skip(2))),
        "params" => walk_schema(&document.workflow.params, binding.from().split('.').skip(1)),
        "steps" => document
            .workflow
            .steps
            .iter()
            .find(|s| s.id == binding.name())
            .and_then(|s| step_output_schemas.get(&s.id).copied())
            .and_then(|s| walk_schema(s, binding.from().split('.').skip(2))),
        _ => None,
    };

    let Some(resolved_schema) = resolved else {
        return fallback();
    };

    let candidate = json!({
        "type": "object",
        "properties": {
            "output": resolved_schema
        },
        "required": ["output"],
        "additionalProperties": false
    });

    if check_asset_marker_locations(&candidate).is_ok()
        && check_inline_text_marker_locations(&candidate).is_ok()
        && check_host_path_marker_locations(&candidate).is_ok()
    {
        candidate
    } else {
        fallback()
    }
}

fn synthesize_input_schema(document: &DynamicWorkflowDocument) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (name, schema) in &document.workflow.inputs {
        properties.insert(name.clone(), schema.clone());
        required.push(Value::String(name.clone()));
    }
    properties.insert("params".to_owned(), document.workflow.params.clone());
    if params_are_required(&document.workflow.params) {
        required.push(Value::String("params".to_owned()));
    }

    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn extension_for_media_type(media_type: Option<&str>, src_path: &Path) -> String {
    match media_type {
        Some("text/markdown") => "md".to_owned(),
        Some("text/plain") => "txt".to_owned(),
        Some("application/json") => "json".to_owned(),
        Some("image/png") => "png".to_owned(),
        Some("image/jpeg") => "jpg".to_owned(),
        Some("image/webp") => "webp".to_owned(),
        _ => src_path
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| !e.is_empty())
            .unwrap_or("bin")
            .to_owned(),
    }
}

fn escape_pointer_segment(seg: &str) -> String {
    seg.replace('~', "~0").replace('/', "~1")
}

fn collect_step_input_sources(
    value: &Value,
    pointer: &str,
    step_ids: &BTreeMap<String, StepId>,
    out: &mut BTreeMap<String, StepInputSource>,
) {
    match value {
        Value::Object(map) if map.contains_key("$from") => {
            if let Ok(binding) = Binding::parse(value)
                && binding.root() == "steps"
                && let Some(actual_step_id) = step_ids.get(binding.name())
            {
                let mut remainder = binding.from().split('.').skip(2).peekable();
                let output_pointer = if remainder.peek().is_none() {
                    String::new()
                } else {
                    let mut ptr = String::new();
                    for seg in remainder {
                        ptr.push('/');
                        ptr.push_str(&escape_pointer_segment(seg));
                    }
                    ptr
                };
                out.insert(
                    pointer.to_owned(),
                    StepInputSource {
                        step_id: actual_step_id.clone(),
                        output_pointer,
                    },
                );
            }
        }
        Value::Object(map) => {
            for (key, child) in map {
                let next = format!("{pointer}/{}", escape_pointer_segment(key));
                collect_step_input_sources(child, &next, step_ids, out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                let next = format!("{pointer}/{index}");
                collect_step_input_sources(child, &next, step_ids, out);
            }
        }
        _ => {}
    }
}

fn resolve_one(
    op_name: &str,
    docs_by_op: &HashMap<String, DynamicWorkflowDocument>,
    base_ops: &HashMap<String, BaseOpInfo>,
    visiting: &mut HashSet<String>,
    resolved: &mut HashMap<String, ResolvedDynamicWorkflow>,
) -> ResolvedDynamicWorkflow {
    if let Some(existing) = resolved.get(op_name) {
        return existing.clone();
    }

    let doc = docs_by_op
        .get(op_name)
        .expect("resolve_one called for known dynamic workflow op")
        .clone();
    let description = doc.description.clone().unwrap_or_else(|| doc.name.clone());
    let input_schema = synthesize_input_schema(&doc);

    if !visiting.insert(op_name.to_owned()) {
        let diag = dynamic_workflow_caller_error(
            "CYCLIC_WORKFLOW",
            format!("dynamic workflow `{op_name}` contains a cyclic reference"),
        );
        let entry = ResolvedDynamicWorkflow {
            document: doc,
            spec: OperationSpec {
                name: op_name.to_owned(),
                description,
                input_schema,
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "output": { "type": "string", "format": "asset" }
                    },
                    "required": ["output"],
                    "additionalProperties": false
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: false,
                    idempotent: false,
                    deterministic: false,
                    open_world: false,
                },
            },
            health: Health::Unavailable(diag),
            changes_documents: false,
            transitive_ops: Vec::new(),
        };
        return entry;
    }

    let mut step_specs: Vec<OperationSpec> = Vec::with_capacity(doc.workflow.steps.len());
    let mut step_output_schemas: HashMap<String, &Value> = HashMap::new();
    let mut first_error: Option<Diagnostic> = doc
        .workflow
        .validate()
        .err()
        .and_then(|errs| errs.into_iter().next());
    let mut changes_documents = false;
    let mut transitive_ops: Vec<String> = Vec::new();
    let mut all_steps_resolved = true;

    // Pre-resolve step specs in a temporary vec so we can borrow output_schemas.
    let mut resolved_step_specs: Vec<(String, OperationSpec)> = Vec::new();
    for step in &doc.workflow.steps {
        transitive_ops.push(step.op.clone());
        if let Some(base) = base_ops.get(&step.op) {
            if !base.health.is_available() && first_error.is_none() {
                first_error = Some(match &base.health {
                    Health::Unavailable(diag) => diag.clone(),
                    Health::Available => unreachable!(),
                });
            }
            if base.changes_documents {
                changes_documents = true;
            }
            resolved_step_specs.push((step.id.clone(), base.spec.clone()));
            step_specs.push(base.spec.clone());
        } else if docs_by_op.contains_key(&step.op) {
            if visiting.contains(&step.op) {
                all_steps_resolved = false;
                first_error = Some(dynamic_workflow_caller_error(
                    "CYCLIC_WORKFLOW",
                    format!(
                        "dynamic workflow `{op_name}` has a cyclic dependency on `{}`",
                        step.op
                    ),
                ));
            } else {
                let nested = resolve_one(&step.op, docs_by_op, base_ops, visiting, resolved);
                if !nested.health.is_available()
                    && first_error.is_none()
                    && let Health::Unavailable(diag) = &nested.health
                {
                    first_error = Some(diag.clone());
                }
                if nested.changes_documents {
                    changes_documents = true;
                }
                transitive_ops.extend(nested.transitive_ops);
                resolved_step_specs.push((step.id.clone(), nested.spec.clone()));
                step_specs.push(nested.spec);
            }
        } else {
            all_steps_resolved = false;
            if first_error.is_none() {
                first_error = Some(op_caller_error(
                    op_name,
                    "UNKNOWN_OPERATION",
                    format!(
                        "step `{}` references unknown operation `{}`",
                        step.id, step.op
                    ),
                ));
            }
        }
    }

    for (step_id, spec) in &resolved_step_specs {
        step_output_schemas.insert(step_id.clone(), &spec.output_schema);
    }

    let output_schema = synthesize_output_schema(&doc, &step_output_schemas);
    let annotations = if all_steps_resolved {
        OperationAnnotations {
            read_only: step_specs.iter().all(|s| s.annotations.read_only)
                && !has_asset_field(&output_schema),
            destructive: step_specs.iter().any(|s| s.annotations.destructive),
            idempotent: step_specs.iter().all(|s| s.annotations.idempotent),
            deterministic: step_specs.iter().all(|s| s.annotations.deterministic),
            open_world: step_specs.iter().any(|s| s.annotations.open_world),
        }
    } else {
        OperationAnnotations {
            read_only: false,
            destructive: false,
            idempotent: false,
            deterministic: false,
            open_world: false,
        }
    };

    let health = match first_error {
        Some(diag) => Health::Unavailable(diag),
        None => Health::Available,
    };

    visiting.remove(op_name);

    let entry = ResolvedDynamicWorkflow {
        document: doc,
        spec: OperationSpec {
            name: op_name.to_owned(),
            description,
            input_schema,
            output_schema,
            annotations,
        },
        health,
        changes_documents,
        transitive_ops,
    };
    resolved.insert(op_name.to_owned(), entry.clone());
    entry
}

impl OperationService {
    pub(super) async fn resolve_dynamic_workflows(
        &self,
        project: &ProjectId,
    ) -> HashMap<String, ResolvedDynamicWorkflow> {
        let service = self
            .dynamic_workflows
            .read()
            .expect("dynamic_workflows slot poisoned")
            .clone();
        let Some(service) = service else {
            return HashMap::new();
        };

        let docs = match service.list_documents(project).await {
            Ok(docs) => docs,
            Err(error) => {
                tracing::warn!(
                    project_id = %project,
                    error = %error.message(),
                    "failed to list dynamic workflows for operation projection"
                );
                return HashMap::new();
            }
        };
        if docs.is_empty() {
            return HashMap::new();
        }

        let mut base_ops: HashMap<String, BaseOpInfo> = HashMap::new();
        let mut docs_by_op: HashMap<String, DynamicWorkflowDocument> = HashMap::new();
        {
            let kernel_guard = self.kernel_ops.read().expect("kernel_ops poisoned");
            for op in kernel_guard.values() {
                let spec = op.spec().clone();
                base_ops.insert(
                    spec.name.clone(),
                    BaseOpInfo {
                        spec,
                        health: Health::Available,
                        changes_documents: op.changes_documents(),
                    },
                );
            }
            for doc in docs {
                let op_name = format!("dynamic_workflow.{}", doc.id);
                if !is_mcp_safe_dynamic_workflow_op(&op_name, &kernel_guard) {
                    tracing::warn!(
                        op = %op_name,
                        "skipping dynamic workflow whose operation name is unsafe for MCP projection"
                    );
                    continue;
                }
                docs_by_op.insert(op_name, doc);
            }
        }

        for registered in self.mod_registry.list() {
            let is_reserved =
                mod_sdk::RESERVED_NAMESPACES.contains(&registered.manifest.id.as_str());
            let health = if is_reserved {
                Health::Unavailable(op_caller_error(
                    &registered.manifest.id,
                    "RESERVED_NAMESPACE",
                    format!("mod `{}` uses reserved namespace", registered.manifest.id),
                ))
            } else {
                registered.health.clone()
            };
            for spec in &registered.manifest.operations {
                base_ops.insert(
                    spec.name.clone(),
                    BaseOpInfo {
                        spec: spec.clone(),
                        health: health.clone(),
                        changes_documents: false,
                    },
                );
            }
        }

        let mut visiting = HashSet::new();
        let mut resolved = HashMap::new();
        let mut op_names: Vec<String> = docs_by_op.keys().cloned().collect();
        op_names.sort();
        for op_name in op_names {
            resolve_one(
                &op_name,
                &docs_by_op,
                &base_ops,
                &mut visiting,
                &mut resolved,
            );
        }
        resolved
    }

    pub(super) async fn list_dynamic_workflow_infos(
        &self,
        project: &ProjectId,
    ) -> Vec<OperationInfo> {
        let resolved = self.resolve_dynamic_workflows(project).await;
        let mut infos: Vec<OperationInfo> = resolved
            .into_values()
            .map(|entry| {
                let agent_permitted = agent_may_invoke(&entry.spec.name)
                    && entry.transitive_ops.iter().all(|op| agent_may_invoke(op));
                OperationInfo {
                    spec: entry.spec,
                    provider: OperationProvider::DynamicWorkflow {
                        id: entry.document.id.to_string(),
                    },
                    health: entry.health,
                    agent_permitted,
                }
            })
            .collect();
        infos.sort_by(|a, b| a.spec.name.cmp(&b.spec.name));
        infos
    }

    pub(super) async fn invoke_dynamic_workflow(
        &self,
        project: &ProjectId,
        request: &InvokeRequest,
        resolved: &ResolvedDynamicWorkflow,
        scratch_dir: &Path,
        workflow_stack: &[DynamicWorkflowId],
    ) -> Result<Value, Diagnostic> {
        let mut next_stack = workflow_stack.to_vec();
        next_stack.push(resolved.document.id.clone());

        let ordered_steps = resolved
            .document
            .workflow
            .ordered_steps()
            .map_err(|errors| {
                errors.into_iter().next().unwrap_or_else(|| {
                    dynamic_workflow_caller_error("INVALID_WORKFLOW", "invalid workflow")
                })
            })?;

        let mut inputs = BTreeMap::new();
        for key in resolved.document.workflow.inputs.keys() {
            if let Some(val) = request.input.get(key) {
                inputs.insert(key.clone(), val.clone());
            }
        }
        let params = request
            .input
            .get("params")
            .cloned()
            .unwrap_or_else(|| json!({}));

        let mut steps = BTreeMap::new();
        let mut step_ids: BTreeMap<String, StepId> = BTreeMap::new();

        for wf_step in ordered_steps {
            let mut input_sources = BTreeMap::new();
            collect_step_input_sources(&wf_step.input, "", &step_ids, &mut input_sources);

            let scope = BindingScope {
                inputs: &inputs,
                params: &params,
                steps: &steps,
            };
            let step_input = resolve_value(&wf_step.input, &scope).map_err(|error| {
                dynamic_workflow_caller_error(
                    "INVALID_BINDING",
                    format!("step `{}` binding failed: {error}", wf_step.id),
                )
            })?;

            let inner_req = InvokeRequest {
                name: wf_step.op.clone(),
                input: step_input,
                context: request.context.clone(),
                cancel: request.cancel.clone(),
                run: None,
                input_sources,
            };
            let result = Box::pin(self.invoke_with_stack(project, inner_req, &next_stack)).await?;
            step_ids.insert(wf_step.id.clone(), result.step.id);
            steps.insert(wf_step.id.clone(), result.output);
        }

        let scope = BindingScope {
            inputs: &inputs,
            params: &params,
            steps: &steps,
        };
        let final_val = resolve_value(&resolved.document.workflow.output, &scope).map_err(|e| {
            dynamic_workflow_caller_error(
                "INVALID_BINDING",
                format!("workflow output binding failed: {e}"),
            )
        })?;
        let mut raw_output = json!({ "output": final_val });

        let asset_store = self.asset_store(project).await;
        let out_fields = find_asset_fields(&resolved.spec.output_schema, &raw_output);
        for (idx, field) in out_fields.into_iter().enumerate() {
            let asset_id_str = raw_output
                .pointer(&field.pointer)
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    op_internal_error(
                        &request.name,
                        "INVALID_OUTPUT",
                        format!("output asset field at `{}` must be a string", field.pointer),
                    )
                })?;
            let asset_id = AssetId::try_new(asset_id_str).map_err(|reason| {
                op_internal_error(
                    &request.name,
                    "INVALID_OUTPUT",
                    format!(
                        "output asset field `{}` holds invalid asset id: {reason}",
                        field.pointer
                    ),
                )
            })?;
            let record = asset_store.get_asset(&asset_id).await?.ok_or_else(|| {
                op_internal_error(
                    &request.name,
                    "ASSET_NOT_FOUND",
                    format!("output asset `{asset_id}` not found"),
                )
            })?;
            let src_path = asset_store.content_path(&record.hash);
            let ext = extension_for_media_type(
                field
                    .media_type
                    .as_deref()
                    .or(Some(record.media_type.as_str())),
                &src_path,
            );
            let rel_name = format!("out_{idx}.{ext}");
            let dest_path = scratch_dir.join(&rel_name);
            tokio::fs::copy(&src_path, &dest_path).await.map_err(|e| {
                op_internal_error(
                    &request.name,
                    "IO",
                    format!("failed to stage output asset `{}`: {e}", src_path.display()),
                )
            })?;
            set_json_pointer(&mut raw_output, &field.pointer, Value::String(rel_name))?;
        }

        Ok(raw_output)
    }

    pub(super) async fn preview_dynamic_workflow(
        &self,
        project: &ProjectId,
        name: &str,
        input: &Value,
        resolved: &ResolvedDynamicWorkflow,
        preview_dir: &Path,
        cancel: CancelToken,
    ) -> Result<Value, Diagnostic> {
        let mut ephemeral_assets: HashMap<String, (PathBuf, Option<String>)> = HashMap::new();
        let mut counter = 0usize;
        let mut raw_output = Box::pin(self.preview_dynamic_workflow_inner(
            project,
            name,
            input,
            resolved,
            preview_dir,
            &mut ephemeral_assets,
            &mut counter,
            &[],
            cancel,
        ))
        .await?;

        let asset_store = self.asset_store(project).await;
        let out_fields = find_asset_fields(&resolved.spec.output_schema, &raw_output);
        for (idx, field) in out_fields.into_iter().enumerate() {
            let token_or_id = raw_output
                .pointer(&field.pointer)
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    op_internal_error(
                        name,
                        "INVALID_OUTPUT",
                        format!("output asset field at `{}` must be a string", field.pointer),
                    )
                })?;

            let (src_path, media_type) = if let Some((path, mt)) = ephemeral_assets.get(token_or_id)
            {
                (path.clone(), mt.clone())
            } else {
                let asset_id = AssetId::try_new(token_or_id).map_err(|reason| {
                    op_internal_error(
                        name,
                        "INVALID_OUTPUT",
                        format!("output asset `{token_or_id}` invalid: {reason}"),
                    )
                })?;
                let record = asset_store.get_asset(&asset_id).await?.ok_or_else(|| {
                    op_internal_error(
                        name,
                        "ASSET_NOT_FOUND",
                        format!("output asset `{asset_id}` not found"),
                    )
                })?;
                (
                    asset_store.content_path(&record.hash),
                    Some(record.media_type),
                )
            };

            let ext = extension_for_media_type(
                field.media_type.as_deref().or(media_type.as_deref()),
                &src_path,
            );
            let rel_name = format!("out_{idx}.{ext}");
            let dest_path = preview_dir.join(&rel_name);
            tokio::fs::copy(&src_path, &dest_path).await.map_err(|e| {
                op_internal_error(
                    name,
                    "IO",
                    format!(
                        "failed to stage preview output `{}`: {e}",
                        src_path.display()
                    ),
                )
            })?;
            set_json_pointer(&mut raw_output, &field.pointer, Value::String(rel_name))?;
        }

        Ok(raw_output)
    }

    #[allow(clippy::too_many_arguments)]
    async fn preview_dynamic_workflow_inner(
        &self,
        project: &ProjectId,
        name: &str,
        input: &Value,
        resolved: &ResolvedDynamicWorkflow,
        work_dir: &Path,
        ephemeral_assets: &mut HashMap<String, (PathBuf, Option<String>)>,
        counter: &mut usize,
        workflow_stack: &[DynamicWorkflowId],
        cancel: CancelToken,
    ) -> Result<Value, Diagnostic> {
        if workflow_stack.contains(&resolved.document.id) {
            return Err(dynamic_workflow_caller_error(
                "CYCLIC_WORKFLOW",
                format!("dynamic workflow `{name}` contains a cyclic invocation"),
            ));
        }
        if let Health::Unavailable(diag) = &resolved.health {
            return Err(diag.clone());
        }
        if resolved.spec.annotations.open_world {
            return Err(op_caller_error(
                name,
                "OPEN_WORLD_PREVIEW_REFUSED",
                "preview is not supported for open-world operations",
            ));
        }
        if resolved.changes_documents {
            return Err(op_caller_error(
                name,
                "DOCUMENT_CHANGE_PREVIEW_REFUSED",
                "preview is not supported for operations that mutate documents",
            ));
        }

        let mut next_stack = workflow_stack.to_vec();
        next_stack.push(resolved.document.id.clone());

        let ordered_steps = resolved
            .document
            .workflow
            .ordered_steps()
            .map_err(|errors| {
                errors.into_iter().next().unwrap_or_else(|| {
                    dynamic_workflow_caller_error("INVALID_WORKFLOW", "invalid workflow")
                })
            })?;

        let mut inputs = BTreeMap::new();
        for key in resolved.document.workflow.inputs.keys() {
            if let Some(val) = input.get(key) {
                inputs.insert(key.clone(), val.clone());
            }
        }
        let params = input.get("params").cloned().unwrap_or_else(|| json!({}));
        let mut steps = BTreeMap::new();
        let asset_store = self.asset_store(project).await;

        for (step_idx, wf_step) in ordered_steps.into_iter().enumerate() {
            let scope = BindingScope {
                inputs: &inputs,
                params: &params,
                steps: &steps,
            };
            let step_input = resolve_value(&wf_step.input, &scope).map_err(|error| {
                dynamic_workflow_caller_error(
                    "INVALID_BINDING",
                    format!("step `{}` binding failed: {error}", wf_step.id),
                )
            })?;

            let step_dir = work_dir.join(format!("step_{step_idx}"));
            let found = self.find_operation(project, &wf_step.op).await?;
            let step_spec = found.spec().clone();

            if step_spec.annotations.open_world {
                return Err(op_caller_error(
                    &wf_step.op,
                    "OPEN_WORLD_PREVIEW_REFUSED",
                    "preview is not supported for open-world operations",
                ));
            }
            if let FoundOperation::Kernel { op, .. } = &found
                && op.changes_documents()
            {
                return Err(op_caller_error(
                    &wf_step.op,
                    "DOCUMENT_CHANGE_PREVIEW_REFUSED",
                    "preview is not supported for operations that mutate documents",
                ));
            }

            let validator = jsonschema::validator_for(&step_spec.input_schema).map_err(|e| {
                op_internal_error(
                    &wf_step.op,
                    "INVALID_SCHEMA",
                    format!("invalid input schema for `{}`: {e}", wf_step.op),
                )
            })?;
            if let Some(violations) = mod_sdk::schema::violations(&validator, &step_input) {
                return Err(op_caller_error(
                    &wf_step.op,
                    "INVALID_INPUT",
                    format!("input validation failed for `{}`: {violations}", wf_step.op),
                ));
            }
            let inline_input_bytes = inline_text_bytes(&step_spec.input_schema, &step_input);
            if inline_input_bytes > MAX_INLINE_TEXT_BYTES {
                return Err(op_caller_error(
                    &wf_step.op,
                    "INLINE_TEXT_TOO_LARGE",
                    format!("inline text input exceeds {MAX_INLINE_TEXT_BYTES} bytes"),
                ));
            }

            let step_output = match found {
                FoundOperation::DynamicWorkflow { resolved: nested } => {
                    tokio::fs::create_dir_all(&step_dir).await.map_err(|e| {
                        op_internal_error(&wf_step.op, "IO", format!("create preview dir: {e}"))
                    })?;
                    Box::pin(self.preview_dynamic_workflow_inner(
                        project,
                        &wf_step.op,
                        &step_input,
                        &nested,
                        &step_dir,
                        ephemeral_assets,
                        counter,
                        &next_stack,
                        cancel.clone(),
                    ))
                    .await?
                }
                FoundOperation::Kernel { .. } | FoundOperation::Mod { .. } => {
                    let mut input_for_provider = step_input.clone();
                    for field in find_asset_fields(&step_spec.input_schema, &step_input) {
                        let val = step_input
                            .pointer(&field.pointer)
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                op_caller_error(
                                    &wf_step.op,
                                    "INVALID_INPUT",
                                    format!("asset field at `{}` must be a string", field.pointer),
                                )
                            })?;
                        let abs_path = if let Some((path, mt)) = ephemeral_assets.get(val) {
                            if let (Some(pattern), Some(actual)) = (&field.media_type, mt)
                                && !matches_media_type_pattern(pattern, actual)
                            {
                                return Err(op_caller_error(
                                    &wf_step.op,
                                    "MEDIA_TYPE_MISMATCH",
                                    format!(
                                        "asset media type `{actual}` does not match `{pattern}`"
                                    ),
                                ));
                            }
                            path.clone()
                        } else {
                            let asset_id = AssetId::try_new(val).map_err(|reason| {
                                op_caller_error(
                                    &wf_step.op,
                                    "INVALID_INPUT",
                                    format!("invalid asset id at `{}`: {reason}", field.pointer),
                                )
                            })?;
                            let record =
                                asset_store.get_asset(&asset_id).await?.ok_or_else(|| {
                                    op_caller_error(
                                        &wf_step.op,
                                        "ASSET_NOT_FOUND",
                                        format!("asset `{asset_id}` not found"),
                                    )
                                })?;
                            if let Some(pattern) = &field.media_type
                                && !matches_media_type_pattern(pattern, &record.media_type)
                            {
                                return Err(op_caller_error(
                                    &wf_step.op,
                                    "MEDIA_TYPE_MISMATCH",
                                    format!(
                                        "asset `{asset_id}` media type `{}` does not match `{pattern}`",
                                        record.media_type
                                    ),
                                ));
                            }
                            asset_store.content_path(&record.hash)
                        };
                        set_json_pointer(
                            &mut input_for_provider,
                            &field.pointer,
                            Value::String(abs_path.to_string_lossy().to_string()),
                        )?;
                    }

                    tokio::fs::create_dir_all(&step_dir).await.map_err(|e| {
                        op_internal_error(&wf_step.op, "IO", format!("create preview dir: {e}"))
                    })?;
                    let invocation =
                        Invocation::new(&wf_step.op, input_for_provider, &step_dir, cancel.clone());
                    let mut raw_step_out = match found {
                        FoundOperation::Kernel { op, .. } => {
                            let call = KernelCall {
                                invocation,
                                project_id: project.clone(),
                                context: OperationContext::new(
                                    CommandActor::new(CommandActorKind::Human),
                                    CommandProvenance::Direct,
                                ),
                                caller_input: step_input.clone(),
                            };
                            op.invoke(call).await?
                        }
                        FoundOperation::Mod { id, .. } => {
                            self.mod_registry.invoke(&id, invocation).await?
                        }
                        FoundOperation::DynamicWorkflow { .. } => unreachable!(),
                    };

                    let out_validator = jsonschema::validator_for(&step_spec.output_schema)
                        .map_err(|e| {
                            op_internal_error(
                                &wf_step.op,
                                "INVALID_SCHEMA",
                                format!("invalid output schema for `{}`: {e}", wf_step.op),
                            )
                        })?;
                    if let Some(violations) =
                        mod_sdk::schema::violations(&out_validator, &raw_step_out)
                    {
                        return Err(op_internal_error(
                            &wf_step.op,
                            "INVALID_OUTPUT",
                            format!(
                                "output validation failed for `{}`: {violations}",
                                wf_step.op
                            ),
                        ));
                    }

                    for field in find_asset_fields(&step_spec.output_schema, &raw_step_out) {
                        let rel_str = raw_step_out
                            .pointer(&field.pointer)
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                op_internal_error(
                                    &wf_step.op,
                                    "INVALID_OUTPUT",
                                    format!(
                                        "output asset field at `{}` must be a string",
                                        field.pointer
                                    ),
                                )
                            })?;
                        let abs_file_path = validate_relative_path(&step_dir, rel_str)?;
                        *counter += 1;
                        let token = format!("__preview_asset_{counter}__");
                        ephemeral_assets.insert(token.clone(), (abs_file_path, field.media_type));
                        set_json_pointer(&mut raw_step_out, &field.pointer, Value::String(token))?;
                    }
                    raw_step_out
                }
            };

            steps.insert(wf_step.id.clone(), step_output);
        }

        let scope = BindingScope {
            inputs: &inputs,
            params: &params,
            steps: &steps,
        };
        let final_val = resolve_value(&resolved.document.workflow.output, &scope).map_err(|e| {
            dynamic_workflow_caller_error(
                "INVALID_BINDING",
                format!("workflow output binding failed: {e}"),
            )
        })?;
        Ok(json!({ "output": final_val }))
    }
}
