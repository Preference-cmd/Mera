mod desktop_host;
mod event_bridge;
mod mcp_dev;

#[cfg(feature = "aigc")]
use desktop_host::WorkerSwitchResultDto;
use desktop_host::{
    DesktopHostState, default_workspace_path, resolve_app_data_dir, resolve_mods_dir,
};
use mera_app_host::{
    AppHostError, AppHostErrorCode, AssetStore, InvokeResult, OperationInfo,
    dto::{
        ArtifactMetadataDto, HealthResponse, ModBrowseResponseDto, ModDownloadTaskDto, ModEntryDto,
        ModInstallPreviewDto, ModListResponseDto, ModRuntimeEnvironmentDto, ModSourcesListDto,
        NodeDefDto, RunWorkflowResponse, ThirdPartySourceDto,
    },
};
#[cfg(feature = "aigc")]
use mera_app_host::{
    BackendSelection, WorkerInstallationDto,
    dto::{
        ComputeProfileDto, ModelCardDto, ModelCatalogEntryDto, ModelDownloadOutput, ModelFilters,
        ModelInfoDto,
    },
};
use mera_core::command::{CommandResult, WorkflowChange};
use mera_core::event::Event;
use serde::Serialize;
use tauri::{Manager, ipc::Channel};

#[derive(Debug, Clone, Serialize)]
struct TauriCommandError {
    code: AppHostErrorCode,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
}

impl TauriCommandError {
    fn bootstrap(message: impl Into<String>) -> Self {
        Self {
            code: AppHostErrorCode::BootstrapFailed,
            message: message.into(),
            details: None,
        }
    }

    #[allow(dead_code)]
    fn command(message: impl Into<String>) -> Self {
        Self {
            code: AppHostErrorCode::CommandFailed,
            message: message.into(),
            details: None,
        }
    }
}

impl std::fmt::Display for TauriCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for TauriCommandError {}

fn app_host_command_error(error: AppHostError) -> TauriCommandError {
    TauriCommandError {
        code: error.code(),
        message: error.to_string(),
        details: error.details(),
    }
}

#[cfg(feature = "aigc")]
fn switch_deadline(deadline_secs: Option<f64>) -> Result<std::time::Duration, TauriCommandError> {
    match deadline_secs {
        None => Ok(std::time::Duration::from_secs(30)),
        Some(secs) if secs > 0.0 => Ok(std::time::Duration::from_secs_f64(secs)),
        Some(_) => Err(TauriCommandError::command(
            "deadline_secs must be a positive number of seconds",
        )),
    }
}

// ─── Existing commands ───────────────────────────────────────────

#[tauri::command]
fn health(state: tauri::State<'_, DesktopHostState>) -> Result<HealthResponse, TauriCommandError> {
    Ok(state.health())
}

#[cfg(feature = "aigc")]
#[tauri::command]
fn get_compute_profile(
    state: tauri::State<'_, DesktopHostState>,
) -> Result<ComputeProfileDto, TauriCommandError> {
    Ok(state.compute_profile())
}

#[tauri::command]
fn get_node_defs(
    state: tauri::State<'_, DesktopHostState>,
) -> Result<Vec<NodeDefDto>, TauriCommandError> {
    let response = state.list_node_defs();
    Ok(response.nodes)
}

#[cfg(feature = "aigc")]
#[tauri::command]
async fn list_models(
    state: tauri::State<'_, DesktopHostState>,
) -> Result<Vec<ModelInfoDto>, TauriCommandError> {
    state.list_models().await.map_err(app_host_command_error)
}

#[tauri::command]
async fn run_workflow(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    workflow: serde_json::Value,
) -> Result<RunWorkflowResponse, TauriCommandError> {
    state
        .run_workflow_for_project(&project_id, workflow)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn cancel_run(
    state: tauri::State<'_, DesktopHostState>,
    run_id: String,
) -> Result<(), TauriCommandError> {
    state.cancel_run(&run_id).map_err(app_host_command_error)
}

#[tauri::command]
async fn resolve_artifact(
    state: tauri::State<'_, DesktopHostState>,
    artifact_id: String,
) -> Result<ArtifactMetadataDto, TauriCommandError> {
    state
        .resolve_artifact(&artifact_id)
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn open_artifact(
    state: tauri::State<'_, DesktopHostState>,
    artifact_id: String,
) -> Result<(), TauriCommandError> {
    state
        .open_artifact(&artifact_id)
        .map_err(app_host_command_error)
}

// ─── Model download commands ───────────────────────────────────────

/// Download a HuggingFace model. Progress arrives on `subscribe_events` as
/// `aigc.download.*` mod events carrying the caller's `download_id`.
#[cfg(feature = "aigc")]
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn download_huggingface_model(
    state: tauri::State<'_, DesktopHostState>,
    download_id: String,
    repo_id: String,
    revision: Option<String>,
    allow_patterns: Option<Vec<String>>,
    target_relative_dir: String,
    overwrite: Option<String>,
    auto_detect: Option<bool>,
    from_catalog: Option<bool>,
) -> Result<ModelDownloadOutput, TauriCommandError> {
    state
        .download_huggingface_model(
            download_id,
            repo_id,
            revision,
            allow_patterns,
            target_relative_dir,
            overwrite,
            auto_detect,
            from_catalog,
        )
        .await
        .map_err(app_host_command_error)
}

/// Search HuggingFace models via the catalog.
///
/// Returns a list of model catalog entries matching the query and filters.
#[cfg(feature = "aigc")]
#[tauri::command]
async fn search_models(
    state: tauri::State<'_, DesktopHostState>,
    query: String,
    filters: Option<ModelFilters>,
) -> Result<Vec<ModelCatalogEntryDto>, TauriCommandError> {
    use mera_model_acquisition::{ModelSearchQuery, SortBy};

    let sort = match filters.as_ref().map(|f| f.sort.as_str()) {
        Some("likes") => SortBy::Likes,
        Some("trending") => SortBy::Trending,
        Some("lastModified") => SortBy::LastModified,
        _ => SortBy::Downloads,
    };

    let search_query = ModelSearchQuery {
        search: if query.is_empty() { None } else { Some(query) },
        pipeline_tag: filters.as_ref().and_then(|f| f.pipeline_tag.clone()),
        library_name: filters.as_ref().and_then(|f| f.library_name.clone()),
        tags: filters.as_ref().map(|f| f.tags.clone()).unwrap_or_default(),
        sort,
        limit: filters.map(|f| f.limit).unwrap_or(20),
    };

    state
        .search_models(&search_query)
        .await
        .map_err(app_host_command_error)
}

/// Fetch the full model card for a HuggingFace repository.
#[cfg(feature = "aigc")]
#[tauri::command]
async fn get_model_card(
    state: tauri::State<'_, DesktopHostState>,
    repo_id: String,
) -> Result<ModelCardDto, TauriCommandError> {
    state
        .get_model_card(&repo_id)
        .await
        .map_err(app_host_command_error)
}

// ─── Worker switch commands (BE-32) ─────────────────────────────

/// Drain in-flight runs (waiting up to `deadline_secs`) and switch the
/// active worker to the installed worker for `target` (a backend instance
/// id such as `burn:wgpu:default`).
#[cfg(feature = "aigc")]
#[tauri::command]
async fn drain_and_switch_worker(
    state: tauri::State<'_, DesktopHostState>,
    target: String,
    deadline_secs: Option<f64>,
) -> Result<WorkerSwitchResultDto, TauriCommandError> {
    let deadline = switch_deadline(deadline_secs)?;
    state
        .drain_and_switch_worker(&target, deadline)
        .await
        .map_err(app_host_command_error)
}

/// Cancel in-flight runs, then switch the active worker to the installed
/// worker for `target`.
#[cfg(feature = "aigc")]
#[tauri::command]
async fn cancel_and_switch_worker(
    state: tauri::State<'_, DesktopHostState>,
    target: String,
    deadline_secs: Option<f64>,
) -> Result<WorkerSwitchResultDto, TauriCommandError> {
    let deadline = switch_deadline(deadline_secs)?;
    state
        .cancel_and_switch_worker(&target, deadline)
        .await
        .map_err(app_host_command_error)
}

/// List installed workers usable as switch targets.
#[cfg(feature = "aigc")]
#[tauri::command]
fn list_worker_switch_targets(
    state: tauri::State<'_, DesktopHostState>,
) -> Result<Vec<WorkerInstallationDto>, TauriCommandError> {
    state
        .list_worker_switch_targets()
        .map_err(app_host_command_error)
}

/// Drain in-flight runs (waiting up to the re-bootstrap deadline) and
/// re-run the workspace bootstrap with `selection` (`"burn"`).
///
/// Resolves to the compute profile of the rebuilt workspace.
#[cfg(feature = "aigc")]
#[tauri::command]
async fn rebootstrap_backend(
    state: tauri::State<'_, DesktopHostState>,
    selection: BackendSelection,
) -> Result<ComputeProfileDto, TauriCommandError> {
    state
        .rebootstrap_backend(selection)
        .await
        .map_err(app_host_command_error)
}

// ─── Project / board surface (AR-39) ───────────────────────────────

#[tauri::command]
async fn list_projects(
    state: tauri::State<'_, DesktopHostState>,
) -> Result<Vec<mera_app_host::dto::ProjectDto>, TauriCommandError> {
    state.list_projects().await.map_err(app_host_command_error)
}

#[tauri::command]
async fn create_project(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    metadata: mera_app_host::dto::ProjectMetadataInputDto,
) -> Result<mera_app_host::dto::ProjectDto, TauriCommandError> {
    state
        .create_project(project_id, metadata)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn load_project(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
) -> Result<mera_app_host::dto::ProjectDto, TauriCommandError> {
    state
        .load_project(&project_id)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn update_project(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    metadata: mera_app_host::dto::ProjectMetadataInputDto,
) -> Result<mera_app_host::dto::ProjectDto, TauriCommandError> {
    state
        .update_project(&project_id, metadata)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn delete_project(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
) -> Result<(), TauriCommandError> {
    state
        .delete_project(&project_id)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn set_active_project(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
) -> Result<mera_app_host::dto::ProjectDto, TauriCommandError> {
    state
        .set_active_project(&project_id)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn subscribe_events(
    state: tauri::State<'_, DesktopHostState>,
    channel: Channel<Event>,
) -> Result<(), TauriCommandError> {
    state.subscribe_events(channel);
    Ok(())
}

#[tauri::command]
async fn board_snapshot(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
) -> Result<mera_app_host::dto::BoardSnapshotDto, TauriCommandError> {
    state
        .board_snapshot(&project_id)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn preview_board_commands(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    command_batch: serde_json::Value,
) -> Result<mera_app_host::dto::BoardCommandResultDto, TauriCommandError> {
    state
        .preview_board_commands(&project_id, command_batch)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn apply_board_commands(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    command_batch: serde_json::Value,
) -> Result<mera_app_host::dto::BoardCommandResultDto, TauriCommandError> {
    state
        .apply_board_commands(&project_id, command_batch)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn undo_board(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    expected_entry_id: Option<String>,
) -> Result<Option<mera_app_host::dto::BoardCommandResultDto>, TauriCommandError> {
    state
        .undo_board(&project_id, expected_entry_id.as_deref())
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn redo_board(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    expected_entry_id: Option<String>,
) -> Result<Option<mera_app_host::dto::BoardCommandResultDto>, TauriCommandError> {
    state
        .redo_board(&project_id, expected_entry_id.as_deref())
        .await
        .map_err(app_host_command_error)
}

// ─── Workflow command commands ───────────────────────────────────

/// Preview a command batch (dry-run). Returns diagnostics without mutating.
#[tauri::command]
fn preview_workflow_commands(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    workflow_id: String,
    command_batch: serde_json::Value,
) -> Result<CommandResult<WorkflowChange>, TauriCommandError> {
    state
        .preview_workflow_commands(project_id, workflow_id, command_batch)
        .map_err(app_host_command_error)
}

/// Apply a command batch directly.
#[tauri::command]
async fn apply_workflow_commands(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    workflow_id: String,
    command_batch: serde_json::Value,
    _approved_by: Option<serde_json::Value>,
) -> Result<CommandResult<WorkflowChange>, TauriCommandError> {
    state
        .apply_workflow_commands(project_id, workflow_id, command_batch, _approved_by)
        .await
        .map_err(app_host_command_error)
}

/// Approve a pending workflow proposal (human approval of build-mode output).
#[tauri::command]
async fn approve_proposal(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    workflow_id: String,
) -> Result<CommandResult<WorkflowChange>, TauriCommandError> {
    state
        .approve_proposal(project_id, workflow_id)
        .await
        .map_err(app_host_command_error)
}

// ─── Workflow persistence commands ──────────────────────────────

/// Save a workflow to the workspace `workflows/` directory as JSON.
///
/// `workflow_json` must be a full `mera_core::workflow::Workflow`
/// payload. Returns the path the file was written to.
#[tauri::command]
async fn save_workflow(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    workflow_id: String,
    workflow_json: serde_json::Value,
) -> Result<String, TauriCommandError> {
    state
        .save_workflow_for_project(&project_id, &workflow_id, workflow_json)
        .await
        .map(|path| path.display().to_string())
        .map_err(app_host_command_error)
}

/// Load a workflow (JSON) from the workspace `workflows/` directory.
#[tauri::command]
async fn load_workflow(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    workflow_id: String,
) -> Result<serde_json::Value, TauriCommandError> {
    state
        .load_workflow_json_for_project(&project_id, &workflow_id)
        .await
        .map_err(app_host_command_error)
}

/// List saved workflows (newest first) as `{ id, modified_millis }` objects.
#[tauri::command]
fn list_workflows(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
) -> Result<Vec<serde_json::Value>, TauriCommandError> {
    state
        .list_saved_workflows_for_project(&project_id)
        .map_err(app_host_command_error)
}

// ─── Operation commands (W-a) ────────────────────────────────────────────────

#[tauri::command]
async fn op_list(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
) -> Result<Vec<OperationInfo>, TauriCommandError> {
    state
        .op_list(&project_id)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn op_invoke(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    name: String,
    input: serde_json::Value,
) -> Result<InvokeResult, TauriCommandError> {
    state
        .op_invoke(&project_id, name, input)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn op_preview(
    state: tauri::State<'_, DesktopHostState>,
    project_id: String,
    name: String,
    input: serde_json::Value,
    preview_key: String,
) -> Result<serde_json::Value, TauriCommandError> {
    state
        .op_preview(&project_id, name, input, preview_key)
        .await
        .map_err(app_host_command_error)
}

/// Cancel an in-flight `op_preview` by its key. Unknown keys are a no-op.
#[tauri::command]
async fn op_preview_cancel(
    state: tauri::State<'_, DesktopHostState>,
    preview_key: String,
) -> Result<(), TauriCommandError> {
    state
        .op_preview_cancel(&preview_key)
        .await
        .map_err(app_host_command_error)
}

// ─── Mod manager commands (M2) ──────────────────────────────────

#[tauri::command]
fn mods_list(
    state: tauri::State<'_, DesktopHostState>,
) -> Result<ModListResponseDto, TauriCommandError> {
    Ok(state.mods_list())
}

#[tauri::command]
fn mods_open_dir(state: tauri::State<'_, DesktopHostState>) -> Result<(), TauriCommandError> {
    state.mods_open_dir().map_err(app_host_command_error)
}

#[tauri::command]
fn mods_preview_archive(
    state: tauri::State<'_, DesktopHostState>,
    path: String,
) -> Result<ModInstallPreviewDto, TauriCommandError> {
    state
        .mods_preview_archive(&path)
        .map_err(app_host_command_error)
}

#[tauri::command]
fn mods_preview_directory(
    state: tauri::State<'_, DesktopHostState>,
    path: String,
) -> Result<ModInstallPreviewDto, TauriCommandError> {
    state
        .mods_preview_directory(&path)
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn mods_install_archive(
    state: tauri::State<'_, DesktopHostState>,
    path: String,
    consent_granted: bool,
    consent_permission_expansion: bool,
    enabled: bool,
    agent_visible_operations: Vec<String>,
) -> Result<ModEntryDto, TauriCommandError> {
    state
        .mods_install_archive(
            &path,
            consent_granted,
            consent_permission_expansion,
            enabled,
            agent_visible_operations,
        )
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn mods_install_directory(
    state: tauri::State<'_, DesktopHostState>,
    path: String,
    consent_granted: bool,
    consent_permission_expansion: bool,
    enabled: bool,
    agent_visible_operations: Vec<String>,
) -> Result<ModEntryDto, TauriCommandError> {
    state
        .mods_install_directory(
            &path,
            consent_granted,
            consent_permission_expansion,
            enabled,
            agent_visible_operations,
        )
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn mods_set_enabled(
    state: tauri::State<'_, DesktopHostState>,
    mod_id: String,
    enabled: bool,
) -> Result<ModEntryDto, TauriCommandError> {
    state
        .mods_set_enabled(&mod_id, enabled)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn mods_set_agent_visibility(
    state: tauri::State<'_, DesktopHostState>,
    mod_id: String,
    operations: Vec<String>,
) -> Result<ModEntryDto, TauriCommandError> {
    state
        .mods_set_agent_visibility(&mod_id, operations)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn mods_reload_local(
    state: tauri::State<'_, DesktopHostState>,
    mod_id: String,
    consent_permission_expansion: bool,
) -> Result<ModEntryDto, TauriCommandError> {
    state
        .mods_reload_local(&mod_id, consent_permission_expansion)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn mods_uninstall(
    state: tauri::State<'_, DesktopHostState>,
    mod_id: String,
    remove_files: bool,
) -> Result<(), TauriCommandError> {
    state
        .mods_uninstall(&mod_id, remove_files)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
fn mods_sources_list(
    state: tauri::State<'_, DesktopHostState>,
) -> Result<ModSourcesListDto, TauriCommandError> {
    state.mods_sources_list().map_err(app_host_command_error)
}

#[tauri::command]
fn mods_sources_add(
    state: tauri::State<'_, DesktopHostState>,
    name: String,
    url: String,
    public_key_ed25519: Option<String>,
) -> Result<ThirdPartySourceDto, TauriCommandError> {
    state
        .mods_sources_add(&name, &url, public_key_ed25519.as_deref())
        .map_err(app_host_command_error)
}

#[tauri::command]
fn mods_sources_set_enabled(
    state: tauri::State<'_, DesktopHostState>,
    source_id: String,
    enabled: bool,
) -> Result<ThirdPartySourceDto, TauriCommandError> {
    state
        .mods_sources_set_enabled(&source_id, enabled)
        .map_err(app_host_command_error)
}

#[tauri::command]
fn mods_sources_remove(
    state: tauri::State<'_, DesktopHostState>,
    source_id: String,
) -> Result<(), TauriCommandError> {
    state
        .mods_sources_remove(&source_id)
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn mods_browse(
    state: tauri::State<'_, DesktopHostState>,
    source_id: Option<String>,
    force_refresh: bool,
) -> Result<ModBrowseResponseDto, TauriCommandError> {
    state
        .mods_browse(source_id.as_deref(), force_refresh)
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn mods_download_start(
    state: tauri::State<'_, DesktopHostState>,
    source_id: String,
    mod_id: String,
    version: Option<String>,
) -> Result<ModDownloadTaskDto, TauriCommandError> {
    state
        .mods_download_start(&source_id, &mod_id, version.as_deref())
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
fn mods_download_status(
    state: tauri::State<'_, DesktopHostState>,
    task_id: String,
) -> Result<ModDownloadTaskDto, TauriCommandError> {
    state
        .mods_download_status(&task_id)
        .map_err(app_host_command_error)
}

#[tauri::command]
fn mods_download_cancel(
    state: tauri::State<'_, DesktopHostState>,
    task_id: String,
) -> Result<ModDownloadTaskDto, TauriCommandError> {
    state
        .mods_download_cancel(&task_id)
        .map_err(app_host_command_error)
}

#[tauri::command]
async fn mods_download_confirm_install(
    state: tauri::State<'_, DesktopHostState>,
    task_id: String,
    consent_permission_expansion: bool,
    agent_visible_operations: Vec<String>,
) -> Result<ModEntryDto, TauriCommandError> {
    state
        .mods_download_confirm_install(
            &task_id,
            consent_permission_expansion,
            agent_visible_operations,
        )
        .await
        .map_err(app_host_command_error)
}

#[tauri::command]
fn mods_runtime_status(
    state: tauri::State<'_, DesktopHostState>,
) -> Result<ModRuntimeEnvironmentDto, TauriCommandError> {
    Ok(state.mods_runtime_status())
}

#[tauri::command]
async fn mods_uv_bootstrap(
    state: tauri::State<'_, DesktopHostState>,
    archive_url: Option<String>,
    expected_sha256: Option<String>,
) -> Result<ModRuntimeEnvironmentDto, TauriCommandError> {
    state
        .mods_uv_bootstrap(archive_url.as_deref(), expected_sha256.as_deref())
        .await
        .map_err(app_host_command_error)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ParsedAssetUri {
    Asset { project: String, asset_id: String },
    Library { asset_id: String },
    Preview { key: String, relative_path: String },
}

fn parse_path_segments(path: &str) -> Result<ParsedAssetUri, String> {
    let trimmed = path.trim_start_matches('/');
    if let Some((first, rest)) = trimmed.split_once('/') {
        if first == "preview" {
            if let Some((key, rel)) = rest.split_once('/') {
                let key = key.trim();
                let rel = rel.trim_start_matches('/');
                if key.is_empty() || rel.is_empty() {
                    return Err("missing preview key or relative path".to_string());
                }
                if !mera_app_host::is_valid_preview_key(key) {
                    return Err(format!("invalid preview key: {key}"));
                }
                return Ok(ParsedAssetUri::Preview {
                    key: key.to_string(),
                    relative_path: rel.to_string(),
                });
            } else {
                return Err("missing relative path in preview URI".to_string());
            }
        } else if first == "library" {
            let asset_id = rest.trim_matches('/');
            if asset_id.is_empty() {
                return Err("missing asset id".to_string());
            }
            if mera_core::model::AssetId::try_new(asset_id).is_err() {
                return Err(format!("invalid asset id: {asset_id}"));
            }
            return Ok(ParsedAssetUri::Library {
                asset_id: asset_id.to_string(),
            });
        } else {
            let asset_id = rest.trim_matches('/');
            if first.is_empty() || asset_id.is_empty() {
                return Err("missing project or asset id".to_string());
            }
            if mera_core::model::ProjectId::try_new(first).is_err() {
                return Err(format!("invalid project id: {first}"));
            }
            if mera_core::model::AssetId::try_new(asset_id).is_err() {
                return Err(format!("invalid asset id: {asset_id}"));
            }
            return Ok(ParsedAssetUri::Asset {
                project: first.to_string(),
                asset_id: asset_id.to_string(),
            });
        }
    }
    Err("missing project or asset id".to_string())
}

pub(crate) fn parse_asset_uri(uri: &tauri::http::Uri) -> Result<ParsedAssetUri, String> {
    let scheme = uri.scheme_str().unwrap_or_default();
    let host = uri.host();
    let path = uri.path();

    let is_custom_scheme = scheme == "mera-asset";
    let is_localhost_http =
        (scheme == "http" || scheme == "https") && host == Some("mera-asset.localhost");

    if !is_custom_scheme && !is_localhost_http {
        return Err(format!("invalid mera-asset URI: {uri}"));
    }

    if is_localhost_http {
        return parse_path_segments(path).map_err(|_| format!("invalid mera-asset URI: {uri}"));
    }

    if host == Some("library") {
        let asset_id = path.trim_matches('/');
        if asset_id.is_empty() {
            return Err("missing asset id".to_string());
        }
        if mera_core::model::AssetId::try_new(asset_id).is_err() {
            return Err(format!("invalid asset id: {asset_id}"));
        }
        return Ok(ParsedAssetUri::Library {
            asset_id: asset_id.to_string(),
        });
    }

    if host == Some("preview") {
        let trimmed_path = path.trim_start_matches('/');
        if let Some((key, rel)) = trimmed_path.split_once('/') {
            let key = key.trim();
            let rel = rel.trim_start_matches('/');
            if key.is_empty() || rel.is_empty() {
                return Err("missing preview key or relative path".to_string());
            }
            if !mera_app_host::is_valid_preview_key(key) {
                return Err(format!("invalid preview key: {key}"));
            }
            return Ok(ParsedAssetUri::Preview {
                key: key.to_string(),
                relative_path: rel.to_string(),
            });
        } else {
            return Err("missing relative path in preview URI".to_string());
        }
    }

    if let Some(host_str) = host
        && !host_str.is_empty()
        && host_str != "localhost"
    {
        let asset_id = path.trim_matches('/');
        if asset_id.is_empty() {
            return Err("missing asset id".to_string());
        }
        if mera_core::model::ProjectId::try_new(host_str).is_err() {
            return Err(format!("invalid project id: {host_str}"));
        }
        if mera_core::model::AssetId::try_new(asset_id).is_err() {
            return Err(format!("invalid asset id: {asset_id}"));
        }
        return Ok(ParsedAssetUri::Asset {
            project: host_str.to_string(),
            asset_id: asset_id.to_string(),
        });
    }

    parse_path_segments(path).map_err(|_| format!("invalid mera-asset URI: {uri}"))
}

fn add_cors_if_allowed(
    mut builder: tauri::http::response::Builder,
    origin: Option<&str>,
) -> tauri::http::response::Builder {
    const ALLOWED_ORIGINS: &[&str] = &[
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
        "http://localhost:1420",
    ];
    builder = builder.header("Vary", "Origin");
    if let Some(orig) = origin
        && ALLOWED_ORIGINS.contains(&orig)
    {
        builder = builder.header("Access-Control-Allow-Origin", orig);
    }
    builder
}

pub(crate) fn handle_asset_uri_request(
    state: &DesktopHostState,
    uri: &tauri::http::Uri,
    origin: Option<&str>,
) -> tauri::http::Response<Vec<u8>> {
    let parsed = match parse_asset_uri(uri) {
        Ok(p) => p,
        Err(err) => {
            return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                .status(tauri::http::StatusCode::BAD_REQUEST)
                .header("Content-Type", "text/plain")
                .body(err.into_bytes())
                .unwrap_or_default();
        }
    };

    match parsed {
        ParsedAssetUri::Asset { project, asset_id } => {
            let id = match mera_core::model::AssetId::try_new(&asset_id) {
                Ok(id) => id,
                Err(_) => {
                    return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                        .status(tauri::http::StatusCode::BAD_REQUEST)
                        .header("Content-Type", "text/plain")
                        .body(b"invalid asset id".to_vec())
                        .unwrap_or_default();
                }
            };
            let project_id = match state.require_project_sync(&project) {
                Ok(id) => id,
                Err(AppHostError::UnknownProject { .. }) => {
                    return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                        .status(tauri::http::StatusCode::NOT_FOUND)
                        .header("Content-Type", "text/plain")
                        .body(b"project not found".to_vec())
                        .unwrap_or_default();
                }
                Err(_) => {
                    return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                        .status(tauri::http::StatusCode::BAD_REQUEST)
                        .header("Content-Type", "text/plain")
                        .body(b"invalid project id".to_vec())
                        .unwrap_or_default();
                }
            };
            let workspace = state.workspace_arc();
            let project_dir = workspace.config().paths().project_dir(project_id.as_str());
            let store = AssetStore::new(&project_dir);
            let record = match store.get_asset_sync(&id) {
                Ok(Some(r)) => r,
                Ok(None) => {
                    return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                        .status(tauri::http::StatusCode::NOT_FOUND)
                        .header("Content-Type", "text/plain")
                        .body(b"asset not found".to_vec())
                        .unwrap_or_default();
                }
                Err(e) => {
                    return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                        .status(tauri::http::StatusCode::INTERNAL_SERVER_ERROR)
                        .header("Content-Type", "text/plain")
                        .body(e.message().as_bytes().to_vec())
                        .unwrap_or_default();
                }
            };

            let file_path = store.content_path(&record.hash);
            let bytes = match std::fs::read(&file_path) {
                Ok(b) => b,
                Err(e) => {
                    return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                        .status(tauri::http::StatusCode::NOT_FOUND)
                        .header("Content-Type", "text/plain")
                        .body(format!("asset content file not found: {e}").into_bytes())
                        .unwrap_or_default();
                }
            };

            add_cors_if_allowed(tauri::http::Response::builder(), origin)
                .status(tauri::http::StatusCode::OK)
                .header("Content-Type", record.media_type)
                .header("Content-Length", record.size.to_string())
                .body(bytes)
                .unwrap_or_default()
        }
        ParsedAssetUri::Library { asset_id } => {
            let id = match mera_core::model::AssetId::try_new(&asset_id) {
                Ok(id) => id,
                Err(_) => {
                    return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                        .status(tauri::http::StatusCode::BAD_REQUEST)
                        .header("Content-Type", "text/plain")
                        .body(b"invalid asset id".to_vec())
                        .unwrap_or_default();
                }
            };
            let workspace = state.workspace_arc();
            let store = workspace.library_service().asset_store();
            let record = match store.get_asset_sync(&id) {
                Ok(Some(r)) => r,
                Ok(None) => {
                    return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                        .status(tauri::http::StatusCode::NOT_FOUND)
                        .header("Content-Type", "text/plain")
                        .body(b"asset not found".to_vec())
                        .unwrap_or_default();
                }
                Err(e) => {
                    return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                        .status(tauri::http::StatusCode::INTERNAL_SERVER_ERROR)
                        .header("Content-Type", "text/plain")
                        .body(e.message().as_bytes().to_vec())
                        .unwrap_or_default();
                }
            };

            let file_path = store.content_path(&record.hash);
            let bytes = match std::fs::read(&file_path) {
                Ok(b) => b,
                Err(e) => {
                    return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                        .status(tauri::http::StatusCode::NOT_FOUND)
                        .header("Content-Type", "text/plain")
                        .body(format!("asset content file not found: {e}").into_bytes())
                        .unwrap_or_default();
                }
            };

            add_cors_if_allowed(tauri::http::Response::builder(), origin)
                .status(tauri::http::StatusCode::OK)
                .header("Content-Type", record.media_type)
                .header("Content-Length", record.size.to_string())
                .body(bytes)
                .unwrap_or_default()
        }
        ParsedAssetUri::Preview { key, relative_path } => {
            if !mera_app_host::is_valid_preview_key(&key) {
                return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                    .status(tauri::http::StatusCode::BAD_REQUEST)
                    .header("Content-Type", "text/plain")
                    .body(b"invalid preview key".to_vec())
                    .unwrap_or_default();
            }

            let workspace = state.workspace_arc();
            let file_path = match workspace
                .operation_service()
                .resolve_preview_file(&key, &relative_path)
            {
                Ok(p) => p,
                Err(e) => {
                    let is_bad_request = match &e {
                        AppHostError::Diagnostic(d) => {
                            d.code().as_str().contains("INVALID_PREVIEW_KEY")
                        }
                        _ => e.to_string().contains("preview key"),
                    };
                    let is_forbidden = match &e {
                        AppHostError::Diagnostic(d) => {
                            let code = d.code().as_str();
                            code.contains("ESCAPE") || code.contains("SYMLINK")
                        }
                        _ => false,
                    } || {
                        let msg = e.to_string();
                        msg.contains("symlink")
                            || msg.contains("escape")
                            || msg.contains("relative")
                    };
                    let status = if is_bad_request {
                        tauri::http::StatusCode::BAD_REQUEST
                    } else if is_forbidden {
                        tauri::http::StatusCode::FORBIDDEN
                    } else {
                        tauri::http::StatusCode::NOT_FOUND
                    };
                    return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                        .status(status)
                        .header("Content-Type", "text/plain")
                        .body(e.to_string().into_bytes())
                        .unwrap_or_default();
                }
            };

            let bytes = match std::fs::read(&file_path) {
                Ok(b) => b,
                Err(e) => {
                    return add_cors_if_allowed(tauri::http::Response::builder(), origin)
                        .status(tauri::http::StatusCode::NOT_FOUND)
                        .header("Content-Type", "text/plain")
                        .body(format!("preview file not found: {e}").into_bytes())
                        .unwrap_or_default();
                }
            };

            let media_type = mera_app_host::resolve_media_type(&file_path, None);
            add_cors_if_allowed(tauri::http::Response::builder(), origin)
                .status(tauri::http::StatusCode::OK)
                .header("Content-Type", media_type)
                .header("Content-Length", bytes.len().to_string())
                // A preview key is reused, so the same URL can name different
                // bytes after a re-preview.
                .header("Cache-Control", "no-store")
                .body(bytes)
                .unwrap_or_default()
        }
    }
}

/// Deadline for stopping child worker and mod processes on exit.
const EXIT_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Drive `future` to completion on Tauri's async runtime from the synchronous
/// exit callback, giving up after `timeout`.
///
/// The exit callback runs on the UI thread with no Tokio context, so it must
/// go through Tauri's runtime instead of `tokio::runtime::Handle::current()`.
/// Returning at the deadline keeps a stuck child process from blocking the
/// application forever, and the timeout is enforced by the runtime.
fn block_on_with_timeout<F>(future: F, timeout: std::time::Duration) -> bool
where
    F: std::future::Future<Output = ()>,
{
    tauri::async_runtime::block_on(
        async move { tokio::time::timeout(timeout, future).await.is_ok() },
    )
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    match tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .register_uri_scheme_protocol("mera-asset", |ctx, req| {
            let app_handle = ctx.app_handle();
            let state = app_handle.state::<DesktopHostState>();
            let origin = req
                .headers()
                .get(tauri::http::header::ORIGIN)
                .and_then(|v| v.to_str().ok());
            handle_asset_uri_request(&state, req.uri(), origin)
        })
        .setup(|app| {
            let default_app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| TauriCommandError::bootstrap(error.to_string()))?;
            let app_data_env = std::env::var_os("MERA_APP_DATA_DIR");
            let app_data_dir = resolve_app_data_dir(app_data_env.as_deref(), default_app_data_dir)
                .map_err(TauriCommandError::bootstrap)?;
            let workspace_path = default_workspace_path(&app_data_dir);
            let home_dir = app.path().home_dir().ok().or_else(std::env::home_dir);
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(std::path::Path::to_path_buf));
            let local_app_data_dir = std::env::var_os("LOCALAPPDATA")
                .map(std::path::PathBuf::from)
                .or_else(|| app.path().local_data_dir().ok());
            let (mods_dir, mods_warning) = resolve_mods_dir(
                std::env::var_os("MERA_MODS_DIR").as_deref(),
                app_data_env.is_some(),
                &app_data_dir,
                home_dir.as_deref(),
                exe_dir.as_deref(),
                local_app_data_dir.as_deref(),
            )
            .map_err(TauriCommandError::bootstrap)?;
            if let Some(warning) = mods_warning {
                eprintln!("[mera] {warning}");
            }
            let state = tauri::async_runtime::block_on(DesktopHostState::bootstrap_with_roots(
                &app_data_dir,
                &workspace_path,
                &mods_dir,
            ))
            .map_err(|error| TauriCommandError::bootstrap(error.to_string()))?;
            app.manage(state);
            let adapter_state = tauri::async_runtime::block_on(mcp_dev::start(app.handle()));
            app.manage(adapter_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Existing commands
            health,
            #[cfg(feature = "aigc")]
            get_compute_profile,
            get_node_defs,
            #[cfg(feature = "aigc")]
            list_models,
            run_workflow,
            cancel_run,
            resolve_artifact,
            open_artifact,
            // Project / board commands (AR-39)
            list_projects,
            create_project,
            load_project,
            update_project,
            delete_project,
            set_active_project,
            subscribe_events,
            board_snapshot,
            preview_board_commands,
            apply_board_commands,
            undo_board,
            redo_board,
            // Workflow command commands
            preview_workflow_commands,
            apply_workflow_commands,
            approve_proposal,
            // Workflow persistence commands
            save_workflow,
            load_workflow,
            list_workflows,
            // Operation commands (W-a)
            op_list,
            op_invoke,
            op_preview,
            op_preview_cancel,
            // Mod manager commands (M2 & M3)
            mods_list,
            mods_open_dir,
            mods_preview_archive,
            mods_preview_directory,
            mods_install_archive,
            mods_install_directory,
            mods_set_enabled,
            mods_set_agent_visibility,
            mods_reload_local,
            mods_uninstall,
            mods_sources_list,
            mods_sources_add,
            mods_sources_set_enabled,
            mods_sources_remove,
            mods_browse,
            mods_download_start,
            mods_download_status,
            mods_download_cancel,
            mods_download_confirm_install,
            mods_runtime_status,
            mods_uv_bootstrap,
            // Model download commands
            #[cfg(feature = "aigc")]
            download_huggingface_model,
            // Catalog commands
            #[cfg(feature = "aigc")]
            search_models,
            #[cfg(feature = "aigc")]
            get_model_card,
            // Worker switch commands
            #[cfg(feature = "aigc")]
            drain_and_switch_worker,
            #[cfg(feature = "aigc")]
            cancel_and_switch_worker,
            #[cfg(feature = "aigc")]
            list_worker_switch_targets,
            // Re-bootstrap command
            #[cfg(feature = "aigc")]
            rebootstrap_backend,
        ])
        .build(tauri::generate_context!())
    {
        Ok(app) => app.run(|app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let adapter_state = app_handle.state::<mcp_dev::AgentAdapterState>();
                if !block_on_with_timeout(adapter_state.shutdown(), EXIT_SHUTDOWN_TIMEOUT) {
                    eprintln!("[mera] MCP dev connection shutdown timed out after 5s; continuing");
                }
                let state = app_handle.state::<DesktopHostState>();
                if !block_on_with_timeout(state.shutdown(), EXIT_SHUTDOWN_TIMEOUT) {
                    eprintln!("[tauri] worker and mod shutdown timed out after 5s; exiting");
                    std::process::exit(1);
                }
            }
        }),
        Err(error) => {
            eprintln!("[mera] failed to build the application: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::desktop_host::{DesktopHostState, default_workspace_path};
    use super::{TauriCommandError, app_host_command_error, block_on_with_timeout};
    use mera_app_host::dto::HealthDto;

    fn temp_dir(prefix: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("mera-tauri-host-{prefix}-{nonce}"))
    }

    #[test]
    fn workflow_save_list_load_roundtrip() {
        let base_path = temp_dir("workflow-persistence");
        let state = tauri::async_runtime::block_on(DesktopHostState::bootstrap(&base_path))
            .expect("desktop host state should bootstrap");

        let workflow_json = serde_json::json!({
            "schema_version": "mera.workflow.v1",
            "id": "test-workflow",
            "version": 1,
            "metadata": { "name": "Roundtrip" },
            "interface": { "inputs": [], "outputs": [] },
            "nodes": [{
                "id": "n1",
                "type_id": "model",
                "label": "Model",
                "params": { "seed": { "type": "integer", "value": 42 } }
            }],
            "edges": [],
            "layout": { "nodes": { "n1": { "x": 1.0, "y": 2.0 } } }
        });

        let saved_path =
            tauri::async_runtime::block_on(state.save_workflow("test-workflow", workflow_json))
                .expect("save workflow");
        assert!(
            saved_path.ends_with("test-workflow.json"),
            "expected path to end with test-workflow.json, got {}",
            saved_path.display()
        );

        let summaries = state.list_saved_workflows().expect("list workflows");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0]["id"], "test-workflow");

        let loaded = tauri::async_runtime::block_on(state.load_workflow_json("test-workflow"))
            .expect("load workflow");
        assert_eq!(loaded["id"], "test-workflow");
        assert_eq!(loaded["metadata"]["name"], "Roundtrip");
        assert_eq!(loaded["layout"]["nodes"]["n1"]["x"], 1.0);
        assert_eq!(
            loaded["nodes"][0]["params"]["seed"],
            serde_json::json!({ "type": "integer", "value": 42 })
        );

        let _ = std::fs::remove_dir_all(&base_path);
    }

    #[test]
    fn exit_shutdown_completes_without_a_tokio_context() {
        let base_path = temp_dir("exit-shutdown");
        let state = tauri::async_runtime::block_on(DesktopHostState::bootstrap(&base_path))
            .expect("desktop host state should bootstrap");

        // The Tauri exit callback runs on the UI thread with no Tokio context,
        // outside any runtime. Driving shutdown through the same helper must
        // complete without panicking, and a repeated call stays cheap.
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "the test must start outside a Tokio context, like the exit callback"
        );
        assert!(
            block_on_with_timeout(state.shutdown(), std::time::Duration::from_secs(5)),
            "shutdown should finish before the deadline"
        );
        assert!(
            block_on_with_timeout(state.shutdown(), std::time::Duration::from_secs(5)),
            "a repeated shutdown should also finish"
        );

        let _ = std::fs::remove_dir_all(&base_path);
    }

    #[test]
    fn exit_shutdown_returns_at_the_deadline() {
        let deadline = std::time::Duration::from_millis(10);
        let started = std::time::Instant::now();
        assert!(
            !block_on_with_timeout(std::future::pending(), deadline),
            "an unfinished shutdown must report the deadline"
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed >= deadline,
            "the deadline must be honored, not bypassed; returned after {elapsed:?}"
        );
        assert!(
            elapsed < deadline + std::time::Duration::from_secs(1),
            "the exit callback must return close to the deadline; took {elapsed:?}"
        );
    }

    #[test]
    fn project_board_workflow_surface_is_project_scoped() {
        let base_path = temp_dir("project-board-surface");
        let state = tauri::async_runtime::block_on(DesktopHostState::bootstrap(&base_path))
            .expect("desktop host state should bootstrap");
        let metadata = mera_app_host::dto::ProjectMetadataInputDto {
            name: "Project A".to_owned(),
            description: "A project".to_owned(),
            created_at: Some("2026-08-20T00:00:00Z".to_owned()),
            updated_at: Some("2026-08-20T00:00:00Z".to_owned()),
        };
        tauri::async_runtime::block_on(
            state.create_project("project-a".to_owned(), metadata.clone()),
        )
        .expect("project A creates");
        tauri::async_runtime::block_on(state.create_project("project-b".to_owned(), metadata))
            .expect("project B creates");
        let board = tauri::async_runtime::block_on(state.board_snapshot("project-a"))
            .expect("project A board loads");
        assert_eq!(board.project_id, "project-a");
        assert_eq!(board.version, 0);
        let workflow = serde_json::json!({
            "schema_version": "mera.workflow.v1",
            "id": "wf-a",
            "version": 1,
            "metadata": { "name": "Project A workflow" },
            "interface": { "inputs": [], "outputs": [] },
            "nodes": [],
            "edges": [],
            "layout": { "nodes": {} }
        });
        let path = tauri::async_runtime::block_on(state.save_workflow_for_project(
            "project-a",
            "wf-a",
            workflow,
        ))
        .expect("project A workflow saves");
        assert!(path.ends_with("projects/project-a/workflows/wf-a.json"));
        let loaded = tauri::async_runtime::block_on(
            state.load_workflow_json_for_project("project-a", "wf-a"),
        )
        .expect("project A workflow loads");
        assert_eq!(loaded["metadata"]["name"], "Project A workflow");
        tauri::async_runtime::block_on(state.delete_project("project-a"))
            .expect("project A deletes");
        assert!(!base_path.join("projects/project-a").exists());
        assert!(tauri::async_runtime::block_on(state.load_project("project-a")).is_err());
        assert!(tauri::async_runtime::block_on(state.board_snapshot("project-a")).is_err());
        assert!(
            tauri::async_runtime::block_on(
                state.load_workflow_json_for_project("project-a", "wf-a")
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(&base_path);
    }
    #[test]
    fn default_workspace_path_uses_app_data_workspace_child() {
        let app_data_dir = temp_dir("app-data");

        assert_eq!(
            default_workspace_path(&app_data_dir),
            app_data_dir.join("workspace")
        );
    }

    #[test]
    fn desktop_host_bootstrap_creates_workspace_layout_and_health() {
        let base_path = temp_dir("bootstrap");
        let state = tauri::async_runtime::block_on(DesktopHostState::bootstrap(&base_path))
            .expect("desktop host state should bootstrap");

        assert_eq!(state.workspace_base_path(), base_path);
        #[cfg(feature = "aigc")]
        assert!(
            state
                .worker_management()
                .list_installed()
                .expect("worker inventory")
                .is_empty()
        );
        assert!(base_path.join("models").is_dir());
        assert!(base_path.join("input").is_dir());
        assert!(base_path.join("output").is_dir());
        assert!(base_path.join("workflows").is_dir());
        assert!(base_path.join("config").is_dir());

        let health = state.health();
        assert_eq!(health.workspace, "mera-tauri-host");
        assert_eq!(health.kernel, HealthDto::Available);
        #[cfg(feature = "aigc")]
        assert_eq!(health.aigc, HealthDto::Available);
        #[cfg(not(feature = "aigc"))]
        assert!(matches!(health.aigc, HealthDto::Unavailable { .. }));

        let _ = std::fs::remove_dir_all(&base_path);
    }

    #[cfg(feature = "aigc")]
    #[test]
    fn desktop_host_compute_profile_comes_from_workspace_host() {
        let base_path = temp_dir("compute-profile");
        let state = tauri::async_runtime::block_on(DesktopHostState::bootstrap(&base_path))
            .expect("desktop host state should bootstrap");

        let profile = state.compute_profile();
        assert!(
            profile
                .backend_profiles
                .iter()
                .any(|backend| backend.backend == "burn"),
            "expected compute profile to include the app-host burn profile"
        );
        let total_instances = profile
            .backend_profiles
            .iter()
            .map(|bp| bp.instances.len())
            .sum::<usize>();
        assert_eq!(
            total_instances, 0,
            "profile has no available instances without an installed Burn worker"
        );

        let _ = std::fs::remove_dir_all(&base_path);
    }

    #[cfg(feature = "aigc")]
    #[test]
    fn rebootstrap_rejects_unsupported_candle_selection() {
        let result: Result<mera_app_host::BackendSelection, _> = serde_json::from_str("\"candle\"");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("candle"));
    }

    #[cfg(feature = "aigc")]
    #[test]
    fn desktop_host_rebootstrap_swaps_backend_and_keeps_workspace() {
        use mera_app_host::BackendSelection;

        let base_path = temp_dir("rebootstrap");
        let state = tauri::async_runtime::block_on(DesktopHostState::bootstrap(&base_path))
            .expect("desktop host state should bootstrap");

        let profile =
            tauri::async_runtime::block_on(state.rebootstrap_backend(BackendSelection::Burn))
                .expect("rebootstrap to burn");

        assert!(
            !profile.backend_profiles.is_empty(),
            "rebootstrap must return the rebuilt compute profile"
        );
        assert_eq!(state.workspace_base_path(), base_path);
        assert_eq!(state.health().kernel, HealthDto::Available);
        assert_eq!(state.health().aigc, HealthDto::Available);

        let _ = std::fs::remove_dir_all(&base_path);
    }

    // ─── Catalog command parameter validation tests ────────────────

    #[cfg(feature = "aigc")]
    #[test]
    fn search_models_query_parsing_default_filters() {
        use mera_app_host::dto::ModelFilters;
        use mera_model_acquisition::{ModelSearchQuery, SortBy};

        let filters = ModelFilters::default();
        let sort = match filters.sort.as_str() {
            "likes" => SortBy::Likes,
            "trending" => SortBy::Trending,
            "lastModified" => SortBy::LastModified,
            _ => SortBy::Downloads,
        };

        let query = ModelSearchQuery {
            search: None,
            pipeline_tag: filters.pipeline_tag,
            library_name: filters.library_name,
            tags: filters.tags,
            sort,
            limit: filters.limit,
        };

        assert!(query.search.is_none());
        assert!(query.pipeline_tag.is_none());
        assert!(query.library_name.is_none());
        assert!(query.tags.is_empty());
        assert_eq!(query.sort, SortBy::Downloads);
        assert_eq!(query.limit, 20);
    }

    #[cfg(feature = "aigc")]
    #[test]
    fn search_models_query_parsing_with_text_search() {
        use mera_app_host::dto::ModelFilters;
        use mera_model_acquisition::{ModelSearchQuery, SortBy};

        let query_text = "stable diffusion".to_string();
        let filters = ModelFilters {
            pipeline_tag: Some("text-to-image".to_string()),
            sort: "likes".to_string(),
            limit: 5,
            ..Default::default()
        };

        let sort = match filters.sort.as_str() {
            "likes" => SortBy::Likes,
            "trending" => SortBy::Trending,
            "lastModified" => SortBy::LastModified,
            _ => SortBy::Downloads,
        };

        let query = ModelSearchQuery {
            search: if query_text.is_empty() {
                None
            } else {
                Some(query_text)
            },
            pipeline_tag: filters.pipeline_tag,
            library_name: filters.library_name,
            tags: filters.tags,
            sort,
            limit: filters.limit,
        };

        assert_eq!(query.search.as_deref(), Some("stable diffusion"));
        assert_eq!(query.pipeline_tag.as_deref(), Some("text-to-image"));
        assert_eq!(query.sort, SortBy::Likes);
        assert_eq!(query.limit, 5);
    }

    #[cfg(feature = "aigc")]
    #[test]
    fn search_models_query_parsing_empty_search_becomes_none() {
        use mera_app_host::dto::ModelFilters;
        use mera_model_acquisition::{ModelSearchQuery, SortBy};

        let query_text = String::new();
        let filters = ModelFilters::default();
        let sort = SortBy::Downloads;

        let query = ModelSearchQuery {
            search: if query_text.is_empty() {
                None
            } else {
                Some(query_text)
            },
            pipeline_tag: filters.pipeline_tag,
            library_name: filters.library_name,
            tags: filters.tags,
            sort,
            limit: filters.limit,
        };

        assert!(query.search.is_none());
    }

    #[cfg(feature = "aigc")]
    #[test]
    fn model_card_dto_conversion() {
        use mera_app_host::dto::ModelCardDto;
        use mera_model_acquisition::{
            ModelCard, ModelCardData, ModelCatalogEntry, ModelRepoFormat,
        };

        let card = ModelCard {
            entry: ModelCatalogEntry {
                id: "org/model".to_string(),
                author: Some("org".to_string()),
                pipeline_tag: Some("text-to-image".to_string()),
                tags: vec!["diffusers".to_string()],
                downloads: 1000,
                likes: 50,
                last_modified: None,
                private: false,
            },
            siblings: vec![],
            card_data: Some(ModelCardData {
                model_summary: Some("A test model".to_string()),
                ..Default::default()
            }),
            detected_format: ModelRepoFormat::Diffusers,
            component_mapping: None,
            estimated_download_size: 1_000_000,
        };

        let dto = ModelCardDto::from(card);
        assert_eq!(dto.entry.id, "org/model");
        assert_eq!(dto.detected_format, "Diffusers");
        assert_eq!(dto.estimated_download_size, 1_000_000);
        assert_eq!(dto.model_summary.as_deref(), Some("A test model"));
        assert_eq!(dto.file_count, 0);
        assert!(dto.components.is_empty());
    }

    #[cfg(feature = "aigc")]
    #[test]
    fn model_catalog_entry_dto_conversion() {
        use mera_app_host::dto::ModelCatalogEntryDto;
        use mera_model_acquisition::ModelCatalogEntry;

        let entry = ModelCatalogEntry {
            id: "org/model".to_string(),
            author: Some("org".to_string()),
            pipeline_tag: Some("text-to-image".to_string()),
            tags: vec!["diffusers".to_string(), "safetensors".to_string()],
            downloads: 5000,
            likes: 100,
            last_modified: Some("2025-01-01T00:00:00Z".to_string()),
            private: false,
        };

        let dto = ModelCatalogEntryDto::from(entry);
        assert_eq!(dto.id, "org/model");
        assert_eq!(dto.author.as_deref(), Some("org"));
        assert_eq!(dto.downloads, 5000);
        assert_eq!(dto.likes, 100);
        assert_eq!(dto.tags.len(), 2);
        assert!(!dto.private);
    }

    #[cfg(feature = "aigc")]
    #[test]
    fn model_filters_default_values() {
        use mera_app_host::dto::ModelFilters;

        let filters = ModelFilters::default();
        assert!(filters.pipeline_tag.is_none());
        assert!(filters.library_name.is_none());
        assert!(filters.tags.is_empty());
        assert_eq!(filters.sort, "downloads");
        assert_eq!(filters.limit, 20);
    }

    // ─── BE-31 structured error payload tests ────────────────────

    #[test]
    fn command_error_serializes_structured_payload() {
        let error = TauriCommandError::command("boom");
        let value = serde_json::to_value(&error).expect("serialize");
        assert_eq!(value["code"], "command_failed");
        assert_eq!(value["message"], "boom");
        assert!(
            value.get("details").is_none(),
            "details should be omitted when absent"
        );
    }

    #[test]
    fn bootstrap_error_serializes_with_bootstrap_code() {
        let error = TauriCommandError::bootstrap("setup failed");
        let value = serde_json::to_value(&error).expect("serialize");
        assert_eq!(value["code"], "bootstrap_failed");
    }

    #[test]
    fn app_host_errors_map_to_structured_codes() {
        use mera_app_host::AppHostError;
        use mera_app_host::AppHostErrorCode;
        use mera_core::model::RunId;

        let error = AppHostError::UnknownRun {
            run_id: RunId::new("run_x"),
        };
        let mapped = app_host_command_error(error);
        assert_eq!(mapped.code, AppHostErrorCode::NotFound);
        assert_eq!(
            mapped.details.as_ref().expect("run details")["run_id"],
            "run_x"
        );

        let mapped = app_host_command_error(AppHostError::WorkflowJson {
            path: std::path::PathBuf::new(),
            message: "bad json".to_owned(),
        });
        assert_eq!(mapped.code, AppHostErrorCode::WorkflowInvalid);
    }

    #[cfg(feature = "aigc")]
    #[test]
    fn worker_switch_errors_map_to_worker_unavailable() {
        use mera_app_host::{AppHostError, AppHostErrorCode, WorkerSwitchError};

        let mapped = app_host_command_error(AppHostError::WorkerSwitch(
            WorkerSwitchError::NoActiveWorker,
        ));
        assert_eq!(mapped.code, AppHostErrorCode::WorkerUnavailable);
        assert_eq!(
            mapped.message,
            WorkerSwitchError::NoActiveWorker.to_string()
        );
        let value = serde_json::to_value(&mapped).expect("serialize");
        assert_eq!(value["code"], "worker_unavailable");
    }

    #[cfg(feature = "aigc")]
    #[test]
    fn switch_deadline_validates_input() {
        assert!(super::switch_deadline(None).is_ok());
        assert!(super::switch_deadline(Some(5.0)).is_ok());
        assert!(super::switch_deadline(Some(0.0)).is_err());
        assert!(super::switch_deadline(Some(-1.0)).is_err());
    }
    // ─── W-a operation & asset URI tests ─────────────────────────────────────────

    #[test]
    fn parse_asset_uri_validates_and_parses() {
        use super::{ParsedAssetUri, parse_asset_uri};

        // Project asset URI
        let uri: tauri::http::Uri = "mera-asset://my-project/asset-123".parse().unwrap();
        assert_eq!(
            parse_asset_uri(&uri).unwrap(),
            ParsedAssetUri::Asset {
                project: "my-project".to_string(),
                asset_id: "asset-123".to_string(),
            }
        );

        // Preview URI
        let uri: tauri::http::Uri = "mera-asset://preview/key-1/sub/image.png".parse().unwrap();
        assert_eq!(
            parse_asset_uri(&uri).unwrap(),
            ParsedAssetUri::Preview {
                key: "key-1".to_string(),
                relative_path: "sub/image.png".to_string(),
            }
        );

        // Another valid project URI
        let uri: tauri::http::Uri = "mera-asset://another-project/asset-456".parse().unwrap();
        assert_eq!(
            parse_asset_uri(&uri).unwrap(),
            ParsedAssetUri::Asset {
                project: "another-project".to_string(),
                asset_id: "asset-456".to_string(),
            }
        );

        // Windows/Android forms
        let uri: tauri::http::Uri = "http://mera-asset.localhost/my-project/asset-123"
            .parse()
            .unwrap();
        assert_eq!(
            parse_asset_uri(&uri).unwrap(),
            ParsedAssetUri::Asset {
                project: "my-project".to_string(),
                asset_id: "asset-123".to_string(),
            }
        );

        let uri: tauri::http::Uri = "https://mera-asset.localhost/my-project/asset-123"
            .parse()
            .unwrap();
        assert_eq!(
            parse_asset_uri(&uri).unwrap(),
            ParsedAssetUri::Asset {
                project: "my-project".to_string(),
                asset_id: "asset-123".to_string(),
            }
        );

        let uri: tauri::http::Uri = "http://mera-asset.localhost/preview/key-1/sub/image.png"
            .parse()
            .unwrap();
        assert_eq!(
            parse_asset_uri(&uri).unwrap(),
            ParsedAssetUri::Preview {
                key: "key-1".to_string(),
                relative_path: "sub/image.png".to_string(),
            }
        );

        let uri: tauri::http::Uri = "https://mera-asset.localhost/preview/key-1/sub/image.png"
            .parse()
            .unwrap();
        assert_eq!(
            parse_asset_uri(&uri).unwrap(),
            ParsedAssetUri::Preview {
                key: "key-1".to_string(),
                relative_path: "sub/image.png".to_string(),
            }
        );

        let uri: tauri::http::Uri = "mera-asset://localhost/my-project/asset-123"
            .parse()
            .unwrap();
        assert_eq!(
            parse_asset_uri(&uri).unwrap(),
            ParsedAssetUri::Asset {
                project: "my-project".to_string(),
                asset_id: "asset-123".to_string(),
            }
        );

        // Global library asset URI forms
        let uri: tauri::http::Uri = "mera-asset://library/asset-123".parse().unwrap();
        assert_eq!(
            parse_asset_uri(&uri).unwrap(),
            ParsedAssetUri::Library {
                asset_id: "asset-123".to_string(),
            }
        );

        let uri: tauri::http::Uri = "mera-asset://localhost/library/asset-123".parse().unwrap();
        assert_eq!(
            parse_asset_uri(&uri).unwrap(),
            ParsedAssetUri::Library {
                asset_id: "asset-123".to_string(),
            }
        );

        let uri: tauri::http::Uri = "http://mera-asset.localhost/library/asset-123"
            .parse()
            .unwrap();
        assert_eq!(
            parse_asset_uri(&uri).unwrap(),
            ParsedAssetUri::Library {
                asset_id: "asset-123".to_string(),
            }
        );

        let uri: tauri::http::Uri = "https://mera-asset.localhost/library/asset-123"
            .parse()
            .unwrap();
        assert_eq!(
            parse_asset_uri(&uri).unwrap(),
            ParsedAssetUri::Library {
                asset_id: "asset-123".to_string(),
            }
        );

        // Missing / invalid library asset id
        let uri: tauri::http::Uri = "mera-asset://library/".parse().unwrap();
        assert!(parse_asset_uri(&uri).is_err());

        let uri: tauri::http::Uri = "mera-asset://library/nested/asset".parse().unwrap();
        assert!(parse_asset_uri(&uri).is_err());

        let uri: tauri::http::Uri = "http://mera-asset.localhost/library/nested/asset"
            .parse()
            .unwrap();
        assert!(parse_asset_uri(&uri).is_err());

        // Invalid scheme / host
        let uri: tauri::http::Uri = "http://localhost/asset-123".parse().unwrap();
        assert!(parse_asset_uri(&uri).is_err());

        let uri: tauri::http::Uri = "http://other.localhost/my-project/asset-123"
            .parse()
            .unwrap();
        assert!(parse_asset_uri(&uri).is_err());

        // Missing relative path for preview
        let uri: tauri::http::Uri = "mera-asset://preview/key-only".parse().unwrap();
        assert!(parse_asset_uri(&uri).is_err());

        let uri: tauri::http::Uri = "http://mera-asset.localhost/preview/key-only"
            .parse()
            .unwrap();
        assert!(parse_asset_uri(&uri).is_err());
    }

    #[test]
    fn op_commands_and_asset_uri_protocol_serving() {
        use super::{ParsedAssetUri, handle_asset_uri_request, parse_asset_uri};
        use mera_app_host::dto::ProjectMetadataInputDto;

        let base_path = temp_dir("desktop-ops");
        let state = tauri::async_runtime::block_on(DesktopHostState::bootstrap(&base_path))
            .expect("desktop host state should bootstrap");

        // 1. op_list returns kernel ops including asset.import
        let ops =
            tauri::async_runtime::block_on(state.op_list("default")).expect("op_list succeeds");
        assert!(ops.iter().any(|op| op.spec.name == "asset.import"));

        // Create a test project
        let project_dto = tauri::async_runtime::block_on(state.create_project(
            "test-op-proj".to_string(),
            ProjectMetadataInputDto {
                name: "Test Op Proj".to_string(),
                description: "desc".to_string(),
                created_at: None,
                updated_at: None,
            },
        ))
        .expect("create project");
        assert_eq!(project_dto.id, "test-op-proj");

        // 2. Prepare an input file and invoke asset.import
        let input_path = base_path.join("sample.txt");
        std::fs::write(&input_path, b"hello asset uri").unwrap();

        let invoke_res = tauri::async_runtime::block_on(state.op_invoke(
            "test-op-proj",
            "asset.import".to_string(),
            serde_json::json!({ "paths": [input_path.to_str().unwrap()] }),
        ))
        .expect("op_invoke asset.import succeeds");

        // Actor must be Human
        assert_eq!(
            invoke_res.step.context.actor().kind(),
            mera_core::command::CommandActorKind::Human
        );

        let asset_id = invoke_res.output["assets"][0].as_str().unwrap();

        // 3. Test mera-asset://<project>/<asset id>
        let asset_uri: tauri::http::Uri = format!("mera-asset://test-op-proj/{asset_id}")
            .parse()
            .unwrap();
        let resp = handle_asset_uri_request(&state, &asset_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::OK);
        assert_eq!(resp.headers().get("Content-Type").unwrap(), "text/plain");
        assert_eq!(resp.body(), b"hello asset uri");

        // 4. Test 404 for nonexistent asset
        let missing_uri: tauri::http::Uri = "mera-asset://test-op-proj/asset-nonexistent"
            .parse()
            .unwrap();
        let resp = handle_asset_uri_request(&state, &missing_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::NOT_FOUND);

        // 5. Test 404 for unknown project
        let unknown_proj_uri: tauri::http::Uri = format!("mera-asset://unknown-proj/{asset_id}")
            .parse()
            .unwrap();
        let resp = handle_asset_uri_request(&state, &unknown_proj_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::NOT_FOUND);

        // 5b. Test 400 for malformed asset id, project id, or preview key
        let bad_asset_uri: tauri::http::Uri = "mera-asset://test-op-proj/sub/nested/asset"
            .parse()
            .unwrap();
        let resp = handle_asset_uri_request(&state, &bad_asset_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::BAD_REQUEST);

        let bad_proj_uri: tauri::http::Uri = format!("mera-asset://../{asset_id}").parse().unwrap();
        let resp = handle_asset_uri_request(&state, &bad_proj_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::BAD_REQUEST);

        let bad_preview_uri: tauri::http::Uri = "mera-asset://preview/../escape/preview.png"
            .parse()
            .unwrap();
        let resp = handle_asset_uri_request(&state, &bad_preview_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::BAD_REQUEST);

        // 6. Test preview serving
        let preview_key = "test-preview-key";
        let workspace = state.workspace_arc();
        let preview_dir = workspace
            .operation_service()
            .preview_dir(preview_key)
            .unwrap();
        std::fs::create_dir_all(&preview_dir).unwrap();
        let preview_file = preview_dir.join("preview.png");
        std::fs::write(&preview_file, b"\x89PNG\r\n\x1a\n").unwrap();

        let preview_uri: tauri::http::Uri =
            format!("mera-asset://preview/{preview_key}/preview.png")
                .parse()
                .unwrap();
        let resp = handle_asset_uri_request(&state, &preview_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::OK);
        assert_eq!(resp.headers().get("Content-Type").unwrap(), "image/png");
        assert_eq!(resp.headers().get("Cache-Control").unwrap(), "no-store");
        assert_eq!(resp.body(), b"\x89PNG\r\n\x1a\n");

        // 6b. The cache-busting query is ignored by the parser and still served
        let preview_uri_busted: tauri::http::Uri =
            format!("mera-asset://preview/{preview_key}/preview.png?v=7")
                .parse()
                .unwrap();
        assert_eq!(
            parse_asset_uri(&preview_uri_busted).unwrap(),
            ParsedAssetUri::Preview {
                key: preview_key.to_string(),
                relative_path: "preview.png".to_string(),
            }
        );
        let resp = handle_asset_uri_request(&state, &preview_uri_busted, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::OK);
        assert_eq!(resp.body(), b"\x89PNG\r\n\x1a\n");

        // 7. Test preview symlink rejection (forbidden)
        #[cfg(unix)]
        {
            let symlink_path = preview_dir.join("sym.png");
            let _ = std::os::unix::fs::symlink(&preview_file, &symlink_path);
            let sym_uri: tauri::http::Uri = format!("mera-asset://preview/{preview_key}/sym.png")
                .parse()
                .unwrap();
            let resp = handle_asset_uri_request(&state, &sym_uri, None);
            assert_eq!(resp.status(), tauri::http::StatusCode::FORBIDDEN);
        }

        // 8. Test Windows/Android localhost URI forms
        let win_asset_uri: tauri::http::Uri =
            format!("http://mera-asset.localhost/test-op-proj/{asset_id}")
                .parse()
                .unwrap();
        let resp = handle_asset_uri_request(&state, &win_asset_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::OK);
        assert_eq!(resp.body(), b"hello asset uri");

        let win_preview_uri: tauri::http::Uri =
            format!("http://mera-asset.localhost/preview/{preview_key}/preview.png")
                .parse()
                .unwrap();
        let resp = handle_asset_uri_request(&state, &win_preview_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::OK);
        assert_eq!(resp.body(), b"\x89PNG\r\n\x1a\n");

        // 9. Test CORS origin echo only for exact allowed origins
        let allowed_origins = [
            "tauri://localhost",
            "http://tauri.localhost",
            "https://tauri.localhost",
            "http://localhost:1420",
        ];
        for origin in allowed_origins {
            let resp = handle_asset_uri_request(&state, &asset_uri, Some(origin));
            assert_eq!(resp.status(), tauri::http::StatusCode::OK);
            assert_eq!(
                resp.headers().get("Access-Control-Allow-Origin").unwrap(),
                origin
            );
            assert_eq!(resp.headers().get("Vary").unwrap(), "Origin");
        }

        // Disallowed origins must not get Access-Control-Allow-Origin
        let disallowed_origins = [
            "http://evil.com",
            "http://localhost:3000",
            "*",
            "https://mera.app",
        ];
        for origin in disallowed_origins {
            let resp = handle_asset_uri_request(&state, &asset_uri, Some(origin));
            assert_eq!(resp.status(), tauri::http::StatusCode::OK);
            assert!(resp.headers().get("Access-Control-Allow-Origin").is_none());
            assert_eq!(resp.headers().get("Vary").unwrap(), "Origin");
        }

        // None origin must not get Access-Control-Allow-Origin
        let resp = handle_asset_uri_request(&state, &asset_uri, None);
        assert!(resp.headers().get("Access-Control-Allow-Origin").is_none());
        assert_eq!(resp.headers().get("Vary").unwrap(), "Origin");

        // 10. Test global library asset serving
        let lib_file = base_path.join("lib_sample.txt");
        std::fs::write(&lib_file, b"library asset content").unwrap();

        let lib_add_res = tauri::async_runtime::block_on(state.op_invoke(
            "test-op-proj",
            "library.add".to_string(),
            serde_json::json!({ "paths": [lib_file.to_str().unwrap()] }),
        ))
        .expect("library.add succeeds");
        let lib_asset_id = lib_add_res.output["items"][0]["id"].as_str().unwrap();

        // 10a. Custom scheme mera-asset://library/<id>
        let lib_uri: tauri::http::Uri = format!("mera-asset://library/{lib_asset_id}")
            .parse()
            .unwrap();
        let resp = handle_asset_uri_request(&state, &lib_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::OK);
        assert_eq!(resp.headers().get("Content-Type").unwrap(), "text/plain");
        assert_eq!(resp.body(), b"library asset content");

        // 10b. Localhost forms
        let lib_local_uri: tauri::http::Uri =
            format!("mera-asset://localhost/library/{lib_asset_id}")
                .parse()
                .unwrap();
        let resp = handle_asset_uri_request(&state, &lib_local_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::OK);
        assert_eq!(resp.body(), b"library asset content");

        let lib_win_uri: tauri::http::Uri =
            format!("http://mera-asset.localhost/library/{lib_asset_id}")
                .parse()
                .unwrap();
        let resp = handle_asset_uri_request(&state, &lib_win_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::OK);
        assert_eq!(resp.body(), b"library asset content");

        // 10c. Nonexistent library asset returns 404
        let lib_missing_uri: tauri::http::Uri =
            "mera-asset://library/asset-nonexistent".parse().unwrap();
        let resp = handle_asset_uri_request(&state, &lib_missing_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::NOT_FOUND);

        // 10d. Removed but not yet purged library asset is still served
        tauri::async_runtime::block_on(state.op_invoke(
            "test-op-proj",
            "library.remove".to_string(),
            serde_json::json!({ "ids": [lib_asset_id] }),
        ))
        .expect("library.remove succeeds");
        let resp = handle_asset_uri_request(&state, &lib_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::OK);
        assert_eq!(resp.body(), b"library asset content");

        // 10e. Purged library asset returns 404
        tauri::async_runtime::block_on(state.op_invoke(
            "test-op-proj",
            "library.purge".to_string(),
            serde_json::json!({}),
        ))
        .expect("library.purge succeeds");
        let resp = handle_asset_uri_request(&state, &lib_uri, None);
        assert_eq!(resp.status(), tauri::http::StatusCode::NOT_FOUND);

        // 11. Test reserved project IDs "library" and "preview" are rejected
        let res = tauri::async_runtime::block_on(state.create_project(
            "library".to_string(),
            ProjectMetadataInputDto {
                name: "Library Project".to_string(),
                description: "".to_string(),
                created_at: None,
                updated_at: None,
            },
        ));
        assert!(res.is_err());

        let res = tauri::async_runtime::block_on(state.create_project(
            "preview".to_string(),
            ProjectMetadataInputDto {
                name: "Preview Project".to_string(),
                description: "".to_string(),
                created_at: None,
                updated_at: None,
            },
        ));
        assert!(res.is_err());

        let _ = std::fs::remove_dir_all(&base_path);
        let _ = std::fs::remove_dir_all(&preview_dir);
    }

    #[test]
    fn desktop_mod_commands_list_preview_and_guard_consent() {
        let base_path = temp_dir("desktop-mods");
        let state = tauri::async_runtime::block_on(DesktopHostState::bootstrap(&base_path))
            .expect("desktop host state should bootstrap");

        // 1. Initially empty mods list carries the isolated mods_root.
        let list = state.mods_list();
        assert!(list.mods.is_empty());
        assert!(list.mods_root.ends_with("mods"));

        // 2. Prepare an unpacked mod directory for static preview & consent check.
        let dev_dir = base_path.join("sample-dev-mod");
        std::fs::create_dir_all(dev_dir.join("bin")).unwrap();
        std::fs::write(dev_dir.join("bin/sample"), b"placeholder").unwrap();
        std::fs::write(
            dev_dir.join("mod-package.json"),
            serde_json::json!({
                "schema_version": 1,
                "id": "sample_ext",
                "version": "0.1.0",
                "target": "noarch",
                "api": "0.1",
                "entrypoint": { "kind": "binary", "path": "bin/sample" },
                "permissions": { "recommended_agent_operations": ["sample_ext.run"] }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dev_dir.join("manifest.json"),
            serde_json::json!({
                "id": "sample_ext",
                "name": "Sample Ext",
                "version": "0.1.0",
                "api": "0.1",
                "operations": [{
                    "name": "sample_ext.run",
                    "description": "Run sample",
                    "input_schema": { "type": "object", "properties": {} },
                    "output_schema": { "type": "object", "properties": {} },
                    "annotations": {
                        "read_only": false,
                        "destructive": false,
                        "idempotent": false,
                        "open_world": false,
                        "deterministic": true
                    }
                }],
                "events": []
            })
            .to_string(),
        )
        .unwrap();

        let preview = state
            .mods_preview_directory(dev_dir.to_str().unwrap())
            .expect("static directory preview succeeds");
        assert_eq!(preview.id, "sample_ext");
        assert!(preview.requires_explicit_consent);
        assert!(preview.operations[0].recommended_for_agent);

        let unconsented = tauri::async_runtime::block_on(state.mods_install_directory(
            dev_dir.to_str().unwrap(),
            false,
            false,
            true,
            vec![],
        ))
        .expect_err("missing consent must be rejected");
        assert_eq!(
            unconsented.code(),
            mera_app_host::AppHostErrorCode::PermissionDenied
        );

        let unknown_toggle =
            tauri::async_runtime::block_on(state.mods_set_enabled("missing_mod", true))
                .expect_err("unknown mod toggle must fail");
        assert_eq!(
            unknown_toggle.code(),
            mera_app_host::AppHostErrorCode::NotFound
        );

        // 3. M3 remote sources, runtime status, and download cancel guards.
        let sources_initial = state.mods_sources_list().expect("sources list");
        assert!(sources_initial.third_party.is_empty());

        let bad_source = state
            .mods_sources_add("Insecure", "http://example.com/index.json", None)
            .expect_err("non-loopback http source must be rejected");
        assert_eq!(
            bad_source.code(),
            mera_app_host::AppHostErrorCode::CommandFailed
        );

        let added_src = state
            .mods_sources_add("Community", "https://example.com/index.json", None)
            .expect("valid https source added");
        let disabled_src = state
            .mods_sources_set_enabled(&added_src.id, false)
            .expect("disable source");
        assert!(!disabled_src.enabled);
        state
            .mods_sources_remove(&added_src.id)
            .expect("remove source");

        let runtime = state.mods_runtime_status();
        #[cfg(target_os = "macos")]
        {
            assert_eq!(runtime.sandbox_backend, "seatbelt");
            assert!(runtime.sandbox_enforced);
        }
        assert!(state.mods_download_cancel("missing-task").is_err());

        let _ = std::fs::remove_dir_all(&base_path);
    }
}
