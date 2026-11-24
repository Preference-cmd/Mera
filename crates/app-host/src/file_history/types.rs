use std::collections::BTreeMap;
use std::path::Path;

use mera_core::command::CommandActor;
use mera_core::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
    DiagnosticTargetDomain,
};
use mera_core::event::Timestamp;
use mera_core::model::{DiagnosticId, FileSnapshotId, ProjectId, WorkspaceFolderId};
use mera_core::task::CancelToken;
use serde::{Deserialize, Serialize};

use crate::{AppHostError, AppHostResult};

/// Default per-file content capture limit (200 MiB).
pub const DEFAULT_MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;

/// Origin of a recorded file snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotSource {
    TurnStart,
    TurnEnd,
    AcpWrite,
    Manual,
    PreRestore,
}

/// Kind of filesystem entry tracked in file history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEntryKind {
    File,
    Symlink,
    Deleted,
}

/// Immutable metadata record for one snapshot of a workspace path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSnapshotRecord {
    pub id: FileSnapshotId,
    pub folder_id: WorkspaceFolderId,
    pub rel_path: String,
    pub kind: FileEntryKind,
    pub hash: Option<String>,
    pub size: u64,
    pub mtime_secs: i64,
    pub mtime_nanos: u32,
    pub recorded_at: Timestamp,
    pub source: SnapshotSource,
    pub turn_id: Option<String>,
    pub actor: Option<CommandActor>,
    pub oversized: bool,
    pub symlink_target: Option<String>,
    pub pinned: bool,
    pub note: Option<String>,
}

/// Lifecycle phase of a project's file history scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    Idle,
    Scanning,
    Ready,
    Cancelled,
    Failed,
}

/// Live or completed status of a project's file history scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHistoryScanStatus {
    pub project_id: ProjectId,
    pub phase: ScanPhase,
    pub ready: bool,
    pub visited_files: u64,
    pub hashed_files: u64,
    pub recorded_snapshots: u64,
    pub skipped_ignored: u64,
    pub skipped_oversized: u64,
    pub skipped_over_budget: u64,
    pub deleted_files: u64,
    pub cancelled: bool,
    pub error: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

impl FileHistoryScanStatus {
    pub(crate) fn idle(project_id: ProjectId) -> Self {
        Self {
            project_id,
            phase: ScanPhase::Idle,
            ready: false,
            visited_files: 0,
            hashed_files: 0,
            recorded_snapshots: 0,
            skipped_ignored: 0,
            skipped_oversized: 0,
            skipped_over_budget: 0,
            deleted_files: 0,
            cancelled: false,
            error: None,
            started_at: None,
            finished_at: None,
        }
    }
}

/// Parameters controlling a file history scan.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub source: SnapshotSource,
    pub turn_id: Option<String>,
    pub actor: Option<CommandActor>,
    pub note: Option<String>,
    pub pinned: bool,
    pub cancel: CancelToken,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            source: SnapshotSource::Manual,
            turn_id: None,
            actor: None,
            note: None,
            pinned: false,
            cancel: CancelToken::new(),
        }
    }
}

impl ScanOptions {
    #[must_use]
    pub fn for_source(source: SnapshotSource) -> Self {
        Self {
            source,
            ..Self::default()
        }
    }
}

/// Counters produced by a completed or cancelled scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSummary {
    pub visited_files: u64,
    pub hashed_files: u64,
    pub recorded_snapshots: u64,
    pub skipped_ignored: u64,
    pub skipped_oversized: u64,
    pub skipped_over_budget: u64,
    pub deleted_files: u64,
    pub added_blob_bytes: u64,
    pub cancelled: bool,
}

/// Outcome of restoring a single snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreOutcome {
    pub restored_snapshot_id: FileSnapshotId,
    pub pre_restore_snapshot_id: FileSnapshotId,
    pub folder_id: WorkspaceFolderId,
    pub rel_path: String,
    pub kind: FileEntryKind,
    pub moved_to_trash: bool,
}

/// Outcome of manual per-project history cleanup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHistoryCleanOutcome {
    pub project_id: ProjectId,
    pub removed_records: usize,
    pub kept_records: usize,
    pub removed_blobs: usize,
    pub freed_bytes: u64,
}

/// Abstraction for moving overwritten or deleted files to the OS trash.
pub trait TrashMover: Send + Sync + std::fmt::Debug {
    fn move_to_trash(&self, path: &Path) -> Result<(), String>;
}

/// Default [`TrashMover`] backed by the system trash (`trash::delete`).
#[derive(Debug, Default)]
pub struct SystemTrashMover;

impl TrashMover for SystemTrashMover {
    fn move_to_trash(&self, path: &Path) -> Result<(), String> {
        trash::delete(path).map_err(|err| err.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum IndexLine {
    Snapshot(Box<FileSnapshotRecord>),
    SetPin {
        id: FileSnapshotId,
        pinned: bool,
        updated_at: Timestamp,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CachedFileState {
    pub kind: FileEntryKind,
    pub size: u64,
    pub mtime_secs: i64,
    pub mtime_nanos: u32,
    pub hash: Option<String>,
    pub oversized: bool,
    pub symlink_target: Option<String>,
}

impl CachedFileState {
    pub(crate) fn matches_record(&self, rec: &FileSnapshotRecord) -> bool {
        self.kind == rec.kind
            && self.size == rec.size
            && self.mtime_secs == rec.mtime_secs
            && self.mtime_nanos == rec.mtime_nanos
            && self.hash == rec.hash
            && self.oversized == rec.oversized
            && self.symlink_target == rec.symlink_target
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ScanStateDoc {
    pub schema_version: u32,
    pub entries: BTreeMap<String, BTreeMap<String, CachedFileState>>,
}

pub(crate) fn file_history_caller_error(code: &str, message: impl Into<String>) -> AppHostError {
    AppHostError::Diagnostic(Box::new(Diagnostic::new(
        DiagnosticId::new(format!("file_history:{code}")),
        DiagnosticCode::new(format!("FILE_HISTORY/CALLER_{code}")),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("file_history"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new("file_history")),
    )))
}

pub(crate) fn file_history_internal_error(code: &str, message: impl Into<String>) -> AppHostError {
    AppHostError::Diagnostic(Box::new(Diagnostic::new(
        DiagnosticId::new(format!("file_history:{code}")),
        DiagnosticCode::new(format!("FILE_HISTORY/INTERNAL_{code}")),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("file_history"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new("file_history")),
    )))
}

/// Validate and normalize a workspace-relative POSIX path.
///
/// Rejects empty paths, absolute paths, backslashes, NUL bytes, Windows drive
/// prefixes, and `.` or `..` traversal components.
pub(crate) fn normalize_rel_path(raw: &str) -> AppHostResult<String> {
    if raw.is_empty() {
        return Err(file_history_caller_error(
            "INVALID_REL_PATH",
            "relative path must not be empty",
        ));
    }
    if raw.contains('\0') {
        return Err(file_history_caller_error(
            "INVALID_REL_PATH",
            "relative path must not contain NUL bytes",
        ));
    }
    if raw.contains('\\') {
        return Err(file_history_caller_error(
            "INVALID_REL_PATH",
            format!("relative path `{raw}` must use POSIX `/` separators"),
        ));
    }
    if raw.starts_with('/') {
        return Err(file_history_caller_error(
            "INVALID_REL_PATH",
            format!("relative path `{raw}` must not be absolute"),
        ));
    }
    if raw.as_bytes().get(1) == Some(&b':')
        && raw
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic())
    {
        return Err(file_history_caller_error(
            "INVALID_REL_PATH",
            format!("relative path `{raw}` must not carry a drive prefix"),
        ));
    }

    let mut normalized = Vec::new();
    for seg in raw.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(file_history_caller_error(
                "INVALID_REL_PATH",
                format!("relative path `{raw}` contains invalid segment `{seg}`"),
            ));
        }
        normalized.push(seg);
    }

    Ok(normalized.join("/"))
}

pub(crate) fn is_snapshot_protected(source: SnapshotSource, pinned: bool) -> bool {
    pinned
        || matches!(
            source,
            SnapshotSource::TurnStart | SnapshotSource::PreRestore
        )
}

pub(crate) fn metadata_mtime(meta: &std::fs::Metadata) -> (i64, u32) {
    match meta.modified() {
        Ok(time) => match time.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => (
                i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
                d.subsec_nanos(),
            ),
            Err(err) => {
                let d = err.duration();
                let secs = i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
                if d.subsec_nanos() == 0 {
                    (-secs, 0)
                } else {
                    (-secs - 1, 1_000_000_000 - d.subsec_nanos())
                }
            }
        },
        Err(_) => (0, 0),
    }
}
