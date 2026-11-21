use std::path::PathBuf;
use std::sync::Arc;

use mera_config::{AppConfig, AppPaths};
use mera_core::event::EventBus;
use mod_sdk::ModRegistry;

use super::{WorkspaceHost, migrate_project_layout};
use crate::{
    AppHostError, ModManagerService, ModStorePaths, ModVisibilityTable, WorkerStoragePaths,
    WorkspaceScope,
};

/// Builder for constructing a [`WorkspaceHost`].
///
/// All configuration is expressed as builder methods; the async [`build`](Self::build)
/// phase handles project layout migration and service wiring.
///
/// # Examples
///
/// ```rust,no_run
/// # use mera_app_host::WorkspaceHostBuilder;
/// # use mera_app_host::WorkspaceScope;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let host = WorkspaceHostBuilder::new(WorkspaceScope::new("my-workspace"), "/tmp/workspace")
///     .build()
///     .await?;
/// # Ok(())
/// # }
/// ```
pub struct WorkspaceHostBuilder {
    workspace_scope: WorkspaceScope,
    base_path: PathBuf,
    app_data_root: Option<PathBuf>,
    mods_root: Option<PathBuf>,
    worker_storage_paths: Option<WorkerStoragePaths>,
    event_bus: Option<EventBus>,
    mod_registry: Option<Arc<ModRegistry>>,
    mod_manager: Option<Arc<ModManagerService>>,
}

impl WorkspaceHostBuilder {
    /// Create a new builder with the required parameters.
    ///
    /// `workspace_scope` identifies the workspace for logging and isolation.
    /// `base_path` is the root directory for workspace data (config, assets, etc.).
    pub fn new(workspace_scope: WorkspaceScope, base_path: impl Into<PathBuf>) -> Self {
        Self {
            workspace_scope,
            base_path: base_path.into(),
            app_data_root: None,
            mods_root: None,
            worker_storage_paths: None,
            event_bus: None,
            mod_registry: None,
            mod_manager: None,
        }
    }

    /// Set an existing event bus.
    pub fn event_bus(mut self, bus: EventBus) -> Self {
        self.event_bus = Some(bus);
        self
    }

    /// Set the application data root used to derive default storage paths.
    pub fn app_data_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.app_data_root = Some(path.into());
        self
    }

    /// Set an explicit directory for managed mod installations (`inventory.json`, `packages/`, `staging/`).
    ///
    /// When unset, defaults to `<app_data_root>/mods` (or `<base_path>/mods` if `app_data_root` is unset).
    pub fn mods_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.mods_root = Some(path.into());
        self
    }

    /// Set an explicit [`WorkerStoragePaths`] override for legacy worker storage accounting.
    pub fn worker_storage_paths(mut self, paths: WorkerStoragePaths) -> Self {
        self.worker_storage_paths = Some(paths);
        self
    }

    /// Set a custom mod registry.
    pub fn mod_registry(mut self, registry: Arc<ModRegistry>) -> Self {
        self.mod_registry = Some(registry);
        self
    }

    /// Share an existing [`ModManagerService`] instance.
    pub fn mod_manager(mut self, manager: Arc<ModManagerService>) -> Self {
        self.mod_manager = Some(manager);
        self
    }

    /// Build the [`WorkspaceHost`].
    pub async fn build(self) -> Result<WorkspaceHost, AppHostError> {
        let config = AppConfig::new(AppPaths::new(&self.base_path));
        migrate_project_layout(&config).await?;

        let event_bus = self.event_bus.unwrap_or_default();
        let mod_registry = self
            .mod_registry
            .unwrap_or_else(|| Arc::new(ModRegistry::empty()));
        let mod_manager = match self.mod_manager {
            Some(manager) => manager,
            None => {
                let paths = match self.mods_root {
                    Some(mods_root) => ModStorePaths::from_mods_root(mods_root),
                    None => {
                        let root = self
                            .app_data_root
                            .as_deref()
                            .unwrap_or_else(|| config.paths().base_path());
                        ModStorePaths::new(root)
                    }
                };
                Arc::new(ModManagerService::new(
                    paths,
                    Arc::clone(&mod_registry),
                    Arc::new(ModVisibilityTable::new()),
                    event_bus.clone(),
                )?)
            }
        };
        mod_manager.set_workspace_projects_dir(config.paths().projects_dir().to_path_buf());
        let operation_service = Arc::new(crate::OperationService::new(
            config.paths().clone(),
            Arc::clone(&mod_registry),
        ));
        operation_service.set_mod_visibility(mod_manager.visibility_table());

        let worker_storage_paths = match self.worker_storage_paths {
            Some(paths) => paths,
            None => match &self.app_data_root {
                Some(app_data_root) => WorkerStoragePaths::new(app_data_root),
                None => WorkerStoragePaths::new(self.base_path.with_extension("app-data")),
            },
        };

        let host = WorkspaceHost::assemble(
            self.workspace_scope,
            Arc::new(config),
            event_bus,
            operation_service,
            mod_registry,
            mod_manager,
            worker_storage_paths,
        );
        host.storage_budget().spawn_startup_calibration();
        Ok(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(prefix: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tid = std::thread::current().id();
        std::env::temp_dir().join(format!("mera-builder-{prefix}-{nonce:?}-{tid:?}"))
    }

    #[tokio::test]
    async fn builder_with_defaults_succeeds() {
        let base = temp_dir("defaults");
        let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test"), &base)
            .build()
            .await
            .expect("builder with defaults should succeed");

        assert_eq!(host.base_path(), base);
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn builder_with_app_data_root() {
        let base = temp_dir("app-data");
        let app_data = base.join("app-data");
        fs::create_dir_all(&app_data).unwrap();

        let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test"), &base)
            .app_data_root(&app_data)
            .build()
            .await
            .expect("builder with app data root");

        assert_eq!(host.base_path(), base);
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn builder_with_event_bus() {
        let base = temp_dir("sinks");
        let event_bus = EventBus::new();

        let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test"), &base)
            .event_bus(event_bus)
            .build()
            .await
            .expect("builder with event bus");

        assert_eq!(host.base_path(), base);
        let _ = fs::remove_dir_all(&base);
    }
}
