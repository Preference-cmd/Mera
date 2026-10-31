//! Canonical policy for operations an agent caller may not invoke.
//!
//! The policy is enforced in `OperationService::invoke`, which is the one path
//! every operation call takes, including a dynamic workflow step that carries
//! its caller's context. Surfaces that list operations (such as an operator
//! adapter) read the same predicate, so what they expose and what a call may
//! reach cannot drift apart.

/// Operations that an agent caller may never invoke.
///
/// `asset.import` reads files from arbitrary host paths. It stays human-only
/// until the agent file boundary is defined.
///
/// `dynamic_workflow.resume` executes the resumed run's remaining steps with
/// the `OperationContext` stored on the run, so an agent could borrow the
/// human actor that started it and reach operations this policy denies. It
/// stays human-only until a run records the resuming caller separately.
pub const AGENT_DENIED_OPERATIONS: &[&str] = &["asset.import", "dynamic_workflow.resume"];

/// Whether an agent caller may invoke `operation`.
///
/// Only agent callers are restricted; human, importer and system callers keep
/// their behavior.
pub fn agent_may_invoke(operation: &str) -> bool {
    !AGENT_DENIED_OPERATIONS.contains(&operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_import_is_denied_to_agents() {
        assert!(!agent_may_invoke("asset.import"));
    }

    #[test]
    fn dynamic_workflow_resume_is_denied_to_agents() {
        assert!(!agent_may_invoke("dynamic_workflow.resume"));
    }

    #[test]
    fn other_operations_remain_available_to_agents() {
        assert!(agent_may_invoke("asset.list"));
        assert!(agent_may_invoke("board.add_notes"));
        assert!(agent_may_invoke("dynamic_workflow.run"));
    }
}
