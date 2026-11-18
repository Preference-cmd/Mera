use mera_core::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
    DiagnosticTargetDomain,
};
use mera_core::model::DiagnosticId;
use serde::{Deserialize, Serialize};

use crate::{ConfigDocument, ConfigValidationContext};

/// Default fraction of total disk capacity reserved as free space floor (10%).
pub const DEFAULT_DISK_FLOOR_RATIO: f64 = 0.10;

/// Maximum configurable fraction of total disk capacity for the free space floor (90%).
pub const MAX_DISK_FLOOR_RATIO: f64 = 0.90;

/// Target ratio of `limit_bytes` aimed for during automatic budget recovery (90%).
pub const DEFAULT_RECOVERY_TARGET_RATIO: f64 = 0.90;

/// Threshold above which the 50 GiB unconfigured-budget nudge is surfaced.
pub const FIFTY_GIB_BYTES: u64 = 50 * 1024 * 1024 * 1024;

/// Persisted storage budget configuration stored under `config_dir`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageBudgetConfig {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,

    /// Optional managed storage cap in bytes. `None` means unlimited.
    #[serde(default)]
    pub limit_bytes: Option<u64>,

    /// Minimum free disk ratio in `(0.0, 0.9]` before recovery/confirmation triggers.
    #[serde(default = "default_disk_floor_ratio")]
    pub disk_floor_ratio: f64,

    /// Whether the user has dismissed the >50 GiB unconfigured-budget nudge.
    #[serde(default)]
    pub nudge_dismissed: bool,
}

fn default_schema_version() -> String {
    StorageBudgetConfig::SCHEMA_VERSION.to_owned()
}

fn default_disk_floor_ratio() -> f64 {
    DEFAULT_DISK_FLOOR_RATIO
}

impl Default for StorageBudgetConfig {
    fn default() -> Self {
        Self {
            schema_version: default_schema_version(),
            limit_bytes: None,
            disk_floor_ratio: DEFAULT_DISK_FLOOR_RATIO,
            nudge_dismissed: false,
        }
    }
}

impl StorageBudgetConfig {
    /// Returns `true` if `ratio` is finite and within `(0.0, 0.9]`.
    pub fn is_valid_disk_floor_ratio(ratio: f64) -> bool {
        ratio.is_finite() && ratio > 0.0 && ratio <= MAX_DISK_FLOOR_RATIO
    }

    /// Returns `true` if `limit_bytes` is either `None` or a positive byte count.
    pub fn is_valid_limit_bytes(limit_bytes: Option<u64>) -> bool {
        !matches!(limit_bytes, Some(0))
    }

    /// Returns a validated disk floor ratio, falling back to `DEFAULT_DISK_FLOOR_RATIO`
    /// if the persisted value is out of range.
    pub fn effective_disk_floor_ratio(&self) -> f64 {
        if Self::is_valid_disk_floor_ratio(self.disk_floor_ratio) {
            self.disk_floor_ratio
        } else {
            DEFAULT_DISK_FLOOR_RATIO
        }
    }

    /// Returns a validated limit in bytes, treating `Some(0)` as `None` (unlimited)
    /// for runtime safety while `validate` surfaces the invalid value.
    pub fn effective_limit_bytes(&self) -> Option<u64> {
        self.limit_bytes.filter(|&limit| limit > 0)
    }

    /// Validate all fields, returning a descriptive error on the first violation.
    pub fn validate_values(&self) -> Result<(), String> {
        if !Self::is_valid_disk_floor_ratio(self.disk_floor_ratio) {
            return Err(format!(
                "disk_floor_ratio ({}) must be finite and in (0.0, {MAX_DISK_FLOOR_RATIO}]",
                self.disk_floor_ratio
            ));
        }
        if !Self::is_valid_limit_bytes(self.limit_bytes) {
            return Err("limit_bytes must be greater than 0 when specified".to_owned());
        }
        Ok(())
    }
}

impl ConfigDocument for StorageBudgetConfig {
    const KEY: &'static str = "storage_budget.json";
    const SCHEMA_VERSION: &'static str = "1";

    fn validate(&self, _context: &ConfigValidationContext) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();

        if !Self::is_valid_disk_floor_ratio(self.disk_floor_ratio) {
            diagnostics.push(Diagnostic::new(
                DiagnosticId::new("config:storage_budget:invalid_disk_floor_ratio"),
                DiagnosticCode::new("CONFIG/STORAGE_BUDGET_INVALID_DISK_FLOOR_RATIO"),
                DiagnosticSeverity::Error,
                DiagnosticSourceName::new("config"),
                format!(
                    "disk_floor_ratio ({}) must be finite and in (0.0, {MAX_DISK_FLOOR_RATIO}]",
                    self.disk_floor_ratio
                ),
                DiagnosticTarget::new(DiagnosticTargetDomain::new("config")).with_id(Self::KEY),
            ));
        }

        if !Self::is_valid_limit_bytes(self.limit_bytes) {
            diagnostics.push(Diagnostic::new(
                DiagnosticId::new("config:storage_budget:invalid_limit_bytes"),
                DiagnosticCode::new("CONFIG/STORAGE_BUDGET_INVALID_LIMIT_BYTES"),
                DiagnosticSeverity::Error,
                DiagnosticSourceName::new("config"),
                "limit_bytes must be greater than 0 when specified",
                DiagnosticTarget::new(DiagnosticTargetDomain::new("config")).with_id(Self::KEY),
            ));
        }

        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppConfig, AppPaths};
    use tempfile::tempdir;

    #[test]
    fn defaults_match_spec() {
        let cfg = StorageBudgetConfig::default();
        assert_eq!(cfg.schema_version, "1");
        assert_eq!(cfg.limit_bytes, None);
        assert!((cfg.disk_floor_ratio - 0.10).abs() < f64::EPSILON);
        assert!(!cfg.nudge_dismissed);
        assert!(cfg.validate_values().is_ok());
    }

    #[test]
    fn validates_disk_floor_ratio_bounds() {
        let mut cfg = StorageBudgetConfig::default();
        for invalid in [0.0, -0.1, 0.9001, 1.0, f64::NAN, f64::INFINITY] {
            cfg.disk_floor_ratio = invalid;
            assert!(
                cfg.validate_values().is_err(),
                "expected error for {invalid}"
            );
            assert!((cfg.effective_disk_floor_ratio() - 0.10).abs() < f64::EPSILON);
        }

        for valid in [0.01, 0.10, 0.5, 0.90] {
            cfg.disk_floor_ratio = valid;
            assert!(cfg.validate_values().is_ok(), "expected ok for {valid}");
            assert!((cfg.effective_disk_floor_ratio() - valid).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn validates_limit_bytes() {
        let cfg = StorageBudgetConfig {
            limit_bytes: Some(0),
            ..Default::default()
        };
        assert!(cfg.validate_values().is_err());
        assert_eq!(cfg.effective_limit_bytes(), None);

        let valid = StorageBudgetConfig {
            limit_bytes: Some(1024),
            ..Default::default()
        };
        assert!(valid.validate_values().is_ok());
        assert_eq!(valid.effective_limit_bytes(), Some(1024));
    }

    #[tokio::test]
    async fn persists_via_config_handle() {
        let temp = tempdir().unwrap();
        let app_config = AppConfig::new(AppPaths::new(temp.path()));
        let handle = app_config.config::<StorageBudgetConfig>().unwrap();

        let (initial, report) = handle.load().await.unwrap();
        assert_eq!(initial, StorageBudgetConfig::default());
        assert!(report.diagnostics().is_empty());

        let updated = StorageBudgetConfig {
            limit_bytes: Some(25 * 1024 * 1024 * 1024),
            disk_floor_ratio: 0.15,
            nudge_dismissed: true,
            ..Default::default()
        };
        let save_report = handle.save(&updated).await.unwrap();
        assert!(save_report.diagnostics().is_empty());

        let (reloaded, reload_report) = handle.load().await.unwrap();
        assert!(reload_report.diagnostics().is_empty());
        assert_eq!(reloaded, updated);
    }

    #[tokio::test]
    async fn validation_diagnostics_surface_on_invalid_persisted_config() {
        let temp = tempdir().unwrap();
        let paths = AppPaths::new(temp.path());
        paths.ensure_all().await.unwrap();
        let path = paths.config_dir().join(StorageBudgetConfig::KEY);
        tokio::fs::write(
            &path,
            r#"{"limit_bytes": 0, "disk_floor_ratio": 0.95, "nudge_dismissed": false}"#,
        )
        .await
        .unwrap();

        let app_config = AppConfig::new(paths);
        let handle = app_config.config::<StorageBudgetConfig>().unwrap();
        let (_loaded, report) = handle.load().await.unwrap();
        assert!(!report.diagnostics().is_empty());
        assert_eq!(report.diagnostics().len(), 2);
    }
}
