use serde::Serialize;

/// Machine-readable failure classification carried in IPC error payloads.
///
/// The frontend branches on the snake_case string form of these codes
/// (e.g. `not_found`, `conflict`) instead of parsing free-form messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppHostErrorCode {
    /// Workspace bootstrap failed.
    BootstrapFailed,
    /// Generic command failure (no more specific classification applies).
    CommandFailed,
    /// The named provider is not registered. Nothing produces it while no
    /// agent integration is wired in; kept so the code set the frontend
    /// matches on stays stable.
    UnknownProvider,
    /// The caller supplied an unsafe path or reference.
    PermissionDenied,
    /// The requested entity does not exist.
    NotFound,
    /// Version or state conflict (e.g. stale board version).
    Conflict,
    /// A filesystem operation failed.
    Io,
}

impl AppHostErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BootstrapFailed => "bootstrap_failed",
            Self::CommandFailed => "command_failed",
            Self::UnknownProvider => "unknown_provider",
            Self::PermissionDenied => "permission_denied",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Io => "io_error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppHostError;
    use mera_core::model::{CollectionId, DocumentVersion, HistoryEntryId, ProjectId};

    #[test]
    fn unknown_entities_classify_as_not_found() {
        let project_id = ProjectId::new("proj-1");
        assert_eq!(
            AppHostError::UnknownProject {
                project_id: project_id.clone(),
            }
            .code(),
            AppHostErrorCode::NotFound
        );
        assert_eq!(
            AppHostError::UnknownBoard {
                project_id: project_id.clone(),
            }
            .code(),
            AppHostErrorCode::NotFound
        );
        assert_eq!(
            AppHostError::UnknownCollection {
                project_id,
                collection_id: CollectionId::new("col-1"),
            }
            .code(),
            AppHostErrorCode::NotFound
        );
    }

    #[test]
    fn conflicting_board_state_classifies_as_conflict() {
        let project_id = ProjectId::new("proj-1");
        assert_eq!(
            AppHostError::BoardVersionConflict {
                project_id: project_id.clone(),
                base_version: DocumentVersion::new(1),
                current_version: DocumentVersion::new(2),
            }
            .code(),
            AppHostErrorCode::Conflict
        );
        assert_eq!(
            AppHostError::BoardHistoryMismatch {
                project_id,
                action: "undo",
                expected_entry_id: HistoryEntryId::new("h-1"),
                undo_entry_id: None,
                redo_entry_id: None,
            }
            .code(),
            AppHostErrorCode::Conflict
        );
    }

    #[test]
    fn unsafe_project_id_classifies_as_permission_denied() {
        assert_eq!(
            AppHostError::ProjectIdPathUnsafe {
                project_id: "../escape".to_owned(),
            }
            .code(),
            AppHostErrorCode::PermissionDenied
        );
    }

    #[test]
    fn unknown_provider_keeps_its_wire_name() {
        assert_eq!(
            AppHostErrorCode::UnknownProvider.as_str(),
            "unknown_provider"
        );
        assert_eq!(
            serde_json::to_value(AppHostErrorCode::UnknownProvider).expect("serialize"),
            "unknown_provider"
        );
    }

    #[test]
    fn bootstrap_config_failure_classifies_as_bootstrap_failed() {
        assert_eq!(
            AppHostError::BootstrapConfig(mera_config::ConfigError::PathInvalid {
                key: "bad".to_owned(),
                reason: "invalid".to_owned(),
            })
            .code(),
            AppHostErrorCode::BootstrapFailed
        );
    }

    #[test]
    fn io_classifies_as_io() {
        assert_eq!(
            AppHostError::Io {
                path: std::path::PathBuf::from("/tmp/x"),
                message: "boom".to_owned(),
            }
            .code(),
            AppHostErrorCode::Io
        );
    }
}
