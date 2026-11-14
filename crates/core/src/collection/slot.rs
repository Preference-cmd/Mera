//! Collection slot representation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::command::CommandActor;
use crate::model::{AssetId, SlotId, StepId};
use crate::step::StepInputSource;

/// A downstream recipe step that could not be automatically replayed and must
/// be re-confirmed or re-run before the slot is up to date.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StaleStep {
    pub step_id: StepId,
    pub op: String,
    pub op_version: String,
    pub input: serde_json::Value,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub input_sources: BTreeMap<String, StepInputSource>,
    pub actor: CommandActor,
}

/// A single slot inside a collection pointing to an asset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    pub id: SlotId,
    pub asset: AssetId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_steps: Vec<StaleStep>,
}

impl Slot {
    /// Create a new slot.
    pub fn new(id: impl Into<SlotId>, asset: impl Into<AssetId>) -> Self {
        Self {
            id: id.into(),
            asset: asset.into(),
            stale_steps: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_serde_roundtrip() {
        let slot = Slot::new("slot-1", "asset-1");
        let json = serde_json::to_string(&slot).expect("serialize");
        let back: Slot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(slot, back);
    }
}
