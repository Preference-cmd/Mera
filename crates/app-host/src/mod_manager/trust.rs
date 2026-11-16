//! Archive inspection, upgrade checks, and consent evaluation for mods.

use std::collections::BTreeSet;
use std::path::Path;

use mera_package_installer::{ExtractionLimits, decompress_gzip, read_tar_member, sha256_hex};
use mod_sdk::ModManifest;
use mod_sdk::package::{
    ModPackageManifest, ModPermissions, newly_added_operations, validate_package_against_manifest,
};
use serde::Serialize;

use super::inventory::{InstalledModRecord, ModInstallSource};
use super::service::ModManagerError;

const MAX_MANIFEST_JSON_BYTES: u64 = 1024 * 1024;

/// Parsed metadata read from a `.rmod.tar.gz` archive or local directory
/// before any child binary is executed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InspectedModPackage {
    pub package: ModPackageManifest,
    pub manifest: ModManifest,
    pub package_sha256: Option<String>,
}

/// Security and compatibility diff computed before installing or upgrading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpgradeCheck {
    pub is_upgrade: bool,
    pub source_conflict: Option<String>,
    pub permission_expansions: Vec<String>,
    pub newly_added_operations: Vec<String>,
}

/// Summary surfaced to consent prompts prior to installation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModTrustSummary {
    pub inspected: InspectedModPackage,
    pub source: ModInstallSource,
    pub upgrade: UpgradeCheck,
    pub requires_explicit_consent: bool,
    pub requires_permission_consent: bool,
}

/// Inspect a `.rmod.tar.gz` file on disk without extracting the full tree or launching binaries.
pub fn inspect_rmod_archive(path: &Path) -> Result<InspectedModPackage, ModManagerError> {
    let bytes = std::fs::read(path).map_err(|e| ModManagerError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    inspect_rmod_bytes(&bytes)
}

/// Inspect in-memory `.rmod.tar.gz` bytes and validate `mod-package.json` + `manifest.json`.
pub fn inspect_rmod_bytes(archive_bytes: &[u8]) -> Result<InspectedModPackage, ModManagerError> {
    let sha256 = sha256_hex(archive_bytes);
    let tar_bytes = decompress_gzip(archive_bytes, &ExtractionLimits::default()).map_err(|e| {
        ModManagerError::ArchiveInvalid {
            reason: e.to_string(),
        }
    })?;

    let package_bytes = read_tar_member(
        &tar_bytes,
        "mod-package.json",
        None,
        MAX_MANIFEST_JSON_BYTES,
    )
    .map_err(|e| ModManagerError::ArchiveInvalid {
        reason: e.to_string(),
    })?
    .ok_or_else(|| ModManagerError::ArchiveInvalid {
        reason: "archive missing required `mod-package.json`".to_owned(),
    })?;
    let manifest_bytes =
        read_tar_member(&tar_bytes, "manifest.json", None, MAX_MANIFEST_JSON_BYTES)
            .map_err(|e| ModManagerError::ArchiveInvalid {
                reason: e.to_string(),
            })?
            .ok_or_else(|| ModManagerError::ArchiveInvalid {
                reason: "archive missing required `manifest.json`".to_owned(),
            })?;

    parse_and_validate_metadata(&package_bytes, &manifest_bytes, Some(sha256))
}

/// Inspect an unpacked local mod directory (`mod-package.json` + `manifest.json`).
pub fn inspect_local_directory(dir: &Path) -> Result<InspectedModPackage, ModManagerError> {
    let package_path = dir.join("mod-package.json");
    let manifest_path = dir.join("manifest.json");
    let package_bytes = std::fs::read(&package_path).map_err(|e| ModManagerError::Io {
        path: package_path,
        message: e.to_string(),
    })?;
    let manifest_bytes = std::fs::read(&manifest_path).map_err(|e| ModManagerError::Io {
        path: manifest_path,
        message: e.to_string(),
    })?;
    parse_and_validate_metadata(&package_bytes, &manifest_bytes, None)
}

fn parse_and_validate_metadata(
    package_bytes: &[u8],
    manifest_bytes: &[u8],
    package_sha256: Option<String>,
) -> Result<InspectedModPackage, ModManagerError> {
    let package: ModPackageManifest =
        serde_json::from_slice(package_bytes).map_err(|e| ModManagerError::ArchiveInvalid {
            reason: format!("invalid `mod-package.json`: {e}"),
        })?;
    let manifest: ModManifest =
        serde_json::from_slice(manifest_bytes).map_err(|e| ModManagerError::ArchiveInvalid {
            reason: format!("invalid `manifest.json`: {e}"),
        })?;

    validate_package_against_manifest(&package, &manifest)
        .map_err(|diag| ModManagerError::Diagnostic(Box::new(diag)))?;

    Ok(InspectedModPackage {
        package,
        manifest,
        package_sha256,
    })
}

/// Evaluate origin compatibility, permission expansion, and newly introduced operations
/// against an optional existing installation and bundled mod IDs.
#[must_use]
pub fn evaluate_upgrade(
    existing: Option<&InstalledModRecord>,
    bundled_ids: &BTreeSet<String>,
    incoming: &InspectedModPackage,
    incoming_source: &ModInstallSource,
) -> UpgradeCheck {
    if let Some(record) = existing {
        let source_conflict = if record.source.is_same_origin(incoming_source) {
            None
        } else {
            Some(format!(
                "mod `{}` is already installed from `{}` and cannot be overwritten from `{}`",
                record.manifest.id,
                record.source.label(),
                incoming_source.label()
            ))
        };
        let permission_expansions = incoming
            .package
            .permissions
            .expansion_reasons(&record.granted_permissions);
        let newly_added_operations = newly_added_operations(&record.manifest, &incoming.manifest);
        UpgradeCheck {
            is_upgrade: true,
            source_conflict,
            permission_expansions,
            newly_added_operations,
        }
    } else {
        let source_conflict = if bundled_ids.contains(&incoming.manifest.id)
            && !matches!(incoming_source, ModInstallSource::Official { .. })
        {
            Some(format!(
                "mod `{}` conflicts with bundled mod `{}` and can only be overridden from an official source",
                incoming.manifest.id, incoming.manifest.id
            ))
        } else {
            None
        };
        let newly_added_operations = incoming
            .manifest
            .operations
            .iter()
            .map(|op| op.name.clone())
            .collect();
        UpgradeCheck {
            is_upgrade: false,
            source_conflict,
            permission_expansions: Vec::new(),
            newly_added_operations,
        }
    }
}

/// Build the full user-facing trust preview for a candidate mod package.
#[must_use]
pub fn summarize_trust(
    existing: Option<&InstalledModRecord>,
    bundled_ids: &BTreeSet<String>,
    inspected: InspectedModPackage,
    source: ModInstallSource,
) -> ModTrustSummary {
    let upgrade = evaluate_upgrade(existing, bundled_ids, &inspected, &source);
    let requires_explicit_consent = source.requires_explicit_consent()
        && (!upgrade.is_upgrade || matches!(source, ModInstallSource::LocalArchive { .. }));
    let requires_permission_consent = !upgrade.permission_expansions.is_empty();
    ModTrustSummary {
        inspected,
        source,
        upgrade,
        requires_explicit_consent,
        requires_permission_consent,
    }
}

/// Helper to compare two `ModPermissions` values explicitly.
#[must_use]
pub fn permissions_expanded(old: &ModPermissions, new: &ModPermissions) -> Vec<String> {
    new.expansion_reasons(old)
}
