//! Kernel operations for the global asset library:
//! - `library.add`
//! - `library.collect`
//! - `library.list`
//! - `library.place`
//! - `library.purge`
//! - `library.rename`
//! - `library.tag`
//! - `library.remove`
//! - `library.undo`
//! - `library.redo`

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use mera_core::board::{BoardItemKind, BoardItemPosition, BoardItemSize};
use mera_core::collection::Slot;
use mera_core::command::CommandStatus;
use mera_core::diagnostic::Diagnostic;
use mera_core::model::{ArtifactId, AssetId, BoardItemId, CollectionId, SlotId, StepId};
use mod_sdk::{OperationAnnotations, OperationSpec};
use serde_json::{Value, json};

use crate::asset_store::AssetStore;
use crate::board_service::BoardService;
use crate::collection_service::CollectionService;
use crate::library_service::{
    CollectItem, LibraryListFilter, LibraryService, library_caller_error, library_internal_error,
};
use crate::operation_service::{
    AssetImportOp, AssetListOp, KernelCall, KernelOp, OperationService,
    resolve_library_items_for_import,
};

/// The built-in `library.add` kernel operation.
pub struct LibraryAddOp {
    spec: OperationSpec,
    library_service: Arc<LibraryService>,
}

impl LibraryAddOp {
    pub fn new(library_service: Arc<LibraryService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "library.add".to_owned(),
                description: "Add files to the global asset library".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "paths": {
                            "type": "array",
                            "items": { "type": "string", "format": "host-path" },
                            "minItems": 1
                        }
                    },
                    "required": ["paths"],
                    "additionalProperties": false,
                    "examples": [{ "paths": ["/path/to/image.png"] }]
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "name": { "type": "string" },
                                    "deduplicated": { "type": "boolean" }
                                },
                                "required": ["id", "name", "deduplicated"],
                                "additionalProperties": false
                            }
                        },
                        "version": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["items", "version"],
                    "additionalProperties": false,
                    "examples": [{
                        "items": [{ "id": "asset-1", "name": "image.png", "deduplicated": false }],
                        "version": 1
                    }]
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: false,
                    idempotent: false,
                    deterministic: true,
                    open_world: false,
                },
            },
            library_service,
        }
    }
}

#[async_trait]
impl KernelOp for LibraryAddOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let paths_val = call
            .caller_input
            .get("paths")
            .and_then(Value::as_array)
            .ok_or_else(|| library_caller_error("INVALID_INPUT", "missing or non-array 'paths'"))?;

        let mut paths = Vec::with_capacity(paths_val.len());
        for p in paths_val {
            let s = p.as_str().ok_or_else(|| {
                library_caller_error("INVALID_INPUT", "path item must be a string")
            })?;
            paths.push(PathBuf::from(s));
        }

        let res = self.library_service.add(paths, &call.context).await?;
        let items: Vec<Value> = res
            .items
            .iter()
            .map(|item| {
                json!({
                    "id": item.id.as_str(),
                    "name": item.name,
                    "deduplicated": item.deduplicated,
                })
            })
            .collect();

        Ok(json!({
            "items": items,
            "version": res.version,
        }))
    }
}

/// The built-in `library.list` kernel operation.
pub struct LibraryListOp {
    spec: OperationSpec,
    library_service: Arc<LibraryService>,
}

impl LibraryListOp {
    pub fn new(library_service: Arc<LibraryService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "library.list".to_owned(),
                description: "List items in the global asset library".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name_contains": { "type": "string" },
                        "tag": { "type": "string" },
                        "media_type": { "type": "string" },
                        "include_removed": { "type": "boolean" }
                    },
                    "additionalProperties": false,
                    "examples": [{}]
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "name": { "type": "string" },
                                    "tags": {
                                        "type": "array",
                                        "items": { "type": "string" }
                                    },
                                    "media_type": { "type": "string" },
                                    "size": { "type": "integer", "minimum": 0 },
                                    "hash": { "type": "string" },
                                    "added_at": { "type": "string" },
                                    "removed": { "type": "boolean" },
                                    "source": { "type": "object" }
                                },
                                "required": ["id", "name", "tags", "media_type", "size", "hash", "added_at", "removed", "source"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["items"],
                    "additionalProperties": false,
                    "examples": [{ "items": [] }]
                }),
                annotations: OperationAnnotations {
                    read_only: true,
                    destructive: false,
                    idempotent: true,
                    deterministic: true,
                    open_world: false,
                },
            },
            library_service,
        }
    }
}

#[async_trait]
impl KernelOp for LibraryListOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let name_contains = call
            .caller_input
            .get("name_contains")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let tag = call
            .caller_input
            .get("tag")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let media_type = call
            .caller_input
            .get("media_type")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let include_removed = call
            .caller_input
            .get("include_removed")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let filter = LibraryListFilter {
            name_contains,
            tag,
            media_type,
            include_removed,
        };

        let items = self.library_service.list(&filter).await?;
        let items_json: Vec<Value> = items
            .into_iter()
            .map(|item| {
                json!({
                    "id": item.id.as_str(),
                    "name": item.name,
                    "tags": item.tags,
                    "media_type": item.media_type,
                    "size": item.size,
                    "hash": item.hash,
                    "added_at": item.added_at,
                    "removed": item.removed,
                    "source": item.source,
                })
            })
            .collect();

        Ok(json!({ "items": items_json }))
    }
}

/// The built-in `library.rename` kernel operation.
pub struct LibraryRenameOp {
    spec: OperationSpec,
    library_service: Arc<LibraryService>,
}

impl LibraryRenameOp {
    pub fn new(library_service: Arc<LibraryService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "library.rename".to_owned(),
                description: "Rename an item in the global asset library".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "name": { "type": "string" }
                    },
                    "required": ["id", "name"],
                    "additionalProperties": false,
                    "examples": [{ "id": "asset-1", "name": "new_name.png" }]
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "version": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["version"],
                    "additionalProperties": false,
                    "examples": [{ "version": 1 }]
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: false,
                    idempotent: false,
                    deterministic: true,
                    open_world: false,
                },
            },
            library_service,
        }
    }
}

#[async_trait]
impl KernelOp for LibraryRenameOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let id_str = call
            .caller_input
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| library_caller_error("INVALID_INPUT", "missing 'id'"))?;
        let name_str = call
            .caller_input
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| library_caller_error("INVALID_INPUT", "missing 'name'"))?;

        let id =
            AssetId::try_new(id_str).map_err(|e| library_caller_error("INVALID_ASSET_ID", e))?;

        let version = self
            .library_service
            .rename(id, name_str.to_owned(), &call.context)
            .await?;

        Ok(json!({ "version": version }))
    }
}

/// The built-in `library.tag` kernel operation.
pub struct LibraryTagOp {
    spec: OperationSpec,
    library_service: Arc<LibraryService>,
}

impl LibraryTagOp {
    pub fn new(library_service: Arc<LibraryService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "library.tag".to_owned(),
                description: "Set tags on an item in the global asset library".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    },
                    "required": ["id", "tags"],
                    "additionalProperties": false,
                    "examples": [{ "id": "asset-1", "tags": ["tag1", "tag2"] }]
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "version": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["version"],
                    "additionalProperties": false,
                    "examples": [{ "version": 1 }]
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: false,
                    idempotent: false,
                    deterministic: true,
                    open_world: false,
                },
            },
            library_service,
        }
    }
}

#[async_trait]
impl KernelOp for LibraryTagOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let id_str = call
            .caller_input
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| library_caller_error("INVALID_INPUT", "missing 'id'"))?;
        let tags_arr = call
            .caller_input
            .get("tags")
            .and_then(Value::as_array)
            .ok_or_else(|| library_caller_error("INVALID_INPUT", "missing or non-array 'tags'"))?;

        let id =
            AssetId::try_new(id_str).map_err(|e| library_caller_error("INVALID_ASSET_ID", e))?;

        let mut tags = Vec::with_capacity(tags_arr.len());
        for t in tags_arr {
            let s = t.as_str().ok_or_else(|| {
                library_caller_error("INVALID_INPUT", "tag item must be a string")
            })?;
            tags.push(s.to_owned());
        }

        let version = self
            .library_service
            .set_tags(id, tags, &call.context)
            .await?;

        Ok(json!({ "version": version }))
    }
}

/// The built-in `library.remove` kernel operation.
pub struct LibraryRemoveOp {
    spec: OperationSpec,
    library_service: Arc<LibraryService>,
}

impl LibraryRemoveOp {
    pub fn new(library_service: Arc<LibraryService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "library.remove".to_owned(),
                description: "Mark items as removed from the global asset library".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1
                        }
                    },
                    "required": ["ids"],
                    "additionalProperties": false,
                    "examples": [{ "ids": ["asset-1"] }]
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "version": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["version"],
                    "additionalProperties": false,
                    "examples": [{ "version": 1 }]
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: true,
                    idempotent: false,
                    deterministic: true,
                    open_world: false,
                },
            },
            library_service,
        }
    }
}

#[async_trait]
impl KernelOp for LibraryRemoveOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let ids_arr = call
            .caller_input
            .get("ids")
            .and_then(Value::as_array)
            .ok_or_else(|| library_caller_error("INVALID_INPUT", "missing or non-array 'ids'"))?;
        if ids_arr.is_empty() {
            return Err(library_caller_error("EMPTY_IDS", "ids must not be empty"));
        }

        let mut ids = Vec::with_capacity(ids_arr.len());
        for id_val in ids_arr {
            let s = id_val
                .as_str()
                .ok_or_else(|| library_caller_error("INVALID_INPUT", "id item must be a string"))?;
            let asset_id =
                AssetId::try_new(s).map_err(|e| library_caller_error("INVALID_ASSET_ID", e))?;
            ids.push(asset_id);
        }

        let version = self.library_service.remove(ids, &call.context).await?;

        Ok(json!({ "version": version }))
    }
}

/// The built-in `library.undo` kernel operation.
pub struct LibraryUndoOp {
    spec: OperationSpec,
    library_service: Arc<LibraryService>,
}

impl LibraryUndoOp {
    pub fn new(library_service: Arc<LibraryService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "library.undo".to_owned(),
                description: "Undo the last global library modification".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                    "examples": [{}]
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "applied": { "type": "boolean" },
                        "version": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["applied", "version"],
                    "additionalProperties": false,
                    "examples": [{ "applied": true, "version": 1 }]
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: false,
                    idempotent: false,
                    deterministic: true,
                    open_world: false,
                },
            },
            library_service,
        }
    }
}

#[async_trait]
impl KernelOp for LibraryUndoOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    async fn invoke(&self, _call: KernelCall) -> Result<Value, Diagnostic> {
        let snapshot = self
            .library_service
            .snapshot()
            .await
            .map_err(|e| library_caller_error("SNAPSHOT_FAILED", e.to_string()))?;

        let maybe_res = self.library_service.undo().await?;

        let (applied, version) = match maybe_res {
            Some(v) => (true, v),
            None => (false, snapshot.version().get()),
        };

        Ok(json!({ "applied": applied, "version": version }))
    }
}

/// The built-in `library.redo` kernel operation.
pub struct LibraryRedoOp {
    spec: OperationSpec,
    library_service: Arc<LibraryService>,
}

impl LibraryRedoOp {
    pub fn new(library_service: Arc<LibraryService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "library.redo".to_owned(),
                description: "Redo the last undone global library modification".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                    "examples": [{}]
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "applied": { "type": "boolean" },
                        "version": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["applied", "version"],
                    "additionalProperties": false,
                    "examples": [{ "applied": true, "version": 1 }]
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: false,
                    idempotent: false,
                    deterministic: true,
                    open_world: false,
                },
            },
            library_service,
        }
    }
}

#[async_trait]
impl KernelOp for LibraryRedoOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    async fn invoke(&self, _call: KernelCall) -> Result<Value, Diagnostic> {
        let snapshot = self
            .library_service
            .snapshot()
            .await
            .map_err(|e| library_caller_error("SNAPSHOT_FAILED", e.to_string()))?;

        let maybe_res = self.library_service.redo().await?;

        let (applied, version) = match maybe_res {
            Some(v) => (true, v),
            None => (false, snapshot.version().get()),
        };

        Ok(json!({ "applied": applied, "version": version }))
    }
}

/// The built-in `library.collect` kernel operation.
pub struct LibraryCollectOp {
    spec: OperationSpec,
    library_service: Arc<LibraryService>,
}

impl LibraryCollectOp {
    pub fn new(library_service: Arc<LibraryService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "library.collect".to_owned(),
                description: "Collect project assets into the global asset library".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "asset": {
                                        "type": "string",
                                        "format": "asset"
                                    },
                                    "name": {
                                        "type": "string"
                                    }
                                },
                                "required": ["asset"],
                                "additionalProperties": false
                            },
                            "minItems": 1
                        }
                    },
                    "required": ["items"],
                    "additionalProperties": false
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "name": { "type": "string" },
                                    "deduplicated": { "type": "boolean" }
                                },
                                "required": ["id", "name", "deduplicated"],
                                "additionalProperties": false
                            }
                        },
                        "version": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["items", "version"],
                    "additionalProperties": false
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: false,
                    idempotent: false,
                    deterministic: false,
                    open_world: false,
                },
            },
            library_service,
        }
    }
}

#[async_trait]
impl KernelOp for LibraryCollectOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let caller_items = call
            .caller_input
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| library_caller_error("INVALID_INPUT", "missing 'items' array"))?;

        let inv_items = call
            .invocation
            .input
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                library_caller_error("INVALID_INPUT", "missing 'items' array in invocation")
            })?;

        if caller_items.is_empty() {
            return Err(library_caller_error(
                "EMPTY_ITEMS",
                "items must not be empty",
            ));
        }

        let project_store = crate::asset_store::AssetStore::new(
            self.library_service
                .paths()
                .project_dir(call.project_id.as_str()),
        );

        let mut collect_items = Vec::with_capacity(caller_items.len());
        for (caller_item, inv_item) in caller_items.iter().zip(inv_items.iter()) {
            let asset_id_str = caller_item
                .get("asset")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    library_caller_error("INVALID_INPUT", "asset id must be a string")
                })?;
            let path_str = inv_item
                .get("asset")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    library_caller_error("INVALID_INPUT", "resolved asset path must be a string")
                })?;
            let name = caller_item
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned);

            let asset_id = AssetId::new(asset_id_str);
            let media_type = project_store
                .get_asset(&asset_id)
                .await?
                .map(|r| r.media_type);

            collect_items.push(CollectItem {
                asset_id,
                path: PathBuf::from(path_str),
                name,
                media_type,
            });
        }

        let res = self
            .library_service
            .collect(call.project_id, collect_items, &call.context)
            .await?;

        let items: Vec<Value> = res
            .items
            .iter()
            .map(|item| {
                json!({
                    "id": item.id.as_str(),
                    "name": item.name,
                    "deduplicated": item.deduplicated,
                })
            })
            .collect();

        Ok(json!({
            "items": items,
            "version": res.version,
        }))
    }
}

/// The built-in `library.purge` kernel operation.
pub struct LibraryPurgeOp {
    spec: OperationSpec,
    library_service: Arc<LibraryService>,
}

impl LibraryPurgeOp {
    pub fn new(library_service: Arc<LibraryService>) -> Self {
        Self {
            spec: OperationSpec {
                name: "library.purge".to_owned(),
                description: "Permanently purge removed entries and unreferenced files from the global asset library".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "purged_entries": { "type": "integer", "minimum": 0 },
                        "deleted_files": { "type": "integer", "minimum": 0 },
                        "freed_bytes": { "type": "integer", "minimum": 0 },
                        "version": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["purged_entries", "deleted_files", "freed_bytes", "version"],
                    "additionalProperties": false
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: true,
                    idempotent: true,
                    deterministic: false,
                    open_world: false,
                },
            },
            library_service,
        }
    }
}

#[async_trait]
impl KernelOp for LibraryPurgeOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let res = self.library_service.purge(&call.context).await?;
        Ok(json!({
            "purged_entries": res.purged_entries,
            "deleted_files": res.deleted_files,
            "freed_bytes": res.freed_bytes,
            "version": res.version,
        }))
    }
}

/// The built-in `library.place` kernel operation.
///
/// Imports one or more global library entries into the project and places a
/// single card on the project board centered at `position` (`asset_ref` for
/// one file; `collection` for multiple files), returning the board
/// `history_entry_id` so one undo reverses the placement.
pub struct LibraryPlaceOp {
    spec: OperationSpec,
    library_service: Arc<LibraryService>,
    collection_service: Arc<CollectionService>,
    board_service: Arc<BoardService>,
}

impl LibraryPlaceOp {
    pub fn new(
        library_service: Arc<LibraryService>,
        collection_service: Arc<CollectionService>,
        board_service: Arc<BoardService>,
    ) -> Self {
        Self {
            spec: OperationSpec {
                name: "library.place".to_owned(),
                description:
                    "Import library items into the project and place one card on the board"
                        .to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "library_items": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1
                        },
                        "position": {
                            "type": "object",
                            "properties": {
                                "x": { "type": "number" },
                                "y": { "type": "number" }
                            },
                            "required": ["x", "y"],
                            "additionalProperties": false
                        },
                        "name": { "type": "string" }
                    },
                    "required": ["library_items", "position"],
                    "additionalProperties": false,
                    "examples": [{
                        "library_items": ["lib-item-1"],
                        "position": { "x": 240, "y": 160 }
                    }]
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "assets": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "collection_id": { "type": ["string", "null"] },
                        "board_item_id": { "type": "string" },
                        "history_entry_id": { "type": ["string", "null"] },
                        "library_sources": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "item": { "type": "string" },
                                    "hash": { "type": "string" },
                                    "name": { "type": "string" }
                                },
                                "required": ["item", "hash", "name"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": [
                        "assets",
                        "collection_id",
                        "board_item_id",
                        "history_entry_id",
                        "library_sources"
                    ],
                    "additionalProperties": false
                }),
                annotations: OperationAnnotations {
                    read_only: false,
                    destructive: false,
                    idempotent: false,
                    deterministic: false,
                    open_world: false,
                },
            },
            library_service,
            collection_service,
            board_service,
        }
    }
}

#[async_trait]
impl KernelOp for LibraryPlaceOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    fn changes_documents(&self) -> bool {
        true
    }

    fn activity_summary(
        &self,
        _caller_input: &Value,
        output: &Value,
    ) -> Option<(String, String, Value)> {
        let sources = output.get("library_sources").and_then(Value::as_array)?;
        let count = sources.len();
        if count == 0 {
            return None;
        }
        let first_name = sources
            .first()
            .and_then(|src| src.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("file");
        let summary = if count == 1 {
            format!("Imported {first_name} from library")
        } else {
            format!("Imported {count} files from library")
        };
        Some((
            "activity.library.place".to_owned(),
            summary,
            json!({ "count": count, "name": first_name }),
        ))
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let items_val = call
            .caller_input
            .get("library_items")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                library_caller_error("INVALID_INPUT", "missing 'library_items' array")
            })?;

        let pos_obj = call
            .caller_input
            .get("position")
            .and_then(Value::as_object)
            .ok_or_else(|| library_caller_error("INVALID_INPUT", "missing 'position' object"))?;
        let center_x = pos_obj
            .get("x")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .ok_or_else(|| library_caller_error("INVALID_INPUT", "invalid position.x"))?;
        let center_y = pos_obj
            .get("y")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .ok_or_else(|| library_caller_error("INVALID_INPUT", "invalid position.y"))?;

        let custom_name = call
            .caller_input
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let resolved =
            resolve_library_items_for_import(&self.library_service, items_val, "library.place")
                .await?;

        let step_id_str = call
            .invocation
            .output_dir
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("step-library-place");
        let step_id = StepId::new(step_id_str);
        let project_dir = self
            .library_service
            .paths()
            .project_dir(call.project_id.as_str());
        let asset_store = AssetStore::new(project_dir);

        let mut ingested_ids = Vec::with_capacity(resolved.len());
        let mut library_sources = Vec::with_capacity(resolved.len());
        for item in &resolved {
            let record = asset_store
                .ingest_file(step_id.clone(), &item.content_path, Some(&item.media_type))
                .await?;
            ingested_ids.push(record.id);
            library_sources.push(json!({
                "item": item.asset_id.as_str(),
                "hash": item.hash,
                "name": item.name,
            }));
        }

        static PLACE_COUNTER: AtomicU64 = AtomicU64::new(1);
        let count = PLACE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = jiff::Timestamp::now().as_nanosecond();

        let (collection_id, kind, size) = if ingested_ids.len() == 1 {
            let kind = BoardItemKind::AssetRef {
                artifact_id: ArtifactId::new(ingested_ids[0].as_str()),
            };
            (None, kind, BoardItemSize::new(240, 160))
        } else {
            let col_id = CollectionId::new(format!("col-{nanos:x}-{count:x}"));
            let default_name = format!("{} (+{})", resolved[0].name, resolved.len() - 1);
            let col_name = custom_name.unwrap_or(&default_name);
            let slots: Vec<Slot> = ingested_ids
                .iter()
                .enumerate()
                .map(|(i, aid)| Slot::new(SlotId::new(format!("slot-{}", i + 1)), aid.clone()))
                .collect();
            let doc = self
                .collection_service
                .create(&call.project_id, &col_id, col_name, slots)
                .await
                .map_err(|e| library_internal_error("COLLECTION_CREATE_FAILED", e.to_string()))?;
            self.collection_service.emit_changed(&doc);
            (
                Some(col_id.clone()),
                BoardItemKind::Collection {
                    collection_id: col_id,
                },
                BoardItemSize::new(300, 200),
            )
        };

        let board_item_id = BoardItemId::new(format!("item-lib-{nanos:x}-{count:x}"));
        let pos = BoardItemPosition::new(
            (center_x - (size.width as f64) / 2.0).round() as i64,
            (center_y - (size.height as f64) / 2.0).round() as i64,
        );

        let board_res = self
            .board_service
            .append_item(
                &call.project_id,
                &call.context,
                board_item_id.clone(),
                kind,
                pos,
                size,
            )
            .await;

        let history_entry_id = match board_res {
            Ok(result) if result.status() == CommandStatus::Applied => {
                result.history_entry_id().map(ToString::to_string)
            }
            Ok(_rejected) => {
                if let Some(col_id) = &collection_id {
                    let _ = self
                        .collection_service
                        .remove(&call.project_id, col_id)
                        .await;
                }
                return Err(library_internal_error(
                    "BOARD_REJECTED",
                    "adding placed item to board was rejected",
                ));
            }
            Err(e) => {
                if let Some(col_id) = &collection_id {
                    let _ = self
                        .collection_service
                        .remove(&call.project_id, col_id)
                        .await;
                }
                return Err(library_internal_error("BOARD_APPLY_FAILED", e.to_string()));
            }
        };

        let assets_out: Vec<Value> = ingested_ids
            .iter()
            .map(|id| Value::String(id.as_str().to_owned()))
            .collect();

        Ok(json!({
            "assets": assets_out,
            "collection_id": collection_id.as_ref().map(CollectionId::as_str),
            "board_item_id": board_item_id.as_str(),
            "history_entry_id": history_entry_id,
            "library_sources": library_sources,
        }))
    }
}

/// Register all global library operations with an [`OperationService`].
pub fn register_library_ops(op_service: &OperationService, library_service: Arc<LibraryService>) {
    op_service.register_kernel_op(Arc::new(AssetImportOp::with_library(Arc::clone(
        &library_service,
    ))));
    op_service.register_kernel_op(Arc::new(AssetListOp::with_library(
        library_service.paths().clone(),
        Arc::clone(&library_service),
    )));
    op_service.register_kernel_op(Arc::new(LibraryAddOp::new(Arc::clone(&library_service))));
    op_service.register_kernel_op(Arc::new(LibraryCollectOp::new(Arc::clone(
        &library_service,
    ))));
    op_service.register_kernel_op(Arc::new(LibraryListOp::new(Arc::clone(&library_service))));
    op_service.register_kernel_op(Arc::new(LibraryPurgeOp::new(Arc::clone(&library_service))));
    op_service.register_kernel_op(Arc::new(LibraryRenameOp::new(Arc::clone(&library_service))));
    op_service.register_kernel_op(Arc::new(LibraryTagOp::new(Arc::clone(&library_service))));
    op_service.register_kernel_op(Arc::new(LibraryRemoveOp::new(Arc::clone(&library_service))));
    op_service.register_kernel_op(Arc::new(LibraryUndoOp::new(Arc::clone(&library_service))));
    op_service.register_kernel_op(Arc::new(LibraryRedoOp::new(Arc::clone(&library_service))));
}

/// Register the `library.place` operation requiring library, collection, and board services.
pub fn register_library_place_op(
    op_service: &OperationService,
    library_service: Arc<LibraryService>,
    collection_service: Arc<CollectionService>,
    board_service: Arc<BoardService>,
) {
    op_service.register_kernel_op(Arc::new(LibraryPlaceOp::new(
        library_service,
        collection_service,
        board_service,
    )));
}
