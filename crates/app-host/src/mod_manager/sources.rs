//! Persistent third-party mod source registry and official catalog policy.

use std::path::Path;

use jiff::Timestamp;
use mera_package_installer::atomic_write_file;
use mera_package_installer::tuf::{RootMetadata, verify_root};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use super::inventory::ModStorePaths;
use super::service::ModManagerError;

/// Schema version for `<mods_root>/sources.json`.
pub const MOD_SOURCES_SCHEMA_VERSION: u16 = 1;

/// Default official mod catalog discovery URL.
pub const DEFAULT_OFFICIAL_MOD_CATALOG_URL: &str =
    "https://github.com/Preference-cmd/Mera/releases/latest";

/// Optional environment override for the official mod catalog URL (primarily for tests/staging).
pub const OFFICIAL_MOD_CATALOG_URL_ENV: &str = "MERA_MOD_CATALOG_URL";

/// Optional environment override pointing at a JSON TUF `RootMetadata` file.
pub const OFFICIAL_MOD_CATALOG_ROOT_FILE_ENV: &str = "MERA_MOD_CATALOG_ROOT_FILE";

const EMBEDDED_MOD_CATALOG_ROOT: &str = include_str!("../../assets/mod-catalog-root.json");

/// Configuration for the built-in official TUF mod catalog.
#[derive(Debug, Clone)]
pub struct OfficialCatalogPolicy {
    pub discovery_url: String,
    pub trusted_root: RootMetadata,
}

impl OfficialCatalogPolicy {
    /// Load the embedded official root and resolve any environment overrides.
    pub fn from_env() -> Result<Self, ModManagerError> {
        let discovery_url = std::env::var(OFFICIAL_MOD_CATALOG_URL_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_OFFICIAL_MOD_CATALOG_URL.to_owned());

        let root_json = match std::env::var(OFFICIAL_MOD_CATALOG_ROOT_FILE_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            Some(path) => std::fs::read_to_string(&path).map_err(|e| ModManagerError::Io {
                path: path.into(),
                message: e.to_string(),
            })?,
            None => EMBEDDED_MOD_CATALOG_ROOT.to_owned(),
        };

        let trusted_root: RootMetadata =
            serde_json::from_str(&root_json).map_err(|e| ModManagerError::RemoteCatalog {
                message: format!("invalid official mod catalog root metadata: {e}"),
            })?;
        verify_root(&trusted_root, None).map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("official mod catalog root failed verification: {e}"),
        })?;

        Ok(Self {
            discovery_url,
            trusted_root,
        })
    }
}

/// Persisted third-party index source configured by the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThirdPartySourceConfig {
    pub id: String,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    #[serde(default)]
    pub public_key_ed25519: Option<String>,
    pub added_at: String,
    #[serde(default)]
    pub last_refreshed_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
}

/// Serialized contents of `<mods_root>/sources.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModSourcesFile {
    pub schema_version: u16,
    #[serde(default)]
    pub sources: Vec<ThirdPartySourceConfig>,
}

impl Default for ModSourcesFile {
    fn default() -> Self {
        Self {
            schema_version: MOD_SOURCES_SCHEMA_VERSION,
            sources: Vec::new(),
        }
    }
}

/// Validate that a third-party source URL uses HTTPS (allowing loopback HTTP for local testing).
pub fn validate_remote_source_url(raw_url: &str) -> Result<Url, ModManagerError> {
    let trimmed = raw_url.trim();
    let parsed = Url::parse(trimmed).map_err(|e| ModManagerError::InvalidInput {
        message: format!("invalid source URL `{trimmed}`: {e}"),
    })?;
    let host = parsed.host_str().unwrap_or_default();
    let is_loopback = host == "127.0.0.1" || host == "localhost" || host == "::1";
    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" if is_loopback => Ok(parsed),
        other => Err(ModManagerError::InvalidInput {
            message: format!(
                "remote mod source must use `https://` (got `{other}://`); loopback `http://` is only allowed on localhost"
            ),
        }),
    }
}

/// Validate an optional 64-character hex Ed25519 public key string.
pub fn validate_ed25519_public_key(raw: Option<&str>) -> Result<Option<String>, ModManagerError> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let bytes = hex::decode(raw).map_err(|e| ModManagerError::InvalidInput {
        message: format!("invalid Ed25519 public key hex: {e}"),
    })?;
    let key_bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| ModManagerError::InvalidInput {
            message: "Ed25519 public key must be 32 bytes (64 hex characters)".to_owned(),
        })?;
    ed25519_dalek::VerifyingKey::from_bytes(&key_bytes).map_err(|e| {
        ModManagerError::InvalidInput {
            message: format!("invalid Ed25519 verifying key: {e}"),
        }
    })?;
    Ok(Some(raw.to_ascii_lowercase()))
}

/// Atomic persistence helper for `<mods_root>/sources.json`.
#[derive(Debug, Clone)]
pub struct ModSourcesStore {
    paths: ModStorePaths,
}

impl ModSourcesStore {
    #[must_use]
    pub fn new(paths: ModStorePaths) -> Self {
        Self { paths }
    }

    pub fn load(&self) -> Result<ModSourcesFile, ModManagerError> {
        let path: &Path = &self.paths.sources_file();
        match std::fs::read(path) {
            Ok(bytes) => {
                let parsed: ModSourcesFile =
                    serde_json::from_slice(&bytes).map_err(|e| ModManagerError::Io {
                        path: path.to_path_buf(),
                        message: format!("invalid sources.json: {e}"),
                    })?;
                if parsed.schema_version != MOD_SOURCES_SCHEMA_VERSION {
                    return Err(ModManagerError::Io {
                        path: path.to_path_buf(),
                        message: format!(
                            "unsupported sources.json schema_version {}",
                            parsed.schema_version
                        ),
                    });
                }
                Ok(parsed)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ModSourcesFile::default()),
            Err(e) => Err(ModManagerError::Io {
                path: path.to_path_buf(),
                message: e.to_string(),
            }),
        }
    }

    pub fn save(&self, file: &ModSourcesFile) -> Result<(), ModManagerError> {
        let bytes = serde_json::to_vec_pretty(file).map_err(|e| ModManagerError::Io {
            path: self.paths.sources_file(),
            message: format!("failed to serialize sources.json: {e}"),
        })?;
        atomic_write_file(&self.paths.sources_file(), &bytes).map_err(|e| ModManagerError::Io {
            path: self.paths.sources_file(),
            message: e.to_string(),
        })
    }

    pub fn add_source(
        &self,
        name: &str,
        url: &str,
        public_key_ed25519: Option<&str>,
    ) -> Result<ThirdPartySourceConfig, ModManagerError> {
        let trimmed_name = name.trim();
        if trimmed_name.is_empty() {
            return Err(ModManagerError::InvalidInput {
                message: "source name must not be empty".to_owned(),
            });
        }
        let parsed_url = validate_remote_source_url(url)?;
        let normalized_url = parsed_url.to_string();
        let validated_key = validate_ed25519_public_key(public_key_ed25519)?;

        let mut file = self.load()?;
        if file.sources.iter().any(|s| s.url == normalized_url) {
            return Err(ModManagerError::InvalidInput {
                message: format!("source `{normalized_url}` is already configured"),
            });
        }

        let slug = make_source_id(trimmed_name, &file.sources);
        let record = ThirdPartySourceConfig {
            id: slug,
            name: trimmed_name.to_owned(),
            url: normalized_url,
            enabled: true,
            public_key_ed25519: validated_key,
            added_at: Timestamp::now().to_string(),
            last_refreshed_at: None,
            last_error: None,
        };
        file.sources.push(record.clone());
        self.save(&file)?;
        Ok(record)
    }

    pub fn set_enabled(
        &self,
        source_id: &str,
        enabled: bool,
    ) -> Result<ThirdPartySourceConfig, ModManagerError> {
        let mut file = self.load()?;
        let found = file
            .sources
            .iter_mut()
            .find(|s| s.id == source_id)
            .ok_or_else(|| ModManagerError::InvalidInput {
                message: format!("unknown third-party source `{source_id}`"),
            })?;
        found.enabled = enabled;
        let updated = found.clone();
        self.save(&file)?;
        Ok(updated)
    }

    pub fn remove_source(&self, source_id: &str) -> Result<(), ModManagerError> {
        let mut file = self.load()?;
        let before = file.sources.len();
        file.sources.retain(|s| s.id != source_id);
        if file.sources.len() == before {
            return Err(ModManagerError::InvalidInput {
                message: format!("unknown third-party source `{source_id}`"),
            });
        }
        self.save(&file)
    }

    pub fn record_refresh_status(
        &self,
        source_id: &str,
        error: Option<String>,
    ) -> Result<(), ModManagerError> {
        let mut file = self.load()?;
        if let Some(source) = file.sources.iter_mut().find(|s| s.id == source_id) {
            source.last_refreshed_at = Some(Timestamp::now().to_string());
            source.last_error = error;
            self.save(&file)?;
        }
        Ok(())
    }
}

fn make_source_id(name: &str, existing: &[ThirdPartySourceConfig]) -> String {
    let base: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    let stem = if base.is_empty() { "source" } else { &base };
    let mut candidate = format!("src-{stem}");
    let mut n = 2usize;
    while existing.iter().any(|s| s.id == candidate) || candidate == "official" {
        candidate = format!("src-{stem}-{n}");
        n += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_url_requires_https_except_loopback() {
        assert!(validate_remote_source_url("https://mods.example.com/index.json").is_ok());
        assert!(validate_remote_source_url("http://127.0.0.1:8080/index.json").is_ok());
        assert!(validate_remote_source_url("http://localhost:3000/index.json").is_ok());
        assert!(validate_remote_source_url("http://example.com/index.json").is_err());
        assert!(validate_remote_source_url("file:///tmp/index.json").is_err());
    }

    #[test]
    fn sources_store_round_trip_and_deduplicates_urls() {
        let temp = tempfile::tempdir().unwrap();
        let store = ModSourcesStore::new(ModStorePaths::from_mods_root(temp.path()));

        let added = store
            .add_source(
                "Community Mods",
                "https://example.com/mods/index.json",
                None,
            )
            .unwrap();
        assert!(added.enabled);
        assert!(added.id.starts_with("src-"));

        assert!(
            store
                .add_source("Dup", "https://example.com/mods/index.json", None)
                .is_err()
        );

        let toggled = store.set_enabled(&added.id, false).unwrap();
        assert!(!toggled.enabled);

        store.remove_source(&added.id).unwrap();
        assert!(store.load().unwrap().sources.is_empty());
    }
}
