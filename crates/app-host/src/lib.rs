//! Host-neutral application service shell.
//!
//! This crate owns the V1 workspace host boundary that host adapters (such as
//! the Tauri desktop app) call into. It deliberately keeps concrete UI types out of
//! the public API and delegates domain semantics to the lower-level crates.

#![deny(unsafe_code)]

pub mod activity;
mod app_host;
pub mod asset_store;
pub mod board_ops;
mod board_service;
pub mod collection_ops;
pub mod collection_service;
mod document_store;
pub mod dto;
pub mod dynamic_workflow_ops;
pub mod dynamic_workflow_service;
mod error;
mod error_code;
pub mod file_history;
pub mod library_ops;
pub mod library_service;
pub mod mod_manager;
pub mod operation_policy;
pub mod operation_service;
pub mod project_ops;
pub mod project_service;
pub mod recipe_ops;
pub mod recipe_service;
pub mod script_env;
pub mod step_log;
pub mod storage_budget;
mod workspace;
mod workspace_scope;

pub use activity::{
    ACTIVITY_LIST_DEFAULT_LIMIT, ACTIVITY_LIST_MAX_LIMIT, ActivityKind, ActivityListOp,
    ActivityLog, ActivityRecord, ActivityService, register_activity_ops,
};
pub use app_host::AppHost;
pub use asset_store::{AssetStore, resolve_media_type};
pub use board_ops::{BoardMutatorOp, BoardPreviewOp, BoardReadOp, register_board_ops};
pub use board_service::BoardService;
pub use collection_ops::{
    CollectionCreateOp, CollectionGetOp, CollectionListOp, CollectionRedoOp, CollectionSetSlotOp,
    CollectionUndoOp, register_collection_ops,
};
pub use collection_service::{CollectionService, CollectionSummary};
pub use dynamic_workflow_ops::{
    DynamicWorkflowCancelOp, DynamicWorkflowCreateOp, DynamicWorkflowFromRecipeOp,
    DynamicWorkflowGetOp, DynamicWorkflowGetRunOp, DynamicWorkflowListOp,
    DynamicWorkflowListRunsOp, DynamicWorkflowResumeOp, DynamicWorkflowRunOp,
    register_dynamic_workflow_ops,
};
pub use dynamic_workflow_service::{
    DYNAMIC_WORKFLOW_SLOT_CONCURRENCY, DynamicWorkflowResult, DynamicWorkflowService,
    DynamicWorkflowSummary,
};
pub use error::{AppHostError, AppHostResult};
pub use error_code::AppHostErrorCode;
pub use file_history::{
    DEFAULT_MAX_FILE_BYTES, FileEntryKind, FileHistoryCleanOutcome, FileHistoryRecoveryHook,
    FileHistoryScanStatus, FileHistoryService, FileSnapshotRecord, RestoreOutcome, ScanOptions,
    ScanPhase, ScanSummary, SnapshotSource, SystemTrashMover, TrashMover,
};
pub use library_ops::{
    LibraryAddOp, LibraryCollectOp, LibraryListOp, LibraryPlaceOp, LibraryPurgeOp, LibraryRedoOp,
    LibraryRemoveOp, LibraryRenameOp, LibraryTagOp, LibraryUndoOp, register_library_ops,
    register_library_place_op,
};
pub use library_service::{
    AddResult, AddedItem, CollectItem, LibraryItem, LibraryListFilter, LibraryService, PurgeResult,
};
pub use mod_manager::{
    InspectedModPackage, InstallModRequest, InstalledModInfo, InstalledModRecord,
    MOD_INVENTORY_SCHEMA_VERSION, ModInstallSource, ModInventory, ModInventoryStore,
    ModManagerError, ModManagerService, ModStorePaths, ModTrustSummary, ModVisibilityTable,
    UpgradeCheck,
};
pub use operation_policy::{AGENT_DENIED_OPERATIONS, agent_may_invoke};
pub use operation_service::{
    AssetImportOp, AssetListOp, InvokeRequest, InvokeResult, KernelCall, KernelOp,
    MAX_INLINE_TEXT_BYTES, OperationInfo, OperationProvider, OperationService,
    is_valid_preview_key, validate_preview_key,
};
pub use project_service::ProjectService;
pub use recipe_ops::{RecipeEditOp, RecipeGetOp, register_recipe_ops};
pub use recipe_service::{
    RecipeAssetInfo, RecipeEditOutcome, RecipeService, RecipeSnapshot, RecipeStep,
};
pub use step_log::StepLog;
pub use storage_budget::{
    CleanupEntryStatus, CleanupJournal, CleanupJournalEntry, CleanupRunState, DiskSpaceInfo,
    DiskSpaceProvider, Fs2DiskSpaceProvider, PREFLIGHT_LIBRARY_THRESHOLD_BYTES, RecoveryHook,
    StorageBudgetService, StorageCategory, WorkerStoragePaths,
};
pub use workspace::{WorkspaceHost, WorkspaceHostBuilder};
pub use workspace_scope::WorkspaceScope;
