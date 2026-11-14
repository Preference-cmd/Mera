//! Runtime mod installer, lifecycle service, and hot-swap coordinator.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::Utc;
use mod_sdk::package::{EntrypointSpec, validate_package_against_manifest};
use mod_sdk::{Invocation, Mod, ModContext, ModManifest, ModRegistry, ProcessMod, ProcessModSpec};
use reimagine_core::diagnostic::Diagnostic;
use reimagine_core::event::{Event, EventBus};
use reimagine_core::health::Health;
use reimagine_package_installer::{
    ExtractionLimits, FsLock, atomic_promote_dir, extract_tar_gz_safely, validate_relative_path,
};
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;

use super::inventory::{
    InstalledModInfo, InstalledModRecord, ModInstallSource, ModInventory, ModInventoryStore,
    ModStorePaths, ModVisibilityTable, mod_unavailable_diagnostic,
};
use super::trust::{
    InspectedModPackage, ModTrustSummary, inspect_local_directory, inspect_rmod_archive,
    inspect_rmod_bytes, summarize_trust,
};

static STAGING_NONCE: AtomicU64 = AtomicU64::new(1);

/// Failures returned by [`ModManagerService`].
#[derive(Debug)]
pub enum ModManagerError {
    ConsentRequired {
        mod_id: String,
        source: String,
    },
    SourceConflict {
        mod_id: String,
        existing_source: String,
        incoming_source: String,
    },
    PermissionExpansionRequiresConsent {
        mod_id: String,
        reasons: Vec<String>,
    },
    ManifestMismatch {
        mod_id: String,
        reason: String,
    },
    SelfCheckFailed {
        mod_id: String,
        reason: String,
    },
    ArchiveInvalid {
        reason: String,
    },
    UnknownMod {
        mod_id: String,
    },
    UnknownOperation {
        mod_id: String,
        operation: String,
    },
    Busy {
        message: String,
    },
    Io {
        path: PathBuf,
        message: String,
    },
    Diagnostic(Box<Diagnostic>),
}

impl fmt::Display for ModManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConsentRequired { mod_id, source } => write!(
                f,
                "explicit user consent is required to install `{mod_id}` from `{source}`"
            ),
            Self::SourceConflict {
                mod_id,
                existing_source,
                incoming_source,
            } => write!(
                f,
                "mod `{mod_id}` is already installed from `{existing_source}` and cannot be overwritten from `{incoming_source}`"
            ),
            Self::PermissionExpansionRequiresConsent { mod_id, reasons } => write!(
                f,
                "upgrading mod `{mod_id}` expands permissions and requires confirmation: {}",
                reasons.join("; ")
            ),
            Self::ManifestMismatch { mod_id, reason } => {
                write!(f, "mod `{mod_id}` manifest mismatch: {reason}")
            }
            Self::SelfCheckFailed { mod_id, reason } => {
                write!(f, "mod `{mod_id}` self-check failed: {reason}")
            }
            Self::ArchiveInvalid { reason } => write!(f, "invalid mod package archive: {reason}"),
            Self::UnknownMod { mod_id } => write!(f, "unknown installed mod `{mod_id}`"),
            Self::UnknownOperation { mod_id, operation } => write!(
                f,
                "mod `{mod_id}` declares no operation named `{operation}`"
            ),
            Self::Busy { message } => write!(f, "mod manager busy: {message}"),
            Self::Io { path, message } => {
                write!(f, "I/O error at `{}`: {message}", path.display())
            }
            Self::Diagnostic(diag) => write!(f, "{}", diag.message()),
        }
    }
}

impl std::error::Error for ModManagerError {}

/// Options controlling consent, activation, and initial per-operation Agent visibility.
#[derive(Debug, Clone, Default)]
pub struct InstallModRequest {
    /// Required when `source` is third-party or local.
    pub consent_granted: bool,
    /// Required when upgrading a mod whose declared permissions expanded.
    pub consent_permission_expansion: bool,
    /// Whether the installed mod should immediately be started and active.
    pub enabled: bool,
    /// Explicit per-operation Agent allow-list (`<mod_id>.<op>`).
    ///
    /// On a fresh install, `None` leaves ALL operations hidden from Agent callers.
    /// On an upgrade, `None` preserves previously granted operations that still
    /// exist in the updated manifest while keeping newly added operations hidden.
    pub agent_visible_operations: Option<BTreeSet<String>>,
}

impl InstallModRequest {
    #[must_use]
    pub fn with_consent(mut self) -> Self {
        self.consent_granted = true;
        self
    }

    #[must_use]
    pub fn enable(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    #[must_use]
    pub fn consent_permission_expansion(mut self, consented: bool) -> Self {
        self.consent_permission_expansion = consented;
        self
    }

    #[must_use]
    pub fn agent_visible_operations(mut self, operations: BTreeSet<String>) -> Self {
        self.agent_visible_operations = Some(operations);
        self
    }
}

struct StagingGuard {
    dir: Option<PathBuf>,
}

impl StagingGuard {
    fn new(dir: PathBuf) -> Self {
        Self { dir: Some(dir) }
    }

    fn path(&self) -> &Path {
        self.dir.as_deref().expect("staging guard still active")
    }

    fn defuse(&mut self) {
        self.dir = None;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Placeholder [`Mod`] implementation used when an enabled installed mod fails
/// to spawn at startup or during enable so its operations stay surfaced as `Unavailable`.
struct UnavailableStubMod {
    manifest: ModManifest,
    diagnostic: Diagnostic,
}

#[async_trait]
impl Mod for UnavailableStubMod {
    fn manifest(&self) -> &ModManifest {
        &self.manifest
    }

    async fn start(&self, _ctx: ModContext) -> Result<(), Diagnostic> {
        Err(self.diagnostic.clone())
    }

    async fn invoke(&self, _invocation: Invocation) -> Result<Value, Diagnostic> {
        Err(self.diagnostic.clone())
    }

    async fn stop(&self) -> Result<(), Diagnostic> {
        Ok(())
    }
}

/// Application service managing installed mods, staging verification, persistence,
/// hot-swapping in [`ModRegistry`], and per-operation Agent visibility.
pub struct ModManagerService {
    store: ModInventoryStore,
    registry: Arc<ModRegistry>,
    visibility: Arc<ModVisibilityTable>,
    event_bus: EventBus,
    bundled_ids: RwLock<BTreeSet<String>>,
    inventory: RwLock<ModInventory>,
    op_mutex: AsyncMutex<()>,
}

impl std::fmt::Debug for ModManagerService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModManagerService")
            .field("mods_root", &self.store.paths().mods_root())
            .finish_non_exhaustive()
    }
}

impl ModManagerService {
    /// Construct a manager anchored at `paths`, hydrating any existing `inventory.json`.
    pub fn new(
        paths: ModStorePaths,
        registry: Arc<ModRegistry>,
        visibility: Arc<ModVisibilityTable>,
        event_bus: EventBus,
    ) -> Result<Self, ModManagerError> {
        let store = ModInventoryStore::new(paths);
        let inventory = store.load().map_err(|e| ModManagerError::Io {
            path: store.paths().inventory_file(),
            message: e.to_string(),
        })?;
        visibility.sync_from_inventory(&inventory);
        Ok(Self {
            store,
            registry,
            visibility,
            event_bus,
            bundled_ids: RwLock::new(BTreeSet::new()),
            inventory: RwLock::new(inventory),
            op_mutex: AsyncMutex::new(()),
        })
    }

    /// Record bundled mod IDs (such as `"text"`) so non-official installs cannot
    /// collide silently with first-party bundled mods.
    pub fn register_bundled_mod_id(&self, mod_id: impl Into<String>) {
        self.bundled_ids
            .write()
            .expect("bundled_ids poisoned")
            .insert(mod_id.into());
    }

    /// Access the shared [`ModVisibilityTable`] wired into [`crate::OperationService`].
    #[must_use]
    pub fn visibility_table(&self) -> Arc<ModVisibilityTable> {
        Arc::clone(&self.visibility)
    }

    /// Access the underlying [`ModStorePaths`].
    #[must_use]
    pub fn paths(&self) -> &ModStorePaths {
        self.store.paths()
    }

    /// List all installed mods from `inventory.json` joined with live registry health.
    #[must_use]
    pub fn list_installed(&self) -> Vec<InstalledModInfo> {
        let registry_health: std::collections::HashMap<String, Health> = self
            .registry
            .list()
            .into_iter()
            .map(|entry| (entry.manifest.id.clone(), entry.health))
            .collect();
        let inventory = self.inventory.read().expect("inventory lock poisoned");
        inventory
            .mods
            .values()
            .cloned()
            .map(|record| {
                let health = registry_health.get(&record.manifest.id).cloned().unwrap_or(
                    if record.enabled {
                        Health::Unavailable(mod_unavailable_diagnostic(
                            &record.manifest.id,
                            "NOT_LOADED",
                            format!("mod `{}` is enabled but not loaded", record.manifest.id),
                        ))
                    } else {
                        Health::Unavailable(mod_unavailable_diagnostic(
                            &record.manifest.id,
                            "DISABLED",
                            format!("mod `{}` is disabled", record.manifest.id),
                        ))
                    },
                );
                InstalledModInfo { record, health }
            })
            .collect()
    }

    /// Preview archive metadata, origin conflicts, permission expansions, and new operations
    /// without extracting files or running any mod code.
    pub fn preview_archive_file(
        &self,
        archive_path: &Path,
        source: ModInstallSource,
    ) -> Result<ModTrustSummary, ModManagerError> {
        let inspected = inspect_rmod_archive(archive_path)?;
        Ok(self.build_trust_summary(inspected, source))
    }

    /// Preview in-memory `.rmod.tar.gz` bytes without modifying disk state.
    pub fn preview_archive_bytes(
        &self,
        archive_bytes: &[u8],
        source: ModInstallSource,
    ) -> Result<ModTrustSummary, ModManagerError> {
        let inspected = inspect_rmod_bytes(archive_bytes)?;
        Ok(self.build_trust_summary(inspected, source))
    }

    /// Preview an unpacked local mod directory without launching its binary.
    pub fn preview_local_directory(&self, dir: &Path) -> Result<ModTrustSummary, ModManagerError> {
        let inspected = inspect_local_directory(dir)?;
        let source = ModInstallSource::LocalDirectory {
            path: dir.to_path_buf(),
        };
        Ok(self.build_trust_summary(inspected, source))
    }

    fn build_trust_summary(
        &self,
        inspected: InspectedModPackage,
        source: ModInstallSource,
    ) -> ModTrustSummary {
        let inventory = self.inventory.read().expect("inventory lock poisoned");
        let existing = inventory.mods.get(&inspected.manifest.id);
        let bundled = self.bundled_ids.read().expect("bundled_ids poisoned");
        summarize_trust(existing, &bundled, inspected, source)
    }

    /// Install or upgrade a `.rmod.tar.gz` file on disk.
    pub async fn install_archive_file(
        &self,
        archive_path: &Path,
        source: ModInstallSource,
        request: InstallModRequest,
    ) -> Result<InstalledModInfo, ModManagerError> {
        let bytes = std::fs::read(archive_path).map_err(|e| ModManagerError::Io {
            path: archive_path.to_path_buf(),
            message: e.to_string(),
        })?;
        self.install_archive_bytes(&bytes, source, request).await
    }

    /// Install or upgrade a mod from `.rmod.tar.gz` bytes.
    pub async fn install_archive_bytes(
        &self,
        archive_bytes: &[u8],
        source: ModInstallSource,
        request: InstallModRequest,
    ) -> Result<InstalledModInfo, ModManagerError> {
        let _guard = self.op_mutex.lock().await;
        let _fs_lock = FsLock::acquire(&self.store.paths().lock_file()).map_err(|e| {
            ModManagerError::Busy {
                message: e.to_string(),
            }
        })?;

        let inspected = inspect_rmod_bytes(archive_bytes)?;
        let (existing, next_visible) =
            self.validate_install_preconditions(&inspected, &source, &request)?;

        let nonce = STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
        let staging_dir = self
            .store
            .paths()
            .staging_root()
            .join(format!("{}-{nonce}", inspected.manifest.id));
        std::fs::create_dir_all(&staging_dir).map_err(|e| ModManagerError::Io {
            path: staging_dir.clone(),
            message: e.to_string(),
        })?;
        let mut staging_guard = StagingGuard::new(staging_dir);

        extract_tar_gz_safely(
            archive_bytes,
            staging_guard.path(),
            &ExtractionLimits::default(),
            &[],
            None,
        )
        .map_err(|e| ModManagerError::ArchiveInvalid {
            reason: e.to_string(),
        })?;

        let rel_entry = match &inspected.package.entrypoint {
            EntrypointSpec::Binary { path } => path.as_str(),
        };
        self.run_self_check(&inspected, staging_guard.path(), rel_entry)
            .await?;

        let target_dir = self
            .store
            .paths()
            .package_version_dir(&inspected.manifest.id, &inspected.manifest.version);
        atomic_promote_dir(staging_guard.path(), &target_dir).map_err(|e| ModManagerError::Io {
            path: target_dir.clone(),
            message: e.to_string(),
        })?;
        staging_guard.defuse();

        self.finalize_installation(
            inspected,
            source,
            target_dir,
            next_visible,
            request.enabled,
            existing.is_some(),
        )
        .await
    }

    /// Mount and activate a local unpacked mod directory (primarily for mod development).
    pub async fn install_local_directory(
        &self,
        dir: &Path,
        request: InstallModRequest,
    ) -> Result<InstalledModInfo, ModManagerError> {
        let _guard = self.op_mutex.lock().await;
        let _fs_lock = FsLock::acquire(&self.store.paths().lock_file()).map_err(|e| {
            ModManagerError::Busy {
                message: e.to_string(),
            }
        })?;

        let canonical_dir = std::fs::canonicalize(dir).map_err(|e| ModManagerError::Io {
            path: dir.to_path_buf(),
            message: e.to_string(),
        })?;
        let source = ModInstallSource::LocalDirectory {
            path: canonical_dir.clone(),
        };
        let inspected = inspect_local_directory(&canonical_dir)?;
        let (existing, next_visible) =
            self.validate_install_preconditions(&inspected, &source, &request)?;

        let rel_entry = match &inspected.package.entrypoint {
            EntrypointSpec::Binary { path } => path.as_str(),
        };
        self.run_self_check(&inspected, &canonical_dir, rel_entry)
            .await?;

        self.finalize_installation(
            inspected,
            source,
            canonical_dir,
            next_visible,
            request.enabled,
            existing.is_some(),
        )
        .await
    }

    /// Startup hook: cleans leftover staging directories, populates [`ModVisibilityTable`],
    /// and starts every enabled installed mod into [`ModRegistry`].
    pub async fn load_enabled_mods_at_startup(&self) -> Vec<InstalledModInfo> {
        let _guard = self.op_mutex.lock().await;
        self.cleanup_stale_staging();

        let records: Vec<InstalledModRecord> = {
            let inventory = self.inventory.read().expect("inventory lock poisoned");
            self.visibility.sync_from_inventory(&inventory);
            inventory
                .mods
                .values()
                .filter(|r| r.enabled)
                .cloned()
                .collect()
        };

        for record in records {
            let instance = self.spawn_record_or_unavailable(&record).await;
            let mod_id = record.manifest.id.clone();
            let entry = self
                .register_or_replace_in_registry(&mod_id, instance)
                .await
                .unwrap_or_else(|_| mod_sdk::ModEntry {
                    manifest: Arc::new(record.manifest.clone()),
                    health: Health::Unavailable(mod_unavailable_diagnostic(
                        &mod_id,
                        "STARTUP_FAILED",
                        format!("failed to register mod `{mod_id}` at startup"),
                    )),
                });
            if let Health::Unavailable(diag) = entry.health {
                self.event_bus.publish(Event::mod_event(
                    "mod.health_changed",
                    json!({
                        "modId": mod_id,
                        "available": false,
                        "diagnostic": diag.message(),
                    }),
                ));
            }
        }

        self.list_installed()
    }

    /// Enable or disable an installed mod without restarting the desktop app.
    pub async fn set_enabled(
        &self,
        mod_id: &str,
        enabled: bool,
    ) -> Result<InstalledModInfo, ModManagerError> {
        let _guard = self.op_mutex.lock().await;
        let _fs_lock = FsLock::acquire(&self.store.paths().lock_file()).map_err(|e| {
            ModManagerError::Busy {
                message: e.to_string(),
            }
        })?;

        let mut next_inventory = self
            .inventory
            .read()
            .expect("inventory lock poisoned")
            .clone();
        let record =
            next_inventory
                .mods
                .get_mut(mod_id)
                .ok_or_else(|| ModManagerError::UnknownMod {
                    mod_id: mod_id.to_owned(),
                })?;
        record.enabled = enabled;
        let record_snapshot = record.clone();

        self.store
            .save(&next_inventory)
            .map_err(|msg| ModManagerError::Io {
                path: self.store.paths().inventory_file(),
                message: msg,
            })?;
        *self.inventory.write().expect("inventory lock poisoned") = next_inventory;

        let health = if enabled {
            let instance = self.spawn_record_or_unavailable(&record_snapshot).await;
            let entry = self
                .register_or_replace_in_registry(mod_id, instance)
                .await
                .map_err(|d| ModManagerError::Diagnostic(Box::new(d)))?;
            self.event_bus
                .publish(Event::mod_event("mod.enabled", json!({ "modId": mod_id })));
            if let Health::Unavailable(diag) = &entry.health {
                self.event_bus.publish(Event::mod_event(
                    "mod.health_changed",
                    json!({
                        "modId": mod_id,
                        "available": false,
                        "diagnostic": diag.message(),
                    }),
                ));
            }
            entry.health
        } else {
            let _ = self.registry.unregister_and_stop(mod_id).await;
            self.event_bus
                .publish(Event::mod_event("mod.disabled", json!({ "modId": mod_id })));
            Health::Unavailable(mod_unavailable_diagnostic(
                mod_id,
                "DISABLED",
                format!("mod `{mod_id}` is disabled"),
            ))
        };

        Ok(InstalledModInfo {
            record: record_snapshot,
            health,
        })
    }

    /// Explicitly update the per-operation Agent visibility allow-list (`<mod_id>.<op>`).
    pub async fn set_agent_visible_operations(
        &self,
        mod_id: &str,
        operations: BTreeSet<String>,
    ) -> Result<InstalledModInfo, ModManagerError> {
        let _guard = self.op_mutex.lock().await;
        let _fs_lock = FsLock::acquire(&self.store.paths().lock_file()).map_err(|e| {
            ModManagerError::Busy {
                message: e.to_string(),
            }
        })?;

        let mut next_inventory = self
            .inventory
            .read()
            .expect("inventory lock poisoned")
            .clone();
        let record =
            next_inventory
                .mods
                .get_mut(mod_id)
                .ok_or_else(|| ModManagerError::UnknownMod {
                    mod_id: mod_id.to_owned(),
                })?;
        validate_requested_operations(&operations, &record.manifest)?;
        record.agent_visible_operations = operations.clone();
        let record_snapshot = record.clone();

        self.store
            .save(&next_inventory)
            .map_err(|msg| ModManagerError::Io {
                path: self.store.paths().inventory_file(),
                message: msg,
            })?;
        *self.inventory.write().expect("inventory lock poisoned") = next_inventory;
        self.visibility
            .set_mod_visibility(mod_id, operations.clone());

        self.event_bus.publish(Event::mod_event(
            "mod.visibility_changed",
            json!({
                "modId": mod_id,
                "agentVisibleOperations": operations,
            }),
        ));

        let health = self
            .registry
            .list()
            .into_iter()
            .find(|entry| entry.manifest.id == mod_id)
            .map(|entry| entry.health)
            .unwrap_or_else(|| {
                Health::Unavailable(mod_unavailable_diagnostic(
                    mod_id,
                    "DISABLED",
                    format!("mod `{mod_id}` is disabled"),
                ))
            });
        Ok(InstalledModInfo {
            record: record_snapshot,
            health,
        })
    }

    /// Stop and remove an installed mod from `ModRegistry`, `inventory.json`, and optionally
    /// delete its extracted package directory under `<mods_root>/packages/<mod_id>`.
    pub async fn uninstall(&self, mod_id: &str, remove_files: bool) -> Result<(), ModManagerError> {
        let _guard = self.op_mutex.lock().await;
        let _fs_lock = FsLock::acquire(&self.store.paths().lock_file()).map_err(|e| {
            ModManagerError::Busy {
                message: e.to_string(),
            }
        })?;

        let mut next_inventory = self
            .inventory
            .read()
            .expect("inventory lock poisoned")
            .clone();
        let removed =
            next_inventory
                .mods
                .remove(mod_id)
                .ok_or_else(|| ModManagerError::UnknownMod {
                    mod_id: mod_id.to_owned(),
                })?;

        let _ = self.registry.unregister_and_stop(mod_id).await;

        self.store
            .save(&next_inventory)
            .map_err(|msg| ModManagerError::Io {
                path: self.store.paths().inventory_file(),
                message: msg,
            })?;
        *self.inventory.write().expect("inventory lock poisoned") = next_inventory;
        self.visibility.remove_mod(mod_id);

        if remove_files && !matches!(removed.source, ModInstallSource::LocalDirectory { .. }) {
            let pkg_root = self.store.paths().mod_package_root(mod_id);
            if pkg_root.exists() {
                let _ = std::fs::remove_dir_all(pkg_root);
            }
        }

        self.event_bus.publish(Event::mod_event(
            "mod.uninstalled",
            json!({ "modId": mod_id }),
        ));
        Ok(())
    }

    fn validate_install_preconditions(
        &self,
        inspected: &InspectedModPackage,
        source: &ModInstallSource,
        request: &InstallModRequest,
    ) -> Result<(Option<InstalledModRecord>, BTreeSet<String>), ModManagerError> {
        validate_package_against_manifest(&inspected.package, &inspected.manifest)
            .map_err(|d| ModManagerError::Diagnostic(Box::new(d)))?;

        validate_relative_path(&inspected.manifest.id).map_err(|e| {
            ModManagerError::ArchiveInvalid {
                reason: format!("unsafe mod id path `{}`: {e}", inspected.manifest.id),
            }
        })?;
        validate_relative_path(&inspected.manifest.version).map_err(|e| {
            ModManagerError::ArchiveInvalid {
                reason: format!(
                    "unsafe mod version path `{}`: {e}",
                    inspected.manifest.version
                ),
            }
        })?;

        if let Some(requested_ops) = &request.agent_visible_operations {
            validate_requested_operations(requested_ops, &inspected.manifest)?;
        }

        let existing = self
            .inventory
            .read()
            .expect("inventory lock poisoned")
            .mods
            .get(&inspected.manifest.id)
            .cloned();
        let bundled = self.bundled_ids.read().expect("bundled_ids poisoned");
        let summary = summarize_trust(
            existing.as_ref(),
            &bundled,
            inspected.clone(),
            source.clone(),
        );
        drop(bundled);

        if let Some(conflict) = summary.upgrade.source_conflict {
            let existing_source = existing
                .as_ref()
                .map(|r| r.source.label())
                .unwrap_or_else(|| "bundled".to_owned());
            return Err(ModManagerError::SourceConflict {
                mod_id: inspected.manifest.id.clone(),
                existing_source,
                incoming_source: conflict,
            });
        }
        if summary.requires_explicit_consent && !request.consent_granted {
            return Err(ModManagerError::ConsentRequired {
                mod_id: inspected.manifest.id.clone(),
                source: source.label(),
            });
        }
        if summary.requires_permission_consent && !request.consent_permission_expansion {
            return Err(ModManagerError::PermissionExpansionRequiresConsent {
                mod_id: inspected.manifest.id.clone(),
                reasons: summary.upgrade.permission_expansions,
            });
        }

        let next_visible = match (&request.agent_visible_operations, &existing) {
            (Some(explicit), _) => {
                ModVisibilityTable::retain_declared_operations(explicit, &inspected.manifest)
            }
            (None, Some(prev)) => ModVisibilityTable::retain_declared_operations(
                &prev.agent_visible_operations,
                &inspected.manifest,
            ),
            (None, None) => BTreeSet::new(),
        };

        Ok((existing, next_visible))
    }

    async fn run_self_check(
        &self,
        inspected: &InspectedModPackage,
        package_root: &Path,
        rel_entrypoint: &str,
    ) -> Result<(), ModManagerError> {
        let entrypoint_path =
            resolve_entrypoint_path(&inspected.manifest.id, package_root, rel_entrypoint)?;
        ensure_executable_bit(&entrypoint_path)?;

        let probe = ProcessMod::spawn(ProcessModSpec::new(&entrypoint_path))
            .await
            .map_err(|diag| ModManagerError::SelfCheckFailed {
                mod_id: inspected.manifest.id.clone(),
                reason: diag.message().to_owned(),
            })?;
        let live_manifest = probe.manifest().clone();
        let _ = probe.stop().await;

        if live_manifest != inspected.manifest {
            return Err(ModManagerError::ManifestMismatch {
                mod_id: inspected.manifest.id.clone(),
                reason: format!(
                    "binary initialize manifest ({}@{} with {} operations) disagrees with static manifest.json ({}@{} with {} operations)",
                    live_manifest.id,
                    live_manifest.version,
                    live_manifest.operations.len(),
                    inspected.manifest.id,
                    inspected.manifest.version,
                    inspected.manifest.operations.len(),
                ),
            });
        }
        Ok(())
    }

    async fn finalize_installation(
        &self,
        inspected: InspectedModPackage,
        source: ModInstallSource,
        install_dir: PathBuf,
        next_visible: BTreeSet<String>,
        enabled: bool,
        is_upgrade: bool,
    ) -> Result<InstalledModInfo, ModManagerError> {
        let mod_id = inspected.manifest.id.clone();
        let version = inspected.manifest.version.clone();
        let record = InstalledModRecord {
            manifest: inspected.manifest,
            granted_permissions: inspected.package.permissions.clone(),
            package: inspected.package,
            source,
            package_sha256: inspected.package_sha256,
            install_dir,
            enabled,
            installed_at: Utc::now().to_rfc3339(),
            agent_visible_operations: next_visible.clone(),
        };

        let mut next_inventory = self
            .inventory
            .read()
            .expect("inventory lock poisoned")
            .clone();
        next_inventory.mods.insert(mod_id.clone(), record.clone());
        self.store
            .save(&next_inventory)
            .map_err(|msg| ModManagerError::Io {
                path: self.store.paths().inventory_file(),
                message: msg,
            })?;
        *self.inventory.write().expect("inventory lock poisoned") = next_inventory;
        self.visibility.set_mod_visibility(&mod_id, next_visible);

        let health = if enabled {
            let instance = self.spawn_record_or_unavailable(&record).await;
            let entry = self
                .register_or_replace_in_registry(&mod_id, instance)
                .await
                .map_err(|d| ModManagerError::Diagnostic(Box::new(d)))?;
            entry.health
        } else {
            let _ = self.registry.unregister_and_stop(&mod_id).await;
            Health::Unavailable(mod_unavailable_diagnostic(
                &mod_id,
                "DISABLED",
                format!("mod `{mod_id}` is installed but disabled"),
            ))
        };

        self.event_bus.publish(Event::mod_event(
            "mod.installed",
            json!({
                "modId": mod_id,
                "version": version,
                "upgrade": is_upgrade,
                "enabled": enabled,
            }),
        ));
        if let Health::Unavailable(diag) = &health
            && enabled
        {
            self.event_bus.publish(Event::mod_event(
                "mod.health_changed",
                json!({
                    "modId": record.manifest.id,
                    "available": false,
                    "diagnostic": diag.message(),
                }),
            ));
        }

        Ok(InstalledModInfo { record, health })
    }

    async fn register_or_replace_in_registry(
        &self,
        mod_id: &str,
        instance: Arc<dyn Mod>,
    ) -> Result<mod_sdk::ModEntry, Diagnostic> {
        let already_loaded = self
            .registry
            .list()
            .iter()
            .any(|entry| entry.manifest.id == mod_id);
        if already_loaded {
            self.registry
                .replace_and_restart(mod_id, instance, &self.event_bus)
                .await
        } else {
            self.registry
                .register_and_start(instance, &self.event_bus)
                .await
        }
    }

    async fn spawn_record_or_unavailable(&self, record: &InstalledModRecord) -> Arc<dyn Mod> {
        let rel_entry = match &record.package.entrypoint {
            EntrypointSpec::Binary { path } => path.as_str(),
        };
        let entrypoint =
            match resolve_entrypoint_path(&record.manifest.id, &record.install_dir, rel_entry) {
                Ok(p) => p,
                Err(err) => {
                    return Arc::new(UnavailableStubMod {
                        manifest: record.manifest.clone(),
                        diagnostic: mod_unavailable_diagnostic(
                            &record.manifest.id,
                            "ENTRYPOINT_MISSING",
                            err.to_string(),
                        ),
                    });
                }
            };
        match ProcessMod::spawn(ProcessModSpec::new(&entrypoint)).await {
            Ok(process_mod) => {
                if process_mod.manifest() != &record.manifest {
                    let _ = process_mod.stop().await;
                    return Arc::new(UnavailableStubMod {
                        manifest: record.manifest.clone(),
                        diagnostic: mod_unavailable_diagnostic(
                            &record.manifest.id,
                            "MANIFEST_DRIFT",
                            format!(
                                "mod `{}` live manifest differs from recorded inventory",
                                record.manifest.id
                            ),
                        ),
                    });
                }
                Arc::new(process_mod)
            }
            Err(diagnostic) => Arc::new(UnavailableStubMod {
                manifest: record.manifest.clone(),
                diagnostic,
            }),
        }
    }

    fn cleanup_stale_staging(&self) {
        let staging_root = self.store.paths().staging_root();
        let Ok(entries) = std::fs::read_dir(&staging_root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(path);
            } else {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn validate_requested_operations(
    operations: &BTreeSet<String>,
    manifest: &ModManifest,
) -> Result<(), ModManagerError> {
    let declared: BTreeSet<&str> = manifest
        .operations
        .iter()
        .map(|op| op.name.as_str())
        .collect();
    for op in operations {
        if !declared.contains(op.as_str()) {
            return Err(ModManagerError::UnknownOperation {
                mod_id: manifest.id.clone(),
                operation: op.clone(),
            });
        }
    }
    Ok(())
}

fn resolve_entrypoint_path(
    mod_id: &str,
    package_root: &Path,
    rel_entrypoint: &str,
) -> Result<PathBuf, ModManagerError> {
    validate_relative_path(rel_entrypoint).map_err(|e| ModManagerError::SelfCheckFailed {
        mod_id: mod_id.to_owned(),
        reason: e.to_string(),
    })?;
    let candidate = package_root.join(rel_entrypoint);
    if !candidate.is_file() {
        return Err(ModManagerError::SelfCheckFailed {
            mod_id: mod_id.to_owned(),
            reason: format!("entrypoint binary missing at `{}`", candidate.display()),
        });
    }
    Ok(candidate)
}

fn ensure_executable_bit(path: &Path) -> Result<(), ModManagerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path).map_err(|e| ModManagerError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let mode = metadata.permissions().mode();
        if mode & 0o100 == 0 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o755)).map_err(
                |e| ModManagerError::Io {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                },
            )?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
