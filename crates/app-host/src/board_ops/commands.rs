//! The shared input-to-commands builder for the four board mutators and
//! `board.preview`, plus the automatic note layout and the generated ids.

#![allow(clippy::result_large_err)]

use mod_sdk::OperationSpec;
use reimagine_core::board::{
    BoardChange, BoardCommand, BoardDocument, BoardItemKind, BoardItemPosition, BoardItemSize,
};
use reimagine_core::diagnostic::Diagnostic;
use reimagine_core::model::{BoardItemId, DocumentVersion};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::operation_service::MAX_INLINE_TEXT_BYTES;

use super::board_caller_error;
use super::board_internal_error;
use super::specs::{AUTO_LAYOUT_GAP, DEFAULT_NOTE_HEIGHT, DEFAULT_NOTE_WIDTH, MAX_BATCH_ITEMS};

/// Validate a mutator input that arrived inside `board.preview`, where the
/// outer schema cannot see the target operation's fields. The inline-text cap
/// is applied here as well because the outer preview schema carries no
/// inline-text markers.
pub(super) fn validate_mutator_input(
    op_name: &str,
    spec: &OperationSpec,
    input: &Value,
) -> Result<(), Diagnostic> {
    let validator = jsonschema::validator_for(&spec.input_schema).map_err(|error| {
        board_internal_error(
            op_name,
            "INVALID_SCHEMA",
            format!("invalid input schema for `{}`: {error}", spec.name),
        )
    })?;
    if let Some(violations) = mod_sdk::schema::violations(&validator, input) {
        return Err(board_caller_error(
            op_name,
            "INVALID_INPUT",
            format!("input validation failed for `{}`: {violations}", spec.name),
        ));
    }

    let inline_bytes = mod_sdk::schema::find_inline_text_fields(&spec.input_schema, input)
        .into_iter()
        .fold(0usize, |total, field| total.saturating_add(field.bytes));
    if inline_bytes > MAX_INLINE_TEXT_BYTES {
        return Err(board_caller_error(
            op_name,
            "INLINE_TEXT_TOO_LARGE",
            format!(
                "inline text input is {inline_bytes} bytes; the limit is \
                 {MAX_INLINE_TEXT_BYTES} bytes. Pass the text as an asset instead"
            ),
        ));
    }
    Ok(())
}

pub(super) fn caller_base_version(
    input: &Value,
    op_name: &str,
) -> Result<Option<DocumentVersion>, Diagnostic> {
    let Some(value) = input.get("base_version") else {
        return Ok(None);
    };
    value
        .as_u64()
        .map(|version| Some(DocumentVersion::new(version)))
        .ok_or_else(|| {
            board_caller_error(
                op_name,
                "INVALID_INPUT",
                "'base_version' must be a non-negative integer",
            )
        })
}

/// Deduplicated ids of the items the applied changes touched, in first-touch
/// order.
pub(super) fn touched_item_ids(changes: &[BoardChange]) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    for change in changes {
        let item_id = change.item_id().to_string();
        if !targets.contains(&item_id) {
            targets.push(item_id);
        }
    }
    targets
}

/// Commands built from one mutator input, plus the ids of any notes the
/// builder created.
pub(super) struct PreparedCommands {
    pub(super) commands: Vec<BoardCommand>,
    /// Generated note ids in input order; empty for every other mutator.
    pub(super) item_ids: Vec<String>,
}

pub(super) fn build_commands(
    operation: &str,
    input: &Value,
    base_version: DocumentVersion,
    board: &BoardDocument,
    op_name: &str,
) -> Result<PreparedCommands, Diagnostic> {
    match operation {
        "board.add_notes" => build_add_notes(input, base_version, board, op_name),
        "board.update_notes" => build_update_notes(input, op_name),
        "board.arrange" => build_arrange(input, op_name),
        "board.remove_items" => build_remove_items(input, op_name),
        other => Err(board_caller_error(
            op_name,
            "INVALID_INPUT",
            format!("`{other}` is not a board mutator"),
        )),
    }
}

fn batch_array<'a>(
    input: &'a Value,
    key: &str,
    op_name: &str,
) -> Result<&'a Vec<Value>, Diagnostic> {
    let array = input.get(key).and_then(Value::as_array).ok_or_else(|| {
        board_caller_error(op_name, "INVALID_INPUT", format!("missing '{key}' array"))
    })?;
    if array.is_empty() || array.len() > MAX_BATCH_ITEMS {
        return Err(board_caller_error(
            op_name,
            "INVALID_INPUT",
            format!("'{key}' must hold between 1 and {MAX_BATCH_ITEMS} items"),
        ));
    }
    Ok(array)
}

fn item_id_field(value: Option<&Value>, op_name: &str) -> Result<BoardItemId, Diagnostic> {
    let raw = value
        .and_then(Value::as_str)
        .ok_or_else(|| board_caller_error(op_name, "INVALID_INPUT", "missing 'item_id' string"))?;
    BoardItemId::try_new(raw).map_err(|reason| {
        board_caller_error(
            op_name,
            "INVALID_INPUT",
            format!("invalid board item id `{raw}`: {reason}"),
        )
    })
}

fn content_field(value: &Value, op_name: &str) -> Result<String, Diagnostic> {
    value
        .get("content")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| board_caller_error(op_name, "INVALID_INPUT", "missing 'content' string"))
}

fn optional_position(
    value: Option<&Value>,
    op_name: &str,
) -> Result<Option<BoardItemPosition>, Diagnostic> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let x = value.get("x").and_then(Value::as_i64).ok_or_else(|| {
        board_caller_error(op_name, "INVALID_INPUT", "'position.x' must be an integer")
    })?;
    let y = value.get("y").and_then(Value::as_i64).ok_or_else(|| {
        board_caller_error(op_name, "INVALID_INPUT", "'position.y' must be an integer")
    })?;
    Ok(Some(BoardItemPosition::new(x, y)))
}

fn optional_size(
    value: Option<&Value>,
    op_name: &str,
) -> Result<Option<BoardItemSize>, Diagnostic> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let width = value.get("width").and_then(Value::as_u64).ok_or_else(|| {
        board_caller_error(op_name, "INVALID_INPUT", "'size.width' must be an integer")
    })?;
    let height = value.get("height").and_then(Value::as_u64).ok_or_else(|| {
        board_caller_error(op_name, "INVALID_INPUT", "'size.height' must be an integer")
    })?;
    Ok(Some(BoardItemSize::new(width, height)))
}

fn build_add_notes(
    input: &Value,
    base_version: DocumentVersion,
    board: &BoardDocument,
    op_name: &str,
) -> Result<PreparedCommands, Diagnostic> {
    let notes = batch_array(input, "notes", op_name)?;
    let mut parsed = Vec::with_capacity(notes.len());
    for note in notes {
        let content = content_field(note, op_name)?;
        let position = optional_position(note.get("position"), op_name)?;
        let size = optional_size(note.get("size"), op_name)?
            .unwrap_or(BoardItemSize::new(DEFAULT_NOTE_WIDTH, DEFAULT_NOTE_HEIGHT));
        parsed.push((content, position, size));
    }

    // Automatic notes go into one column to the right of the bounding box of
    // the existing items and the explicitly positioned notes in this batch,
    // so the column cannot land on top of either. An empty board starts at
    // the origin.
    let mut bounds: Option<LayoutRect> = None;
    for item in board.items().values() {
        bounds = Some(union_rect(
            bounds,
            LayoutRect::of(item.position(), item.size()),
        ));
    }
    for (_, position, size) in &parsed {
        if let Some(position) = position {
            bounds = Some(union_rect(bounds, LayoutRect::of(position, size)));
        }
    }
    let (column_x, mut cursor_y) = match bounds {
        Some(bounds) => (bounds.right.saturating_add(AUTO_LAYOUT_GAP), bounds.top),
        None => (0, 0),
    };

    let mut commands = Vec::with_capacity(parsed.len());
    let mut item_ids = Vec::with_capacity(parsed.len());
    for (index, (content, position, size)) in parsed.into_iter().enumerate() {
        let item_id = generated_note_id("board.add_notes", base_version, input, index);
        let position = match position {
            Some(position) => position,
            None => {
                let position = BoardItemPosition::new(column_x, cursor_y);
                cursor_y = cursor_y
                    .saturating_add(dimension(size.height))
                    .saturating_add(AUTO_LAYOUT_GAP);
                position
            }
        };
        item_ids.push(item_id.to_string());
        commands.push(BoardCommand::AddItem {
            item_id,
            kind: BoardItemKind::Note { content },
            position,
            size,
        });
    }

    Ok(PreparedCommands { commands, item_ids })
}

fn build_update_notes(input: &Value, op_name: &str) -> Result<PreparedCommands, Diagnostic> {
    let updates = batch_array(input, "updates", op_name)?;
    let mut commands = Vec::with_capacity(updates.len());
    for update in updates {
        let item_id = item_id_field(update.get("item_id"), op_name)?;
        let content = content_field(update, op_name)?;
        commands.push(BoardCommand::UpdateNote { item_id, content });
    }
    Ok(PreparedCommands {
        commands,
        item_ids: Vec::new(),
    })
}

fn build_arrange(input: &Value, op_name: &str) -> Result<PreparedCommands, Diagnostic> {
    let items = batch_array(input, "items", op_name)?;
    let mut commands = Vec::with_capacity(items.len());
    for item in items {
        let item_id = item_id_field(item.get("item_id"), op_name)?;
        let position = optional_position(item.get("position"), op_name)?;
        let size = optional_size(item.get("size"), op_name)?;
        let z = match item.get("z") {
            None => None,
            Some(value) if value.is_null() => None,
            Some(value) => {
                let raw = value.as_i64().ok_or_else(|| {
                    board_caller_error(op_name, "INVALID_INPUT", "'z' must be an integer")
                })?;
                Some(i32::try_from(raw).map_err(|_| {
                    board_caller_error(op_name, "INVALID_INPUT", "'z' must fit in a 32-bit integer")
                })?)
            }
        };

        if position.is_none() && size.is_none() && z.is_none() {
            return Err(board_caller_error(
                op_name,
                "INVALID_INPUT",
                "each arranged item must set at least one of position, size or z",
            ));
        }

        if let Some(position) = position {
            commands.push(BoardCommand::MoveItem {
                item_id: item_id.clone(),
                position,
            });
        }
        if let Some(size) = size {
            commands.push(BoardCommand::ResizeItem {
                item_id: item_id.clone(),
                size,
            });
        }
        if let Some(z) = z {
            commands.push(BoardCommand::SetZ { item_id, z });
        }
    }
    Ok(PreparedCommands {
        commands,
        item_ids: Vec::new(),
    })
}

fn build_remove_items(input: &Value, op_name: &str) -> Result<PreparedCommands, Diagnostic> {
    let item_ids = batch_array(input, "item_ids", op_name)?;
    let mut commands = Vec::with_capacity(item_ids.len());
    for value in item_ids {
        let item_id = item_id_field(Some(value), op_name)?;
        commands.push(BoardCommand::RemoveItem { item_id });
    }
    Ok(PreparedCommands {
        commands,
        item_ids: Vec::new(),
    })
}

/// The id of a generated note: a SHA-256 seed over the operation name, the
/// base version, the canonical input payload and the item index. Preview and
/// execution of the same input therefore name the same items.
fn generated_note_id(
    operation: &str,
    base_version: DocumentVersion,
    input: &Value,
    index: usize,
) -> BoardItemId {
    // The base version is already a seed component, so drop it from the
    // payload: an explicit `base_version` and the implicit current version
    // then produce the same ids for the same notes.
    let mut payload = input.clone();
    if let Value::Object(map) = &mut payload {
        map.remove("base_version");
    }
    let seed = format!(
        "{operation}\n{}\n{}\n{index}",
        base_version.get(),
        canonical_json(&payload)
    );
    let digest = Sha256::digest(seed.as_bytes());
    BoardItemId::new(format!("item-note-{}", hex::encode(&digest[..16])))
}

/// Serialize a JSON value with object keys sorted at every level, so the id
/// seed does not depend on the caller's key order.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            let mut out = String::from("{");
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*key).clone()).to_string());
                out.push(':');
                if let Some(value) = map.get(key.as_str()) {
                    out.push_str(&canonical_json(value));
                }
            }
            out.push('}');
            out
        }
        Value::Array(items) => {
            let entries: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", entries.join(","))
        }
        scalar => scalar.to_string(),
    }
}

/// Axis-aligned rectangle used for the automatic note layout.
#[derive(Debug, Clone, Copy)]
struct LayoutRect {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

impl LayoutRect {
    fn of(position: &BoardItemPosition, size: &BoardItemSize) -> Self {
        Self {
            left: position.x,
            top: position.y,
            right: position.x.saturating_add(dimension(size.width)),
            bottom: position.y.saturating_add(dimension(size.height)),
        }
    }
}

fn union_rect(bounds: Option<LayoutRect>, rect: LayoutRect) -> LayoutRect {
    match bounds {
        None => rect,
        Some(bounds) => LayoutRect {
            left: bounds.left.min(rect.left),
            top: bounds.top.min(rect.top),
            right: bounds.right.max(rect.right),
            bottom: bounds.bottom.max(rect.bottom),
        },
    }
}

/// Convert a `u64` size to the `i64` canvas space, saturating instead of
/// wrapping at the extreme end of the input range.
fn dimension(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
