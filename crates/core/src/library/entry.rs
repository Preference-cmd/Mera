//! Library entry domain model.

use crate::model::{AssetId, ProjectId};

/// Provenance of an asset entry in the global library.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LibrarySource {
    /// Ingested from a host file path.
    File { path: String },
    /// Collected from an existing project asset.
    Project {
        project_id: ProjectId,
        asset_id: AssetId,
    },
}

/// An entry in the global asset library index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LibraryEntry {
    pub name: String,
    pub tags: Vec<String>,
    pub removed: bool,
    pub added_at: String,
    pub source: LibrarySource,
}

impl LibraryEntry {
    pub fn new(
        name: impl Into<String>,
        tags: Vec<String>,
        removed: bool,
        added_at: impl Into<String>,
        source: LibrarySource,
    ) -> Self {
        Self {
            name: name.into(),
            tags,
            removed,
            added_at: added_at.into(),
            source,
        }
    }
}
