use mera_core::health::Health;
use mod_sdk::package::ModPermissions;
use mod_sdk::{ModEntry, OperationSpec, has_host_path_field};
use serde::Serialize;

use crate::mod_manager::{
    InstalledModInfo, InstalledModRecord, ModInstallSource, ModTrustSummary, ModVisibilityTable,
};
use crate::operation_policy::agent_may_invoke;

/// Installation source presented to desktop UI clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModSourceDto {
    Bundled,
    Official { catalog_url: String },
    ThirdPartyUrl { source_url: String },
    LocalArchive { filename: String },
    LocalDirectory { path: String },
}

impl From<&ModInstallSource> for ModSourceDto {
    fn from(source: &ModInstallSource) -> Self {
        match source {
            ModInstallSource::Official { catalog_url } => Self::Official {
                catalog_url: catalog_url.clone(),
            },
            ModInstallSource::ThirdPartyUrl { source_url } => Self::ThirdPartyUrl {
                source_url: source_url.clone(),
            },
            ModInstallSource::LocalArchive { filename } => Self::LocalArchive {
                filename: filename.clone(),
            },
            ModInstallSource::LocalDirectory { path } => Self::LocalDirectory {
                path: path.display().to_string(),
            },
        }
    }
}

/// Summary of a single operation declared by a mod, including capability/risk markers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModOperationSummaryDto {
    pub name: String,
    pub description: String,
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub deterministic: bool,
    pub open_world: bool,
    pub uses_host_path: bool,
    pub recommended_for_agent: bool,
}

impl ModOperationSummaryDto {
    fn from_spec(op: &OperationSpec, recommended_ops: &[String]) -> Self {
        Self {
            name: op.name.clone(),
            description: op.description.clone(),
            read_only: op.annotations.read_only,
            destructive: op.annotations.destructive,
            idempotent: op.annotations.idempotent,
            deterministic: op.annotations.deterministic,
            open_world: op.annotations.open_world,
            uses_host_path: has_host_path_field(&op.input_schema),
            recommended_for_agent: recommended_ops.iter().any(|name| name == &op.name),
        }
    }
}

/// Availability status and optional diagnostic for a mod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModHealthDto {
    pub status: String,
    pub diagnostic_code: Option<String>,
    pub message: Option<String>,
}

impl From<&Health> for ModHealthDto {
    fn from(health: &Health) -> Self {
        match health {
            Health::Available => Self {
                status: "available".to_owned(),
                diagnostic_code: None,
                message: None,
            },
            Health::Unavailable(diag) => Self {
                status: "unavailable".to_owned(),
                diagnostic_code: Some(diag.code().as_str().to_owned()),
                message: Some(diag.message().to_owned()),
            },
        }
    }
}

/// Combined view of a bundled or managed mod returned to the UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModEntryDto {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api: String,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub source: ModSourceDto,
    pub install_dir: Option<String>,
    pub package_sha256: Option<String>,
    pub installed_at: Option<String>,
    pub enabled: bool,
    pub read_only_bundled: bool,
    pub health: ModHealthDto,
    pub permissions: ModPermissions,
    pub operations: Vec<ModOperationSummaryDto>,
    pub agent_visible_operations: Vec<String>,
}

impl ModEntryDto {
    /// Construct a read-only entry for a first-party bundled mod running directly in [`mod_sdk::ModRegistry`].
    #[must_use]
    pub fn from_bundled_entry(entry: &ModEntry) -> Self {
        let operations: Vec<ModOperationSummaryDto> = entry
            .manifest
            .operations
            .iter()
            .map(|op| ModOperationSummaryDto::from_spec(op, &[]))
            .collect();
        let host_paths = operations.iter().any(|op| op.uses_host_path);
        let agent_visible_operations: Vec<String> = entry
            .manifest
            .operations
            .iter()
            .filter(|op| agent_may_invoke(&op.name))
            .map(|op| op.name.clone())
            .collect();
        Self {
            id: entry.manifest.id.clone(),
            name: entry.manifest.name.clone(),
            version: entry.manifest.version.clone(),
            api: entry.manifest.api.clone(),
            author: None,
            homepage: None,
            license: None,
            source: ModSourceDto::Bundled,
            install_dir: None,
            package_sha256: None,
            installed_at: None,
            enabled: true,
            read_only_bundled: true,
            health: ModHealthDto::from(&entry.health),
            permissions: ModPermissions {
                host_paths,
                events_subscribe: Vec::new(),
                events_publish: entry
                    .manifest
                    .events
                    .iter()
                    .map(|event| event.kind.clone())
                    .collect(),
                recommended_agent_operations: Vec::new(),
                network: false,
                filesystem_note: None,
            },
            operations,
            agent_visible_operations,
        }
    }

    /// Construct an entry from a managed installation record and live health.
    #[must_use]
    pub fn from_installed(info: InstalledModInfo) -> Self {
        let record = info.record;
        let recommended = &record.package.permissions.recommended_agent_operations;
        let operations = record
            .manifest
            .operations
            .iter()
            .map(|op| ModOperationSummaryDto::from_spec(op, recommended))
            .collect();
        Self {
            id: record.manifest.id,
            name: record.manifest.name,
            version: record.manifest.version,
            api: record.manifest.api,
            author: record.package.author,
            homepage: record.package.homepage,
            license: record.package.license,
            source: ModSourceDto::from(&record.source),
            install_dir: Some(record.install_dir.display().to_string()),
            package_sha256: record.package_sha256,
            installed_at: Some(record.installed_at),
            enabled: record.enabled,
            read_only_bundled: false,
            health: ModHealthDto::from(&info.health),
            permissions: record.package.permissions,
            operations,
            agent_visible_operations: record.agent_visible_operations.into_iter().collect(),
        }
    }
}

/// Top-level payload returned by `mods_list`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModListResponseDto {
    pub mods_root: String,
    pub mods: Vec<ModEntryDto>,
}

/// Pre-installation trust, compatibility, and Agent visibility preview returned before consent.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModInstallPreviewDto {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api: String,
    pub target: String,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub source: ModSourceDto,
    pub package_sha256: Option<String>,
    pub permissions: ModPermissions,
    pub operations: Vec<ModOperationSummaryDto>,
    pub is_upgrade: bool,
    pub source_conflict: Option<String>,
    pub permission_expansions: Vec<String>,
    pub newly_added_operations: Vec<String>,
    pub existing_agent_visible_operations: Vec<String>,
    pub requires_explicit_consent: bool,
    pub requires_permission_consent: bool,
}

impl ModInstallPreviewDto {
    /// Convert a static trust evaluation and optional existing record into a UI preview payload.
    #[must_use]
    pub fn from_trust_summary(
        summary: ModTrustSummary,
        existing: Option<&InstalledModRecord>,
    ) -> Self {
        let inspected = summary.inspected;
        let recommended = &inspected.package.permissions.recommended_agent_operations;
        let operations = inspected
            .manifest
            .operations
            .iter()
            .map(|op| ModOperationSummaryDto::from_spec(op, recommended))
            .collect();
        let existing_agent_visible_operations: Vec<String> = existing
            .map(|rec| {
                ModVisibilityTable::retain_declared_operations(
                    &rec.agent_visible_operations,
                    &inspected.manifest,
                )
            })
            .unwrap_or_default()
            .into_iter()
            .collect();

        Self {
            id: inspected.manifest.id,
            name: inspected.manifest.name,
            version: inspected.manifest.version,
            api: inspected.manifest.api,
            target: inspected.package.target,
            author: inspected.package.author,
            homepage: inspected.package.homepage,
            license: inspected.package.license,
            source: ModSourceDto::from(&summary.source),
            package_sha256: inspected.package_sha256,
            permissions: inspected.package.permissions,
            operations,
            is_upgrade: summary.upgrade.is_upgrade,
            source_conflict: summary.upgrade.source_conflict,
            permission_expansions: summary.upgrade.permission_expansions,
            newly_added_operations: summary.upgrade.newly_added_operations,
            existing_agent_visible_operations,
            requires_explicit_consent: summary.requires_explicit_consent,
            requires_permission_consent: summary.requires_permission_consent,
        }
    }
}
