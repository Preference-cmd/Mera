//! Per-project workspace file history, incremental scanning, single-file restore,
//! and protected storage reclamation.

mod recovery;
mod restore;
mod scanner;
mod service;
mod store;
mod types;

pub use recovery::FileHistoryRecoveryHook;
pub use service::FileHistoryService;
pub use types::{
    DEFAULT_MAX_FILE_BYTES, FileEntryKind, FileHistoryCleanOutcome, FileHistoryScanStatus,
    FileSnapshotRecord, RestoreOutcome, ScanOptions, ScanPhase, ScanSummary, SnapshotSource,
    SystemTrashMover, TrashMover,
};
