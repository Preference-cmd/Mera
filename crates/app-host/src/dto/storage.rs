//! Storage budget status, preflight, and cleanup DTOs.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Category-by-category byte usage across Mera-managed directories.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCategoriesDto {
    /// Global library (`library/` excluding `library/scratch/` and `library/thumbnails/`).
    pub library_bytes: u64,
    /// Subset of `library_bytes` held by `removed` entries pending manual `library.purge`.
    pub library_removed_bytes: u64,
    /// Managed local models (`models/`).
    pub models_bytes: u64,
    /// Installed workers and worker inventory (`<app_data_root>/workers/` excluding `staging/`).
    pub workers_bytes: u64,
    /// Installed mods, mod data, and uv runtime (`<mods_root>/` excluding `staging/` and `runtime/uv-cache/`).
    pub mods_bytes: u64,
    /// Reclaimable cache (`tmp/`, `library/scratch/`, `projects/*/scratch/`, worker/mod `staging/`, thumbnail caches, and `runtime/uv-cache/`).
    pub cache_bytes: u64,
    /// Total across all managed projects (`assets_bytes + history_bytes` per project, excluding `scratch/` and `thumbnails/`).
    pub projects_total_bytes: u64,
    /// Subset of `projects_total_bytes` held under `projects/<id>/history/`.
    pub history_total_bytes: u64,
    /// Legacy workspace directories (`input/`, `output/`, `workflows/`).
    pub legacy_bytes: u64,
}

impl StorageCategoriesDto {
    /// Sum of all non-overlapping accounted managed categories.
    pub fn total_bytes(&self) -> u64 {
        self.library_bytes
            .saturating_add(self.models_bytes)
            .saturating_add(self.workers_bytes)
            .saturating_add(self.mods_bytes)
            .saturating_add(self.cache_bytes)
            .saturating_add(self.projects_total_bytes)
            .saturating_add(self.legacy_bytes)
    }
}

/// Per-project storage breakdown exposed inside [`StorageStatusDto`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStorageDto {
    pub project_id: String,
    pub name: String,
    pub assets_bytes: u64,
    pub history_bytes: u64,
    pub total_bytes: u64,
}

/// Overall storage budget status snapshot exposed to host IPC callers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageStatusDto {
    pub total_bytes: u64,
    pub limit_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
    pub disk_available_bytes: Option<u64>,
    pub disk_floor_ratio: f64,
    pub disk_floor_bytes: Option<u64>,
    pub over_budget: bool,
    pub below_disk_floor: bool,
    pub show_fifty_gb_nudge: bool,
    pub nudge_dismissed: bool,
    pub scan_running: bool,
    pub last_calibrated_at: Option<String>,
    pub categories: StorageCategoriesDto,
    pub projects: Vec<ProjectStorageDto>,
}

/// Write operation kind evaluated by [`crate::StorageBudgetService::preflight_write`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageWriteKind {
    Library,
    Model,
    Worker,
    Mod,
}

/// Input payload accepted by storage preflight IPC commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoragePreflightInputDto {
    pub kind: StorageWriteKind,
    pub estimated_bytes: u64,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
}

/// Reason a preflight check requests user confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoragePreflightReason {
    OverBudget,
    WouldExceedBudget,
    BelowDiskFloor,
}

/// Preflight check result indicating whether user confirmation is required before writing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoragePreflightResultDto {
    pub requires_confirm: bool,
    pub reason: Option<StoragePreflightReason>,
    pub projected_total_bytes: u64,
    pub limit_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub estimated_bytes: u64,
    pub current_total_bytes: u64,
    pub disk_floor_bytes: Option<u64>,
    pub over_budget: bool,
    pub would_exceed_budget: bool,
    pub below_disk_floor: bool,
    pub would_drop_below_disk_floor: bool,
}

/// Summary of an explicit cache cleanup run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCleanResultDto {
    pub freed_bytes: u64,
    pub deleted_files: usize,
    pub status: StorageStatusDto,
}
