//! Project domain aggregate root.
//!
//! A Project is the top-level container for all creative work in Reimagine.
//! It owns boards, workflows, assets, and runs.

use crate::event::Timestamp;
use crate::model::{ProjectId, WorkspaceFolderId};

/// A project workspace folder serving as a boundary for agent file operations.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceFolder {
    id: WorkspaceFolderId,
    path: String,
}

impl WorkspaceFolder {
    /// Create a new workspace folder descriptor.
    ///
    /// `path` must be a canonical absolute path.
    pub fn new(id: WorkspaceFolderId, path: impl Into<String>) -> Self {
        Self {
            id,
            path: path.into(),
        }
    }

    /// The unique identifier of the workspace folder.
    pub fn id(&self) -> &WorkspaceFolderId {
        &self.id
    }

    /// The canonical absolute path of the workspace folder.
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// Project domain aggregate root.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Project {
    id: ProjectId,
    metadata: ProjectMetadata,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    folders: Vec<WorkspaceFolder>,
}

impl Project {
    /// Create a new project with the given id and metadata.
    pub fn new(id: ProjectId, metadata: ProjectMetadata) -> Self {
        Self {
            id,
            metadata,
            folders: Vec::new(),
        }
    }

    /// Create a new project with the given id, metadata, and workspace folders.
    pub fn with_folders(
        id: ProjectId,
        metadata: ProjectMetadata,
        folders: Vec<WorkspaceFolder>,
    ) -> Self {
        Self {
            id,
            metadata,
            folders,
        }
    }

    /// Get the project id.
    pub fn id(&self) -> &ProjectId {
        &self.id
    }

    /// Get the project metadata.
    pub fn metadata(&self) -> &ProjectMetadata {
        &self.metadata
    }

    /// Get a mutable reference to the project metadata.
    pub fn metadata_mut(&mut self) -> &mut ProjectMetadata {
        &mut self.metadata
    }

    /// Get the project's workspace folders.
    pub fn folders(&self) -> &[WorkspaceFolder] {
        &self.folders
    }

    /// Add a workspace folder to the project.
    pub fn add_folder(&mut self, folder: WorkspaceFolder) {
        self.folders.push(folder);
    }

    /// Remove a workspace folder from the project by id.
    /// Returns whether a folder was removed.
    pub fn remove_folder(&mut self, folder_id: &WorkspaceFolderId) -> bool {
        if let Some(pos) = self.folders.iter().position(|f| f.id() == folder_id) {
            self.folders.remove(pos);
            true
        } else {
            false
        }
    }
}

/// Metadata for a project.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectMetadata {
    name: String,
    description: String,
    created_at: Timestamp,
    updated_at: Timestamp,
}

impl ProjectMetadata {
    /// Create new project metadata.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        created_at: Timestamp,
        updated_at: Timestamp,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            created_at,
            updated_at,
        }
    }

    /// Get the project name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the project description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Get the creation timestamp.
    pub fn created_at(&self) -> &Timestamp {
        &self.created_at
    }

    /// Get the last update timestamp.
    pub fn updated_at(&self) -> &Timestamp {
        &self.updated_at
    }

    /// Set the project name.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// Set the project description.
    pub fn set_description(&mut self, description: impl Into<String>) {
        self.description = description.into();
    }

    /// Set the update timestamp.
    pub fn set_updated_at(&mut self, updated_at: Timestamp) {
        self.updated_at = updated_at;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProjectId;

    #[test]
    fn project_creation() {
        let id = ProjectId::new("my-project");
        let metadata = ProjectMetadata::new(
            "My Project",
            "A test project",
            Timestamp::new("2026-01-01T00:00:00Z"),
            Timestamp::new("2026-01-01T00:00:00Z"),
        );

        let project = Project::new(id.clone(), metadata.clone());
        assert_eq!(project.id(), &id);
        assert_eq!(project.metadata(), &metadata);
    }

    #[test]
    fn project_metadata_accessors() {
        let metadata = ProjectMetadata::new(
            "Test",
            "Description",
            Timestamp::new("2026-01-01T00:00:00Z"),
            Timestamp::new("2026-01-02T00:00:00Z"),
        );

        assert_eq!(metadata.name(), "Test");
        assert_eq!(metadata.description(), "Description");
        assert_eq!(metadata.created_at().as_str(), "2026-01-01T00:00:00Z");
        assert_eq!(metadata.updated_at().as_str(), "2026-01-02T00:00:00Z");
    }

    #[test]
    fn project_metadata_mutation() {
        let mut metadata = ProjectMetadata::new(
            "Old Name",
            "Old Description",
            Timestamp::new("2026-01-01T00:00:00Z"),
            Timestamp::new("2026-01-01T00:00:00Z"),
        );

        metadata.set_name("New Name");
        metadata.set_description("New Description");
        metadata.set_updated_at(Timestamp::new("2026-01-03T00:00:00Z"));

        assert_eq!(metadata.name(), "New Name");
        assert_eq!(metadata.description(), "New Description");
        assert_eq!(metadata.updated_at().as_str(), "2026-01-03T00:00:00Z");
    }

    #[test]
    fn workspace_folder_accessors() {
        let folder_id = WorkspaceFolderId::new("folder-1-1");
        let folder = WorkspaceFolder::new(folder_id.clone(), "/tmp/workspace");
        assert_eq!(folder.id(), &folder_id);
        assert_eq!(folder.path(), "/tmp/workspace");
    }

    #[test]
    fn project_folders_mutation() {
        let id = ProjectId::new("proj-1");
        let metadata = ProjectMetadata::new(
            "Test",
            "Desc",
            Timestamp::new("2026-01-01T00:00:00Z"),
            Timestamp::new("2026-01-01T00:00:00Z"),
        );
        let mut project = Project::new(id, metadata);
        assert!(project.folders().is_empty());

        let f1 = WorkspaceFolder::new(WorkspaceFolderId::new("f-1"), "/path/one");
        let f2 = WorkspaceFolder::new(WorkspaceFolderId::new("f-2"), "/path/two");
        project.add_folder(f1.clone());
        project.add_folder(f2.clone());
        assert_eq!(project.folders(), &[f1, f2]);

        assert!(project.remove_folder(&WorkspaceFolderId::new("f-1")));
        assert_eq!(project.folders().len(), 1);
        assert_eq!(project.folders()[0].id().as_str(), "f-2");
        assert!(!project.remove_folder(&WorkspaceFolderId::new("unknown")));
    }

    #[test]
    fn project_serde_backwards_compatibility() {
        // A project JSON without the "folders" key must deserialize with empty folders.
        let json_without_folders = r#"{
            "id": "legacy-project",
            "metadata": {
                "name": "Legacy",
                "description": "Old project",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z"
            }
        }"#;

        let project: Project =
            serde_json::from_str(json_without_folders).expect("deserialize legacy");
        assert_eq!(project.id().as_str(), "legacy-project");
        assert!(project.folders().is_empty());

        // Serializing a project with empty folders omits "folders"
        let serialized = serde_json::to_string(&project).expect("serialize");
        assert!(!serialized.contains("\"folders\""));

        // Serializing a project with folders includes "folders"
        let mut with_folders = project;
        with_folders.add_folder(WorkspaceFolder::new(
            WorkspaceFolderId::new("f-1"),
            "/test/dir",
        ));
        let serialized_with_folders =
            serde_json::to_string(&with_folders).expect("serialize with folders");
        assert!(serialized_with_folders.contains("\"folders\""));

        let roundtrip: Project = serde_json::from_str(&serialized_with_folders).expect("roundtrip");
        assert_eq!(roundtrip.folders().len(), 1);
        assert_eq!(roundtrip.folders()[0].path(), "/test/dir");
    }

    #[test]
    fn project_serde_roundtrip() {
        let id = ProjectId::new("test-project");
        let metadata = ProjectMetadata::new(
            "Test Project",
            "A test project",
            Timestamp::new("2026-01-01T00:00:00Z"),
            Timestamp::new("2026-01-02T00:00:00Z"),
        );

        let project = Project::new(id, metadata);
        let json = serde_json::to_string(&project).expect("serialize");
        let back: Project = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(project, back);
    }

    #[test]
    fn project_metadata_serde_roundtrip() {
        let metadata = ProjectMetadata::new(
            "Test",
            "Description",
            Timestamp::new("2026-01-01T00:00:00Z"),
            Timestamp::new("2026-01-02T00:00:00Z"),
        );

        let json = serde_json::to_string(&metadata).expect("serialize");
        let back: ProjectMetadata = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(metadata, back);
    }
}
