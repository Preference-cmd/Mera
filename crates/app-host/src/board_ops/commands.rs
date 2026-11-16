//! The shared input-to-commands builder for the four board mutators and
//! `board.preview`, plus the automatic note layout and the generated ids.
//!
//! Parsing and materialization are separate steps. [`parse`] validates and
//! canonicalizes a caller input once, outside the session lock; the plan it
//! returns then materializes commands under the lock from the document and
//! base version the batch will actually use. Only `add_notes` needs that
//! state, because it lays notes out around the current items and seeds their
//! ids from the base version.

#![allow(clippy::result_large_err)]

use mera_core::board::{
    BoardCommand, BoardDocument, BoardItemKind, BoardItemPosition, BoardItemSize,
};
use mera_core::diagnostic::Diagnostic;
use mera_core::model::{BoardItemId, DocumentVersion};
use mod_sdk::OperationSpec;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::operation_service::MAX_INLINE_TEXT_BYTES;

use super::board_caller_error;
use super::board_internal_error;
use super::specs::{
    AUTO_LAYOUT_GAP, BoardMutator, DEFAULT_NOTE_HEIGHT, DEFAULT_NOTE_WIDTH, MAX_BATCH_ITEMS,
};

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

/// Deduplicated ids of the items the applied changes touched, in first-touch
/// order.
pub(super) fn touched_item_ids(changes: &[mera_core::board::BoardChange]) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    for change in changes {
        let item_id = change.item_id().to_string();
        if !targets.contains(&item_id) {
            targets.push(item_id);
        }
    }
    targets
}

/// Copy the core diagnostics into the flat `{ code, message }` shape
/// `board.preview` reports for a rejected batch.
pub(super) fn diagnostics_json(diagnostics: &[Diagnostic]) -> Vec<Value> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "code": diagnostic.code().as_str(),
                "message": diagnostic.message(),
            })
        })
        .collect()
}

/// One parsed board mutator input, ready to become a command batch.
pub(super) struct CommandPlan {
    base_version: Option<DocumentVersion>,
    body: PlanBody,
}

enum PlanBody {
    /// Every mutator except `add_notes` yields its commands from the input
    /// alone.
    Ready(Vec<BoardCommand>),
    /// `add_notes` keeps the parsed notes and the digest of the canonical
    /// input; the board-dependent layout and the ids wait for the lock.
    Notes {
        notes: Vec<PlannedNote>,
        input_digest: [u8; 32],
    },
}

struct PlannedNote {
    content: String,
    position: Option<BoardItemPosition>,
    size: BoardItemSize,
}

impl CommandPlan {
    /// The base version the caller named, if any. `None` means "whatever the
    /// board is at when the batch is built".
    pub(super) fn base_version(&self) -> Option<DocumentVersion> {
        self.base_version
    }

    /// Turn the parsed input into commands. `document` and `base_version` are
    /// the state read under the session lock; only `add_notes` uses them.
    pub(super) fn materialize(
        &self,
        document: &BoardDocument,
        base_version: DocumentVersion,
    ) -> Vec<BoardCommand> {
        match &self.body {
            PlanBody::Ready(commands) => commands.clone(),
            PlanBody::Notes {
                notes,
                input_digest,
            } => materialize_notes(notes, input_digest, document, base_version),
        }
    }
}

impl BoardMutator {
    /// Parse and validate this mutator's input into a command plan. The plan
    /// holds the caller's base version and the parsed body; nothing is
    /// materialized until the session lock is held.
    pub(super) fn parse_body(
        self,
        input: &Value,
        op_name: &str,
    ) -> Result<CommandPlan, Diagnostic> {
        let base_version = caller_base_version(input, op_name)?;
        let body = match self {
            Self::AddNotes => PlanBody::Notes {
                notes: parse_notes(input, op_name)?,
                input_digest: canonical_input_digest(input),
            },
            Self::UpdateNotes => PlanBody::Ready(parse_updates(input, op_name)?),
            Self::Arrange => PlanBody::Ready(parse_arrange(input, op_name)?),
            Self::RemoveItems => PlanBody::Ready(parse_remove_items(input, op_name)?),
        };
        Ok(CommandPlan { base_version, body })
    }
}

/// Parse and validate one mutator input. `op_name` names the operation the
/// diagnostics belong to (`board.preview` when called through the preview).
pub(super) fn parse(
    operation: &str,
    input: &Value,
    op_name: &str,
) -> Result<CommandPlan, Diagnostic> {
    let Some(mutator) = BoardMutator::from_name(operation) else {
        return Err(board_caller_error(
            op_name,
            "INVALID_INPUT",
            format!("`{operation}` is not a board mutator"),
        ));
    };
    mutator.parse_body(input, op_name)
}

fn caller_base_version(
    input: &Value,
    op_name: &str,
) -> Result<Option<DocumentVersion>, Diagnostic> {
    let Some(value) = input.get("base_version") else {
        return Ok(None);
    };
    exact_u64(value, "base_version", op_name).map(|version| Some(DocumentVersion::new(version)))
}

/// Read an exact integer from `value`. The schema validator treats integral
/// numbers such as `120.0` as integers, so the builder accepts the same values
/// instead of rejecting them in `as_u64`/`as_i64`.
fn exact_u64(value: &Value, field: &str, op_name: &str) -> Result<u64, Diagnostic> {
    if let Some(raw) = value.as_u64() {
        return Ok(raw);
    }
    if let Some(raw) = value.as_f64()
        && raw.is_finite()
        && raw.fract() == 0.0
        && (0.0..u64::MAX as f64).contains(&raw)
    {
        return Ok(raw as u64);
    }
    Err(board_caller_error(
        op_name,
        "INVALID_INPUT",
        format!("'{field}' must be an exact integer"),
    ))
}

fn exact_i64(value: &Value, field: &str, op_name: &str) -> Result<i64, Diagnostic> {
    if let Some(raw) = value.as_i64() {
        return Ok(raw);
    }
    if let Some(raw) = value.as_f64()
        && raw.is_finite()
        && raw.fract() == 0.0
        && raw >= i64::MIN as f64
        && raw < -(i64::MIN as f64)
    {
        return Ok(raw as i64);
    }
    Err(board_caller_error(
        op_name,
        "INVALID_INPUT",
        format!("'{field}' must be an exact integer"),
    ))
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
    let x = value
        .get("x")
        .ok_or_else(|| board_caller_error(op_name, "INVALID_INPUT", "missing 'position.x'"))
        .and_then(|x| exact_i64(x, "position.x", op_name))?;
    let y = value
        .get("y")
        .ok_or_else(|| board_caller_error(op_name, "INVALID_INPUT", "missing 'position.y'"))
        .and_then(|y| exact_i64(y, "position.y", op_name))?;
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
    let width = value
        .get("width")
        .ok_or_else(|| board_caller_error(op_name, "INVALID_INPUT", "missing 'size.width'"))
        .and_then(|width| exact_u64(width, "size.width", op_name))?;
    let height = value
        .get("height")
        .ok_or_else(|| board_caller_error(op_name, "INVALID_INPUT", "missing 'size.height'"))
        .and_then(|height| exact_u64(height, "size.height", op_name))?;
    Ok(Some(BoardItemSize::new(width, height)))
}

fn parse_notes(input: &Value, op_name: &str) -> Result<Vec<PlannedNote>, Diagnostic> {
    let notes = batch_array(input, "notes", op_name)?;
    let mut parsed = Vec::with_capacity(notes.len());
    for note in notes {
        let content = content_field(note, op_name)?;
        let position = optional_position(note.get("position"), op_name)?;
        let size = optional_size(note.get("size"), op_name)?
            .unwrap_or(BoardItemSize::new(DEFAULT_NOTE_WIDTH, DEFAULT_NOTE_HEIGHT));
        parsed.push(PlannedNote {
            content,
            position,
            size,
        });
    }
    Ok(parsed)
}

fn parse_updates(input: &Value, op_name: &str) -> Result<Vec<BoardCommand>, Diagnostic> {
    let updates = batch_array(input, "updates", op_name)?;
    let mut commands = Vec::with_capacity(updates.len());
    for update in updates {
        let item_id = item_id_field(update.get("item_id"), op_name)?;
        let content = content_field(update, op_name)?;
        commands.push(BoardCommand::UpdateNote { item_id, content });
    }
    Ok(commands)
}

fn parse_arrange(input: &Value, op_name: &str) -> Result<Vec<BoardCommand>, Diagnostic> {
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
                let raw = exact_i64(value, "z", op_name)?;
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
    Ok(commands)
}

fn parse_remove_items(input: &Value, op_name: &str) -> Result<Vec<BoardCommand>, Diagnostic> {
    let item_ids = batch_array(input, "item_ids", op_name)?;
    let mut commands = Vec::with_capacity(item_ids.len());
    for value in item_ids {
        let item_id = item_id_field(Some(value), op_name)?;
        commands.push(BoardCommand::RemoveItem { item_id });
    }
    Ok(commands)
}

/// Lay the parsed notes out around the current board and name their ids.
fn materialize_notes(
    notes: &[PlannedNote],
    input_digest: &[u8; 32],
    document: &BoardDocument,
    base_version: DocumentVersion,
) -> Vec<BoardCommand> {
    // Automatic notes go into one column to the right of the bounding box of
    // the existing items and the explicitly positioned notes in this batch,
    // so the column cannot land on top of either. An empty board starts at
    // the origin.
    let mut bounds: Option<LayoutRect> = None;
    for item in document.items().values() {
        bounds = Some(union_rect(
            bounds,
            LayoutRect::of(item.position(), item.size()),
        ));
    }
    for note in notes {
        if let Some(position) = note.position {
            bounds = Some(union_rect(bounds, LayoutRect::of(&position, &note.size)));
        }
    }
    let (column_x, mut cursor_y) = match bounds {
        Some(bounds) => (bounds.right.saturating_add(AUTO_LAYOUT_GAP), bounds.top),
        None => (0, 0),
    };

    let mut commands = Vec::with_capacity(notes.len());
    for (index, note) in notes.iter().enumerate() {
        let item_id = generated_note_id(input_digest, base_version, index);
        let position = match note.position {
            Some(position) => position,
            None => {
                let position = BoardItemPosition::new(column_x, cursor_y);
                cursor_y = cursor_y
                    .saturating_add(dimension(note.size.height))
                    .saturating_add(AUTO_LAYOUT_GAP);
                position
            }
        };
        commands.push(BoardCommand::AddItem {
            item_id,
            kind: BoardItemKind::Note {
                content: note.content.clone(),
            },
            position,
            size: note.size,
        });
    }
    commands
}

/// SHA-256 of the canonical input with `base_version` removed. The base
/// version is a separate id component, so an explicit `base_version` and the
/// implicit current version produce the same ids for the same notes.
fn canonical_input_digest(input: &Value) -> [u8; 32] {
    let mut payload = input.clone();
    if let Value::Object(map) = &mut payload {
        map.remove("base_version");
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&Sha256::digest(canonical_json(&payload).as_bytes()));
    digest
}

/// The id of a generated note, derived from the canonical input digest, the
/// base version and the item index. Preview and execution of the same input
/// against the same version therefore name the same items.
fn generated_note_id(
    input_digest: &[u8; 32],
    base_version: DocumentVersion,
    index: usize,
) -> BoardItemId {
    let seed = format!(
        "board.add_notes\n{}\n{}\n{index}",
        base_version.get(),
        hex::encode(input_digest)
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
