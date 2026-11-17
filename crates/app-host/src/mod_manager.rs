//! In-app mod manager: verified package installation, remote TUF & third-party
//! registries, explicit Agent visibility allow-lists, OS sandboxing, optional
//! `uv` script runtime, and runtime hot-swapping over [`mod_sdk::ModRegistry`].

pub mod inventory;
pub mod remote;
pub mod service;
pub mod sources;
pub mod trust;
pub mod uv;

pub use inventory::{
    InstalledModInfo, InstalledModRecord, MOD_INVENTORY_SCHEMA_VERSION, ModInstallSource,
    ModInventory, ModInventoryStore, ModStorePaths, ModVisibilityTable,
};
pub use remote::{
    ModArtifactEntry, ModDownloadManager, ModDownloadStatus, ModDownloadTaskSnapshot,
    OfficialModTargetCustom, PendingRemoteInstall, RemoteCatalogMod, ThirdPartyIndex,
    ThirdPartyIndexEntry, current_host_target, evaluate_compatibility,
};
pub use service::{InstallModRequest, ModManagerError, ModManagerService, SourceBrowseOutcome};
pub use sources::{
    DEFAULT_OFFICIAL_MOD_CATALOG_URL, MOD_SOURCES_SCHEMA_VERSION, ModSourcesFile, ModSourcesStore,
    OFFICIAL_MOD_CATALOG_ROOT_FILE_ENV, OFFICIAL_MOD_CATALOG_URL_ENV, OfficialCatalogPolicy,
    ThirdPartySourceConfig, validate_ed25519_public_key, validate_remote_source_url,
};
pub use trust::{
    InspectedModPackage, ModTrustSummary, UpgradeCheck, evaluate_upgrade, inspect_local_directory,
    inspect_rmod_archive, inspect_rmod_bytes, permissions_expanded,
};
pub use uv::{UvRuntimeStatus, UvSource, default_uv_archive_url, detect_uv_runtime};
