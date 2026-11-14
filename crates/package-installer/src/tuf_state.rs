//! Durable TUF trust state for verified package catalogs.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fs_tx::atomic_write_file;
use crate::tuf::{RootMetadata, TufKey, verify_root};
use crate::tuf_error::CatalogError;

/// In-memory representation of durable TUF trust state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustedCatalogState {
    pub root: RootMetadata,
    pub root_keys: HashMap<String, TufKey>,
    pub stored_timestamp_version: u64,
}

/// Errors that can occur while persisting or restoring trust state.
#[derive(Debug)]
pub enum TrustedStateError {
    Io {
        path: PathBuf,
        message: String,
    },
    Parse {
        path: PathBuf,
        message: String,
    },
    RootKeysMismatch {
        missing: Vec<String>,
        unexpected: Vec<String>,
    },
    RootVerification {
        message: String,
    },
}

impl fmt::Display for TrustedStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => {
                write!(f, "I/O error on `{}`: {}", path.display(), message)
            }
            Self::Parse { path, message } => {
                write!(
                    f,
                    "failed to parse trusted state at `{}`: {}",
                    path.display(),
                    message
                )
            }
            Self::RootKeysMismatch {
                missing,
                unexpected,
            } => {
                write!(
                    f,
                    "stored root keys disagree with derived keys from root metadata: \
                     missing from derived = {missing:?}, unexpected in stored = {unexpected:?}"
                )
            }
            Self::RootVerification { message } => {
                write!(f, "root metadata failed verification: {message}")
            }
        }
    }
}

impl std::error::Error for TrustedStateError {}

impl TrustedCatalogState {
    #[must_use]
    pub fn bootstrap_from_root(root: RootMetadata, root_keys: HashMap<String, TufKey>) -> Self {
        Self {
            root,
            root_keys,
            stored_timestamp_version: 0,
        }
    }

    pub fn record_timestamp_version(&mut self, fetched: u64) -> bool {
        if fetched > self.stored_timestamp_version {
            self.stored_timestamp_version = fetched;
            true
        } else {
            false
        }
    }

    pub fn load(path: &Path) -> Result<Self, TrustedStateError> {
        let bytes = std::fs::read(path).map_err(|e| TrustedStateError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let state: TrustedCatalogState =
            serde_json::from_slice(&bytes).map_err(|e| TrustedStateError::Parse {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;

        let derived = verify_root(&state.root, None).map_err(|e: CatalogError| {
            TrustedStateError::RootVerification {
                message: e.to_string(),
            }
        })?;
        let missing: Vec<String> = state
            .root_keys
            .keys()
            .filter(|id| !derived.contains_key(*id))
            .cloned()
            .collect();
        let unexpected: Vec<String> = derived
            .keys()
            .filter(|id| !state.root_keys.contains_key(*id))
            .cloned()
            .collect();
        if !missing.is_empty() || !unexpected.is_empty() {
            return Err(TrustedStateError::RootKeysMismatch {
                missing,
                unexpected,
            });
        }
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<(), TrustedStateError> {
        let bytes = serde_json::to_vec(self).map_err(|e| TrustedStateError::Io {
            path: path.to_path_buf(),
            message: format!("serialize: {e}"),
        })?;
        atomic_write_file(path, &bytes).map_err(|e| TrustedStateError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }
}

pub fn load_trusted_state(path: &Path) -> Result<TrustedCatalogState, CatalogError> {
    TrustedCatalogState::load(path).map_err(|e| CatalogError::State {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

pub fn save_trusted_state(state: &TrustedCatalogState, path: &Path) -> Result<(), CatalogError> {
    state.save(path).map_err(|e| CatalogError::State {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}
