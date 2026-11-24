use std::path::PathBuf;

use thiserror::Error;

use crate::policy::SandboxProfileKind;

/// Errors returned when probing, resolving, or running a sandboxed process.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// Required OS sandbox backend is unavailable under `fail_closed = true`.
    #[error("required OS sandbox backend is unavailable for `{profile:?}`: {reason}")]
    BackendUnavailable {
        profile: SandboxProfileKind,
        reason: String,
    },

    /// Sandbox policy or launch target is invalid.
    #[error("invalid sandbox policy: {reason}")]
    InvalidPolicy { reason: String },

    /// Failed to prepare sandbox wrapper arguments or mount placeholders.
    #[error("failed to prepare sandbox environment: {reason}")]
    ProfileSetupFailed { reason: String },

    /// Spawning the wrapped or direct child process failed.
    #[error("failed to spawn `{program}`: {source}")]
    SpawnFailed {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Waiting on or reaping the child process failed.
    #[error("failed while waiting on sandboxed process: {source}")]
    WaitFailed {
        #[source]
        source: std::io::Error,
    },
}
