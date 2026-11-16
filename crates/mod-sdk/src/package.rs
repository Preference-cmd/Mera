//! Packaging metadata, permissions declarations, and upgrade checks for
//! downloadable and third-party mods.

use std::collections::BTreeSet;
use std::path::{Component, Path};

use mera_core::diagnostic::Diagnostic;
use serde::{Deserialize, Serialize};

use crate::diagnostic::caller_error;
use crate::manifest::ModManifest;
use crate::registry::validate_and_compile;
use crate::schema;

/// Current supported `mod-package.json` schema version.
pub const MOD_PACKAGE_SCHEMA_VERSION: u16 = 1;

/// Metadata embedded at the root of a `.rmod.tar.gz` package (`mod-package.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModPackageManifest {
    pub schema_version: u16,
    pub id: String,
    pub version: String,
    /// Target triple (e.g. `"aarch64-apple-darwin"`) or `"noarch"`.
    pub target: String,
    /// Semver requirement on [`crate::API_VERSION`], matching `manifest.json`.
    pub api: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    pub entrypoint: EntrypointSpec,
    #[serde(default)]
    pub permissions: ModPermissions,
}

/// How the host launches a mod package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntrypointSpec {
    Binary { path: String },
}

/// Explicit permission declarations reviewed by the user prior to installation
/// or permission-expanding upgrades.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModPermissions {
    /// Whether any operation accepts a workspace `host-path` input.
    #[serde(default)]
    pub host_paths: bool,
    /// Kernel or mod event patterns the mod intends to observe.
    #[serde(default)]
    pub events_subscribe: Vec<String>,
    /// Event patterns the mod intends to publish.
    #[serde(default)]
    pub events_publish: Vec<String>,
    /// Informational author suggestions for Agent visibility; never grants access implicitly.
    #[serde(default)]
    pub recommended_agent_operations: Vec<String>,
    /// Self-declared network usage notice for user consent.
    #[serde(default)]
    pub network: bool,
    /// Self-declared filesystem usage note for user consent.
    #[serde(default)]
    pub filesystem_note: Option<String>,
}

impl ModPermissions {
    /// Returns human-readable descriptions of any permissions added or broadened
    /// relative to `previous`.
    #[must_use]
    pub fn expansion_reasons(&self, previous: &Self) -> Vec<String> {
        let mut reasons = Vec::new();
        if self.host_paths && !previous.host_paths {
            reasons.push("host_paths access enabled".to_owned());
        }
        if self.network && !previous.network {
            reasons.push("network access enabled".to_owned());
        }

        let old_sub: BTreeSet<&str> = previous
            .events_subscribe
            .iter()
            .map(String::as_str)
            .collect();
        for item in &self.events_subscribe {
            if !old_sub.contains(item.as_str()) {
                reasons.push(format!("subscribes to `{item}`"));
            }
        }

        let old_pub: BTreeSet<&str> = previous.events_publish.iter().map(String::as_str).collect();
        for item in &self.events_publish {
            if !old_pub.contains(item.as_str()) {
                reasons.push(format!("publishes `{item}`"));
            }
        }

        if let Some(note) = &self.filesystem_note
            && previous.filesystem_note.as_deref() != Some(note.as_str())
        {
            reasons.push(format!("filesystem scope changed: {note}"));
        }

        reasons
    }
}

/// Validate `package` metadata alone and against the paired `ModManifest`.
pub fn validate_package_against_manifest(
    package: &ModPackageManifest,
    manifest: &ModManifest,
) -> Result<(), Diagnostic> {
    let id = package.id.as_str();
    let reject = |code: &str, message: String| Err(caller_error(id, code, message));

    if package.schema_version != MOD_PACKAGE_SCHEMA_VERSION {
        return reject(
            "UNSUPPORTED_PACKAGE_VERSION",
            format!(
                "mod `{id}` uses package schema_version {}, expected {MOD_PACKAGE_SCHEMA_VERSION}",
                package.schema_version
            ),
        );
    }
    if package.id != manifest.id {
        return reject(
            "PACKAGE_MANIFEST_MISMATCH",
            format!(
                "package id `{}` disagrees with manifest id `{}`",
                package.id, manifest.id
            ),
        );
    }
    if package.version != manifest.version {
        return reject(
            "PACKAGE_MANIFEST_MISMATCH",
            format!(
                "package version `{}` disagrees with manifest version `{}`",
                package.version, manifest.version
            ),
        );
    }
    if package.api != manifest.api {
        return reject(
            "PACKAGE_MANIFEST_MISMATCH",
            format!(
                "package api `{}` disagrees with manifest api `{}`",
                package.api, manifest.api
            ),
        );
    }
    if package.target.trim().is_empty() {
        return reject(
            "INVALID_TARGET",
            format!("mod `{id}` target must not be empty"),
        );
    }
    if semver::Version::parse(&package.version).is_err()
        || package.version.contains('/')
        || package.version.contains('\\')
        || package.version.contains("..")
    {
        return reject(
            "INVALID_VERSION",
            format!(
                "mod `{id}` version `{}` must be a clean semver string",
                package.version
            ),
        );
    }

    match &package.entrypoint {
        EntrypointSpec::Binary { path } => validate_entrypoint_path(id, path)?,
    }

    validate_and_compile(manifest, false)?;

    let schema_uses_host_path = manifest
        .operations
        .iter()
        .any(|op| schema::has_host_path_field(&op.input_schema));
    if schema_uses_host_path && !package.permissions.host_paths {
        return reject(
            "UNDECLARED_PERMISSION",
            format!(
                "mod `{id}` declares an operation with format `host-path` without `permissions.host_paths = true`"
            ),
        );
    }

    Ok(())
}

/// Return operation names introduced in `new_manifest` that did not exist in `old_manifest`.
#[must_use]
pub fn newly_added_operations(
    old_manifest: &ModManifest,
    new_manifest: &ModManifest,
) -> Vec<String> {
    let old_ops: BTreeSet<&str> = old_manifest
        .operations
        .iter()
        .map(|op| op.name.as_str())
        .collect();
    new_manifest
        .operations
        .iter()
        .filter(|op| !old_ops.contains(op.name.as_str()))
        .map(|op| op.name.clone())
        .collect()
}

fn validate_entrypoint_path(id: &str, path: &str) -> Result<(), Diagnostic> {
    let p = Path::new(path);
    if path.is_empty()
        || p.is_absolute()
        || p.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(caller_error(
            id,
            "INVALID_ENTRYPOINT",
            format!("mod `{id}` entrypoint `{path}` must be a clean relative path"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{OperationAnnotations, OperationSpec};

    fn base_manifest(with_host_path: bool) -> ModManifest {
        let input_schema = if with_host_path {
            json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string", "format": "host-path" }
                }
            })
        } else {
            json!({
                "type": "object",
                "properties": { "text": { "type": "string" } }
            })
        };
        ModManifest {
            id: "sample_ext".to_owned(),
            name: "Sample Ext".to_owned(),
            version: "0.1.0".to_owned(),
            api: "0.1".to_owned(),
            operations: vec![OperationSpec {
                name: "sample_ext.run".to_owned(),
                description: "run".to_owned(),
                input_schema,
                output_schema: json!({ "type": "object" }),
                annotations: OperationAnnotations::default(),
            }],
            events: Vec::new(),
        }
    }

    fn base_package(host_paths: bool) -> ModPackageManifest {
        ModPackageManifest {
            schema_version: 1,
            id: "sample_ext".to_owned(),
            version: "0.1.0".to_owned(),
            target: "aarch64-apple-darwin".to_owned(),
            api: "0.1".to_owned(),
            author: Some("Community".to_owned()),
            homepage: None,
            license: Some("MIT".to_owned()),
            entrypoint: EntrypointSpec::Binary {
                path: "bin/sample-ext".to_owned(),
            },
            permissions: ModPermissions {
                host_paths,
                ..Default::default()
            },
        }
    }

    #[test]
    fn rejects_undeclared_host_path_permission() {
        let manifest = base_manifest(true);
        let package = base_package(false);
        let err = validate_package_against_manifest(&package, &manifest).unwrap_err();
        assert_eq!(err.code().as_str(), "MOD/CALLER_UNDECLARED_PERMISSION");
    }

    #[test]
    fn accepts_declared_host_path_permission() {
        let manifest = base_manifest(true);
        let package = base_package(true);
        validate_package_against_manifest(&package, &manifest).unwrap();
    }

    #[test]
    fn rejects_path_traversal_in_version() {
        let mut manifest = base_manifest(false);
        manifest.version = "../../../../tmp/evil".to_owned();
        let mut package = base_package(false);
        package.version = manifest.version.clone();
        let err = validate_package_against_manifest(&package, &manifest).unwrap_err();
        assert_eq!(err.code().as_str(), "MOD/CALLER_INVALID_VERSION");
    }

    #[test]
    fn detects_permission_expansion_on_upgrade() {
        let old = ModPermissions::default();
        let new = ModPermissions {
            host_paths: true,
            network: true,
            events_subscribe: vec!["board.*".to_owned()],
            ..Default::default()
        };
        let reasons = new.expansion_reasons(&old);
        assert_eq!(reasons.len(), 3);
    }

    #[test]
    fn detects_newly_added_operations() {
        let old = base_manifest(false);
        let mut new = base_manifest(false);
        new.operations.push(OperationSpec {
            name: "sample_ext.extra".to_owned(),
            description: "extra".to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            annotations: OperationAnnotations::default(),
        });
        assert_eq!(
            newly_added_operations(&old, &new),
            vec!["sample_ext.extra".to_owned()]
        );
    }
}
