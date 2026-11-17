//! Remote official TUF mod catalog, third-party index client, and
//! cancellable streaming downloads with pre-consent staging isolation.

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use mera_core::task::CancelToken;
use mera_package_installer::tuf::{
    SnapshotMetadata, TargetsMetadata, TimestampMetadata, verify_root, verify_snapshot,
    verify_targets, verify_timestamp,
};
use mera_package_installer::{
    TrustedCatalogState, load_trusted_state, save_trusted_state, sha256_hex,
};
use mod_sdk::API_VERSION;
use mod_sdk::package::ModPermissions;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::inventory::{ModInstallSource, ModStorePaths};
use super::service::ModManagerError;
use super::sources::{OfficialCatalogPolicy, ThirdPartySourceConfig, validate_remote_source_url};
use super::trust::{InspectedModPackage, ModTrustSummary};

const MAX_REMOTE_INDEX_BYTES: usize = 8 * 1024 * 1024;
const MAX_REMOTE_ARCHIVE_BYTES: usize = 512 * 1024 * 1024;
static DOWNLOAD_TASK_NONCE: AtomicU64 = AtomicU64::new(1);

/// Current host target triple used when filtering remote mod artifacts.
#[must_use]
pub fn current_host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => "unknown",
    }
}

/// Metadata stored in TUF `TargetDesc.custom` for an official mod package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfficialModTargetCustom {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    pub api: String,
    pub target: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub permissions: ModPermissions,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub min_runtime_version: Option<String>,
}

/// Top-level JSON document published by a third-party mod source (`index.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThirdPartyIndex {
    pub schema_version: u16,
    pub name: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub mods: Vec<ThirdPartyIndexEntry>,
    /// Optional hex-encoded Ed25519 signature over canonical `{schema_version, name, homepage, mods}`.
    #[serde(default)]
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ThirdPartySignable<'a> {
    schema_version: u16,
    name: &'a str,
    homepage: &'a Option<String>,
    mods: &'a [ThirdPartyIndexEntry],
}

/// A mod listed in a third-party `index.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThirdPartyIndexEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    pub api: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub permissions: ModPermissions,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub artifacts: Vec<ModArtifactEntry>,
}

/// A platform-specific (or `noarch`) package artifact inside [`ThirdPartyIndexEntry`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModArtifactEntry {
    pub target: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

/// Unified normalized remote mod entry returned by official or third-party catalog fetches.
#[derive(Debug, Clone)]
pub struct RemoteCatalogMod {
    pub source_id: String,
    pub source_label: String,
    pub install_source: ModInstallSource,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub api: String,
    pub target: String,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub permissions: ModPermissions,
    pub capabilities: Vec<String>,
    pub download_url: String,
    pub sha256: String,
    pub size_bytes: Option<u64>,
    pub compatible: bool,
    pub compatibility_reason: Option<String>,
}

/// Evaluate target triple and `mod-sdk` API semver compatibility for a remote entry.
#[must_use]
pub fn evaluate_compatibility(target: &str, api_req_str: &str) -> (bool, Option<String>) {
    let host_target = current_host_target();
    if target != "noarch" && target != host_target {
        return (
            false,
            Some(format!(
                "targets `{target}`, current host is `{host_target}`"
            )),
        );
    }

    let Ok(req) = semver::VersionReq::parse(api_req_str) else {
        return (
            false,
            Some(format!("invalid mod API requirement `{api_req_str}`")),
        );
    };
    if !req.matches(&API_VERSION) {
        return (
            false,
            Some(format!(
                "requires mod-sdk API `{api_req_str}`, host provides `{API_VERSION}`"
            )),
        );
    }

    (true, None)
}

fn is_valid_sha256_hex(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Verify an optional Ed25519 signature over a [`ThirdPartyIndex`].
pub fn verify_third_party_index_signature(
    index: &ThirdPartyIndex,
    public_key_hex: &str,
) -> Result<(), ModManagerError> {
    let sig_hex = index
        .signature
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ModManagerError::RemoteCatalog {
            message: format!(
                "source `{}` requires a signed index, but `signature` field is missing",
                index.name
            ),
        })?;

    let key_bytes: [u8; 32] = hex::decode(public_key_hex)
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("invalid configured Ed25519 key: {e}"),
        })?
        .try_into()
        .map_err(|_| ModManagerError::RemoteCatalog {
            message: "configured Ed25519 public key must be 32 bytes".to_owned(),
        })?;
    let verifying_key =
        VerifyingKey::from_bytes(&key_bytes).map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("invalid Ed25519 verifying key: {e}"),
        })?;

    let sig_bytes = hex::decode(sig_hex).map_err(|e| ModManagerError::RemoteCatalog {
        message: format!("invalid index signature hex: {e}"),
    })?;
    let signature =
        Signature::from_slice(&sig_bytes).map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("invalid index signature length: {e}"),
        })?;

    let signable = ThirdPartySignable {
        schema_version: index.schema_version,
        name: &index.name,
        homepage: &index.homepage,
        mods: &index.mods,
    };
    let canonical = serde_json::to_value(&signable)
        .and_then(|val| serde_json::to_vec(&val))
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("failed to canonicalize third-party index: {e}"),
        })?;

    verifying_key
        .verify(&canonical, &signature)
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: format!(
                "third-party index signature verification failed for `{}`: {e}",
                index.name
            ),
        })?;
    Ok(())
}

/// Fetch and verify the official TUF mod catalog (`Root -> Timestamp -> Snapshot -> Targets`).
pub async fn fetch_official_mod_catalog(
    paths: &ModStorePaths,
    policy: &OfficialCatalogPolicy,
) -> Result<Vec<RemoteCatalogMod>, ModManagerError> {
    let discovery_url = validate_remote_source_url(&policy.discovery_url).map_err(|e| match e {
        ModManagerError::InvalidInput { message } => ModManagerError::RemoteCatalog { message },
        other => other,
    })?;

    let discovery_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Mera-ModManager/0.1")
        .build()
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: e.to_string(),
        })?;
    let asset_client = reqwest::Client::builder()
        .user_agent("Mera-ModManager/0.1")
        .build()
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: e.to_string(),
        })?;

    let pinned_base = resolve_official_asset_base(&discovery_client, &discovery_url).await?;

    let state_path = paths.official_tuf_state_file();
    let mut state = if state_path.is_file() {
        load_trusted_state(&state_path).map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("failed to load TUF catalog state: {e}"),
        })?
    } else {
        let root_keys = verify_root(&policy.trusted_root, None).map_err(|e| {
            ModManagerError::RemoteCatalog {
                message: format!("embedded official root invalid: {e}"),
            }
        })?;
        TrustedCatalogState::bootstrap_from_root(policy.trusted_root.clone(), root_keys)
    };

    let expected_keys =
        verify_root(&policy.trusted_root, None).map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("embedded official root invalid: {e}"),
        })?;
    if state
        .root_keys
        .keys()
        .any(|id| !expected_keys.contains_key(id))
    {
        return Err(ModManagerError::RemoteCatalog {
            message: "stored official TUF root keys disagree with embedded root".to_owned(),
        });
    }

    let ts_url =
        pinned_base
            .join("timestamp.json")
            .map_err(|e| ModManagerError::RemoteCatalog {
                message: format!("invalid timestamp URL: {e}"),
            })?;
    let ts_bytes = fetch_bounded_bytes(&asset_client, &ts_url, MAX_REMOTE_INDEX_BYTES).await?;
    let timestamp: TimestampMetadata =
        serde_json::from_slice(&ts_bytes).map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("invalid TUF timestamp.json: {e}"),
        })?;
    verify_timestamp(
        &timestamp,
        &policy.trusted_root,
        state.stored_timestamp_version,
    )
    .map_err(|e| ModManagerError::RemoteCatalog {
        message: format!("TUF timestamp verification failed: {e}"),
    })?;

    let snapshot_meta = timestamp.signed.meta.get("snapshot.json").ok_or_else(|| {
        ModManagerError::RemoteCatalog {
            message: "TUF timestamp missing snapshot.json".to_owned(),
        }
    })?;
    let snapshot_url = pinned_base
        .join(&format!("{}.snapshot.json", snapshot_meta.version))
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("invalid snapshot URL: {e}"),
        })?;
    let snapshot_bytes =
        fetch_bounded_bytes(&asset_client, &snapshot_url, MAX_REMOTE_INDEX_BYTES).await?;
    let snapshot: SnapshotMetadata =
        serde_json::from_slice(&snapshot_bytes).map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("invalid TUF snapshot.json: {e}"),
        })?;
    verify_snapshot(
        &snapshot,
        &snapshot_bytes,
        &policy.trusted_root,
        snapshot_meta,
    )
    .map_err(|e| ModManagerError::RemoteCatalog {
        message: format!("TUF snapshot verification failed: {e}"),
    })?;

    let targets_meta =
        snapshot
            .signed
            .meta
            .get("targets.json")
            .ok_or_else(|| ModManagerError::RemoteCatalog {
                message: "TUF snapshot missing targets.json".to_owned(),
            })?;
    let targets_url = pinned_base
        .join(&format!("{}.targets.json", targets_meta.version))
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("invalid targets URL: {e}"),
        })?;
    let targets_bytes =
        fetch_bounded_bytes(&asset_client, &targets_url, MAX_REMOTE_INDEX_BYTES).await?;
    let targets: TargetsMetadata =
        serde_json::from_slice(&targets_bytes).map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("invalid TUF targets.json: {e}"),
        })?;
    verify_targets(&targets, &targets_bytes, &policy.trusted_root, targets_meta).map_err(|e| {
        ModManagerError::RemoteCatalog {
            message: format!("TUF targets verification failed: {e}"),
        }
    })?;

    if state.record_timestamp_version(timestamp.signed.version) {
        save_trusted_state(&state, &state_path).map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("failed to persist TUF catalog state: {e}"),
        })?;
    }

    let mut out = Vec::new();
    for (target_path, desc) in &targets.signed.targets {
        let Some(custom_val) = &desc.custom else {
            continue;
        };
        let custom: OfficialModTargetCustom =
            serde_json::from_value(custom_val.clone()).map_err(|e| {
                ModManagerError::RemoteCatalog {
                    message: format!("invalid custom metadata for target `{target_path}`: {e}"),
                }
            })?;
        let sha256 = desc
            .hashes
            .get("sha256")
            .filter(|d| is_valid_sha256_hex(d))
            .cloned()
            .ok_or_else(|| ModManagerError::RemoteCatalog {
                message: format!("target `{target_path}` missing valid sha256 hash"),
            })?;
        let download_url =
            pinned_base
                .join(target_path)
                .map_err(|e| ModManagerError::RemoteCatalog {
                    message: format!("invalid target URL `{target_path}`: {e}"),
                })?;
        validate_remote_source_url(download_url.as_str())?;

        let (compatible, compatibility_reason) =
            evaluate_compatibility(&custom.target, &custom.api);

        out.push(RemoteCatalogMod {
            source_id: "official".to_owned(),
            source_label: "Official Mera Registry".to_owned(),
            install_source: ModInstallSource::Official {
                catalog_url: policy.discovery_url.clone(),
            },
            id: custom.id,
            name: custom.name,
            version: custom.version,
            description: custom.description,
            api: custom.api,
            target: custom.target,
            author: custom.author,
            homepage: custom.homepage,
            license: custom.license,
            permissions: custom.permissions,
            capabilities: custom.capabilities,
            download_url: download_url.to_string(),
            sha256,
            size_bytes: Some(desc.length),
            compatible,
            compatibility_reason,
        });
    }

    out.sort_by(|a, b| a.id.cmp(&b.id).then(b.version.cmp(&a.version)));
    Ok(out)
}

async fn resolve_official_asset_base(
    client: &reqwest::Client,
    discovery_url: &Url,
) -> Result<Url, ModManagerError> {
    let url_str = discovery_url.as_str();
    if url_str.contains("/releases/download/") || url_str.ends_with('/') {
        return Ok(ensure_trailing_slash(discovery_url));
    }

    let response = client
        .get(discovery_url.clone())
        .send()
        .await
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("official catalog discovery failed: {e}"),
        })?;
    let status = response.status();
    if status.is_success() {
        return Ok(ensure_trailing_slash(discovery_url));
    }
    if !status.is_redirection() {
        return Err(ModManagerError::RemoteCatalog {
            message: format!("official catalog discovery returned HTTP {status}"),
        });
    }

    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ModManagerError::RemoteCatalog {
            message: "official catalog redirect missing Location header".to_owned(),
        })?;
    let redirect = discovery_url
        .join(location)
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("cannot resolve Location header: {e}"),
        })?;
    if redirect.origin() != discovery_url.origin() {
        return Err(ModManagerError::RemoteCatalog {
            message: "official catalog redirect crossed origin".to_owned(),
        });
    }
    let tag = redirect
        .path()
        .split("/tag/")
        .nth(1)
        .unwrap_or("")
        .trim_end_matches('/');
    if tag.is_empty() || !tag.starts_with("mod-catalog-v") {
        return Err(ModManagerError::RemoteCatalog {
            message: format!(
                "redirect path `{}` has no `mod-catalog-v*` tag",
                redirect.path()
            ),
        });
    }
    let pinned = format!(
        "{}://{}/releases/download/{tag}/",
        redirect.scheme(),
        redirect.authority()
    );
    Url::parse(&pinned).map_err(|e| ModManagerError::RemoteCatalog {
        message: format!("invalid pinned catalog URL: {e}"),
    })
}

#[derive(Debug, Deserialize)]
struct GitHubReleasePayload {
    #[serde(default)]
    assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubReleaseAsset {
    name: String,
    browser_download_url: String,
}

/// Fetch and validate a third-party HTTP `index.json` or GitHub Release source.
pub async fn fetch_third_party_catalog(
    source: &ThirdPartySourceConfig,
) -> Result<Vec<RemoteCatalogMod>, ModManagerError> {
    let source_url = validate_remote_source_url(&source.url)?;
    let client = reqwest::Client::builder()
        .user_agent("Mera-ModManager/0.1")
        .build()
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: e.to_string(),
        })?;

    let raw_bytes = fetch_bounded_bytes(&client, &source_url, MAX_REMOTE_INDEX_BYTES).await?;
    let index_bytes = if is_github_release_api_url(&source_url) {
        let release: GitHubReleasePayload =
            serde_json::from_slice(&raw_bytes).map_err(|e| ModManagerError::RemoteCatalog {
                message: format!("invalid GitHub Release JSON from `{}`: {e}", source.url),
            })?;
        let index_asset = release
            .assets
            .iter()
            .find(|a| a.name == "mod-index.json" || a.name == "index.json")
            .ok_or_else(|| ModManagerError::RemoteCatalog {
                message: format!(
                    "GitHub Release `{}` has no `mod-index.json` or `index.json` asset",
                    source.url
                ),
            })?;
        let asset_url = validate_remote_source_url(&index_asset.browser_download_url)?;
        fetch_bounded_bytes(&client, &asset_url, MAX_REMOTE_INDEX_BYTES).await?
    } else {
        raw_bytes
    };

    let index: ThirdPartyIndex =
        serde_json::from_slice(&index_bytes).map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("invalid third-party index JSON from `{}`: {e}", source.url),
        })?;
    if index.schema_version != 1 {
        return Err(ModManagerError::RemoteCatalog {
            message: format!(
                "unsupported third-party index schema_version {} from `{}`",
                index.schema_version, source.url
            ),
        });
    }

    if let Some(pubkey) = &source.public_key_ed25519 {
        verify_third_party_index_signature(&index, pubkey)?;
    }

    let mut out = Vec::new();
    let host_target = current_host_target();
    for entry in index.mods {
        // Prefer exact host target artifact if present, otherwise `noarch`, otherwise first entry (marked incompatible).
        let chosen = entry
            .artifacts
            .iter()
            .find(|a| a.target == host_target)
            .or_else(|| entry.artifacts.iter().find(|a| a.target == "noarch"))
            .or_else(|| entry.artifacts.first());
        let Some(artifact) = chosen else {
            continue;
        };

        if !is_valid_sha256_hex(&artifact.sha256) {
            return Err(ModManagerError::RemoteCatalog {
                message: format!(
                    "mod `{}` in `{}` has invalid sha256 digest",
                    entry.id, source.name
                ),
            });
        }
        let resolved_download =
            source_url
                .join(&artifact.url)
                .map_err(|e| ModManagerError::RemoteCatalog {
                    message: format!("invalid artifact URL `{}`: {e}", artifact.url),
                })?;
        validate_remote_source_url(resolved_download.as_str())?;

        let (compatible, compatibility_reason) =
            evaluate_compatibility(&artifact.target, &entry.api);

        out.push(RemoteCatalogMod {
            source_id: source.id.clone(),
            source_label: source.name.clone(),
            install_source: ModInstallSource::ThirdPartyUrl {
                source_url: source.url.clone(),
            },
            id: entry.id,
            name: entry.name,
            version: entry.version,
            description: entry.description,
            api: entry.api,
            target: artifact.target.clone(),
            author: entry.author,
            homepage: entry.homepage,
            license: entry.license,
            permissions: entry.permissions,
            capabilities: entry.capabilities,
            download_url: resolved_download.to_string(),
            sha256: artifact.sha256.to_ascii_lowercase(),
            size_bytes: artifact.size_bytes,
            compatible,
            compatibility_reason,
        });
    }

    out.sort_by(|a, b| a.id.cmp(&b.id).then(b.version.cmp(&a.version)));
    Ok(out)
}

fn is_github_release_api_url(url: &Url) -> bool {
    url.host_str() == Some("api.github.com") && url.path().contains("/releases/")
}

fn ensure_trailing_slash(url: &Url) -> Url {
    let mut s = url.to_string();
    if !s.ends_with('/') {
        s.push('/');
    }
    Url::parse(&s).unwrap_or_else(|_| url.clone())
}

async fn fetch_bounded_bytes(
    client: &reqwest::Client,
    url: &Url,
    limit: usize,
) -> Result<Vec<u8>, ModManagerError> {
    let mut response =
        client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| ModManagerError::RemoteCatalog {
                message: format!("request to `{url}` failed: {e}"),
            })?;
    if !response.status().is_success() {
        return Err(ModManagerError::RemoteCatalog {
            message: format!("`{url}` returned HTTP {}", response.status()),
        });
    }
    if let Some(len) = response.content_length()
        && len as usize > limit
    {
        return Err(ModManagerError::RemoteCatalog {
            message: format!("response from `{url}` exceeds size limit ({len} bytes)"),
        });
    }

    let mut buf = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("reading response from `{url}` failed: {e}"),
        })?
    {
        if buf.len().saturating_add(chunk.len()) > limit {
            return Err(ModManagerError::RemoteCatalog {
                message: format!("response from `{url}` exceeds size limit ({limit} bytes)"),
            });
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Lifecycle state of an asynchronous remote mod download task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModDownloadStatus {
    Downloading,
    Verifying,
    AwaitingConsent,
    Installed,
    Cancelled,
    Failed,
}

/// Snapshot of a background remote mod download task.
#[derive(Debug, Clone)]
pub struct ModDownloadTaskSnapshot {
    pub task_id: String,
    pub source_id: String,
    pub mod_id: String,
    pub version: String,
    pub status: ModDownloadStatus,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub error: Option<String>,
    pub pending_consent: Option<(
        ModTrustSummary,
        Option<super::inventory::InstalledModRecord>,
    )>,
}

/// Verified bytes held in memory awaiting explicit user consent before touching `mods/`.
#[derive(Debug, Clone)]
pub struct PendingRemoteInstall {
    pub bytes: Vec<u8>,
    pub source: ModInstallSource,
    pub inspected: InspectedModPackage,
}

#[derive(Debug)]
struct ActiveTaskEntry {
    snapshot: ModDownloadTaskSnapshot,
    cancel: CancelToken,
    pending: Option<PendingRemoteInstall>,
}

/// Coordinates cancellable remote archive downloads and pre-consent staging isolation.
#[derive(Debug, Default)]
pub struct ModDownloadManager {
    tasks: RwLock<HashMap<String, ActiveTaskEntry>>,
}

impl ModDownloadManager {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a new task ID and register initial `Downloading` state.
    pub fn create_task(
        &self,
        source_id: &str,
        mod_id: &str,
        version: &str,
        total_bytes: Option<u64>,
    ) -> (String, CancelToken) {
        let nonce = DOWNLOAD_TASK_NONCE.fetch_add(1, Ordering::Relaxed);
        let task_id = format!("moddl-{nonce}");
        let cancel = CancelToken::new();
        let entry = ActiveTaskEntry {
            snapshot: ModDownloadTaskSnapshot {
                task_id: task_id.clone(),
                source_id: source_id.to_owned(),
                mod_id: mod_id.to_owned(),
                version: version.to_owned(),
                status: ModDownloadStatus::Downloading,
                downloaded_bytes: 0,
                total_bytes,
                error: None,
                pending_consent: None,
            },
            cancel: cancel.clone(),
            pending: None,
        };
        self.tasks
            .write()
            .expect("download tasks lock poisoned")
            .insert(task_id.clone(), entry);
        (task_id, cancel)
    }

    pub fn update_progress(&self, task_id: &str, downloaded_bytes: u64, total_bytes: Option<u64>) {
        if let Some(entry) = self
            .tasks
            .write()
            .expect("download tasks lock poisoned")
            .get_mut(task_id)
        {
            entry.snapshot.downloaded_bytes = downloaded_bytes;
            if total_bytes.is_some() {
                entry.snapshot.total_bytes = total_bytes;
            }
        }
    }

    pub fn set_verifying(&self, task_id: &str) {
        if let Some(entry) = self
            .tasks
            .write()
            .expect("download tasks lock poisoned")
            .get_mut(task_id)
        {
            entry.snapshot.status = ModDownloadStatus::Verifying;
        }
    }

    pub fn set_awaiting_consent(
        &self,
        task_id: &str,
        summary: ModTrustSummary,
        existing: Option<super::inventory::InstalledModRecord>,
        pending: PendingRemoteInstall,
    ) {
        if let Some(entry) = self
            .tasks
            .write()
            .expect("download tasks lock poisoned")
            .get_mut(task_id)
        {
            entry.snapshot.status = ModDownloadStatus::AwaitingConsent;
            entry.snapshot.pending_consent = Some((summary, existing));
            entry.pending = Some(pending);
        }
    }

    pub fn set_installed(&self, task_id: &str) {
        if let Some(entry) = self
            .tasks
            .write()
            .expect("download tasks lock poisoned")
            .get_mut(task_id)
        {
            entry.snapshot.status = ModDownloadStatus::Installed;
            entry.snapshot.pending_consent = None;
            entry.pending = None;
        }
    }

    pub fn set_failed(&self, task_id: &str, error: String) {
        if let Some(entry) = self
            .tasks
            .write()
            .expect("download tasks lock poisoned")
            .get_mut(task_id)
        {
            entry.snapshot.status = ModDownloadStatus::Failed;
            entry.snapshot.error = Some(error);
            entry.pending = None;
        }
    }

    pub fn cancel_task(&self, task_id: &str) -> Result<ModDownloadTaskSnapshot, ModManagerError> {
        let mut guard = self.tasks.write().expect("download tasks lock poisoned");
        let entry = guard
            .get_mut(task_id)
            .ok_or_else(|| ModManagerError::InvalidInput {
                message: format!("unknown download task `{task_id}`"),
            })?;
        entry.cancel.cancel();
        if matches!(
            entry.snapshot.status,
            ModDownloadStatus::Downloading
                | ModDownloadStatus::Verifying
                | ModDownloadStatus::AwaitingConsent
        ) {
            entry.snapshot.status = ModDownloadStatus::Cancelled;
            entry.snapshot.pending_consent = None;
            entry.pending = None;
        }
        Ok(entry.snapshot.clone())
    }

    pub fn get_snapshot(&self, task_id: &str) -> Result<ModDownloadTaskSnapshot, ModManagerError> {
        self.tasks
            .read()
            .expect("download tasks lock poisoned")
            .get(task_id)
            .map(|e| e.snapshot.clone())
            .ok_or_else(|| ModManagerError::InvalidInput {
                message: format!("unknown download task `{task_id}`"),
            })
    }

    pub fn take_pending(&self, task_id: &str) -> Result<PendingRemoteInstall, ModManagerError> {
        let mut guard = self.tasks.write().expect("download tasks lock poisoned");
        let entry = guard
            .get_mut(task_id)
            .ok_or_else(|| ModManagerError::InvalidInput {
                message: format!("unknown download task `{task_id}`"),
            })?;
        entry
            .pending
            .take()
            .ok_or_else(|| ModManagerError::InvalidInput {
                message: format!("download task `{task_id}` is not awaiting consent"),
            })
    }
}

/// Stream a remote mod archive with SHA-256 verification and cancellation support.
pub async fn download_verified_archive<F>(
    download_url: &str,
    expected_sha256: &str,
    expected_length: Option<u64>,
    cancel: &CancelToken,
    mut on_progress: F,
) -> Result<Vec<u8>, ModManagerError>
where
    F: FnMut(u64, Option<u64>),
{
    let url = validate_remote_source_url(download_url)?;
    let client = reqwest::Client::builder()
        .user_agent("Mera-ModManager/0.1")
        .build()
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: e.to_string(),
        })?;

    let mut response =
        client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| ModManagerError::RemoteCatalog {
                message: format!("download request `{url}` failed: {e}"),
            })?;
    if !response.status().is_success() {
        return Err(ModManagerError::RemoteCatalog {
            message: format!("download `{url}` returned HTTP {}", response.status()),
        });
    }

    let total_bytes = response.content_length().or(expected_length);
    if let Some(total) = total_bytes
        && total as usize > MAX_REMOTE_ARCHIVE_BYTES
    {
        return Err(ModManagerError::ArchiveInvalid {
            reason: format!("remote archive size ({total} bytes) exceeds 512 MiB limit"),
        });
    }

    let mut out = Vec::new();
    let mut hasher = Sha256::new();
    let mut downloaded = 0u64;
    let mut last_reported = 0u64;

    loop {
        if cancel.is_cancelled() {
            return Err(ModManagerError::Cancelled);
        }
        let chunk = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(ModManagerError::Cancelled);
            }
            chunk_res = response.chunk() => {
                chunk_res.map_err(|e| ModManagerError::RemoteCatalog {
                    message: format!("download stream failed: {e}"),
                })?
            }
        };
        let Some(bytes) = chunk else {
            break;
        };
        if out.len().saturating_add(bytes.len()) > MAX_REMOTE_ARCHIVE_BYTES {
            return Err(ModManagerError::ArchiveInvalid {
                reason: "remote archive exceeds 512 MiB limit".to_owned(),
            });
        }
        hasher.update(&bytes);
        out.extend_from_slice(&bytes);
        downloaded += bytes.len() as u64;
        if downloaded.saturating_sub(last_reported) >= 32 * 1024 {
            last_reported = downloaded;
            on_progress(downloaded, total_bytes);
        }
    }

    on_progress(downloaded, total_bytes);

    if let Some(expected_len) = expected_length
        && out.len() as u64 != expected_len
    {
        return Err(ModManagerError::ArchiveInvalid {
            reason: format!(
                "downloaded size mismatch: expected {expected_len} bytes, got {}",
                out.len()
            ),
        });
    }

    let actual_sha = hex::encode(hasher.finalize());
    let _ = sha256_hex(&[]);
    if !actual_sha.eq_ignore_ascii_case(expected_sha256) {
        return Err(ModManagerError::ArchiveInvalid {
            reason: format!(
                "SHA-256 verification failed: expected {expected_sha256}, got {actual_sha}"
            ),
        });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    #[test]
    fn third_party_index_signature_accepts_valid_and_rejects_tamper() {
        let seed = [7u8; 32];
        let signing = SigningKey::from_bytes(&seed);
        let pub_hex = hex::encode(signing.verifying_key().to_bytes());

        let mut index = ThirdPartyIndex {
            schema_version: 1,
            name: "Signed Community".to_owned(),
            homepage: Some("https://example.com".to_owned()),
            mods: vec![ThirdPartyIndexEntry {
                id: "demo".to_owned(),
                name: "Demo".to_owned(),
                version: "0.1.0".to_owned(),
                description: Some("Demo mod".to_owned()),
                api: "0.1".to_owned(),
                author: Some("Alice".to_owned()),
                homepage: None,
                license: Some("MIT".to_owned()),
                permissions: ModPermissions::default(),
                capabilities: vec!["text".to_owned()],
                artifacts: vec![ModArtifactEntry {
                    target: "noarch".to_owned(),
                    url: "https://example.com/demo.rmod.tar.gz".to_owned(),
                    sha256: "a".repeat(64),
                    size_bytes: Some(123),
                }],
            }],
            signature: None,
        };

        let signable = ThirdPartySignable {
            schema_version: index.schema_version,
            name: &index.name,
            homepage: &index.homepage,
            mods: &index.mods,
        };
        let canonical = serde_json::to_vec(&serde_json::to_value(&signable).unwrap()).unwrap();
        index.signature = Some(hex::encode(signing.sign(&canonical).to_bytes()));

        verify_third_party_index_signature(&index, &pub_hex).unwrap();

        index.mods[0].version = "9.9.9".to_owned();
        assert!(verify_third_party_index_signature(&index, &pub_hex).is_err());
    }
}
