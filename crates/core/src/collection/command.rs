//! Collection commands and changes.

use crate::collection::slot::StaleStep;
use crate::model::{AssetId, SlotId};

/// A command that mutates a collection document.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CollectionCommand {
    /// Change the asset associated with a slot and clear any stale steps.
    SetSlotAsset { slot_id: SlotId, asset: AssetId },
    /// Atomically update a slot's asset and stale steps.
    UpdateSlot {
        slot_id: SlotId,
        asset: AssetId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stale_steps: Vec<StaleStep>,
    },
}

/// A change record produced by applying a collection command.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CollectionChange {
    /// A slot's asset pointer (and optionally stale steps) changed.
    SlotAssetChanged {
        slot_id: SlotId,
        before: AssetId,
        after: AssetId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        before_stale_steps: Vec<StaleStep>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        after_stale_steps: Vec<StaleStep>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_serde_roundtrip() {
        let cmd = CollectionCommand::SetSlotAsset {
            slot_id: SlotId::new("slot-1"),
            asset: AssetId::new("asset-1"),
        };
        let json = serde_json::to_string(&cmd).expect("serialize");
        let back: CollectionCommand = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cmd, back);
    }

    #[test]
    fn change_serde_roundtrip() {
        let change = CollectionChange::SlotAssetChanged {
            slot_id: SlotId::new("slot-1"),
            before: AssetId::new("asset-0"),
            after: AssetId::new("asset-1"),
            before_stale_steps: Vec::new(),
            after_stale_steps: Vec::new(),
        };
        let json = serde_json::to_string(&change).expect("serialize");
        let back: CollectionChange = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(change, back);
    }
}
