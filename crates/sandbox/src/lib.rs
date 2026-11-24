//! Cross-platform OS-level process sandbox drivers, policy model, resource
//! limits, and bounded process-group execution for Mera.

#![deny(unsafe_code)]

mod backend;
mod error;
mod policy;
mod runner;

pub use backend::{
    SandboxBackend, detect_sandbox_backend, detect_sandbox_backend_for, resolve_sandbox_launch,
};
pub use error::SandboxError;
pub use policy::{NetworkPolicy, ResourceLimits, SandboxLaunch, SandboxPolicy, SandboxProfileKind};
pub use runner::{
    SandboxCancelToken, SandboxExecRequest, SandboxRunOutput, TerminationReason, run_sandboxed,
};
