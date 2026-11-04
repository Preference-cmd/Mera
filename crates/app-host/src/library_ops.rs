//! Kernel operations for the global asset library:
//! - `library.add`
//! - `library.list`
//! - `library.rename`
//! - `library.tag`
//! - `library.remove`
//! - `library.undo`
//! - `library.redo`

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use mod_sdk::{OperationAnnotations, OperationSpec};
use reimagine_core::diagnostic::Diagnostic;
use reimagine_core::model::AssetId;
use serde_json::{Value, json};

use crate::library_service::{LibraryListFilter, LibraryService, library_caller_error};
use crate::operation_service::{KernelCall, KernelOp, OperationService};

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
                            "items": { "type": "string" },
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
                                    "removed": { "type": "boolean" }
                                },
                                "required": ["id", "name", "tags", "media_type", "size", "hash", "added_at", "removed"],
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
                            "items": { "type": "string" }
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

/// Register all global library operations with an [`OperationService`].
pub fn register_library_ops(op_service: &OperationService, library_service: Arc<LibraryService>) {
    op_service.register_kernel_op(Arc::new(LibraryAddOp::new(Arc::clone(&library_service))));
    op_service.register_kernel_op(Arc::new(LibraryListOp::new(Arc::clone(&library_service))));
    op_service.register_kernel_op(Arc::new(LibraryRenameOp::new(Arc::clone(&library_service))));
    op_service.register_kernel_op(Arc::new(LibraryTagOp::new(Arc::clone(&library_service))));
    op_service.register_kernel_op(Arc::new(LibraryRemoveOp::new(Arc::clone(&library_service))));
    op_service.register_kernel_op(Arc::new(LibraryUndoOp::new(Arc::clone(&library_service))));
    op_service.register_kernel_op(Arc::new(LibraryRedoOp::new(Arc::clone(&library_service))));
}
