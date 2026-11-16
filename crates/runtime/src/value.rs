//! Runtime values passed between node executors during a single run.
//!
//! The canonical type is owned by `mera-inference` and
//! re-exported by the `mera-inference` facade as
//! [`ExecutionValue`](mera_inference::ExecutionValue). This module
//! exists for two reasons:
//!
//! 1. Path stability: callers can continue to use
//!    `use mera_runtime::value::RuntimeValue;` without churn.
//! 2. Migration window: the historical `RuntimeValue` /
//!    `RuntimeConditioning` names are preserved as compatibility
//!    aliases until downstream crates fully migrate to the canonical
//!    `ExecutionValue` / `ExecutionConditioning` names.
//!
//! Backend-affine handle types keep their existing names
//! (`RuntimeModelHandle`, `RuntimeClipHandle`, `RuntimeVaeHandle`,
//! `RuntimeLatent`, `RuntimeImage`, `BackendTensorHandle`,
//! `ConditioningMetadata`, `Backend`, `BackendPayloadKey`) because
//! they are part of the public backend contract and have always been
//! defined next to the runtime value envelope.

pub use mera_inference::{
    Backend, BackendPayloadKey, BackendTensorHandle, BackendTensorMetadata, ConditioningMetadata,
    ExecutionConditioning, ExecutionOutput, ExecutionValue, ExecutionValueKind,
    ExecutionValueRetention, RuntimeClipHandle, RuntimeImage, RuntimeLatent, RuntimeModelHandle,
    RuntimeVaeHandle, StageId,
};

/// Compatibility alias for [`ExecutionValue`].
///
/// Prefer [`ExecutionValue`](mera_inference::ExecutionValue)
/// for new code; the alias exists so existing call sites that import
/// `RuntimeValue` from `mera_runtime` continue to compile.
pub type RuntimeValue = ExecutionValue;

/// Compatibility alias for [`ExecutionConditioning`].
///
/// Prefer
/// [`ExecutionConditioning`](mera_inference::ExecutionConditioning)
/// for new code.
pub type RuntimeConditioning = ExecutionConditioning;
