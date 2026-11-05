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
/// `library.add` reads arbitrary host paths, for the same reason `asset.import`
/// is denied today; a later phase opens it with a workspace-path check.
///
/// `library.remove` modifies the shared global library; because the library is
/// shared by every project, only a human removes items from it.
///
/// `library.purge` permanently drops removed entries, deletes unreferenced
/// content files and clears library undo history; because it is irreversible
/// and affects every project, only a human may invoke it.
///
/// `dynamic_workflow.resume` executes the resumed run's remaining steps with
/// the `OperationContext` stored on the run, so an agent could borrow the
/// human actor that started it and reach operations this policy denies. It
/// stays human-only until a run records the resuming caller separately.
pub const AGENT_DENIED_OPERATIONS: &[&str] = &[
    "asset.import",
    "library.add",
    "library.remove",
    "library.purge",
    "dynamic_workflow.resume",
];

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
    fn library_add_is_denied_to_agents() {
        assert!(!agent_may_invoke("library.add"));
    }

    #[test]
    fn library_remove_is_denied_to_agents() {
        assert!(!agent_may_invoke("library.remove"));
    }

    #[test]
    fn library_purge_is_denied_to_agents() {
        assert!(!agent_may_invoke("library.purge"));
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
        assert!(agent_may_invoke("library.collect"));
        assert!(agent_may_invoke("library.list"));
        assert!(agent_may_invoke("library.rename"));
        assert!(agent_may_invoke("library.tag"));
    }
}
