pub mod builder;
pub mod client;
pub mod compatibility;

pub use reimagine_package_installer::{tuf, tuf_error as error, tuf_state as state};

pub use builder::{
    CatalogBundle, CatalogParams, EnvSigningKeyProvider, OnlineSigningRole, RoleDistinctTestKey,
    SigningKeyProvider, TestSigningKey, build_catalog, verify_catalog, write_catalog,
};
pub use client::{CatalogClient, VerifiedCatalog};
pub use compatibility::{CatalogTarget, CompatibilityFilter, HostInfo, TargetCustomMetadata};
pub use error::{CatalogError, CatalogResult, CatalogSigningKeyError};
pub use state::{TrustedCatalogState, TrustedStateError, load_trusted_state, save_trusted_state};

#[cfg(test)]
mod state_tests {
    use std::collections::HashMap;

    use super::builder::{
        OnlineSigningRole, RoleDistinctTestKey, SigningKeyProvider, TestSigningKey,
    };
    use super::tuf::{RoleConfig, RootMetadata, RootSigned, SignatureEntry, TufKey, verify_root};
    use super::{TrustedCatalogState, TrustedStateError};

    fn make_root() -> (RootMetadata, HashMap<String, TufKey>) {
        let root_key = TestSigningKey::new();
        let targets_key = RoleDistinctTestKey::new(OnlineSigningRole::Targets);
        let snapshot_key = RoleDistinctTestKey::new(OnlineSigningRole::Snapshot);
        let timestamp_key = RoleDistinctTestKey::new(OnlineSigningRole::Timestamp);

        let mut keys = HashMap::new();
        keys.insert(root_key.key_id(), root_key.tuf_key());
        keys.insert(targets_key.key_id(), targets_key.tuf_key());
        keys.insert(snapshot_key.key_id(), snapshot_key.tuf_key());
        keys.insert(timestamp_key.key_id(), timestamp_key.tuf_key());

        let root_signed = RootSigned {
            kind: "root".to_string(),
            version: 1,
            expires: "2999-12-31T23:59:59Z".to_string(),
            keys: keys.clone(),
            roles: HashMap::from([
                (
                    "root".to_string(),
                    RoleConfig {
                        keyids: vec![root_key.key_id()],
                        threshold: 1,
                    },
                ),
                (
                    "targets".to_string(),
                    RoleConfig {
                        keyids: vec![root_key.key_id()],
                        threshold: 1,
                    },
                ),
                (
                    "snapshot".to_string(),
                    RoleConfig {
                        keyids: vec![root_key.key_id()],
                        threshold: 1,
                    },
                ),
                (
                    "timestamp".to_string(),
                    RoleConfig {
                        keyids: vec![root_key.key_id()],
                        threshold: 1,
                    },
                ),
            ]),
        };
        let root_payload =
            serde_json::to_vec(&serde_json::to_value(&root_signed).unwrap()).unwrap();
        let sig = root_key.sign(&root_payload);
        let root = RootMetadata {
            signed: root_signed,
            signatures: vec![SignatureEntry {
                keyid: root_key.key_id(),
                sig,
            }],
        };
        let derived = verify_root(&root, None).expect("root verify");
        (root, derived)
    }

    #[test]
    fn round_trip_preserves_state() {
        let (root, keys) = make_root();
        let mut state = TrustedCatalogState::bootstrap_from_root(root.clone(), keys.clone());
        state.record_timestamp_version(7);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog_state.json");
        state.save(&path).expect("save");
        let restored = TrustedCatalogState::load(&path).expect("load");
        assert_eq!(restored.stored_timestamp_version, 7);
        assert_eq!(restored.root.signed.version, root.signed.version);
        assert_eq!(restored.root_keys.len(), keys.len());
    }

    #[test]
    fn corrupt_state_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog_state.json");
        std::fs::write(&path, b"{ this is not valid json").unwrap();
        assert!(matches!(
            TrustedCatalogState::load(&path),
            Err(TrustedStateError::Parse { .. })
        ));
    }

    #[test]
    fn missing_state_does_not_reset_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        assert!(matches!(
            TrustedCatalogState::load(&path),
            Err(TrustedStateError::Io { .. })
        ));
    }

    #[test]
    fn record_timestamp_advances_only_when_strictly_greater() {
        let (root, keys) = make_root();
        let mut state = TrustedCatalogState::bootstrap_from_root(root, keys);
        assert!(state.record_timestamp_version(5));
        assert!(!state.record_timestamp_version(5));
        assert!(state.record_timestamp_version(10));
        assert_eq!(state.stored_timestamp_version, 10);
    }

    #[test]
    fn atomic_save_does_not_leave_partial_file() {
        let (root, keys) = make_root();
        let state = TrustedCatalogState::bootstrap_from_root(root, keys);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog_state.json");
        state.save(&path).expect("save");
        assert!(!path.with_extension("json.tmp").exists());
        assert!(path.exists());
    }
}
