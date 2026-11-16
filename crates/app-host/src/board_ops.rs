//! Kernel operations for the project board:
//! - `board.read`
//! - `board.preview`
//! - `board.add_notes`
//! - `board.update_notes`
//! - `board.arrange`
//! - `board.remove_items`
//!
//! The four mutators and `board.preview` share one input-to-commands builder,
//! so previewing and then executing the same input against the same base
//! version produces identical commands. Each call parses its input once, then
//! materializes the command batch under the board session lock against the
//! document and version it will actually use; an omitted `base_version` is
//! chosen there, not from an earlier snapshot. Generated note ids are derived
//! from a SHA-256 digest of the canonical input, the base version and the item
//! index, so no state is carried between the two calls. A preview the board
//! would reject reports the core diagnostics instead of only its status.

mod commands;
mod ops;
mod specs;

pub use ops::{BoardMutatorOp, BoardPreviewOp, BoardReadOp, register_board_ops};

use mera_core::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
    DiagnosticTargetDomain,
};
use mera_core::model::DiagnosticId;

fn board_caller_error(op_name: &str, code: &str, message: impl Into<String>) -> Diagnostic {
    let namespace = op_name.split('.').next().unwrap_or("board");
    Diagnostic::new(
        DiagnosticId::new(format!("op:{op_name}:{code}")),
        DiagnosticCode::new(format!("{}/CALLER_{code}", namespace.to_uppercase())),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("board_ops"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new(format!("op.{op_name}"))),
    )
}

fn board_internal_error(op_name: &str, code: &str, message: impl Into<String>) -> Diagnostic {
    let namespace = op_name.split('.').next().unwrap_or("board");
    Diagnostic::new(
        DiagnosticId::new(format!("op:{op_name}:{code}")),
        DiagnosticCode::new(format!("{}/INTERNAL_{code}", namespace.to_uppercase())),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("board_ops"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new(format!("op.{op_name}"))),
    )
}
