use std::path::{Path, PathBuf};

/// Directory layout for legacy `<app_data_root>/workers/` directories so storage
/// accounting and cache cleanup continue operating without linking a worker host crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerStoragePaths {
    app_data_root: PathBuf,
}

impl WorkerStoragePaths {
    #[must_use]
    pub fn new(app_data_root: impl Into<PathBuf>) -> Self {
        Self {
            app_data_root: app_data_root.into(),
        }
    }

    /// The root of the worker storage hierarchy (`<app_data_root>/workers`).
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.app_data_root.join("workers")
    }

    /// Lock file path for exclusive-installer mutual exclusion.
    #[must_use]
    pub fn install_lock_path(&self) -> PathBuf {
        self.root().join(".install.lock")
    }

    /// Base directory for staging areas (`<app_data_root>/workers/staging`).
    #[must_use]
    pub fn staging_base(&self) -> PathBuf {
        self.root().join("staging")
    }

    /// Path for a specific staging directory.
    #[must_use]
    pub fn staging_dir(&self, installation_id: &str) -> PathBuf {
        self.staging_base().join(installation_id)
    }

    /// Base directory for installed workers (`<app_data_root>/workers/installed`).
    #[must_use]
    pub fn installed_base(&self) -> PathBuf {
        self.root().join("installed")
    }

    /// The application data root.
    #[must_use]
    pub fn app_data_root(&self) -> &Path {
        &self.app_data_root
    }
}
