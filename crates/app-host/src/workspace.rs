pub mod builder;

use std::path::Path;
use std::sync::Arc;

pub use builder::WorkspaceHostBuilder;
use mera_config::{AppConfig, AppPaths};
use mera_core::event::EventBus;
use mera_core::model::ProjectId;
use mod_sdk::ModRegistry;

use crate::WorkspaceScope;
use crate::project_service::ProjectFolderSource;
use crate::{
    ActivityService, AppHostError, BoardService, CollectionService, DynamicWorkflowService,
    LibraryService, ModManagerService, ModStorePaths, ModVisibilityTable, OperationService,
    ProjectService, StorageBudgetService, WorkerStoragePaths,
};

/// Establish the AR-08 project workflow layout (idempotent).
///
/// Delegates file migration to `mera_config::migrate_to_project_layout`
/// (moves legacy `workflows/*.json` into `projects/default/workflows/`,
/// keeping `.backup` originals) and then materialises the default
/// project's `project.json` + `board.json` documents so the project
/// is discoverable by `ProjectService::list_projects` from the first
/// bootstrap. The documents are written directly because the default
/// project directory tree already exists after the config-level
/// migration, so `create_project`'s ProjectAlreadyExists guard would
/// reject a re-run on a partially-migrated workspace.
pub(crate) async fn migrate_project_layout(config: &AppConfig) -> Result<(), AppHostError> {
    use crate::board_service::ensure_board_file;
    use mera_core::event::Timestamp;
    use mera_core::project::{Project, ProjectMetadata};

    let paths = config.paths();

    // 1. Move legacy top-level workflow documents into the default
    //    project's layout (idempotent; originals kept as .backup).
    mera_config::migrate_to_project_layout(paths)
        .await
        .map_err(AppHostError::BootstrapConfig)?;

    // 2. Materialise a discoverable default project document set.
    let default_project_id = ProjectId::new("default");
    let project_file = paths
        .project_dir(default_project_id.as_str())
        .join("project.json");
    if !project_file.is_file() {
        let metadata = ProjectMetadata::new(
            "Default",
            "Default workspace project created by the AR-08 project-layout migration.",
            Timestamp::new("2026-08-18T00:00:00Z"),
            Timestamp::new("2026-08-18T00:00:00Z"),
        );
        let project = Project::new(default_project_id.clone(), metadata);
        crate::document_store::write_document(&project_file, &project, "project").await?;
    }
    ensure_board_file(paths, &default_project_id).await?;
    Ok(())
}

pub struct WorkspaceHost {
    pub(crate) workspace_scope: WorkspaceScope,
    pub(crate) config: Arc<AppConfig>,
    pub(crate) project_service: Arc<ProjectService>,
    pub(crate) board_service: Arc<BoardService>,
    pub(crate) collection_service: Arc<CollectionService>,
    pub(crate) library_service: Arc<LibraryService>,
    pub(crate) dynamic_workflow_service: Arc<DynamicWorkflowService>,
    pub(crate) event_bus: EventBus,
    pub(crate) operation_service: Arc<OperationService>,
    /// The registry the operation service dispatches to. The host owns the
    /// shutdown call, so a process mod's child does not outlive the application.
    pub(crate) mod_registry: Arc<ModRegistry>,
    pub(crate) mod_manager: Arc<ModManagerService>,
    pub(crate) storage_budget: Arc<StorageBudgetService>,
}

impl std::fmt::Debug for WorkspaceHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceHost")
            .field("workspace_scope", &self.workspace_scope)
            .field("config", &self.config)
            .field("collection_service", &self.collection_service)
            .field("library_service", &self.library_service)
            .field("dynamic_workflow_service", &self.dynamic_workflow_service)
            .field("operation_service", &self.operation_service)
            .finish_non_exhaustive()
    }
}

impl WorkspaceHost {
    pub(crate) fn assemble(
        workspace_scope: WorkspaceScope,
        config: Arc<AppConfig>,
        event_bus: EventBus,
        operation_service: Arc<OperationService>,
        mod_registry: Arc<ModRegistry>,
        mod_manager: Arc<ModManagerService>,
        worker_storage_paths: WorkerStoragePaths,
    ) -> Self {
        let storage_budget = Arc::new(StorageBudgetService::new(
            Arc::clone(&config),
            worker_storage_paths,
            mod_manager.paths().clone(),
        ));
        let collection_service = Arc::new(CollectionService::with_event_bus(
            config.paths().clone(),
            event_bus.clone(),
        ));
        let library_service = Arc::new(LibraryService::with_event_bus(
            config.paths().clone(),
            event_bus.clone(),
        ));
        library_service.set_storage_budget(Arc::clone(&storage_budget));
        let activity_service = Arc::new(ActivityService::new(
            config.paths().clone(),
            event_bus.clone(),
        ));
        operation_service.set_activity_service(Arc::clone(&activity_service));
        operation_service.set_storage_budget(Arc::clone(&storage_budget));
        let board_service = Arc::new(BoardService::with_event_bus_and_activity(
            config.paths().clone(),
            event_bus.clone(),
            Some(Arc::clone(&activity_service)),
        ));
        let project_service = Arc::new(ProjectService::new(
            config.paths().clone(),
            Arc::clone(&board_service),
            Arc::clone(&collection_service),
        ));
        project_service.set_storage_budget(Arc::clone(&storage_budget));
        operation_service
            .set_project_folder_source(Arc::clone(&project_service) as Arc<dyn ProjectFolderSource>);
        crate::collection_ops::register_collection_ops(
            &operation_service,
            Arc::clone(&collection_service),
            Arc::clone(&board_service),
        );
        crate::library_ops::register_library_ops(&operation_service, Arc::clone(&library_service));
        crate::library_ops::register_library_place_op(
            &operation_service,
            Arc::clone(&library_service),
            Arc::clone(&collection_service),
            Arc::clone(&board_service),
        );
        crate::project_ops::register_project_ops(&operation_service, Arc::clone(&project_service));
        crate::board_ops::register_board_ops(&operation_service, Arc::clone(&board_service));
        crate::activity::register_activity_ops(&operation_service, activity_service);
        let dynamic_workflow_service = Arc::new(DynamicWorkflowService::with_event_bus(
            config.paths().clone(),
            event_bus.clone(),
            Arc::downgrade(&operation_service),
            Arc::clone(&collection_service),
        ));
        operation_service.set_dynamic_workflow_service(Arc::clone(&dynamic_workflow_service));
        let recipe_service = Arc::new(crate::recipe_service::RecipeService::new(
            config.paths().clone(),
            Arc::clone(&collection_service),
            Arc::downgrade(&operation_service),
        ));
        crate::dynamic_workflow_ops::register_dynamic_workflow_ops(
            &operation_service,
            Arc::clone(&dynamic_workflow_service),
            Arc::clone(&recipe_service),
        );
        crate::recipe_ops::register_recipe_ops(&operation_service, recipe_service);

        Self {
            workspace_scope,
            config,
            project_service,
            board_service,
            collection_service,
            library_service,
            dynamic_workflow_service,
            event_bus,
            operation_service,
            mod_registry,
            mod_manager,
            storage_budget,
        }
    }

    pub fn with_defaults(
        workspace_scope: WorkspaceScope,
        base_path: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self::with_event_bus(workspace_scope, base_path, EventBus::new())
    }

    pub fn with_event_bus(
        workspace_scope: WorkspaceScope,
        base_path: impl Into<std::path::PathBuf>,
        event_bus: EventBus,
    ) -> Self {
        let config = Arc::new(AppConfig::new(AppPaths::new(base_path)));
        let mod_registry = Arc::new(ModRegistry::empty());
        let mod_manager = default_mod_manager(
            config.paths().base_path(),
            Arc::clone(&mod_registry),
            event_bus.clone(),
        );
        let operation_service = Arc::new(OperationService::new(
            config.paths().clone(),
            Arc::clone(&mod_registry),
        ));
        operation_service.set_mod_visibility(mod_manager.visibility_table());
        let worker_storage_paths =
            WorkerStoragePaths::new(config.paths().base_path().with_extension("app-data"));
        Self::assemble(
            workspace_scope,
            config,
            event_bus,
            operation_service,
            mod_registry,
            mod_manager,
            worker_storage_paths,
        )
    }

    pub async fn try_with_defaults(
        workspace_scope: WorkspaceScope,
        base_path: impl Into<std::path::PathBuf>,
    ) -> Result<Self, AppHostError> {
        Self::try_with_event_bus(workspace_scope, base_path, EventBus::new()).await
    }

    pub async fn try_with_event_bus(
        workspace_scope: WorkspaceScope,
        base_path: impl Into<std::path::PathBuf>,
        event_bus: EventBus,
    ) -> Result<Self, AppHostError> {
        WorkspaceHostBuilder::new(workspace_scope, base_path)
            .event_bus(event_bus)
            .build()
            .await
    }

    /// Bootstrap with the desktop host's app-data convention and an
    /// already started [`ModRegistry`].
    ///
    /// The host dispatches operations to `mod_registry` and stops it from
    /// [`WorkspaceHost::shutdown`], so a process mod's child does not outlive
    /// the application.
    pub async fn try_with_app_data_root_and_event_bus_and_mod_registry(
        workspace_scope: WorkspaceScope,
        base_path: impl Into<std::path::PathBuf>,
        app_data_root: impl Into<std::path::PathBuf>,
        event_bus: EventBus,
        mod_registry: Arc<ModRegistry>,
    ) -> Result<Self, AppHostError> {
        WorkspaceHostBuilder::new(workspace_scope, base_path)
            .app_data_root(app_data_root)
            .event_bus(event_bus)
            .mod_registry(mod_registry)
            .build()
            .await
    }

    /// Establish the AR-08 project workflow layout (idempotent).
    ///
    /// Moves legacy top-level `workflows/*.json` into
    /// `projects/default/workflows/` (originals kept as .backup) and
    /// materialises the default project's `project.json` +
    /// `board.json` documents.
    pub async fn migrate_to_project_layout(&self) -> Result<(), AppHostError> {
        migrate_project_layout(&self.config).await
    }

    pub fn workspace_scope(&self) -> &WorkspaceScope {
        &self.workspace_scope
    }

    pub fn base_path(&self) -> &Path {
        self.config.paths().base_path()
    }

    pub fn config(&self) -> &Arc<AppConfig> {
        &self.config
    }

    /// Delete a project.
    pub async fn delete_project(&self, project_id: &ProjectId) -> Result<(), AppHostError> {
        self.project_service.delete_project(project_id).await
    }

    pub fn event_bus(&self) -> &EventBus {
        &self.event_bus
    }

    pub fn project_service(&self) -> &Arc<ProjectService> {
        &self.project_service
    }

    pub fn board_service(&self) -> &Arc<BoardService> {
        &self.board_service
    }

    pub fn collection_service(&self) -> &Arc<CollectionService> {
        &self.collection_service
    }

    pub fn library_service(&self) -> &Arc<LibraryService> {
        &self.library_service
    }

    pub fn operation_service(&self) -> &Arc<OperationService> {
        &self.operation_service
    }

    pub fn mod_registry(&self) -> &Arc<ModRegistry> {
        &self.mod_registry
    }

    pub fn mod_manager(&self) -> &Arc<ModManagerService> {
        &self.mod_manager
    }

    pub fn storage_budget(&self) -> &Arc<StorageBudgetService> {
        &self.storage_budget
    }

    /// Shut down in-flight dynamic workflow runs and the registered mods, if any.
    ///
    /// Repeated calls stop the mods once: [`ModRegistry::stop`] owns the
    /// idempotency guard.
    pub async fn shutdown(&self) {
        self.dynamic_workflow_service.shutdown();
        self.mod_registry.stop().await;
    }
}

fn default_mod_manager(
    root: &Path,
    mod_registry: Arc<ModRegistry>,
    event_bus: EventBus,
) -> Arc<ModManagerService> {
    Arc::new(
        ModManagerService::new(
            ModStorePaths::new(root),
            mod_registry,
            Arc::new(ModVisibilityTable::new()),
            event_bus,
        )
        .expect("initialize mod manager"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(prefix: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tid = std::thread::current().id();
        std::env::temp_dir().join(format!("mera-app-host-ws-{prefix}-{nonce:?}-{tid:?}"))
    }

    #[test]
    fn workspace_with_defaults_sets_base_path() {
        let base = temp_dir("defaults");
        let workspace = WorkspaceHost::with_defaults(WorkspaceScope::new("test-defaults"), &base);
        assert_eq!(workspace.base_path(), base);
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn production_bootstrap_creates_discoverable_default_project() {
        let base = temp_dir("ar08-default-project");
        let app_data_root = base.join("app-data");
        let workspace =
            WorkspaceHostBuilder::new(WorkspaceScope::new("ar08-default-project"), &base)
                .app_data_root(&app_data_root)
                .build()
                .await
                .expect("production bootstrap establishes the project layout");

        let projects = workspace
            .project_service()
            .list_projects()
            .await
            .expect("list projects after bootstrap");
        assert!(
            projects.iter().any(|p| p.id().as_str() == "default"),
            "default project must be discoverable after production bootstrap"
        );
        assert!(base.join("projects/default/project.json").is_file());
        assert!(base.join("projects/default/board.json").is_file());

        let workspace2 =
            WorkspaceHostBuilder::new(WorkspaceScope::new("ar08-default-project-2"), &base)
                .app_data_root(&app_data_root)
                .build()
                .await
                .expect("second production bootstrap stays idempotent");
        let projects2 = workspace2
            .project_service()
            .list_projects()
            .await
            .expect("list projects after second bootstrap");
        assert_eq!(
            projects2
                .iter()
                .filter(|p| p.id().as_str() == "default")
                .count(),
            1,
            "migration must be idempotent"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn migration_copies_legacy_workflows_into_default_project() {
        let base = temp_dir("ar08-legacy-migrate");
        let app_data_root = base.join("app-data");

        let legacy_dir = base.join("workflows");
        fs::create_dir_all(&legacy_dir).unwrap();
        let legacy = serde_json::json!({
            "schema_version": "mera.workflow.v1",
            "id": "legacy-wf",
            "version": 1,
            "metadata": { "name": "Legacy" },
            "interface": { "inputs": [], "outputs": [] },
            "nodes": [],
            "edges": [],
            "layout": { "nodes": {} }
        });
        fs::write(
            legacy_dir.join("legacy-wf.json"),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();

        let _workspace =
            WorkspaceHostBuilder::new(WorkspaceScope::new("ar08-legacy-migrate"), &base)
                .app_data_root(&app_data_root)
                .build()
                .await
                .expect("bootstrap with legacy workflows");

        let migrated = base.join("projects/default/workflows/legacy-wf.json");
        assert!(
            migrated.is_file(),
            "legacy workflow migrated into default project"
        );
        let _ = fs::remove_dir_all(&base);
    }
}
