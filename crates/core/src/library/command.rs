//! Library commands and changes.

use crate::library::entry::LibraryEntry;
use crate::model::AssetId;

/// A command that mutates the library index document.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LibraryCommand {
    /// Add new entries to the library index.
    AddEntries {
        entries: Vec<(AssetId, LibraryEntry)>,
    },
    /// Rename an active library entry.
    Rename { id: AssetId, name: String },
    /// Set tags for an active library entry.
    SetTags { id: AssetId, tags: Vec<String> },
    /// Mark library entries as removed.
    Remove { ids: Vec<AssetId> },
}

/// A change record produced by applying a library command.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LibraryChange {
    /// Entries were added to the index.
    EntriesAdded {
        entries: Vec<(AssetId, LibraryEntry)>,
    },
    /// Entries were deleted from the index (inverse of AddEntries).
    EntriesDeleted { ids: Vec<AssetId> },
    /// An entry was renamed.
    EntryRenamed {
        id: AssetId,
        before: String,
        after: String,
    },
    /// Tags were updated on an entry.
    EntryTagsSet {
        id: AssetId,
        before: Vec<String>,
        after: Vec<String>,
    },
    /// Entries were marked as removed.
    EntriesRemoved { ids: Vec<AssetId> },
    /// Entries were restored from removed state (inverse of Remove).
    EntriesRestored { ids: Vec<AssetId> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_serde_roundtrip() {
        let entry = LibraryEntry::new(
            "photo.png",
            vec!["tag1".into()],
            false,
            "2026-01-01T00:00:00Z",
        );
        let cmd = LibraryCommand::AddEntries {
            entries: vec![(AssetId::new("asset-1"), entry)],
        };
        let json = serde_json::to_string(&cmd).expect("serialize");
        let back: LibraryCommand = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cmd, back);

        let rename_cmd = LibraryCommand::Rename {
            id: AssetId::new("asset-1"),
            name: "new_name.png".into(),
        };
        let json = serde_json::to_string(&rename_cmd).expect("serialize");
        let back: LibraryCommand = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(rename_cmd, back);
    }

    #[test]
    fn change_serde_roundtrip() {
        let change = LibraryChange::EntryRenamed {
            id: AssetId::new("asset-1"),
            before: "old.png".into(),
            after: "new.png".into(),
        };
        let json = serde_json::to_string(&change).expect("serialize");
        let back: LibraryChange = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(change, back);
    }
}
