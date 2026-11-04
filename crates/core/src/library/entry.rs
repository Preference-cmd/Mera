//! Library entry domain model.

/// An entry in the global asset library index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LibraryEntry {
    pub name: String,
    pub tags: Vec<String>,
    pub removed: bool,
    pub added_at: String,
}

impl LibraryEntry {
    pub fn new(
        name: impl Into<String>,
        tags: Vec<String>,
        removed: bool,
        added_at: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            tags,
            removed,
            added_at: added_at.into(),
        }
    }
}
