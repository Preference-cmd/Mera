use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use mera_backend_worker_host::WorkerStorePaths;
use mera_config::AppPaths;
use mera_core::asset::AssetRecord;
use mera_core::library::LibraryIndexDocument;
use mera_core::project::Project;

use crate::ModStorePaths;
use crate::dto::{ProjectStorageDto, StorageCategoriesDto};

/// Accounted storage categories tracked by [`crate::StorageBudgetService`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageCategory {
    Library,
    Projects,
    History,
    Models,
    Workers,
    Mods,
    Cache,
    Legacy,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ScanSnapshot {
    pub(crate) categories: StorageCategoriesDto,
    pub(crate) projects: Vec<ProjectStorageDto>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AccountingState {
    pub(crate) categories: StorageCategoriesDto,
    pub(crate) projects: BTreeMap<String, ProjectStorageDto>,
    pub(crate) last_calibrated_at: Option<String>,
}

impl AccountingState {
    pub(crate) fn apply_delta(&mut self, category: StorageCategory, delta_bytes: i64) {
        if category == StorageCategory::History {
            apply_signed(&mut self.categories.history_total_bytes, delta_bytes);
            apply_signed(&mut self.categories.projects_total_bytes, delta_bytes);
            return;
        }
        let slot = match category {
            StorageCategory::Library => &mut self.categories.library_bytes,
            StorageCategory::Projects => &mut self.categories.projects_total_bytes,
            StorageCategory::History => unreachable!(),
            StorageCategory::Models => &mut self.categories.models_bytes,
            StorageCategory::Workers => &mut self.categories.workers_bytes,
            StorageCategory::Mods => &mut self.categories.mods_bytes,
            StorageCategory::Cache => &mut self.categories.cache_bytes,
            StorageCategory::Legacy => &mut self.categories.legacy_bytes,
        };
        apply_signed(slot, delta_bytes);
    }

    pub(crate) fn remove_project(&mut self, project_id: &str) {
        self.projects.remove(project_id);
    }

    pub(crate) fn set_calibrated(&mut self, snapshot: ScanSnapshot, timestamp: String) {
        self.categories = snapshot.categories;
        self.projects = snapshot
            .projects
            .into_iter()
            .map(|p| (p.project_id.clone(), p))
            .collect();
        self.last_calibrated_at = Some(timestamp);
    }
}

fn apply_signed(slot: &mut u64, delta_bytes: i64) {
    if delta_bytes >= 0 {
        *slot = slot.saturating_add(delta_bytes as u64);
    } else {
        *slot = slot.saturating_sub(delta_bytes.unsigned_abs());
    }
}

/// Recursively sum regular file byte lengths under `root` without following symlinks.
///
/// Missing or unreadable paths yield `0`.
pub fn dir_size_sync(root: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(root) else {
        return 0;
    };
    if meta.file_type().is_symlink() {
        return 0;
    }
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }

    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file()
                && let Ok(entry_meta) = entry.metadata()
            {
                total = total.saturating_add(entry_meta.len());
            }
        }
    }
    total
}

/// Scan a single project directory into `(assets_bytes, history_bytes, cache_bytes)`.
pub(crate) fn scan_project_dir_sync(project_dir: &Path) -> (u64, u64, u64) {
    let Ok(entries) = std::fs::read_dir(project_dir) else {
        return (0, 0, 0);
    };
    let mut assets_bytes = 0u64;
    let mut history_bytes = 0u64;
    let mut cache_bytes = 0u64;

    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name_lossy = name.to_string_lossy();
        if ft.is_dir() {
            match name_lossy.as_ref() {
                "scratch" | "thumbnails" => {
                    cache_bytes = cache_bytes.saturating_add(dir_size_sync(&path));
                }
                "history" => {
                    history_bytes = history_bytes.saturating_add(dir_size_sync(&path));
                }
                _ => {
                    assets_bytes = assets_bytes.saturating_add(dir_size_sync(&path));
                }
            }
        } else if ft.is_file()
            && let Ok(meta) = entry.metadata()
        {
            assets_bytes = assets_bytes.saturating_add(meta.len());
        }
    }

    (assets_bytes, history_bytes, cache_bytes)
}

fn read_project_name_sync(project_dir: &Path, fallback_id: &str) -> String {
    let path = project_dir.join("project.json");
    let Ok(contents) = std::fs::read_to_string(path) else {
        return fallback_id.to_owned();
    };
    let Ok(project) = serde_json::from_str::<Project>(&contents) else {
        return fallback_id.to_owned();
    };
    let name = project.metadata().name().trim();
    if name.is_empty() {
        fallback_id.to_owned()
    } else {
        name.to_owned()
    }
}

/// Estimate total bytes in `library/assets/` referenced exclusively by `removed` entries.
fn scan_library_removed_bytes_sync(library_dir: &Path) -> u64 {
    let index_path = library_dir.join("index.json");
    let jsonl_path = library_dir.join("assets.jsonl");
    let Ok(index_raw) = std::fs::read_to_string(index_path) else {
        return 0;
    };
    let Ok(index_doc) = serde_json::from_str::<LibraryIndexDocument>(&index_raw) else {
        return 0;
    };
    let Ok(jsonl_raw) = std::fs::read_to_string(jsonl_path) else {
        return 0;
    };

    let mut record_by_id: HashMap<String, (String, u64)> = HashMap::new();
    for line in jsonl_raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<AssetRecord>(trimmed) {
            record_by_id.insert(rec.id.as_str().to_owned(), (rec.hash, rec.size));
        }
    }

    let mut active_hashes: HashSet<String> = HashSet::new();
    let mut removed_hashes: HashMap<String, u64> = HashMap::new();

    for (id, entry) in index_doc.entries() {
        let Some((hash, size)) = record_by_id.get(id.as_str()) else {
            continue;
        };
        if entry.removed {
            removed_hashes.entry(hash.clone()).or_insert(*size);
        } else {
            active_hashes.insert(hash.clone());
        }
    }

    let mut total = 0u64;
    for (hash, recorded_size) in removed_hashes {
        if active_hashes.contains(&hash) {
            continue;
        }
        let prefix = if hash.len() >= 2 { &hash[0..2] } else { "xx" };
        let content_path = library_dir.join("assets").join(prefix).join(&hash);
        if let Ok(meta) = std::fs::symlink_metadata(&content_path)
            && meta.is_file()
        {
            total = total.saturating_add(meta.len().max(recorded_size));
        }
    }
    total
}

/// Perform a full synchronous filesystem accounting scan across `AppPaths`,
/// `WorkerStorePaths`, and `ModStorePaths`.
///
/// External user workspace folders (`Project.folders`) and external model
/// directories are never visited, and symlinks are never followed.
pub(crate) fn scan_workspace_sync(
    app_paths: &AppPaths,
    worker_paths: &WorkerStorePaths,
    mod_paths: &ModStorePaths,
) -> ScanSnapshot {
    let mut categories = StorageCategoriesDto::default();
    let mut projects = Vec::new();

    // 1. Library (`library/` excluding `scratch/` and `thumbnails/`).
    if let Ok(entries) = std::fs::read_dir(app_paths.library_dir()) {
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name_lossy = name.to_string_lossy();
            if ft.is_dir() && matches!(name_lossy.as_ref(), "scratch" | "thumbnails") {
                categories.cache_bytes =
                    categories.cache_bytes.saturating_add(dir_size_sync(&path));
            } else if ft.is_dir() {
                categories.library_bytes = categories
                    .library_bytes
                    .saturating_add(dir_size_sync(&path));
            } else if ft.is_file()
                && let Ok(meta) = entry.metadata()
            {
                categories.library_bytes = categories.library_bytes.saturating_add(meta.len());
            }
        }
    }
    categories.library_removed_bytes = scan_library_removed_bytes_sync(app_paths.library_dir());

    // 2. Projects (`projects/<id>/` split into projects, history, and scratch/thumbnails cache).
    if let Ok(entries) = std::fs::read_dir(app_paths.projects_dir()) {
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                let project_dir = entry.path();
                let project_id = entry.file_name().to_string_lossy().into_owned();
                let name = read_project_name_sync(&project_dir, &project_id);
                let (assets_bytes, history_bytes, cache) = scan_project_dir_sync(&project_dir);
                let total_bytes = assets_bytes.saturating_add(history_bytes);
                categories.projects_total_bytes =
                    categories.projects_total_bytes.saturating_add(total_bytes);
                categories.history_total_bytes =
                    categories.history_total_bytes.saturating_add(history_bytes);
                categories.cache_bytes = categories.cache_bytes.saturating_add(cache);
                projects.push(ProjectStorageDto {
                    project_id,
                    name,
                    assets_bytes,
                    history_bytes,
                    total_bytes,
                });
            } else if ft.is_file()
                && let Ok(meta) = entry.metadata()
            {
                categories.projects_total_bytes =
                    categories.projects_total_bytes.saturating_add(meta.len());
            }
        }
    }
    projects.sort_by(|a, b| a.project_id.cmp(&b.project_id));

    // 3. Managed models (`models/`).
    categories.models_bytes = dir_size_sync(app_paths.models_dir());

    // 4. Ephemeral workspace `tmp/` and thumbnail caches.
    categories.cache_bytes = categories
        .cache_bytes
        .saturating_add(dir_size_sync(&app_paths.tmp_dir()))
        .saturating_add(dir_size_sync(&app_paths.thumbnails_dir()))
        .saturating_add(dir_size_sync(
            &app_paths.base_path().join("cache").join("thumbnails"),
        ));

    // 5. Legacy top-level directories (`input/`, `output/`, `workflows/`).
    categories.legacy_bytes = dir_size_sync(app_paths.input_dir())
        .saturating_add(dir_size_sync(app_paths.output_dir()))
        .saturating_add(dir_size_sync(app_paths.workflows_dir()));

    // 6. Workers (`<app_data_root>/workers/`: `installed/`, `inventory/`, etc. vs `staging/`).
    if let Ok(entries) = std::fs::read_dir(worker_paths.root()) {
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name_lossy = name.to_string_lossy();
            if ft.is_dir() && name_lossy == "staging" {
                categories.cache_bytes =
                    categories.cache_bytes.saturating_add(dir_size_sync(&path));
            } else if ft.is_dir() {
                categories.workers_bytes = categories
                    .workers_bytes
                    .saturating_add(dir_size_sync(&path));
            } else if ft.is_file()
                && name_lossy != ".install.lock"
                && let Ok(meta) = entry.metadata()
            {
                categories.workers_bytes = categories.workers_bytes.saturating_add(meta.len());
            }
        }
    }

    // 7. Mods (`<mods_root>/`: `packages/`, `data/`, `runtime/uv/`, metadata vs `staging/` and `runtime/uv-cache/`).
    if let Ok(entries) = std::fs::read_dir(mod_paths.mods_root()) {
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name_lossy = name.to_string_lossy();
            if ft.is_dir() && name_lossy == "staging" {
                categories.cache_bytes =
                    categories.cache_bytes.saturating_add(dir_size_sync(&path));
            } else if ft.is_dir() && name_lossy == "runtime" {
                scan_mod_runtime_dir_sync(&path, &mut categories);
            } else if ft.is_dir() {
                categories.mods_bytes = categories.mods_bytes.saturating_add(dir_size_sync(&path));
            } else if ft.is_file()
                && name_lossy != "inventory.lock"
                && let Ok(meta) = entry.metadata()
            {
                categories.mods_bytes = categories.mods_bytes.saturating_add(meta.len());
            }
        }
    }

    ScanSnapshot {
        categories,
        projects,
    }
}

fn scan_mod_runtime_dir_sync(runtime_root: &Path, categories: &mut StorageCategoriesDto) {
    let Ok(entries) = std::fs::read_dir(runtime_root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name();
        if ft.is_dir() && name.to_string_lossy() == "uv-cache" {
            categories.cache_bytes = categories.cache_bytes.saturating_add(dir_size_sync(&path));
        } else if ft.is_dir() {
            categories.mods_bytes = categories.mods_bytes.saturating_add(dir_size_sync(&path));
        } else if ft.is_file()
            && let Ok(meta) = entry.metadata()
        {
            categories.mods_bytes = categories.mods_bytes.saturating_add(meta.len());
        }
    }
}
