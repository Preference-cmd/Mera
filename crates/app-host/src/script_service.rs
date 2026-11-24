//! Kernel-hosted `script.python` service with two-phase approval, offline
//! single-step sandbox execution, and replay-friendly environment metadata.

mod approval;
mod runner;
#[cfg(test)]
mod tests;

pub use approval::{
    ApprovedScriptRecord, OnlinePrepareReview, SCRIPT_APPROVALS_FILENAME,
    SCRIPT_APPROVALS_SCHEMA_VERSION, ScriptApprovalReview, compute_execution_digest,
};
pub use runner::{
    DEFAULT_SCRIPT_MEMORY_BYTES, DEFAULT_SCRIPT_TIMEOUT_SECS, MAX_SCRIPT_MEMORY_BYTES,
    MAX_SCRIPT_TIMEOUT_SECS, MIN_SCRIPT_MEMORY_BYTES, ResolvedPythonInterpreter,
    SCRIPT_STDIO_LIMIT_BYTES, ScriptPythonOp, ScriptPythonResolver, ScriptService,
    ScriptServiceOptions, register_script_ops,
};
