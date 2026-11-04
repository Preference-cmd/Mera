//! Library index document holding entries of assets in the global library.

use std::collections::{BTreeMap, HashSet};

use crate::diagnostic::{
    CorrelationId, Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName,
    DiagnosticTarget, DiagnosticTargetDomain,
};
use crate::document::{CommandApplication, Document};
use crate::library::command::{LibraryChange, LibraryCommand};
use crate::library::entry::LibraryEntry;
use crate::model::{AssetId, DiagnosticId, DocumentVersion, HistoryEntryId};

/// The global library index document.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LibraryIndexDocument {
    pub version: DocumentVersion,
    pub entries: BTreeMap<AssetId, LibraryEntry>,
}

impl Default for LibraryIndexDocument {
    fn default() -> Self {
        Self {
            version: DocumentVersion::new(0),
            entries: BTreeMap::new(),
        }
    }
}

impl LibraryIndexDocument {
    /// Create a new library index document.
    pub fn new(version: DocumentVersion, entries: BTreeMap<AssetId, LibraryEntry>) -> Self {
        Self { version, entries }
    }

    /// Access the document version.
    pub fn version(&self) -> DocumentVersion {
        self.version
    }

    /// Access the entries map.
    pub fn entries(&self) -> &BTreeMap<AssetId, LibraryEntry> {
        &self.entries
    }

    /// Access an entry by asset id.
    pub fn entry(&self, id: &AssetId) -> Option<&LibraryEntry> {
        self.entries.get(id)
    }
}

fn library_diagnostic(
    code: &'static str,
    message: String,
    target_id: Option<&str>,
    correlation_id: Option<&CorrelationId>,
) -> Diagnostic {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut target = DiagnosticTarget::new(DiagnosticTargetDomain::new("library"));
    if let Some(id) = target_id {
        target = target.with_id(id);
    }
    let mut diagnostic = Diagnostic::new(
        DiagnosticId::new(format!("library-diag-{count}")),
        DiagnosticCode::new(code),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("core"),
        message,
        target,
    );
    if let Some(correlation_id) = correlation_id.cloned() {
        diagnostic = diagnostic.with_correlation_id(correlation_id);
    }
    diagnostic
}

impl Document for LibraryIndexDocument {
    type Command = LibraryCommand;
    type Change = LibraryChange;
    type Context = ();

    fn version(&self) -> DocumentVersion {
        self.version
    }

    fn set_version(&mut self, version: DocumentVersion) {
        self.version = version;
    }

    fn apply_command(
        &mut self,
        command: &Self::Command,
        _context: &Self::Context,
        correlation_id: Option<&CorrelationId>,
    ) -> Result<CommandApplication<Self::Change>, Vec<Diagnostic>> {
        match command {
            LibraryCommand::AddEntries { entries } => {
                if entries.is_empty() {
                    return Ok(CommandApplication::new(Vec::new(), Vec::new()));
                }

                let mut seen_in_batch = HashSet::new();
                for (id, _) in entries {
                    if !seen_in_batch.insert(id) {
                        return Err(vec![library_diagnostic(
                            "LIBRARY/ENTRY_ALREADY_EXISTS",
                            format!("duplicate entry `{}` in command", id.as_str()),
                            Some(id.as_str()),
                            correlation_id,
                        )]);
                    }
                    if self.entries.contains_key(id) {
                        return Err(vec![library_diagnostic(
                            "LIBRARY/ENTRY_ALREADY_EXISTS",
                            format!("library entry `{}` already exists", id.as_str()),
                            Some(id.as_str()),
                            correlation_id,
                        )]);
                    }
                }

                for (id, entry) in entries {
                    self.entries.insert(id.clone(), entry.clone());
                }

                let forward = vec![LibraryChange::EntriesAdded {
                    entries: entries.clone(),
                }];
                let inverse = vec![LibraryChange::EntriesDeleted {
                    ids: entries.iter().map(|(id, _)| id.clone()).collect(),
                }];

                Ok(CommandApplication::new(forward, inverse))
            }
            LibraryCommand::Rename { id, name } => {
                let Some(entry) = self.entries.get_mut(id) else {
                    return Err(vec![library_diagnostic(
                        "LIBRARY/ENTRY_NOT_FOUND",
                        format!("library entry `{}` not found", id.as_str()),
                        Some(id.as_str()),
                        correlation_id,
                    )]);
                };

                if entry.removed {
                    return Err(vec![library_diagnostic(
                        "LIBRARY/ENTRY_REMOVED",
                        format!("library entry `{}` is removed", id.as_str()),
                        Some(id.as_str()),
                        correlation_id,
                    )]);
                }

                let trimmed = name.trim();
                if trimmed.is_empty() || trimmed.chars().count() > 256 {
                    return Err(vec![library_diagnostic(
                        "LIBRARY/INVALID_NAME",
                        format!(
                            "library entry name must be between 1 and 256 characters (got {})",
                            trimmed.chars().count()
                        ),
                        Some(id.as_str()),
                        correlation_id,
                    )]);
                }

                if entry.name == trimmed {
                    return Ok(CommandApplication::new(Vec::new(), Vec::new()));
                }

                let before = entry.name.clone();
                let after = trimmed.to_owned();
                entry.name = after.clone();

                let forward = vec![LibraryChange::EntryRenamed {
                    id: id.clone(),
                    before: before.clone(),
                    after: after.clone(),
                }];
                let inverse = vec![LibraryChange::EntryRenamed {
                    id: id.clone(),
                    before: after,
                    after: before,
                }];

                Ok(CommandApplication::new(forward, inverse))
            }
            LibraryCommand::SetTags { id, tags } => {
                let Some(entry) = self.entries.get_mut(id) else {
                    return Err(vec![library_diagnostic(
                        "LIBRARY/ENTRY_NOT_FOUND",
                        format!("library entry `{}` not found", id.as_str()),
                        Some(id.as_str()),
                        correlation_id,
                    )]);
                };

                if entry.removed {
                    return Err(vec![library_diagnostic(
                        "LIBRARY/ENTRY_REMOVED",
                        format!("library entry `{}` is removed", id.as_str()),
                        Some(id.as_str()),
                        correlation_id,
                    )]);
                }

                let mut cleaned = Vec::with_capacity(tags.len());
                for tag in tags {
                    let trimmed = tag.trim();
                    if trimmed.is_empty() || trimmed.chars().count() > 64 {
                        return Err(vec![library_diagnostic(
                            "LIBRARY/INVALID_TAG",
                            format!(
                                "library tag must be between 1 and 64 characters (got {})",
                                trimmed.chars().count()
                            ),
                            Some(id.as_str()),
                            correlation_id,
                        )]);
                    }
                    cleaned.push(trimmed.to_owned());
                }
                cleaned.sort();
                cleaned.dedup();

                if cleaned.len() > 32 {
                    return Err(vec![library_diagnostic(
                        "LIBRARY/TOO_MANY_TAGS",
                        format!(
                            "library entry cannot have more than 32 tags (got {})",
                            cleaned.len()
                        ),
                        Some(id.as_str()),
                        correlation_id,
                    )]);
                }

                if entry.tags == cleaned {
                    return Ok(CommandApplication::new(Vec::new(), Vec::new()));
                }

                let before = entry.tags.clone();
                let after = cleaned;
                entry.tags = after.clone();

                let forward = vec![LibraryChange::EntryTagsSet {
                    id: id.clone(),
                    before: before.clone(),
                    after: after.clone(),
                }];
                let inverse = vec![LibraryChange::EntryTagsSet {
                    id: id.clone(),
                    before: after,
                    after: before,
                }];

                Ok(CommandApplication::new(forward, inverse))
            }
            LibraryCommand::Remove { ids } => {
                if ids.is_empty() {
                    return Ok(CommandApplication::new(Vec::new(), Vec::new()));
                }

                let mut seen = HashSet::new();
                for id in ids {
                    if !seen.insert(id) {
                        return Err(vec![library_diagnostic(
                            "LIBRARY/DUPLICATE_ID",
                            format!("duplicate id `{}` in remove command", id.as_str()),
                            Some(id.as_str()),
                            correlation_id,
                        )]);
                    }
                    let Some(entry) = self.entries.get(id) else {
                        return Err(vec![library_diagnostic(
                            "LIBRARY/ENTRY_NOT_FOUND",
                            format!("library entry `{}` not found", id.as_str()),
                            Some(id.as_str()),
                            correlation_id,
                        )]);
                    };
                    if entry.removed {
                        return Err(vec![library_diagnostic(
                            "LIBRARY/ENTRY_REMOVED",
                            format!("library entry `{}` is already removed", id.as_str()),
                            Some(id.as_str()),
                            correlation_id,
                        )]);
                    }
                }

                for id in ids {
                    if let Some(entry) = self.entries.get_mut(id) {
                        entry.removed = true;
                    }
                }

                let forward = vec![LibraryChange::EntriesRemoved { ids: ids.clone() }];
                let inverse = vec![LibraryChange::EntriesRestored { ids: ids.clone() }];

                Ok(CommandApplication::new(forward, inverse))
            }
        }
    }

    fn version_conflict_diagnostic(
        &self,
        current_version: DocumentVersion,
        base_version: DocumentVersion,
        correlation_id: Option<&CorrelationId>,
    ) -> Diagnostic {
        let mut diagnostic = Diagnostic::new(
            DiagnosticId::new(format!("library-version-conflict-{}", base_version.get())),
            DiagnosticCode::new("LIBRARY/VERSION_CONFLICT"),
            DiagnosticSeverity::Error,
            DiagnosticSourceName::new("core"),
            format!(
                "library version conflict: expected {}, got {}",
                current_version, base_version
            ),
            DiagnosticTarget::new(DiagnosticTargetDomain::new("library")),
        );
        if let Some(correlation_id) = correlation_id.cloned() {
            diagnostic = diagnostic.with_correlation_id(correlation_id);
        }
        diagnostic
    }

    fn history_entry_id(&self, batch_id: &str) -> HistoryEntryId {
        HistoryEntryId::new(format!("library-history:{batch_id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{
        CommandActor, CommandActorKind, CommandBatch, CommandProvenance, CommandStatus,
        OperationContext,
    };
    use crate::document::DocumentSession;
    use crate::event::Timestamp;
    use crate::model::CommandBatchId;

    fn test_batch(
        version: DocumentVersion,
        commands: Vec<LibraryCommand>,
    ) -> CommandBatch<LibraryCommand> {
        CommandBatch::new(
            CommandBatchId::new("batch-lib-test"),
            OperationContext::new(
                CommandActor::new(CommandActorKind::Human),
                CommandProvenance::Direct,
            ),
            version,
            Timestamp::new("2026-01-01T00:00:00Z"),
            commands,
        )
    }

    #[test]
    fn add_entries_applies_undo_redo() {
        let mut session = DocumentSession::new(LibraryIndexDocument::default());
        let id1 = AssetId::new("asset-1");
        let id2 = AssetId::new("asset-2");
        let entry1 = LibraryEntry::new(
            "pic1.png",
            vec!["tag1".into()],
            false,
            "2026-01-01T00:00:00Z",
        );
        let entry2 = LibraryEntry::new("pic2.png", vec![], false, "2026-01-01T00:00:00Z");

        let batch = test_batch(
            DocumentVersion::new(0),
            vec![LibraryCommand::AddEntries {
                entries: vec![(id1.clone(), entry1.clone()), (id2.clone(), entry2.clone())],
            }],
        );

        let res = session.apply_batch(&(), batch);
        assert_eq!(res.status(), CommandStatus::Applied);
        assert_eq!(session.version().get(), 1);
        assert_eq!(session.document().entries.len(), 2);
        assert_eq!(session.document().entry(&id1).unwrap().name, "pic1.png");

        // Undo removes them from index
        let undo_res = session.undo().expect("undo");
        assert_eq!(undo_res.status(), CommandStatus::Applied);
        assert_eq!(session.version().get(), 2);
        assert_eq!(session.document().entries.len(), 0);

        // Redo restores them
        let redo_res = session.redo().expect("redo");
        assert_eq!(redo_res.status(), CommandStatus::Applied);
        assert_eq!(session.version().get(), 3);
        assert_eq!(session.document().entries.len(), 2);
        assert_eq!(session.document().entry(&id1).unwrap().name, "pic1.png");
    }

    #[test]
    fn add_entries_rejects_duplicate_or_existing_id() {
        let mut doc = LibraryIndexDocument::default();
        let id = AssetId::new("asset-1");
        let entry = LibraryEntry::new("pic1.png", vec![], false, "2026-01-01T00:00:00Z");

        // Duplicate in batch
        let err = doc
            .apply_command(
                &LibraryCommand::AddEntries {
                    entries: vec![(id.clone(), entry.clone()), (id.clone(), entry.clone())],
                },
                &(),
                None,
            )
            .expect_err("should reject duplicate in batch");
        assert_eq!(err[0].code().as_str(), "LIBRARY/ENTRY_ALREADY_EXISTS");

        // Apply first
        doc.apply_command(
            &LibraryCommand::AddEntries {
                entries: vec![(id.clone(), entry.clone())],
            },
            &(),
            None,
        )
        .expect("apply");

        // Add already existing
        let err = doc
            .apply_command(
                &LibraryCommand::AddEntries {
                    entries: vec![(id.clone(), entry)],
                },
                &(),
                None,
            )
            .expect_err("should reject existing id");
        assert_eq!(err[0].code().as_str(), "LIBRARY/ENTRY_ALREADY_EXISTS");
    }

    #[test]
    fn rename_applies_undo_redo_and_validations() {
        let id = AssetId::new("asset-1");
        let entry = LibraryEntry::new("old.png", vec![], false, "2026-01-01T00:00:00Z");
        let mut entries = BTreeMap::new();
        entries.insert(id.clone(), entry);
        let mut session =
            DocumentSession::new(LibraryIndexDocument::new(DocumentVersion::new(0), entries));

        // Unknown id rejection
        let err = session
            .document()
            .clone()
            .apply_command(
                &LibraryCommand::Rename {
                    id: AssetId::new("nonexistent"),
                    name: "new.png".into(),
                },
                &(),
                None,
            )
            .expect_err("unknown id");
        assert_eq!(err[0].code().as_str(), "LIBRARY/ENTRY_NOT_FOUND");

        // Empty name rejection
        let err = session
            .document()
            .clone()
            .apply_command(
                &LibraryCommand::Rename {
                    id: id.clone(),
                    name: "   ".into(),
                },
                &(),
                None,
            )
            .expect_err("empty name");
        assert_eq!(err[0].code().as_str(), "LIBRARY/INVALID_NAME");

        // Name > 256 chars rejection
        let long_name = "a".repeat(257);
        let err = session
            .document()
            .clone()
            .apply_command(
                &LibraryCommand::Rename {
                    id: id.clone(),
                    name: long_name,
                },
                &(),
                None,
            )
            .expect_err("too long");
        assert_eq!(err[0].code().as_str(), "LIBRARY/INVALID_NAME");

        // Apply rename
        let batch = test_batch(
            DocumentVersion::new(0),
            vec![LibraryCommand::Rename {
                id: id.clone(),
                name: "  new.png  ".into(),
            }],
        );
        let res = session.apply_batch(&(), batch);
        assert_eq!(res.status(), CommandStatus::Applied);
        assert_eq!(session.document().entry(&id).unwrap().name, "new.png");

        // Undo rename
        session.undo().expect("undo");
        assert_eq!(session.document().entry(&id).unwrap().name, "old.png");

        // Redo rename
        session.redo().expect("redo");
        assert_eq!(session.document().entry(&id).unwrap().name, "new.png");
    }

    #[test]
    fn set_tags_applies_undo_redo_and_validations() {
        let id = AssetId::new("asset-1");
        let entry = LibraryEntry::new(
            "pic.png",
            vec!["initial".into()],
            false,
            "2026-01-01T00:00:00Z",
        );
        let mut entries = BTreeMap::new();
        entries.insert(id.clone(), entry);
        let mut session =
            DocumentSession::new(LibraryIndexDocument::new(DocumentVersion::new(0), entries));

        // Unknown id rejection
        let err = session
            .document()
            .clone()
            .apply_command(
                &LibraryCommand::SetTags {
                    id: AssetId::new("nonexistent"),
                    tags: vec!["tag".into()],
                },
                &(),
                None,
            )
            .expect_err("unknown id");
        assert_eq!(err[0].code().as_str(), "LIBRARY/ENTRY_NOT_FOUND");

        // Empty tag rejection
        let err = session
            .document()
            .clone()
            .apply_command(
                &LibraryCommand::SetTags {
                    id: id.clone(),
                    tags: vec!["  ".into()],
                },
                &(),
                None,
            )
            .expect_err("empty tag");
        assert_eq!(err[0].code().as_str(), "LIBRARY/INVALID_TAG");

        // Tag > 64 chars rejection
        let long_tag = "t".repeat(65);
        let err = session
            .document()
            .clone()
            .apply_command(
                &LibraryCommand::SetTags {
                    id: id.clone(),
                    tags: vec![long_tag],
                },
                &(),
                None,
            )
            .expect_err("tag too long");
        assert_eq!(err[0].code().as_str(), "LIBRARY/INVALID_TAG");

        // > 32 tags rejection
        let too_many_tags: Vec<String> = (0..33).map(|i| format!("tag-{i}")).collect();
        let err = session
            .document()
            .clone()
            .apply_command(
                &LibraryCommand::SetTags {
                    id: id.clone(),
                    tags: too_many_tags,
                },
                &(),
                None,
            )
            .expect_err("too many tags");
        assert_eq!(err[0].code().as_str(), "LIBRARY/TOO_MANY_TAGS");

        // Valid tags: trimmed, deduplicated, sorted
        let batch = test_batch(
            DocumentVersion::new(0),
            vec![LibraryCommand::SetTags {
                id: id.clone(),
                tags: vec![
                    "  z-tag ".into(),
                    "a-tag".into(),
                    "z-tag".into(),
                    "m-tag".into(),
                ],
            }],
        );
        let res = session.apply_batch(&(), batch);
        assert_eq!(res.status(), CommandStatus::Applied);
        assert_eq!(
            session.document().entry(&id).unwrap().tags,
            vec!["a-tag", "m-tag", "z-tag"]
        );

        // Undo
        session.undo().expect("undo");
        assert_eq!(session.document().entry(&id).unwrap().tags, vec!["initial"]);

        // Redo
        session.redo().expect("redo");
        assert_eq!(
            session.document().entry(&id).unwrap().tags,
            vec!["a-tag", "m-tag", "z-tag"]
        );
    }

    #[test]
    fn remove_applies_undo_redo_and_validations() {
        let id1 = AssetId::new("asset-1");
        let id2 = AssetId::new("asset-2");
        let entry1 = LibraryEntry::new("pic1.png", vec![], false, "2026-01-01T00:00:00Z");
        let entry2 = LibraryEntry::new("pic2.png", vec![], false, "2026-01-01T00:00:00Z");
        let mut entries = BTreeMap::new();
        entries.insert(id1.clone(), entry1);
        entries.insert(id2.clone(), entry2);
        let mut session =
            DocumentSession::new(LibraryIndexDocument::new(DocumentVersion::new(0), entries));

        // Unknown id rejection
        let err = session
            .document()
            .clone()
            .apply_command(
                &LibraryCommand::Remove {
                    ids: vec![AssetId::new("nonexistent")],
                },
                &(),
                None,
            )
            .expect_err("unknown id");
        assert_eq!(err[0].code().as_str(), "LIBRARY/ENTRY_NOT_FOUND");

        // Duplicate id in remove
        let err = session
            .document()
            .clone()
            .apply_command(
                &LibraryCommand::Remove {
                    ids: vec![id1.clone(), id1.clone()],
                },
                &(),
                None,
            )
            .expect_err("duplicate id");
        assert_eq!(err[0].code().as_str(), "LIBRARY/DUPLICATE_ID");

        // Apply remove
        let batch = test_batch(
            DocumentVersion::new(0),
            vec![LibraryCommand::Remove {
                ids: vec![id1.clone()],
            }],
        );
        let res = session.apply_batch(&(), batch);
        assert_eq!(res.status(), CommandStatus::Applied);
        assert!(session.document().entry(&id1).unwrap().removed);
        assert!(!session.document().entry(&id2).unwrap().removed);

        // Cannot remove already removed
        let err = session
            .document()
            .clone()
            .apply_command(
                &LibraryCommand::Remove {
                    ids: vec![id1.clone()],
                },
                &(),
                None,
            )
            .expect_err("already removed");
        assert_eq!(err[0].code().as_str(), "LIBRARY/ENTRY_REMOVED");

        // Cannot rename removed entry
        let err = session
            .document()
            .clone()
            .apply_command(
                &LibraryCommand::Rename {
                    id: id1.clone(),
                    name: "renamed.png".into(),
                },
                &(),
                None,
            )
            .expect_err("rename removed");
        assert_eq!(err[0].code().as_str(), "LIBRARY/ENTRY_REMOVED");

        // Cannot set tags on removed entry
        let err = session
            .document()
            .clone()
            .apply_command(
                &LibraryCommand::SetTags {
                    id: id1.clone(),
                    tags: vec!["tag".into()],
                },
                &(),
                None,
            )
            .expect_err("set tags removed");
        assert_eq!(err[0].code().as_str(), "LIBRARY/ENTRY_REMOVED");

        // Undo restores removed status
        session.undo().expect("undo");
        assert!(!session.document().entry(&id1).unwrap().removed);

        // Redo re-removes
        session.redo().expect("redo");
        assert!(session.document().entry(&id1).unwrap().removed);
    }
}
