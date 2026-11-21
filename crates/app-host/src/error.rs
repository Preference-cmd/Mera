use mera_config::ConfigError;
use mera_core::model::{CollectionId, DocumentVersion, HistoryEntryId, ProjectId};

use crate::error_code::AppHostErrorCode;

pub type AppHostResult<T> = Result<T, AppHostError>;

#[derive(Debug)]
pub enum AppHostError {
    UnknownProject {
        project_id: ProjectId,
    },
    ProjectAlreadyExists {
        project_id: ProjectId,
    },
    ProjectIdPathUnsafe {
        project_id: String,
    },
    UnknownBoard {
        project_id: ProjectId,
    },
    UnknownCollection {
        project_id: ProjectId,
        collection_id: CollectionId,
    },
    /// An operation-supplied board base version does not match the version
    /// the session holds. Nothing was applied, persisted, emitted or recorded;
    /// the caller must re-read the board and retry on the current version.
    BoardVersionConflict {
        project_id: ProjectId,
        base_version: DocumentVersion,
        current_version: DocumentVersion,
    },
    /// A board undo/redo request named a history entry that is not the
    /// current top of that direction's stack. The board was not mutated and
    /// no activity record was appended. The details carry both current tops
    /// so a caller can re-synchronize its own stacks.
    BoardHistoryMismatch {
        project_id: ProjectId,
        /// The requested direction: `"undo"` or `"redo"`.
        action: &'static str,
        expected_entry_id: HistoryEntryId,
        undo_entry_id: Option<HistoryEntryId>,
        redo_entry_id: Option<HistoryEntryId>,
    },
    Io {
        path: std::path::PathBuf,
        message: String,
    },
    BootstrapConfig(ConfigError),
    ModManager(crate::mod_manager::ModManagerError),
    Diagnostic(Box<mera_core::diagnostic::Diagnostic>),
}

impl std::fmt::Display for AppHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProject { project_id } => {
                write!(f, "unknown project `{project_id}`")
            }
            Self::ProjectAlreadyExists { project_id } => {
                write!(f, "project `{project_id}` already exists")
            }
            Self::ProjectIdPathUnsafe { project_id } => {
                write!(
                    f,
                    "project id `{project_id}` is not safe as a project directory name"
                )
            }
            Self::UnknownBoard { project_id } => {
                write!(f, "project `{project_id}` has no board")
            }
            Self::UnknownCollection {
                project_id,
                collection_id,
            } => {
                write!(
                    f,
                    "project `{project_id}` has no collection `{collection_id}`"
                )
            }
            Self::Diagnostic(diag) => write!(f, "{}", diag.message()),
            Self::BoardVersionConflict {
                project_id,
                base_version,
                current_version,
            } => write!(
                f,
                "board version conflict for project `{project_id}`: the batch is based on version {base_version}, but the board is at version {current_version}"
            ),
            Self::BoardHistoryMismatch {
                project_id,
                action,
                expected_entry_id,
                undo_entry_id,
                redo_entry_id,
            } => write!(
                f,
                "board {action} for project `{project_id}` expected history entry `{expected_entry_id}`, but the current undo entry is {} and the current redo entry is {}",
                option_entry(undo_entry_id),
                option_entry(redo_entry_id)
            ),
            Self::Io { path, message } => {
                write!(f, "io error at `{}`: {message}", path.display())
            }
            Self::BootstrapConfig(error) => write!(f, "config bootstrap failed: {error}"),
            Self::ModManager(error) => write!(f, "{error}"),
        }
    }
}

impl AppHostError {
    /// Machine-readable classification for IPC error payloads.
    pub fn code(&self) -> AppHostErrorCode {
        match self {
            Self::UnknownProject { .. } => AppHostErrorCode::NotFound,
            Self::ProjectAlreadyExists { .. } => AppHostErrorCode::Conflict,
            Self::ProjectIdPathUnsafe { .. } => AppHostErrorCode::PermissionDenied,
            Self::UnknownBoard { .. } => AppHostErrorCode::NotFound,
            Self::UnknownCollection { .. } => AppHostErrorCode::NotFound,
            Self::BoardVersionConflict { .. } => AppHostErrorCode::Conflict,
            Self::BoardHistoryMismatch { .. } => AppHostErrorCode::Conflict,
            Self::Io { .. } => AppHostErrorCode::Io,
            Self::BootstrapConfig(_) => AppHostErrorCode::BootstrapFailed,
            Self::ModManager(error) => match error {
                crate::mod_manager::ModManagerError::SourceConflict { .. }
                | crate::mod_manager::ModManagerError::Busy { .. } => AppHostErrorCode::Conflict,
                crate::mod_manager::ModManagerError::ConsentRequired { .. }
                | crate::mod_manager::ModManagerError::PermissionExpansionRequiresConsent {
                    ..
                } => AppHostErrorCode::PermissionDenied,
                crate::mod_manager::ModManagerError::UnknownMod { .. }
                | crate::mod_manager::ModManagerError::UnknownOperation { .. } => {
                    AppHostErrorCode::NotFound
                }
                crate::mod_manager::ModManagerError::Io { .. } => AppHostErrorCode::Io,
                crate::mod_manager::ModManagerError::InvalidInput { .. }
                | crate::mod_manager::ModManagerError::Cancelled
                | crate::mod_manager::ModManagerError::ManifestMismatch { .. }
                | crate::mod_manager::ModManagerError::SelfCheckFailed { .. }
                | crate::mod_manager::ModManagerError::ArchiveInvalid { .. }
                | crate::mod_manager::ModManagerError::RemoteCatalog { .. }
                | crate::mod_manager::ModManagerError::Diagnostic(_) => {
                    AppHostErrorCode::CommandFailed
                }
            },
            Self::Diagnostic(_) => AppHostErrorCode::CommandFailed,
        }
    }

    /// Optional structured context for the IPC error payload.
    pub fn details(&self) -> Option<serde_json::Value> {
        match self {
            Self::UnknownProject { project_id }
            | Self::ProjectAlreadyExists { project_id }
            | Self::UnknownBoard { project_id } => {
                Some(serde_json::json!({ "project_id": project_id.to_string() }))
            }
            Self::UnknownCollection {
                project_id,
                collection_id,
            } => Some(serde_json::json!({
                "project_id": project_id.to_string(),
                "collection_id": collection_id.to_string(),
            })),
            Self::ProjectIdPathUnsafe { project_id } => {
                Some(serde_json::json!({ "project_id": project_id }))
            }
            Self::BoardVersionConflict {
                project_id,
                base_version,
                current_version,
            } => Some(serde_json::json!({
                "project_id": project_id.to_string(),
                "base_version": base_version.to_string(),
                "current_version": current_version.to_string(),
            })),
            Self::BoardHistoryMismatch {
                project_id,
                action,
                expected_entry_id,
                undo_entry_id,
                redo_entry_id,
            } => Some(serde_json::json!({
                "project_id": project_id.to_string(),
                "action": action,
                "expected_entry_id": expected_entry_id.to_string(),
                "undo_entry_id": undo_entry_id.as_ref().map(ToString::to_string),
                "redo_entry_id": redo_entry_id.as_ref().map(ToString::to_string),
            })),
            Self::Io { path, .. } => {
                Some(serde_json::json!({ "path": path.display().to_string() }))
            }
            Self::ModManager(
                crate::mod_manager::ModManagerError::PermissionExpansionRequiresConsent {
                    mod_id,
                    reasons,
                },
            ) => Some(serde_json::json!({
                "kind": "permission_expansion",
                "mod_id": mod_id,
                "reasons": reasons,
            })),
            Self::Diagnostic(diag) => serde_json::to_value(diag.as_ref()).ok(),
            _ => None,
        }
    }
}

impl std::error::Error for AppHostError {}

fn option_entry(entry_id: &Option<HistoryEntryId>) -> &str {
    entry_id.as_ref().map_or("none", HistoryEntryId::as_str)
}

impl From<ConfigError> for AppHostError {
    fn from(error: ConfigError) -> Self {
        Self::BootstrapConfig(error)
    }
}

impl From<mera_core::diagnostic::Diagnostic> for AppHostError {
    fn from(diag: mera_core::diagnostic::Diagnostic) -> Self {
        Self::Diagnostic(Box::new(diag))
    }
}

impl From<crate::mod_manager::ModManagerError> for AppHostError {
    fn from(error: crate::mod_manager::ModManagerError) -> Self {
        Self::ModManager(error)
    }
}
