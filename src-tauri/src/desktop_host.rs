use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use mera_app_host::WorkspaceScope;
use mera_app_host::dto::{
    BoardCommandResultDto, BoardSnapshotDto, HealthResponse, ModBrowseResponseDto,
    ModDownloadTaskDto, ModEntryDto, ModInstallPreviewDto, ModListResponseDto,
    ModRuntimeEnvironmentDto, ModSourcesListDto, ProjectDto, ProjectMetadataInputDto,
    RemoteModCatalogItemDto, StorageCleanResultDto, StoragePreflightInputDto,
    StoragePreflightResultDto, StorageStatusDto, StorageWriteKind, ThirdPartySourceDto,
};
use mera_app_host::{
    AppHost, AppHostError, DiskSpaceProvider, FileHistoryScanStatus, FileSnapshotRecord,
    Fs2DiskSpaceProvider, InstallModRequest, InvokeRequest, InvokeResult, ModInstallSource,
    ModStorePaths, OperationInfo, RestoreOutcome, ScanOptions, SnapshotSource, WorkerStoragePaths,
    WorkspaceHost, WorkspaceHostBuilder,
    mod_manager::{InstalledModRecord, ModDownloadStatus, default_uv_archive_url},
};
use mera_config::AppPaths;
use mera_core::board::BoardCommand;
use mera_core::command::{
    CommandActor, CommandActorKind, CommandBatch, CommandProvenance, OperationContext,
};
use mera_core::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
    DiagnosticTargetDomain,
};
use mera_core::event::{Event, EventBus};
use mera_core::model::{
    DiagnosticId, FileSnapshotId, HistoryEntryId, ProjectId, WorkspaceFolderId,
};
use mera_core::project::WorkspaceFolder;
use mera_core::task::CancelToken;
use mera_sandbox::detect_sandbox_backend;
use mod_sdk::{ModRegistry, ProcessMod, ProcessModSpec};
use tauri::ipc::Channel;

use crate::event_bridge::TauriEventBridge;

const WORKSPACE_SCOPE: &str = "mera-tauri-host";
const APP_HOST_LOCK: &str = "desktop app_host lock poisoned";

/// Names an explicit `text-mod` binary; must point at an existing file.
const TEXT_MOD_ENV: &str = "MERA_TEXT_MOD";
/// Names an explicit app-data directory override; must be an absolute path.
const APP_DATA_DIR_ENV: &str = "MERA_APP_DATA_DIR";
/// Names an explicit mods directory override; must be an absolute path.
const MODS_DIR_ENV: &str = "MERA_MODS_DIR";
/// The mod binary shipped beside the desktop executable.
const TEXT_MOD_NAME: &str = "text-mod";
/// Bounds the mod's initialize handshake and `mod/start` round-trip.
const TEXT_MOD_STARTUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Resolve the text-mod binary: `MERA_TEXT_MOD` when it names a file,
/// otherwise `text-mod` beside the current executable (`.exe` on Windows).
fn text_mod_candidate(env: Option<&std::ffi::OsStr>, exe_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(value) = env {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
        eprintln!(
            "[mera] {TEXT_MOD_ENV} (`{}`) is not a file; looking beside the executable",
            path.display()
        );
    }
    let candidate = exe_dir?.join(format!("{TEXT_MOD_NAME}{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}

fn resolve_text_mod_path() -> Option<PathBuf> {
    let env = std::env::var_os(TEXT_MOD_ENV);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    text_mod_candidate(env.as_deref(), exe_dir.as_deref())
}

/// Start the bundled text mod into `registry` when no installed `text` mod
/// is active and the bundled binary is present.
///
/// A missing binary or a failed spawn never blocks application startup: the
/// host continues without bundled text operations, and a mod that spawned
/// but failed `mod/start` stays registered as unavailable. The child is
/// stopped by [`WorkspaceHost::shutdown`] on exit.
async fn register_bundled_text_mod_if_missing(registry: &ModRegistry, event_bus: &EventBus) {
    if registry
        .list()
        .iter()
        .any(|entry| entry.manifest.id == "text")
    {
        return;
    }
    let Some(program) = resolve_text_mod_path() else {
        eprintln!("[mera] text mod not found; text operations are unavailable");
        return;
    };
    let spec = ProcessModSpec {
        startup_timeout: TEXT_MOD_STARTUP_TIMEOUT,
        ..ProcessModSpec::new(&program)
    };
    let process_mod = match ProcessMod::spawn(spec).await {
        Ok(process_mod) => process_mod,
        Err(diagnostic) => {
            eprintln!(
                "[mera] failed to spawn the text mod at `{}`: {}",
                program.display(),
                diagnostic.message()
            );
            return;
        }
    };
    match registry
        .register_and_start(Arc::new(process_mod), event_bus)
        .await
    {
        Ok(entry) if entry.health.is_available() => {}
        _ => {
            eprintln!(
                "[mera] the text mod at `{}` did not start; text operations are unavailable",
                program.display()
            );
        }
    }
}

/// Create the workspace event bus and the bridge that forwards its events to
/// subscribed Tauri channels.
fn event_pipeline() -> (EventBus, TauriEventBridge) {
    let event_bus = EventBus::new();
    let event_bridge = TauriEventBridge::new();

    let mut event_rx = event_bus.subscribe();
    let bridge_forwarder = event_bridge.clone();
    tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => bridge_forwarder.broadcast(&event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    (event_bus, event_bridge)
}

#[derive(Debug)]
pub struct DesktopHostState {
    app_host: RwLock<AppHost>,
    event_bridge: TauriEventBridge,
    active_project: RwLock<ProjectId>,
}

impl Clone for DesktopHostState {
    fn clone(&self) -> Self {
        Self {
            app_host: RwLock::new(self.app_host.read().expect(APP_HOST_LOCK).clone()),
            event_bridge: self.event_bridge.clone(),
            active_project: RwLock::new(self.active_project.read().expect(APP_HOST_LOCK).clone()),
        }
    }
}

impl DesktopHostState {
    /// Bootstrap without resolving or spawning a text mod.
    ///
    /// Test-only: the production path discovers `text-mod` beside the
    /// executable or through `MERA_TEXT_MOD`, which would couple tests to
    /// the developer's environment and could start a real child process.
    #[cfg(test)]
    pub async fn bootstrap(workspace_base_path: impl Into<PathBuf>) -> Result<Self, AppHostError> {
        let workspace_base_path = workspace_base_path.into();
        let app_data_root = workspace_base_path.with_extension("app-data");
        let mods_root = app_data_root.join("mods");
        AppPaths::new(&workspace_base_path).ensure_all().await?;
        let (event_bus, event_bridge) = event_pipeline();
        Self::from_parts(
            app_data_root,
            workspace_base_path,
            mods_root,
            event_bus,
            event_bridge,
            Arc::new(ModRegistry::empty()),
            Arc::new(Fs2DiskSpaceProvider),
        )
        .await
    }

    #[cfg(test)]
    pub async fn bootstrap_with_disk_space(
        app_data_root: impl Into<PathBuf>,
        workspace_base_path: impl Into<PathBuf>,
        mods_root: impl Into<PathBuf>,
        available_bytes: u64,
        total_bytes: u64,
    ) -> Result<Self, AppHostError> {
        #[derive(Debug)]
        struct FixedDisk(mera_app_host::DiskSpaceInfo);
        impl DiskSpaceProvider for FixedDisk {
            fn disk_space(
                &self,
                _path: &Path,
            ) -> mera_app_host::AppHostResult<mera_app_host::DiskSpaceInfo> {
                Ok(self.0)
            }
        }
        let app_data_root = app_data_root.into();
        let workspace_base_path = workspace_base_path.into();
        let mods_root = mods_root.into();
        AppPaths::new(&workspace_base_path).ensure_all().await?;
        let (event_bus, event_bridge) = event_pipeline();
        Self::from_parts(
            app_data_root,
            workspace_base_path,
            mods_root,
            event_bus,
            event_bridge,
            Arc::new(ModRegistry::empty()),
            Arc::new(FixedDisk(mera_app_host::DiskSpaceInfo {
                available_bytes,
                total_bytes,
            })),
        )
        .await
    }

    pub async fn bootstrap_with_roots(
        app_data_root: impl Into<PathBuf>,
        workspace_base_path: impl Into<PathBuf>,
        mods_root: impl Into<PathBuf>,
    ) -> Result<Self, AppHostError> {
        let app_data_root = app_data_root.into();
        let workspace_base_path = workspace_base_path.into();
        let mods_root = mods_root.into();
        AppPaths::new(&workspace_base_path).ensure_all().await?;
        let (event_bus, event_bridge) = event_pipeline();
        let mod_registry = Arc::new(ModRegistry::empty());
        let state = Self::from_parts(
            app_data_root,
            workspace_base_path,
            mods_root,
            event_bus.clone(),
            event_bridge,
            Arc::clone(&mod_registry),
            Arc::new(Fs2DiskSpaceProvider),
        )
        .await?;
        if !state.has_installed_mod("text") {
            register_bundled_text_mod_if_missing(&mod_registry, &event_bus).await;
        }
        Ok(state)
    }

    /// Build the host from an ensured workspace, a running event pipeline and
    /// a mod registry.
    async fn from_parts(
        app_data_root: PathBuf,
        workspace_base_path: PathBuf,
        mods_root: PathBuf,
        event_bus: EventBus,
        event_bridge: TauriEventBridge,
        mod_registry: Arc<ModRegistry>,
        disk_space: Arc<dyn DiskSpaceProvider>,
    ) -> Result<Self, AppHostError> {
        let workspace =
            WorkspaceHostBuilder::new(WorkspaceScope::new(WORKSPACE_SCOPE), &workspace_base_path)
                .app_data_root(&app_data_root)
                .mods_root(&mods_root)
                .worker_storage_paths(WorkerStoragePaths::new(&app_data_root))
                .event_bus(event_bus)
                .mod_registry(mod_registry)
                .disk_space_provider(disk_space)
                .build()
                .await?;

        workspace.mod_manager().register_bundled_mod_id("text");
        workspace.mod_manager().load_enabled_mods_at_startup().await;

        Ok(Self {
            app_host: RwLock::new(AppHost::new(workspace)),
            event_bridge,
            active_project: RwLock::new(ProjectId::new("default")),
        })
    }

    #[cfg(test)]
    pub fn workspace_base_path(&self) -> PathBuf {
        self.app_host
            .read()
            .expect(APP_HOST_LOCK)
            .workspace()
            .base_path()
            .to_path_buf()
    }

    pub fn active_project_id(&self) -> ProjectId {
        self.active_project.read().expect(APP_HOST_LOCK).clone()
    }

    pub fn subscribe_events(&self, channel: Channel<Event>) {
        self.event_bridge.subscribe(channel);
    }

    pub async fn list_projects(&self) -> Result<Vec<ProjectDto>, AppHostError> {
        let workspace = self.workspace_arc();
        Ok(workspace
            .project_service()
            .list_projects()
            .await?
            .into_iter()
            .map(ProjectDto::from)
            .collect())
    }

    pub async fn create_project(
        &self,
        project_id: String,
        metadata: ProjectMetadataInputDto,
    ) -> Result<ProjectDto, AppHostError> {
        let project_id = parse_project_id(&project_id)?;
        let now = jiff::Timestamp::now().to_string();
        let workspace = self.workspace_arc();
        let project = workspace
            .project_service()
            .create_project(project_id, metadata.into_domain(&now))
            .await?;
        Ok(project.into())
    }

    pub async fn load_project(&self, project_id: &str) -> Result<ProjectDto, AppHostError> {
        let project_id = parse_project_id(project_id)?;
        let workspace = self.workspace_arc();
        Ok(workspace
            .project_service()
            .load_project(&project_id)
            .await?
            .into())
    }

    pub async fn update_project(
        &self,
        project_id: &str,
        metadata: ProjectMetadataInputDto,
    ) -> Result<ProjectDto, AppHostError> {
        let project_id = parse_project_id(project_id)?;
        let now = jiff::Timestamp::now().to_string();
        let workspace = self.workspace_arc();
        let current = workspace
            .project_service()
            .load_project(&project_id)
            .await?;
        Ok(workspace
            .project_service()
            .update_project(
                &project_id,
                metadata.into_updated_domain(current.metadata().created_at().clone(), &now),
            )
            .await?
            .into())
    }

    pub async fn delete_project(&self, project_id: &str) -> Result<(), AppHostError> {
        let project_id = parse_project_id(project_id)?;
        let workspace = self.workspace_arc();
        workspace
            .project_service()
            .load_project(&project_id)
            .await?;
        workspace.delete_project(&project_id).await?;
        if self.active_project_id() == project_id {
            let fallback = workspace
                .project_service()
                .list_projects()
                .await?
                .into_iter()
                .next()
                .map(|project| project.id().clone())
                .unwrap_or_else(|| ProjectId::new("default"));
            *self.active_project.write().expect(APP_HOST_LOCK) = fallback;
        }
        Ok(())
    }

    pub async fn set_active_project(&self, project_id: &str) -> Result<ProjectDto, AppHostError> {
        let project_id = parse_project_id(project_id)?;
        let workspace = self.workspace_arc();
        let project = workspace
            .project_service()
            .load_project(&project_id)
            .await?;
        *self.active_project.write().expect(APP_HOST_LOCK) = project_id;
        Ok(project.into())
    }

    pub async fn board_snapshot(&self, project_id: &str) -> Result<BoardSnapshotDto, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let workspace = self.workspace_arc();
        Ok(workspace
            .board_service()
            .snapshot(&project_id)
            .await?
            .into())
    }

    pub async fn preview_board_commands(
        &self,
        project_id: &str,
        batch: serde_json::Value,
    ) -> Result<BoardCommandResultDto, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let batch: CommandBatch<BoardCommand> =
            serde_json::from_value(batch).map_err(invalid_board_command_batch)?;
        let workspace = self.workspace_arc();
        Ok(workspace
            .board_service()
            .preview_batch(&project_id, batch)
            .await?
            .into())
    }

    pub async fn apply_board_commands(
        &self,
        project_id: &str,
        batch: serde_json::Value,
    ) -> Result<BoardCommandResultDto, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let batch: CommandBatch<BoardCommand> =
            serde_json::from_value(batch).map_err(invalid_board_command_batch)?;
        let workspace = self.workspace_arc();
        Ok(workspace
            .board_service()
            .apply_batch(&project_id, batch)
            .await?
            .into())
    }

    pub async fn undo_board(
        &self,
        project_id: &str,
        expected_entry_id: Option<&str>,
    ) -> Result<Option<BoardCommandResultDto>, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let expected = expected_entry_id.map(HistoryEntryId::from);
        let workspace = self.workspace_arc();
        Ok(workspace
            .board_service()
            .undo(
                &project_id,
                CommandActor::new(CommandActorKind::Human),
                expected.as_ref(),
            )
            .await?
            .map(Into::into))
    }

    pub async fn redo_board(
        &self,
        project_id: &str,
        expected_entry_id: Option<&str>,
    ) -> Result<Option<BoardCommandResultDto>, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let expected = expected_entry_id.map(HistoryEntryId::from);
        let workspace = self.workspace_arc();
        Ok(workspace
            .board_service()
            .redo(
                &project_id,
                CommandActor::new(CommandActorKind::Human),
                expected.as_ref(),
            )
            .await?
            .map(Into::into))
    }

    async fn require_project(&self, project_id: &str) -> Result<ProjectId, AppHostError> {
        let project_id = parse_project_id(project_id)?;
        let workspace = self.workspace_arc();
        workspace
            .project_service()
            .load_project(&project_id)
            .await?;
        Ok(project_id)
    }

    pub async fn op_list(&self, project_id: &str) -> Result<Vec<OperationInfo>, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let workspace = self.workspace_arc();
        Ok(workspace.operation_service().list(&project_id).await)
    }

    pub async fn op_invoke(
        &self,
        project_id: &str,
        name: String,
        input: serde_json::Value,
    ) -> Result<InvokeResult, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let workspace = self.workspace_arc();
        let context = OperationContext::new(
            CommandActor::new(CommandActorKind::Human),
            CommandProvenance::Direct,
        );
        let cancel = CancelToken::new();
        let request = InvokeRequest {
            name,
            input,
            context,
            cancel,
            run: None,
            input_sources: Default::default(),
        };
        workspace
            .operation_service()
            .invoke(&project_id, request)
            .await
            .map_err(AppHostError::from)
    }

    pub async fn op_preview(
        &self,
        project_id: &str,
        name: String,
        input: serde_json::Value,
        preview_key: String,
    ) -> Result<serde_json::Value, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let workspace = self.workspace_arc();
        let cancel = CancelToken::new();
        workspace
            .operation_service()
            .preview(&project_id, &name, input, &preview_key, cancel)
            .await
            .map_err(AppHostError::from)
    }

    /// Cancel the in-flight preview registered under `preview_key`.
    ///
    /// The key is global to the operation service, so this needs no project and
    /// is a no-op when no preview is running under it.
    pub async fn op_preview_cancel(&self, preview_key: &str) -> Result<(), AppHostError> {
        let workspace = self.workspace_arc();
        workspace
            .operation_service()
            .cancel_preview(preview_key)
            .await
            .map_err(AppHostError::from)
    }

    pub(crate) fn require_project_sync(&self, project_id: &str) -> Result<ProjectId, AppHostError> {
        let project_id = parse_project_id(project_id)?;
        let workspace = self.workspace_arc();
        if !workspace.project_service().contains_project(&project_id) {
            return Err(AppHostError::UnknownProject { project_id });
        }
        Ok(project_id)
    }

    pub(crate) fn workspace_arc(&self) -> Arc<WorkspaceHost> {
        Arc::clone(self.app_host.read().expect(APP_HOST_LOCK).workspace())
    }

    async fn refresh_storage_after_mutation(workspace: &WorkspaceHost, run_recovery: bool) {
        let budget = workspace.storage_budget();
        let _ = budget.calibrate().await;
        if run_recovery {
            let _ = budget.recover_if_needed().await;
        }
    }

    // ─── Storage budget facade ──────────────────────────────────────

    pub async fn storage_status(&self) -> Result<StorageStatusDto, AppHostError> {
        let workspace = self.workspace_arc();
        workspace.storage_budget().status().await
    }

    pub async fn storage_set_budget(
        &self,
        limit_bytes: Option<u64>,
    ) -> Result<StorageStatusDto, AppHostError> {
        let workspace = self.workspace_arc();
        workspace.storage_budget().set_budget(limit_bytes).await
    }

    pub async fn storage_dismiss_nudge(&self) -> Result<StorageStatusDto, AppHostError> {
        let workspace = self.workspace_arc();
        workspace.storage_budget().dismiss_nudge().await
    }

    pub async fn storage_clean_cache(&self) -> Result<StorageCleanResultDto, AppHostError> {
        let workspace = self.workspace_arc();
        workspace.storage_budget().clean_cache().await
    }

    pub async fn storage_clean_project_history(
        &self,
        project_id: &str,
    ) -> Result<StorageCleanResultDto, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let workspace = self.workspace_arc();
        workspace.clean_project_history(&project_id).await
    }

    pub async fn storage_preflight_write(
        &self,
        kind: StorageWriteKind,
        estimated_bytes: u64,
        paths: Vec<PathBuf>,
    ) -> Result<StoragePreflightResultDto, AppHostError> {
        let workspace = self.workspace_arc();
        workspace
            .storage_budget()
            .preflight_write_input(StoragePreflightInputDto {
                kind,
                estimated_bytes,
                paths,
            })
            .await
    }

    // ─── File history & workspace folder facade ─────────────────────

    pub async fn file_history_scan_project(
        &self,
        project_id: &str,
        background: Option<bool>,
    ) -> Result<FileHistoryScanStatus, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let workspace = self.workspace_arc();
        let options = ScanOptions {
            source: SnapshotSource::Manual,
            actor: Some(CommandActor::new(CommandActorKind::Human)),
            ..ScanOptions::default()
        };
        if background.unwrap_or(false) {
            workspace
                .file_history()
                .start_background_scan(&project_id, options)
                .await
        } else {
            workspace
                .file_history()
                .scan_project(&project_id, options)
                .await?;
            Ok(workspace.file_history().scan_status(&project_id))
        }
    }

    pub async fn file_history_scan_status(
        &self,
        project_id: &str,
    ) -> Result<FileHistoryScanStatus, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let workspace = self.workspace_arc();
        Ok(workspace.file_history().scan_status(&project_id))
    }

    pub async fn file_history_cancel_scan(&self, project_id: &str) -> Result<bool, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let workspace = self.workspace_arc();
        Ok(workspace.file_history().cancel_scan(&project_id))
    }

    pub async fn file_history_snapshot_file(
        &self,
        project_id: &str,
        folder_id: &str,
        rel_path: &str,
        note: Option<String>,
        pinned: Option<bool>,
    ) -> Result<Option<FileSnapshotRecord>, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let folder_id = parse_folder_id(folder_id)?;
        let workspace = self.workspace_arc();
        workspace
            .file_history()
            .snapshot_file(
                &project_id,
                &folder_id,
                rel_path,
                SnapshotSource::Manual,
                None,
                Some(CommandActor::new(CommandActorKind::Human)),
                note,
                pinned.unwrap_or(false),
            )
            .await
    }

    pub async fn file_history_list(
        &self,
        project_id: &str,
        folder_id: Option<&str>,
        rel_path: Option<&str>,
    ) -> Result<Vec<FileSnapshotRecord>, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let folder_id = folder_id.map(parse_folder_id).transpose()?;
        let workspace = self.workspace_arc();
        workspace
            .file_history()
            .list_history(&project_id, folder_id.as_ref(), rel_path)
            .await
    }

    pub async fn file_history_set_pinned(
        &self,
        project_id: &str,
        snapshot_id: &str,
        pinned: bool,
    ) -> Result<FileSnapshotRecord, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let snapshot_id = parse_snapshot_id(snapshot_id)?;
        let workspace = self.workspace_arc();
        workspace
            .file_history()
            .set_pinned(&project_id, &snapshot_id, pinned)
            .await
    }

    pub async fn file_history_restore_file(
        &self,
        project_id: &str,
        snapshot_id: &str,
    ) -> Result<RestoreOutcome, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let snapshot_id = parse_snapshot_id(snapshot_id)?;
        let workspace = self.workspace_arc();
        workspace
            .file_history()
            .restore_file(
                &project_id,
                &snapshot_id,
                Some(CommandActor::new(CommandActorKind::Human)),
            )
            .await
    }

    pub async fn file_history_get_ignore_rules(
        &self,
        project_id: &str,
    ) -> Result<String, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let workspace = self.workspace_arc();
        workspace.file_history().get_ignore_rules(&project_id).await
    }

    pub async fn file_history_set_ignore_rules(
        &self,
        project_id: &str,
        rules: &str,
    ) -> Result<(), AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let workspace = self.workspace_arc();
        workspace
            .file_history()
            .set_ignore_rules(&project_id, rules)
            .await
    }

    pub async fn project_relink_folder(
        &self,
        project_id: &str,
        folder_id: &str,
        new_path: &str,
    ) -> Result<WorkspaceFolder, AppHostError> {
        let project_id = self.require_project(project_id).await?;
        let folder_id = parse_folder_id(folder_id)?;
        let workspace = self.workspace_arc();
        workspace
            .project_service()
            .relink_folder(&project_id, &folder_id, new_path)
            .await
    }

    pub fn has_installed_mod(&self, mod_id: &str) -> bool {
        self.workspace_arc().mod_manager().has_installed(mod_id)
    }

    // ─── Mod manager facade ────────────────────────────────────────

    /// List bundled (non-overridden) mods alongside managed mods from `inventory.json`.
    pub fn mods_list(&self) -> ModListResponseDto {
        let workspace = self.workspace_arc();
        let manager = workspace.mod_manager();
        let installed = manager.list_installed();
        let installed_ids: HashSet<String> = installed
            .iter()
            .map(|info| info.record.manifest.id.clone())
            .collect();

        let mut mods: Vec<ModEntryDto> = workspace
            .mod_registry()
            .list()
            .into_iter()
            .filter(|entry| !installed_ids.contains(&entry.manifest.id))
            .map(|entry| ModEntryDto::from_bundled_entry(&entry))
            .collect();
        mods.extend(installed.into_iter().map(ModEntryDto::from_installed));

        ModListResponseDto {
            mods_root: manager.paths().mods_root().display().to_string(),
            mods,
        }
    }

    /// Create (if needed) and open the active `mods_root` folder in the system file manager.
    pub fn mods_open_dir(&self) -> Result<(), AppHostError> {
        let workspace = self.workspace_arc();
        let mods_root = workspace.mod_manager().paths().mods_root().to_path_buf();
        std::fs::create_dir_all(&mods_root).map_err(|e| AppHostError::Io {
            path: mods_root.clone(),
            message: e.to_string(),
        })?;
        opener::open(&mods_root).map_err(|e| AppHostError::Io {
            path: mods_root,
            message: format!("failed to open mods directory: {e}"),
        })
    }

    pub fn mods_preview_archive(&self, path: &str) -> Result<ModInstallPreviewDto, AppHostError> {
        let workspace = self.workspace_arc();
        let manager = workspace.mod_manager();
        let archive_path = Path::new(path);
        let filename = archive_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path)
            .to_owned();
        let summary = manager
            .preview_archive_file(archive_path, ModInstallSource::LocalArchive { filename })?;
        let existing = manager.installed_record(&summary.inspected.manifest.id);
        Ok(ModInstallPreviewDto::from_trust_summary(
            summary,
            existing.as_ref(),
        ))
    }

    pub fn mods_preview_directory(&self, path: &str) -> Result<ModInstallPreviewDto, AppHostError> {
        let workspace = self.workspace_arc();
        let manager = workspace.mod_manager();
        let summary = manager.preview_local_directory(Path::new(path))?;
        let existing = manager.installed_record(&summary.inspected.manifest.id);
        Ok(ModInstallPreviewDto::from_trust_summary(
            summary,
            existing.as_ref(),
        ))
    }

    pub async fn mods_install_archive(
        &self,
        path: &str,
        consent_granted: bool,
        consent_permission_expansion: bool,
        enabled: bool,
        agent_visible_operations: Vec<String>,
    ) -> Result<ModEntryDto, AppHostError> {
        let workspace = self.workspace_arc();
        let archive_path = Path::new(path);
        let filename = archive_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path)
            .to_owned();
        let request = build_install_request(
            consent_granted,
            consent_permission_expansion,
            enabled,
            agent_visible_operations,
        );
        let result = workspace
            .mod_manager()
            .install_archive_file(
                archive_path,
                ModInstallSource::LocalArchive { filename },
                request,
            )
            .await;
        Self::refresh_storage_after_mutation(&workspace, result.is_ok()).await;
        let info = result?;
        Ok(ModEntryDto::from_installed(info))
    }

    pub async fn mods_install_directory(
        &self,
        path: &str,
        consent_granted: bool,
        consent_permission_expansion: bool,
        enabled: bool,
        agent_visible_operations: Vec<String>,
    ) -> Result<ModEntryDto, AppHostError> {
        let workspace = self.workspace_arc();
        let request = build_install_request(
            consent_granted,
            consent_permission_expansion,
            enabled,
            agent_visible_operations,
        );
        let result = workspace
            .mod_manager()
            .install_local_directory(Path::new(path), request)
            .await;
        Self::refresh_storage_after_mutation(&workspace, result.is_ok()).await;
        let info = result?;
        Ok(ModEntryDto::from_installed(info))
    }

    pub async fn mods_set_enabled(
        &self,
        mod_id: &str,
        enabled: bool,
    ) -> Result<ModEntryDto, AppHostError> {
        let workspace = self.workspace_arc();
        let info = workspace.mod_manager().set_enabled(mod_id, enabled).await?;
        Ok(ModEntryDto::from_installed(info))
    }

    pub async fn mods_set_agent_visibility(
        &self,
        mod_id: &str,
        operations: Vec<String>,
    ) -> Result<ModEntryDto, AppHostError> {
        let workspace = self.workspace_arc();
        let set: BTreeSet<String> = operations.into_iter().collect();
        let info = workspace
            .mod_manager()
            .set_agent_visible_operations(mod_id, set)
            .await?;
        Ok(ModEntryDto::from_installed(info))
    }

    pub async fn mods_reload_local(
        &self,
        mod_id: &str,
        consent_permission_expansion: bool,
    ) -> Result<ModEntryDto, AppHostError> {
        let workspace = self.workspace_arc();
        let result = workspace
            .mod_manager()
            .reload_local_mod(mod_id, consent_permission_expansion)
            .await;
        Self::refresh_storage_after_mutation(&workspace, result.is_ok()).await;
        let info = result?;
        Ok(ModEntryDto::from_installed(info))
    }

    pub async fn mods_uninstall(
        &self,
        mod_id: &str,
        remove_files: bool,
    ) -> Result<(), AppHostError> {
        let workspace = self.workspace_arc();
        let uninstall_result = workspace
            .mod_manager()
            .uninstall(mod_id, remove_files)
            .await;
        Self::refresh_storage_after_mutation(&workspace, false).await;
        uninstall_result?;
        if mod_id == "text" {
            let mod_registry = Arc::clone(workspace.mod_registry());
            let event_bus = workspace.event_bus();
            register_bundled_text_mod_if_missing(&mod_registry, event_bus).await;
            if mod_registry
                .list()
                .iter()
                .any(|entry| entry.manifest.id == "text")
            {
                event_bus.publish(Event::mod_event(
                    "mod.enabled",
                    serde_json::json!({ "modId": "text" }),
                ));
            }
        }
        Ok(())
    }

    pub fn mods_sources_list(&self) -> Result<ModSourcesListDto, AppHostError> {
        let workspace = self.workspace_arc();
        let manager = workspace.mod_manager();
        let sources = manager.list_third_party_sources()?;
        Ok(ModSourcesListDto {
            official_url: manager.official_catalog_url(),
            third_party: sources.into_iter().map(ThirdPartySourceDto::from).collect(),
        })
    }

    pub fn mods_sources_add(
        &self,
        name: &str,
        url: &str,
        public_key_ed25519: Option<&str>,
    ) -> Result<ThirdPartySourceDto, AppHostError> {
        let workspace = self.workspace_arc();
        let added =
            workspace
                .mod_manager()
                .add_third_party_source(name, url, public_key_ed25519)?;
        Ok(ThirdPartySourceDto::from(added))
    }

    pub fn mods_sources_set_enabled(
        &self,
        source_id: &str,
        enabled: bool,
    ) -> Result<ThirdPartySourceDto, AppHostError> {
        let workspace = self.workspace_arc();
        let updated = workspace
            .mod_manager()
            .set_third_party_source_enabled(source_id, enabled)?;
        Ok(ThirdPartySourceDto::from(updated))
    }

    pub fn mods_sources_remove(&self, source_id: &str) -> Result<(), AppHostError> {
        let workspace = self.workspace_arc();
        workspace
            .mod_manager()
            .remove_third_party_source(source_id)?;
        Ok(())
    }

    pub async fn mods_browse(
        &self,
        source_id: Option<&str>,
        force_refresh: bool,
    ) -> Result<ModBrowseResponseDto, AppHostError> {
        let workspace = self.workspace_arc();
        let manager = workspace.mod_manager();
        let (items, outcomes) = manager
            .browse_remote_catalog(source_id, force_refresh)
            .await?;
        let installed_map: std::collections::HashMap<String, InstalledModRecord> = manager
            .list_installed()
            .into_iter()
            .map(|info| (info.record.manifest.id.clone(), info.record))
            .collect();
        let bundled_ids = manager.bundled_mod_ids();

        let dto_items = items
            .into_iter()
            .map(|item| {
                let installed = installed_map.get(&item.id);
                RemoteModCatalogItemDto::from_catalog_item(item, installed, &bundled_ids)
            })
            .collect();

        Ok(ModBrowseResponseDto {
            items: dto_items,
            outcomes,
        })
    }

    pub async fn mods_download_start(
        &self,
        source_id: &str,
        mod_id: &str,
        version: Option<&str>,
    ) -> Result<ModDownloadTaskDto, AppHostError> {
        let workspace = self.workspace_arc();
        let manager = workspace.mod_manager();
        let mut event_rx = workspace.event_bus().subscribe();
        let task_id = manager
            .start_remote_download(source_id, mod_id, version)
            .await?;
        let snapshot = manager.download_status(&task_id)?;
        match snapshot.status {
            ModDownloadStatus::Installed => {
                Self::refresh_storage_after_mutation(&workspace, true).await;
            }
            ModDownloadStatus::Failed | ModDownloadStatus::Cancelled => {
                Self::refresh_storage_after_mutation(&workspace, false).await;
            }
            ModDownloadStatus::AwaitingConsent => {}
            ModDownloadStatus::Downloading | ModDownloadStatus::Verifying => {
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    let workspace_bg = Arc::clone(&workspace);
                    let tid = task_id.clone();
                    handle.spawn(async move {
                        loop {
                            match event_rx.recv().await {
                                Ok(Event::Mod { kind, payload })
                                    if kind == "mod.download_progress"
                                        && payload.get("taskId").and_then(|v| v.as_str())
                                            == Some(tid.as_str()) =>
                                {
                                    let status = payload.get("status").and_then(|v| v.as_str());
                                    if matches!(status, Some("installed" | "failed" | "cancelled"))
                                    {
                                        DesktopHostState::refresh_storage_after_mutation(
                                            &workspace_bg,
                                            status == Some("installed"),
                                        )
                                        .await;
                                        break;
                                    }
                                    if status == Some("awaiting_consent") {
                                        break;
                                    }
                                }
                                Ok(_) => continue,
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                    continue;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            }
                        }
                    });
                }
            }
        }
        Ok(ModDownloadTaskDto::from(snapshot))
    }

    pub fn mods_download_status(&self, task_id: &str) -> Result<ModDownloadTaskDto, AppHostError> {
        let workspace = self.workspace_arc();
        let snapshot = workspace.mod_manager().download_status(task_id)?;
        Ok(ModDownloadTaskDto::from(snapshot))
    }

    pub fn mods_download_cancel(&self, task_id: &str) -> Result<ModDownloadTaskDto, AppHostError> {
        let workspace = self.workspace_arc();
        let snapshot = workspace.mod_manager().cancel_remote_download(task_id)?;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let workspace_bg = Arc::clone(&workspace);
            handle.spawn(async move {
                DesktopHostState::refresh_storage_after_mutation(&workspace_bg, false).await;
            });
        }
        Ok(ModDownloadTaskDto::from(snapshot))
    }

    pub async fn mods_download_confirm_install(
        &self,
        task_id: &str,
        consent_permission_expansion: bool,
        agent_visible_operations: Vec<String>,
    ) -> Result<ModEntryDto, AppHostError> {
        let workspace = self.workspace_arc();
        let set: BTreeSet<String> = agent_visible_operations.into_iter().collect();
        let result = workspace
            .mod_manager()
            .confirm_downloaded_install(task_id, consent_permission_expansion, set)
            .await;
        Self::refresh_storage_after_mutation(&workspace, result.is_ok()).await;
        let info = result?;
        Ok(ModEntryDto::from_installed(info))
    }

    pub fn mods_runtime_status(&self) -> ModRuntimeEnvironmentDto {
        let workspace = self.workspace_arc();
        let backend = detect_sandbox_backend();
        ModRuntimeEnvironmentDto {
            sandbox_backend: backend.as_str().to_owned(),
            sandbox_enforced: backend.is_os_enforced(),
            uv: workspace.mod_manager().uv_runtime_status(),
            uv_bootstrap_supported: default_uv_archive_url().is_some(),
        }
    }

    pub async fn mods_uv_bootstrap(
        &self,
        archive_url: Option<&str>,
        expected_sha256: Option<&str>,
    ) -> Result<ModRuntimeEnvironmentDto, AppHostError> {
        let workspace = self.workspace_arc();
        let result = workspace
            .mod_manager()
            .bootstrap_uv(archive_url, expected_sha256)
            .await;
        Self::refresh_storage_after_mutation(&workspace, result.is_ok()).await;
        let uv = result?;
        let backend = detect_sandbox_backend();
        Ok(ModRuntimeEnvironmentDto {
            sandbox_backend: backend.as_str().to_owned(),
            sandbox_enforced: backend.is_os_enforced(),
            uv,
            uv_bootstrap_supported: default_uv_archive_url().is_some(),
        })
    }

    pub fn health(&self) -> HealthResponse {
        HealthResponse::from_workspace(self.app_host.read().expect(APP_HOST_LOCK).workspace())
    }

    /// Shut down the registered mods, if any.
    ///
    /// This is the application-level shutdown hook. Call it when the host
    /// is exiting so that child mod processes are cleaned up instead of
    /// becoming orphans.
    pub async fn shutdown(&self) {
        let app_host = self.app_host.read().expect(APP_HOST_LOCK).clone();
        app_host.shutdown().await;
    }
}

fn parse_absolute_dir_env(
    env_name: &str,
    env: Option<&std::ffi::OsStr>,
) -> Result<Option<PathBuf>, String> {
    let Some(raw) = env else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Err(format!("{env_name} must not be empty"));
    }
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(format!(
            "{env_name} (`{}`) must be an absolute path",
            path.display()
        ));
    }
    Ok(Some(path))
}

pub fn resolve_app_data_dir(
    env: Option<&std::ffi::OsStr>,
    default_dir: PathBuf,
) -> Result<PathBuf, String> {
    Ok(parse_absolute_dir_env(APP_DATA_DIR_ENV, env)?.unwrap_or(default_dir))
}

/// Resolve the managed mods directory.
///
/// Precedence:
/// 1. `MERA_MODS_DIR` (when set, must be a non-empty absolute path).
/// 2. `<resolved_app_data_dir>/mods` when `MERA_APP_DATA_DIR` is set (sandbox/test isolation).
/// 3. Platform default (`~/.mera/mods` on Unix, `{install_dir}/mods` with `%LOCALAPPDATA%\mera\mods`
///    read-only fallback on Windows).
pub fn resolve_mods_dir(
    mods_env: Option<&std::ffi::OsStr>,
    app_data_env_set: bool,
    resolved_app_data_dir: &Path,
    home_dir: Option<&Path>,
    exe_dir: Option<&Path>,
    local_app_data_dir: Option<&Path>,
) -> Result<(PathBuf, Option<String>), String> {
    if let Some(explicit) = parse_absolute_dir_env(MODS_DIR_ENV, mods_env)? {
        return Ok((explicit, None));
    }
    if app_data_env_set {
        return Ok((resolved_app_data_dir.join("mods"), None));
    }
    let (paths, warning) = ModStorePaths::platform_default(
        home_dir,
        exe_dir,
        local_app_data_dir,
        resolved_app_data_dir,
    );
    Ok((paths.mods_root().to_path_buf(), warning))
}

pub fn default_data_path(app_data_dir: impl AsRef<Path>) -> PathBuf {
    app_data_dir.as_ref().join("data")
}

fn build_install_request(
    consent_granted: bool,
    consent_permission_expansion: bool,
    enabled: bool,
    agent_visible_operations: Vec<String>,
) -> InstallModRequest {
    let mut request = InstallModRequest::default()
        .enable(enabled)
        .consent_permission_expansion(consent_permission_expansion)
        .agent_visible_operations(agent_visible_operations.into_iter().collect());
    if consent_granted {
        request = request.with_consent();
    }
    request
}

/// Validate a user-supplied workflow id before it reaches `WorkflowId::new`
/// (which asserts — and would panic — on invalid input).
fn parse_project_id(id: &str) -> Result<ProjectId, AppHostError> {
    let safe = !id.is_empty()
        && id.is_ascii()
        && !id.contains('/')
        && !id.contains('\\')
        && id != "."
        && id != "..";
    if !safe {
        return Err(AppHostError::ProjectIdPathUnsafe {
            project_id: id.to_owned(),
        });
    }
    Ok(ProjectId::new(id))
}

fn parse_folder_id(id: &str) -> Result<WorkspaceFolderId, AppHostError> {
    WorkspaceFolderId::try_new(id).map_err(|reason| {
        AppHostError::Diagnostic(Box::new(Diagnostic::new(
            DiagnosticId::new("file_history:invalid_folder_id"),
            DiagnosticCode::new("FILE_HISTORY/CALLER_INVALID_ID"),
            DiagnosticSeverity::Error,
            DiagnosticSourceName::new("desktop_host"),
            format!("invalid folder id `{id}`: {reason}"),
            DiagnosticTarget::new(DiagnosticTargetDomain::new("file_history")),
        )))
    })
}

fn parse_snapshot_id(id: &str) -> Result<FileSnapshotId, AppHostError> {
    FileSnapshotId::try_new(id).map_err(|reason| {
        AppHostError::Diagnostic(Box::new(Diagnostic::new(
            DiagnosticId::new("file_history:invalid_snapshot_id"),
            DiagnosticCode::new("FILE_HISTORY/CALLER_INVALID_ID"),
            DiagnosticSeverity::Error,
            DiagnosticSourceName::new("desktop_host"),
            format!("invalid snapshot id `{id}`: {reason}"),
            DiagnosticTarget::new(DiagnosticTargetDomain::new("file_history")),
        )))
    })
}

fn invalid_board_command_batch(error: serde_json::Error) -> AppHostError {
    AppHostError::Diagnostic(Box::new(Diagnostic::new(
        DiagnosticId::new("board:invalid_command_batch"),
        DiagnosticCode::new("BOARD/INVALID_COMMAND_BATCH"),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("desktop_host"),
        format!("invalid board command batch: {error}"),
        DiagnosticTarget::new(DiagnosticTargetDomain::new("board")),
    )))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write_file(path: &Path) {
        fs::write(path, b"#!/bin/sh\n").expect("write fixture");
    }

    #[test]
    fn an_existing_env_path_wins_over_the_sibling() {
        let dir = tempfile::tempdir().expect("temp dir");
        let explicit = dir.path().join("custom-mod");
        write_file(&explicit);
        let sibling = dir
            .path()
            .join(format!("{TEXT_MOD_NAME}{}", std::env::consts::EXE_SUFFIX));
        write_file(&sibling);

        assert_eq!(
            text_mod_candidate(Some(explicit.as_os_str()), Some(dir.path())),
            Some(explicit)
        );
    }

    #[test]
    fn a_missing_env_path_falls_back_to_the_sibling() {
        let dir = tempfile::tempdir().expect("temp dir");
        let sibling = dir
            .path()
            .join(format!("{TEXT_MOD_NAME}{}", std::env::consts::EXE_SUFFIX));
        write_file(&sibling);

        assert_eq!(
            text_mod_candidate(
                Some(dir.path().join("missing").as_os_str()),
                Some(dir.path())
            ),
            Some(sibling)
        );
    }

    #[test]
    fn without_an_env_path_only_an_existing_sibling_is_used() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(text_mod_candidate(None, Some(dir.path())), None);

        let sibling = dir
            .path()
            .join(format!("{TEXT_MOD_NAME}{}", std::env::consts::EXE_SUFFIX));
        write_file(&sibling);
        assert_eq!(
            text_mod_candidate(None, Some(dir.path())),
            Some(sibling.clone())
        );
        assert_eq!(text_mod_candidate(None, None), None);
    }

    #[test]
    fn resolve_app_data_dir_keeps_default_when_unset() {
        let default_dir = PathBuf::from("/tmp/default-app-data");
        assert_eq!(
            resolve_app_data_dir(None, default_dir.clone()),
            Ok(default_dir)
        );
    }

    #[test]
    fn resolve_app_data_dir_uses_absolute_override() {
        let default_dir = PathBuf::from("/tmp/default-app-data");
        let override_dir = std::env::temp_dir().join("mera-e2e-app-data");
        assert_eq!(
            resolve_app_data_dir(Some(override_dir.as_os_str()), default_dir),
            Ok(override_dir)
        );
    }

    #[test]
    fn resolve_app_data_dir_rejects_empty_and_relative_paths() {
        let default_dir = PathBuf::from("/tmp/default-app-data");
        let empty_err = resolve_app_data_dir(Some(std::ffi::OsStr::new("")), default_dir.clone())
            .expect_err("empty override should fail");
        assert!(empty_err.contains(APP_DATA_DIR_ENV));

        let rel_err = resolve_app_data_dir(Some(std::ffi::OsStr::new("relative/dir")), default_dir)
            .expect_err("relative override should fail");
        assert!(rel_err.contains(APP_DATA_DIR_ENV));
    }

    #[test]
    fn resolve_mods_dir_uses_explicit_override_even_when_app_data_env_is_set() {
        let temp = tempfile::tempdir().expect("temp dir");
        let app_data = temp.path().join("app-data");
        let home = temp.path().join("home");
        let exe_dir = temp.path().join("install");
        let local_data = temp.path().join("local-data");
        let explicit = temp.path().join("custom-mods");

        let (resolved, warning) = resolve_mods_dir(
            Some(explicit.as_os_str()),
            true,
            &app_data,
            Some(&home),
            Some(&exe_dir),
            Some(&local_data),
        )
        .expect("explicit MERA_MODS_DIR should succeed");
        assert_eq!(resolved, explicit);
        assert!(warning.is_none());
    }

    #[test]
    fn resolve_mods_dir_rejects_empty_and_relative_paths() {
        let temp = tempfile::tempdir().expect("temp dir");
        let app_data = temp.path().join("app-data");

        let empty_err = resolve_mods_dir(
            Some(std::ffi::OsStr::new("")),
            false,
            &app_data,
            None,
            None,
            None,
        )
        .expect_err("empty MERA_MODS_DIR must fail");
        assert!(empty_err.contains(MODS_DIR_ENV));

        let rel_err = resolve_mods_dir(
            Some(std::ffi::OsStr::new("relative/mods")),
            false,
            &app_data,
            None,
            None,
            None,
        )
        .expect_err("relative MERA_MODS_DIR must fail");
        assert!(rel_err.contains(MODS_DIR_ENV));
    }

    #[test]
    fn resolve_mods_dir_isolates_under_app_data_dir_when_app_data_env_is_set() {
        let temp = tempfile::tempdir().expect("temp dir");
        let app_data = temp.path().join("app-data");
        let home = temp.path().join("home");
        let exe_dir = temp.path().join("install");
        let local_data = temp.path().join("local-data");

        let (resolved, warning) = resolve_mods_dir(
            None,
            true,
            &app_data,
            Some(&home),
            Some(&exe_dir),
            Some(&local_data),
        )
        .expect("MERA_APP_DATA_DIR isolation should succeed");
        assert_eq!(resolved, app_data.join("mods"));
        assert!(warning.is_none());
    }

    #[test]
    fn resolve_mods_dir_uses_platform_default_when_env_unset() {
        let temp = tempfile::tempdir().expect("temp dir");
        let app_data = temp.path().join("app-data");
        let home = temp.path().join("home");
        let exe_dir = temp.path().join("install");
        let local_data = temp.path().join("local-data");
        fs::create_dir_all(&exe_dir).expect("create exe dir");

        let (resolved, warning) = resolve_mods_dir(
            None,
            false,
            &app_data,
            Some(&home),
            Some(&exe_dir),
            Some(&local_data),
        )
        .expect("platform default should succeed");
        #[cfg(unix)]
        assert_eq!(resolved, home.join(".mera").join("mods"));
        #[cfg(windows)]
        assert_eq!(resolved, exe_dir.join("mods"));
        assert!(warning.is_none());
    }
}
