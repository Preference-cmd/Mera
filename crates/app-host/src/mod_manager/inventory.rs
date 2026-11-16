//! Persistent mod inventory and per-operation Agent visibility table.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use mera_core::diagnostic::Diagnostic;
use mera_core::health::Health;
use mera_package_installer::atomic_write_file;
use mod_sdk::ModManifest;
use mod_sdk::package::{ModPackageManifest, ModPermissions};
use serde::{Deserialize, Serialize};

use crate::operation_policy::agent_may_invoke;

/// Schema version for `<mods_root>/inventory.json`.
pub const MOD_INVENTORY_SCHEMA_VERSION: u16 = 1;

/// Filesystem layout for managed mod installations (`inventory.json`, `packages/`, `staging/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModStorePaths {
    mods_root: PathBuf,
}

fn usable_absolute_dir(path: Option<&Path>) -> Option<&Path> {
    path.filter(|p| !p.as_os_str().is_empty() && p.is_absolute())
}

impl ModStorePaths {
    /// Store mods directly beneath `<app_data_root>/mods`.
    #[must_use]
    pub fn new(app_data_root: impl AsRef<Path>) -> Self {
        Self {
            mods_root: app_data_root.as_ref().join("mods"),
        }
    }

    /// Use an explicit `mods/` directory directly.
    #[must_use]
    pub fn from_mods_root(mods_root: impl Into<PathBuf>) -> Self {
        Self {
            mods_root: mods_root.into(),
        }
    }

    /// Probe whether `mods_dir` can be created and written by creating and deleting a temporary probe file.
    #[must_use]
    pub fn probe_mods_dir_writable(mods_dir: &Path) -> bool {
        static PROBE_NONCE: AtomicU64 = AtomicU64::new(0);
        if std::fs::create_dir_all(mods_dir).is_err() {
            return false;
        }
        let pid = std::process::id();
        let nonce = PROBE_NONCE.fetch_add(1, Ordering::Relaxed);
        let probe_path = mods_dir.join(format!(".mera-write-probe-{pid}-{nonce}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe_path)
        {
            Ok(file) => {
                drop(file);
                let _ = std::fs::remove_file(&probe_path);
                true
            }
            Err(_) => false,
        }
    }

    /// Resolve the Unix-like default mod directory (`<home>/.mera/mods`), falling back to
    /// `<fallback_app_data_dir>/mods` with a warning when no usable home directory is available.
    #[must_use]
    pub fn unix_default(
        home_dir: Option<&Path>,
        fallback_app_data_dir: &Path,
    ) -> (Self, Option<String>) {
        if let Some(home) = usable_absolute_dir(home_dir) {
            return (Self::from_mods_root(home.join(".mera").join("mods")), None);
        }
        let fallback = Self::new(fallback_app_data_dir);
        let warning = format!(
            "home directory unavailable; falling back to `{}` for mods",
            fallback.mods_root().display()
        );
        (fallback, Some(warning))
    }

    /// Resolve the Windows default mod directory (`<exe_dir>/mods` when writable), falling back
    /// to `<local_app_data_dir>/mera/mods` (or `<fallback_app_data_dir>/mods`) with a warning.
    #[must_use]
    pub fn windows_default_with_probe<F>(
        exe_dir: Option<&Path>,
        local_app_data_dir: Option<&Path>,
        fallback_app_data_dir: &Path,
        is_writable: F,
    ) -> (Self, Option<String>)
    where
        F: FnOnce(&Path) -> bool,
    {
        let fallback = match usable_absolute_dir(local_app_data_dir) {
            Some(local) => local.join("mera").join("mods"),
            None => fallback_app_data_dir.join("mods"),
        };

        if let Some(dir) = usable_absolute_dir(exe_dir) {
            let candidate = dir.join("mods");
            if is_writable(&candidate) {
                return (Self::from_mods_root(candidate), None);
            }
            let warning = format!(
                "install directory mods path `{}` is not writable; using `{}` instead",
                candidate.display(),
                fallback.display()
            );
            return (Self::from_mods_root(fallback), Some(warning));
        }

        let warning = format!(
            "application install directory unavailable; using `{}` for mods",
            fallback.display()
        );
        (Self::from_mods_root(fallback), Some(warning))
    }

    /// Resolve the default managed mods root for the current platform.
    ///
    /// - Unix: `<home>/.mera/mods`, falling back to `<fallback_app_data_dir>/mods` if home is unavailable.
    /// - Windows: `<exe_dir>/mods` when writable, falling back to `<local_app_data_dir>/mera/mods`
    ///   (or `<fallback_app_data_dir>/mods`).
    #[must_use]
    pub fn platform_default(
        home_dir: Option<&Path>,
        exe_dir: Option<&Path>,
        local_app_data_dir: Option<&Path>,
        fallback_app_data_dir: &Path,
    ) -> (Self, Option<String>) {
        #[cfg(unix)]
        {
            let _ = (exe_dir, local_app_data_dir);
            Self::unix_default(home_dir, fallback_app_data_dir)
        }
        #[cfg(windows)]
        {
            let _ = home_dir;
            Self::windows_default_with_probe(
                exe_dir,
                local_app_data_dir,
                fallback_app_data_dir,
                Self::probe_mods_dir_writable,
            )
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (home_dir, exe_dir, local_app_data_dir);
            (Self::new(fallback_app_data_dir), None)
        }
    }

    #[must_use]
    pub fn mods_root(&self) -> &Path {
        &self.mods_root
    }

    #[must_use]
    pub fn inventory_file(&self) -> PathBuf {
        self.mods_root.join("inventory.json")
    }

    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.mods_root.join("inventory.lock")
    }

    #[must_use]
    pub fn staging_root(&self) -> PathBuf {
        self.mods_root.join("staging")
    }

    #[must_use]
    pub fn packages_root(&self) -> PathBuf {
        self.mods_root.join("packages")
    }

    #[must_use]
    pub fn package_version_dir(&self, mod_id: &str, version: &str) -> PathBuf {
        self.packages_root().join(mod_id).join(version)
    }

    #[must_use]
    pub fn mod_package_root(&self, mod_id: &str) -> PathBuf {
        self.packages_root().join(mod_id)
    }
}

/// Installation provenance recorded for each managed mod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModInstallSource {
    Official { catalog_url: String },
    ThirdPartyUrl { source_url: String },
    LocalArchive { filename: String },
    LocalDirectory { path: PathBuf },
}

impl ModInstallSource {
    /// Whether this source requires explicit user consent before installation.
    #[must_use]
    pub fn requires_explicit_consent(&self) -> bool {
        !matches!(self, Self::Official { .. })
    }

    /// Whether `other` belongs to the same trusted origin, permitting upgrades
    /// without silent cross-origin replacement.
    #[must_use]
    pub fn is_same_origin(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Official { .. }, Self::Official { .. }) => true,
            (Self::ThirdPartyUrl { source_url: a }, Self::ThirdPartyUrl { source_url: b }) => {
                a == b
            }
            (Self::LocalArchive { .. }, Self::LocalArchive { .. }) => true,
            (Self::LocalDirectory { path: a }, Self::LocalDirectory { path: b }) => a == b,
            _ => false,
        }
    }

    /// Human-readable origin label for diagnostics and conflict messages.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Official { catalog_url } => format!("official ({catalog_url})"),
            Self::ThirdPartyUrl { source_url } => format!("third-party ({source_url})"),
            Self::LocalArchive { filename } => format!("local archive ({filename})"),
            Self::LocalDirectory { path } => format!("local directory ({})", path.display()),
        }
    }
}

/// On-disk record of a managed mod installation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstalledModRecord {
    pub manifest: ModManifest,
    pub package: ModPackageManifest,
    pub source: ModInstallSource,
    #[serde(default)]
    pub package_sha256: Option<String>,
    /// Package root directory on disk (either under `packages/<id>/<ver>` or a local dev dir).
    pub install_dir: PathBuf,
    pub enabled: bool,
    pub installed_at: String,
    #[serde(default)]
    pub granted_permissions: ModPermissions,
    /// Explicit per-operation Agent allow-list (`<mod_id>.<op>` names).
    #[serde(default)]
    pub agent_visible_operations: BTreeSet<String>,
}

/// Serialized contents of `inventory.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModInventory {
    pub schema_version: u16,
    #[serde(default)]
    pub mods: BTreeMap<String, InstalledModRecord>,
}

impl Default for ModInventory {
    fn default() -> Self {
        Self {
            schema_version: MOD_INVENTORY_SCHEMA_VERSION,
            mods: BTreeMap::new(),
        }
    }
}

/// Combined persisted record and live registry status returned to callers.
#[derive(Debug, Clone, Serialize)]
pub struct InstalledModInfo {
    pub record: InstalledModRecord,
    pub health: Health,
}

/// In-memory per-operation Agent visibility table shared with [`crate::OperationService`].
///
/// Managed mods (those installed or loaded through the mod manager) appear as
/// keys in this table with their explicit `agent_visible_operations` allow-list.
/// Unmanaged mods registered directly in unit tests are absent from the table
/// and retain existing policy behavior.
#[derive(Debug, Clone, Default)]
pub struct ModVisibilityTable {
    managed_mods: Arc<RwLock<HashMap<String, BTreeSet<String>>>>,
}

impl ModVisibilityTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace all managed mod visibility rules atomically from loaded inventory.
    pub fn sync_from_inventory(&self, inventory: &ModInventory) {
        let mut guard = self
            .managed_mods
            .write()
            .expect("mod visibility lock poisoned");
        guard.clear();
        for (mod_id, record) in &inventory.mods {
            guard.insert(mod_id.clone(), record.agent_visible_operations.clone());
        }
    }

    /// Update the allowed Agent operations for a single managed mod.
    pub fn set_mod_visibility(&self, mod_id: &str, allowed: BTreeSet<String>) {
        self.managed_mods
            .write()
            .expect("mod visibility lock poisoned")
            .insert(mod_id.to_owned(), allowed);
    }

    /// Remove a mod from the managed visibility table (on uninstall).
    pub fn remove_mod(&self, mod_id: &str) {
        self.managed_mods
            .write()
            .expect("mod visibility lock poisoned")
            .remove(mod_id);
    }

    /// Whether `mod_id` is currently tracked as a managed mod in `inventory.json`.
    #[must_use]
    pub fn has_mod(&self, mod_id: &str) -> bool {
        self.managed_mods
            .read()
            .expect("mod visibility lock poisoned")
            .contains_key(mod_id)
    }

    /// Whether an operation belonging to `mod_id` is permitted to an Agent caller.
    #[must_use]
    pub fn is_mod_operation_permitted(&self, mod_id: &str, operation_name: &str) -> bool {
        if !agent_may_invoke(operation_name) {
            return false;
        }
        let guard = self
            .managed_mods
            .read()
            .expect("mod visibility lock poisoned");
        match guard.get(mod_id) {
            Some(allowed) => allowed.contains(operation_name),
            None => true,
        }
    }

    /// Retain only entries from `requested` that still exist in `manifest.operations`.
    #[must_use]
    pub fn retain_declared_operations(
        requested: &BTreeSet<String>,
        manifest: &ModManifest,
    ) -> BTreeSet<String> {
        let declared: BTreeSet<&str> = manifest
            .operations
            .iter()
            .map(|op| op.name.as_str())
            .collect();
        requested
            .iter()
            .filter(|name| declared.contains(name.as_str()))
            .cloned()
            .collect()
    }
}

/// Atomic persistence helper for `<mods_root>/inventory.json`.
#[derive(Debug, Clone)]
pub struct ModInventoryStore {
    paths: ModStorePaths,
}

impl ModInventoryStore {
    #[must_use]
    pub fn new(paths: ModStorePaths) -> Self {
        Self { paths }
    }

    #[must_use]
    pub fn paths(&self) -> &ModStorePaths {
        &self.paths
    }

    /// Read `inventory.json`, returning an empty inventory if missing.
    pub fn load(&self) -> Result<ModInventory, std::io::Error> {
        let path = self.paths.inventory_file();
        match std::fs::read(&path) {
            Ok(bytes) => {
                let inventory: ModInventory = serde_json::from_slice(&bytes).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("invalid mod inventory `{}`: {e}", path.display()),
                    )
                })?;
                if inventory.schema_version != MOD_INVENTORY_SCHEMA_VERSION {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "unsupported mod inventory schema_version {}",
                            inventory.schema_version
                        ),
                    ));
                }
                Ok(inventory)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ModInventory::default()),
            Err(e) => Err(e),
        }
    }

    /// Persist `inventory` atomically via temp file + sync + rename.
    pub fn save(&self, inventory: &ModInventory) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(inventory)
            .map_err(|e| format!("failed to serialize mod inventory: {e}"))?;
        atomic_write_file(&self.paths.inventory_file(), &bytes).map_err(|e| e.to_string())
    }
}

/// Build an unavailable placeholder [`Diagnostic`] for startup/toggle spawn errors.
pub(crate) fn mod_unavailable_diagnostic(
    mod_id: &str,
    code: &str,
    message: impl Into<String>,
) -> Diagnostic {
    use mera_core::diagnostic::{
        DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
        DiagnosticTargetDomain,
    };
    use mera_core::model::DiagnosticId;

    Diagnostic::new(
        DiagnosticId::new(format!("mod:{mod_id}:{code}")),
        DiagnosticCode::new(format!("MOD/INTERNAL_{code}")),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("mod_manager"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new(format!("mod.{mod_id}"))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_default_uses_dot_mera_mods_under_home_and_falls_back_when_missing() {
        let temp = tempfile::tempdir().expect("temp dir");
        let home = temp.path().join("home");
        let app_data = temp.path().join("app-data");

        let (paths, warning) = ModStorePaths::unix_default(Some(&home), &app_data);
        assert_eq!(paths.mods_root(), home.join(".mera").join("mods"));
        assert!(warning.is_none());

        let (missing_paths, missing_warn) = ModStorePaths::unix_default(None, &app_data);
        assert_eq!(missing_paths.mods_root(), app_data.join("mods"));
        assert!(missing_warn.is_some());

        let (rel_paths, rel_warn) =
            ModStorePaths::unix_default(Some(Path::new("relative/home")), &app_data);
        assert_eq!(rel_paths.mods_root(), app_data.join("mods"));
        assert!(rel_warn.is_some());
    }

    #[test]
    fn windows_default_uses_exe_dir_when_writable_and_cleans_probe() {
        let temp = tempfile::tempdir().expect("temp dir");
        let exe_dir = temp.path().join("install");
        let local_app_data = temp.path().join("local-app-data");
        let app_data = temp.path().join("app-data");
        std::fs::create_dir_all(&exe_dir).expect("create exe dir");

        let (paths, warning) = ModStorePaths::windows_default_with_probe(
            Some(&exe_dir),
            Some(&local_app_data),
            &app_data,
            ModStorePaths::probe_mods_dir_writable,
        );
        assert_eq!(paths.mods_root(), exe_dir.join("mods"));
        assert!(warning.is_none());

        let entries: Vec<_> = std::fs::read_dir(paths.mods_root())
            .expect("mods dir created by probe")
            .filter_map(Result::ok)
            .collect();
        assert!(
            entries.is_empty(),
            "write probe must leave no temporary file behind: {entries:?}"
        );
    }

    #[test]
    fn windows_default_falls_back_to_local_appdata_when_install_dir_not_writable() {
        let temp = tempfile::tempdir().expect("temp dir");
        let non_dir_exe_path = temp.path().join("not-a-directory");
        std::fs::write(&non_dir_exe_path, b"blocker").expect("create regular file");
        let local_app_data = temp.path().join("local-app-data");
        let app_data = temp.path().join("app-data");

        let (paths, warning) = ModStorePaths::windows_default_with_probe(
            Some(&non_dir_exe_path),
            Some(&local_app_data),
            &app_data,
            ModStorePaths::probe_mods_dir_writable,
        );
        assert_eq!(paths.mods_root(), local_app_data.join("mera").join("mods"));
        let msg = warning.expect("fallback must emit a warning");
        assert!(msg.contains("not writable"), "unexpected warning: {msg}");

        let (no_local_paths, no_local_warn) = ModStorePaths::windows_default_with_probe(
            Some(&non_dir_exe_path),
            None,
            &app_data,
            ModStorePaths::probe_mods_dir_writable,
        );
        assert_eq!(no_local_paths.mods_root(), app_data.join("mods"));
        assert!(no_local_warn.is_some());
    }
}
