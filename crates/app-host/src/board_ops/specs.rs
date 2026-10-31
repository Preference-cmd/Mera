//! Specs, schemas and annotations for the six board operations.

use mod_sdk::{OperationAnnotations, OperationSpec};
use serde_json::{Value, json};

/// Smallest item size a mutator accepts, matching the canvas UI.
pub(super) const MIN_ITEM_WIDTH: u64 = 120;
pub(super) const MIN_ITEM_HEIGHT: u64 = 80;
/// Size given to a note that does not name one.
pub(super) const DEFAULT_NOTE_WIDTH: u64 = 240;
pub(super) const DEFAULT_NOTE_HEIGHT: u64 = 160;
/// Gap between auto-laid-out notes and between them and the existing items.
pub(super) const AUTO_LAYOUT_GAP: i64 = 40;
/// Largest number of entries one mutator call accepts.
pub(super) const MAX_BATCH_ITEMS: usize = 100;

fn position_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "x": { "type": "integer" },
            "y": { "type": "integer" }
        },
        "required": ["x", "y"],
        "additionalProperties": false
    })
}

fn min_size_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "width": { "type": "integer", "minimum": MIN_ITEM_WIDTH },
            "height": { "type": "integer", "minimum": MIN_ITEM_HEIGHT }
        },
        "required": ["width", "height"],
        "additionalProperties": false
    })
}

fn output_size_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "width": { "type": "integer", "minimum": 0 },
            "height": { "type": "integer", "minimum": 0 }
        },
        "required": ["width", "height"],
        "additionalProperties": false
    })
}

fn base_version_schema() -> Value {
    json!({ "type": "integer", "minimum": 0 })
}

fn inline_text_schema() -> Value {
    json!({ "type": "string", "format": "inline-text" })
}

fn mutation_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "version": { "type": "integer", "minimum": 0 },
            "history_entry_id": { "type": ["string", "null"] },
            "board_item_ids": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["version", "history_entry_id", "board_item_ids"],
        "additionalProperties": false,
        "examples": [{
            "version": 3,
            "history_entry_id": "board-history:batch-board-1",
            "board_item_ids": ["item-note-1"]
        }]
    })
}

pub(super) fn read_spec() -> OperationSpec {
    OperationSpec {
        name: "board.read".to_owned(),
        description: "Read the project board: its version and every item".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
            "examples": [{}]
        }),
        output_schema: json!({
            "type": "object",
            "properties": {
                "version": { "type": "integer", "minimum": 0 },
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "kind": { "type": "object" },
                            "position": position_schema(),
                            "size": output_size_schema(),
                            "z": { "type": "integer" },
                            "locked": { "type": "boolean" }
                        },
                        "required": ["id", "kind", "position", "size", "z", "locked"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["version", "items"],
            "additionalProperties": false,
            "examples": [{
                "version": 0,
                "items": [{
                    "id": "item-note-1",
                    "kind": { "type": "note", "content": "Hello" },
                    "position": { "x": 0, "y": 0 },
                    "size": { "width": 240, "height": 160 },
                    "z": 0,
                    "locked": false
                }]
            }]
        }),
        annotations: OperationAnnotations {
            read_only: true,
            destructive: false,
            idempotent: true,
            deterministic: true,
            open_world: false,
        },
    }
}

pub(super) fn preview_spec() -> OperationSpec {
    OperationSpec {
        name: "board.preview".to_owned(),
        description: "Preview one board mutator without changing the board; a rejected batch \
                      returns its diagnostics"
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "One of board.add_notes, board.update_notes, board.arrange \
                                    or board.remove_items"
                },
                "input": { "type": "object" }
            },
            "required": ["operation", "input"],
            "additionalProperties": false,
            "examples": [{
                "operation": "board.add_notes",
                "input": { "notes": [{ "content": "Hello" }] }
            }]
        }),
        output_schema: json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["Applied", "Rejected", "NoOp"]
                },
                "changes": {
                    "type": "array",
                    "items": { "type": "object" }
                },
                "version": { "type": "integer", "minimum": 0 },
                "diagnostics": {
                    "type": "array",
                    "description": "Why the batch would be rejected; empty otherwise",
                    "items": {
                        "type": "object",
                        "properties": {
                            "code": { "type": "string" },
                            "message": { "type": "string" }
                        },
                        "required": ["code", "message"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["status", "changes", "version", "diagnostics"],
            "additionalProperties": false,
            "examples": [{
                "status": "Applied",
                "changes": [{
                    "type": "item_added",
                    "item_id": "item-note-1",
                    "kind": { "type": "note", "content": "Hello" },
                    "position": { "x": 0, "y": 0 },
                    "size": { "width": 240, "height": 160 },
                    "z": 0
                }],
                "version": 1,
                "diagnostics": []
            }]
        }),
        annotations: OperationAnnotations {
            read_only: true,
            destructive: false,
            idempotent: false,
            deterministic: true,
            open_world: false,
        },
    }
}

pub(super) fn add_notes_spec() -> OperationSpec {
    OperationSpec {
        name: "board.add_notes".to_owned(),
        description: "Add one or more notes to the project board as one undoable batch".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "notes": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_BATCH_ITEMS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": inline_text_schema(),
                            "position": position_schema(),
                            "size": min_size_schema()
                        },
                        "required": ["content"],
                        "additionalProperties": false
                    }
                },
                "base_version": base_version_schema()
            },
            "required": ["notes"],
            "additionalProperties": false,
            "examples": [{
                "notes": [
                    { "content": "First note" },
                    { "content": "Second note", "position": { "x": 0, "y": 0 } }
                ]
            }]
        }),
        output_schema: json!({
            "type": "object",
            "properties": {
                "item_ids": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "board_item_ids": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "version": { "type": "integer", "minimum": 0 },
                "history_entry_id": { "type": ["string", "null"] }
            },
            "required": ["item_ids", "board_item_ids", "version", "history_entry_id"],
            "additionalProperties": false,
            "examples": [{
                "item_ids": ["item-note-1"],
                "board_item_ids": ["item-note-1"],
                "version": 1,
                "history_entry_id": "board-history:batch-board-1"
            }]
        }),
        annotations: OperationAnnotations {
            read_only: false,
            destructive: false,
            idempotent: false,
            deterministic: true,
            open_world: false,
        },
    }
}

pub(super) fn update_notes_spec() -> OperationSpec {
    OperationSpec {
        name: "board.update_notes".to_owned(),
        description: "Replace the text of one or more notes as one undoable batch".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "updates": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_BATCH_ITEMS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "item_id": { "type": "string" },
                            "content": inline_text_schema()
                        },
                        "required": ["item_id", "content"],
                        "additionalProperties": false
                    }
                },
                "base_version": base_version_schema()
            },
            "required": ["updates"],
            "additionalProperties": false,
            "examples": [{
                "updates": [{ "item_id": "item-note-1", "content": "Changed" }]
            }]
        }),
        output_schema: mutation_output_schema(),
        annotations: OperationAnnotations {
            read_only: false,
            destructive: false,
            idempotent: false,
            deterministic: true,
            open_world: false,
        },
    }
}

pub(super) fn arrange_spec() -> OperationSpec {
    OperationSpec {
        name: "board.arrange".to_owned(),
        description: "Move, resize or restack board items as one undoable batch".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_BATCH_ITEMS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "item_id": { "type": "string" },
                            "position": position_schema(),
                            "size": min_size_schema(),
                            "z": {
                                "type": "integer",
                                "minimum": i32::MIN,
                                "maximum": i32::MAX
                            }
                        },
                        "required": ["item_id"],
                        "anyOf": [
                            { "required": ["position"] },
                            { "required": ["size"] },
                            { "required": ["z"] }
                        ],
                        "additionalProperties": false
                    }
                },
                "base_version": base_version_schema()
            },
            "required": ["items"],
            "additionalProperties": false,
            "examples": [{
                "items": [{
                    "item_id": "item-note-1",
                    "position": { "x": 40, "y": 40 },
                    "z": 2
                }]
            }]
        }),
        output_schema: mutation_output_schema(),
        annotations: OperationAnnotations {
            read_only: false,
            destructive: false,
            idempotent: true,
            deterministic: true,
            open_world: false,
        },
    }
}

pub(super) fn remove_items_spec() -> OperationSpec {
    OperationSpec {
        name: "board.remove_items".to_owned(),
        description: "Remove one or more items from the board as one undoable batch".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "item_ids": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_BATCH_ITEMS,
                    "items": { "type": "string" }
                },
                "base_version": base_version_schema()
            },
            "required": ["item_ids"],
            "additionalProperties": false,
            "examples": [{ "item_ids": ["item-note-1"] }]
        }),
        output_schema: mutation_output_schema(),
        annotations: OperationAnnotations {
            read_only: false,
            destructive: true,
            idempotent: false,
            deterministic: true,
            open_world: false,
        },
    }
}

/// The spec of one of the four board mutators, for `board.preview`.
pub(super) fn mutator_spec(operation: &str) -> Option<OperationSpec> {
    match operation {
        "board.add_notes" => Some(add_notes_spec()),
        "board.update_notes" => Some(update_notes_spec()),
        "board.arrange" => Some(arrange_spec()),
        "board.remove_items" => Some(remove_items_spec()),
        _ => None,
    }
}
