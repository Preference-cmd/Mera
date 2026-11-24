use mera_sandbox::{SandboxProfileKind, detect_sandbox_backend_for};
use serde::Serialize;

use crate::script_service::{ApprovedScriptRecord, OnlinePrepareReview, ScriptApprovalReview};

/// Snapshot of script sandbox status, pending reviews, and persisted approvals
/// for one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScriptApprovalsSnapshotDto {
    pub project_id: String,
    pub sandbox_backend: String,
    pub sandbox_enforced: bool,
    pub pending_executions: Vec<ScriptApprovalReview>,
    pub pending_online_prepares: Vec<OnlinePrepareReview>,
    pub approved_executions: Vec<ApprovedScriptRecord>,
}

impl ScriptApprovalsSnapshotDto {
    #[must_use]
    pub fn build(
        project_id: impl Into<String>,
        pending_executions: Vec<ScriptApprovalReview>,
        pending_online_prepares: Vec<OnlinePrepareReview>,
        approved_executions: Vec<ApprovedScriptRecord>,
    ) -> Self {
        let backend = detect_sandbox_backend_for(SandboxProfileKind::ScriptStep);
        Self {
            project_id: project_id.into(),
            sandbox_backend: backend.as_str().to_owned(),
            sandbox_enforced: backend.is_os_enforced(),
            pending_executions,
            pending_online_prepares,
            approved_executions,
        }
    }
}
