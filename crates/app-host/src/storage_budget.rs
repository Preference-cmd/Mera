//! Workspace and application-wide storage accounting, budget enforcement, and cache recovery.

mod accounting;
mod paths;
mod recovery;
mod service;

pub(crate) use accounting::scan_project_dir_sync;
pub use accounting::{StorageCategory, dir_size_sync};
pub use paths::WorkerStoragePaths;
pub use recovery::{
    CleanupEntryStatus, CleanupJournal, CleanupJournalEntry, CleanupRunState, RecoveryHook,
};
pub use service::{
    DiskSpaceInfo, DiskSpaceProvider, Fs2DiskSpaceProvider, PREFLIGHT_LIBRARY_THRESHOLD_BYTES,
    StorageBudgetService,
};
