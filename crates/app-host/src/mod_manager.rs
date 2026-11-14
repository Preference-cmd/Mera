//! In-app mod manager: verified package installation, explicit Agent
//! visibility allow-lists, and runtime hot-swapping over [`mod_sdk::ModRegistry`].

pub mod inventory;
pub mod service;
pub mod trust;

pub use inventory::{
    InstalledModInfo, InstalledModRecord, MOD_INVENTORY_SCHEMA_VERSION, ModInstallSource,
    ModInventory, ModInventoryStore, ModStorePaths, ModVisibilityTable,
};
pub use service::{InstallModRequest, ModManagerError, ModManagerService};
pub use trust::{
    InspectedModPackage, ModTrustSummary, UpgradeCheck, evaluate_upgrade, inspect_local_directory,
    inspect_rmod_archive, inspect_rmod_bytes, permissions_expanded,
};
