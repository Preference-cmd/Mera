use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use mera_core::model::{ProjectId, WorkspaceFolderId};

use super::service::FileHistoryService;
use super::store::{
    blob_path, gc_orphan_blobs_sync, read_index_records_sync, rewrite_compacted_index_sync,
};
use super::types::{FileHistoryCleanOutcome, FileSnapshotRecord, SnapshotSource};
use crate::AppHostResult;
use crate::storage_budget::RecoveryHook;

/// Identify the set of record indices in `records` that are protected from reclamation:
/// (a) the latest `SnapshotSource::TurnStart` record per `(folder_id, rel_path)`, and
/// (b) every `pinned == true` record.
pub(crate) fn protected_indices(records: &[FileSnapshotRecord]) -> HashSet<usize> {
    let mut latest_turn_start: HashMap<(WorkspaceFolderId, String), usize> = HashMap::new();
    let mut protected = HashSet::new();

    for (idx, rec) in records.iter().enumerate() {
        if rec.pinned {
            protected.insert(idx);
        }
        if rec.source == SnapshotSource::TurnStart {
            latest_turn_start.insert((rec.folder_id.clone(), rec.rel_path.clone()), idx);
        }
    }

    protected.extend(latest_turn_start.into_values());
    protected
}

fn discover_project_ids_with_history(projects_dir: &Path) -> Vec<ProjectId> {
    let Ok(entries) = std::fs::read_dir(projects_dir) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(project_id) = ProjectId::try_new(name) else {
            continue;
        };
        if entry.path().join("history").join("index.jsonl").is_file() {
            ids.push(project_id);
        }
    }
    ids.sort();
    ids
}

fn compare_timestamps(a: &str, b: &str) -> Ordering {
    match (a.parse::<jiff::Timestamp>(), b.parse::<jiff::Timestamp>()) {
        (Ok(ta), Ok(tb)) => ta.cmp(&tb),
        _ => a.cmp(b),
    }
}

pub(crate) fn clean_project_history_sync(
    project_id: &ProjectId,
    history_dir: &Path,
) -> AppHostResult<FileHistoryCleanOutcome> {
    let records = read_index_records_sync(history_dir)?;
    if records.is_empty() {
        let (removed_blobs, freed_bytes) = gc_orphan_blobs_sync(history_dir, &HashSet::new());
        return Ok(FileHistoryCleanOutcome {
            project_id: project_id.clone(),
            removed_records: 0,
            kept_records: 0,
            removed_blobs,
            freed_bytes,
        });
    }

    let protected = protected_indices(&records);
    let mut kept = Vec::with_capacity(protected.len());
    let mut keep_hashes = HashSet::new();
    let mut removed_records = 0usize;

    for (idx, rec) in records.into_iter().enumerate() {
        if protected.contains(&idx) {
            if let Some(h) = &rec.hash {
                keep_hashes.insert(h.clone());
            }
            kept.push(rec);
        } else {
            removed_records += 1;
        }
    }

    // Crash-safe order: atomically rewrite compacted index.jsonl FIRST, then delete unreferenced blobs.
    rewrite_compacted_index_sync(history_dir, &kept)?;
    let (removed_blobs, freed_bytes) = gc_orphan_blobs_sync(history_dir, &keep_hashes);

    Ok(FileHistoryCleanOutcome {
        project_id: project_id.clone(),
        removed_records,
        kept_records: kept.len(),
        removed_blobs,
        freed_bytes,
    })
}

/// Priority 3 storage recovery hook for workspace file history.
#[derive(Debug)]
pub struct FileHistoryRecoveryHook {
    service: Arc<FileHistoryService>,
}

impl FileHistoryRecoveryHook {
    pub(crate) fn new(service: Arc<FileHistoryService>) -> Self {
        Self { service }
    }
}

struct ProjectPlan {
    project_id: ProjectId,
    history_dir: std::path::PathBuf,
    records: Vec<FileSnapshotRecord>,
    blob_refcounts: HashMap<String, usize>,
    blob_sizes: HashMap<String, u64>,
    remove_indices: HashSet<usize>,
}

struct CandidateRecord {
    recorded_at: String,
    project_idx: usize,
    record_idx: usize,
}

#[async_trait]
impl RecoveryHook for FileHistoryRecoveryHook {
    fn name(&self) -> &'static str {
        "file_history"
    }

    fn priority(&self) -> u8 {
        3
    }

    async fn estimate_reclaimable_bytes(&self) -> u64 {
        let projects_dir = self.service.paths().projects_dir().to_path_buf();
        let project_ids = tokio::task::spawn_blocking({
            let dir = projects_dir.clone();
            move || discover_project_ids_with_history(&dir)
        })
        .await
        .unwrap_or_default();

        let mut total = 0u64;
        for project_id in project_ids {
            let lock = self.service.project_lock(&project_id);
            let _guard = lock.lock().await;
            let history_dir = self
                .service
                .paths()
                .project_history_dir(project_id.as_str());
            let bytes = tokio::task::spawn_blocking(move || -> u64 {
                let Ok(records) = read_index_records_sync(&history_dir) else {
                    return 0;
                };
                let protected = protected_indices(&records);
                let mut protected_hashes = HashSet::new();
                let mut unprotected_hashes = HashSet::new();
                for (idx, rec) in records.iter().enumerate() {
                    if let Some(h) = &rec.hash {
                        if protected.contains(&idx) {
                            protected_hashes.insert(h.clone());
                        } else {
                            unprotected_hashes.insert(h.clone());
                        }
                    }
                }
                let mut sum = 0u64;
                for h in unprotected_hashes {
                    if protected_hashes.contains(&h) {
                        continue;
                    }
                    let path = blob_path(&history_dir, &h);
                    if let Ok(meta) = std::fs::symlink_metadata(&path)
                        && meta.is_file()
                    {
                        sum = sum.saturating_add(meta.len());
                    }
                }
                sum
            })
            .await
            .unwrap_or(0);
            total = total.saturating_add(bytes);
        }
        total
    }

    async fn reclaim(&self, target_bytes: u64) -> AppHostResult<u64> {
        if target_bytes == 0 {
            return Ok(0);
        }

        let projects_dir = self.service.paths().projects_dir().to_path_buf();
        let project_ids = tokio::task::spawn_blocking({
            let dir = projects_dir.clone();
            move || discover_project_ids_with_history(&dir)
        })
        .await
        .unwrap_or_default();

        if project_ids.is_empty() {
            return Ok(0);
        }

        // Acquire per-project locks in sorted ProjectId order so cross-project compaction
        // cannot race with concurrent scans or restores and cannot deadlock.
        let mut guards = Vec::with_capacity(project_ids.len());
        for project_id in &project_ids {
            let lock = self.service.project_lock(project_id);
            guards.push(lock.lock_owned().await);
        }

        let paths = self.service.paths().clone();
        let (total_freed, freed_by_project) =
            tokio::task::spawn_blocking(move || -> AppHostResult<(u64, Vec<(ProjectId, u64)>)> {
                let mut plans = Vec::with_capacity(project_ids.len());
                let mut candidates = Vec::new();

                for (project_idx, project_id) in project_ids.into_iter().enumerate() {
                    let history_dir = paths.project_history_dir(project_id.as_str());
                    let records = read_index_records_sync(&history_dir)?;
                    let protected = protected_indices(&records);

                    let mut blob_refcounts: HashMap<String, usize> = HashMap::new();
                    let mut blob_sizes: HashMap<String, u64> = HashMap::new();

                    for (record_idx, rec) in records.iter().enumerate() {
                        if let Some(h) = &rec.hash {
                            *blob_refcounts.entry(h.clone()).or_insert(0) += 1;
                            blob_sizes.entry(h.clone()).or_insert_with(|| {
                                let p = blob_path(&history_dir, h);
                                std::fs::symlink_metadata(&p)
                                    .ok()
                                    .filter(|m| m.is_file())
                                    .map_or(0, |m| m.len())
                            });
                        }
                        if !protected.contains(&record_idx) {
                            candidates.push(CandidateRecord {
                                recorded_at: rec.recorded_at.as_str().to_owned(),
                                project_idx,
                                record_idx,
                            });
                        }
                    }

                    plans.push(ProjectPlan {
                        project_id,
                        history_dir,
                        records,
                        blob_refcounts,
                        blob_sizes,
                        remove_indices: HashSet::new(),
                    });
                }

                // Global oldest-first selection across all projects.
                candidates.sort_by(|a, b| {
                    compare_timestamps(&a.recorded_at, &b.recorded_at)
                        .then_with(|| a.project_idx.cmp(&b.project_idx))
                        .then_with(|| a.record_idx.cmp(&b.record_idx))
                });

                let mut estimated_freed = 0u64;
                for cand in candidates {
                    if estimated_freed >= target_bytes {
                        break;
                    }
                    let plan = &mut plans[cand.project_idx];
                    plan.remove_indices.insert(cand.record_idx);
                    if let Some(hash) = &plan.records[cand.record_idx].hash
                        && let Some(count) = plan.blob_refcounts.get_mut(hash)
                    {
                        *count = count.saturating_sub(1);
                        if *count == 0 {
                            let sz = plan.blob_sizes.get(hash).copied().unwrap_or(0);
                            estimated_freed = estimated_freed.saturating_add(sz);
                        }
                    }
                }

                let mut actual_total_freed = 0u64;
                let mut per_project = Vec::new();
                for plan in plans {
                    if plan.remove_indices.is_empty() {
                        continue;
                    }
                    let mut kept = Vec::with_capacity(
                        plan.records.len().saturating_sub(plan.remove_indices.len()),
                    );
                    let mut keep_hashes = HashSet::new();
                    for (idx, rec) in plan.records.into_iter().enumerate() {
                        if !plan.remove_indices.contains(&idx) {
                            if let Some(h) = &rec.hash {
                                keep_hashes.insert(h.clone());
                            }
                            kept.push(rec);
                        }
                    }

                    // Crash-safe order: rewrite compacted index.jsonl first, then delete orphan blobs.
                    rewrite_compacted_index_sync(&plan.history_dir, &kept)?;
                    let (_, freed) = gc_orphan_blobs_sync(&plan.history_dir, &keep_hashes);
                    actual_total_freed = actual_total_freed.saturating_add(freed);
                    if freed > 0 {
                        per_project.push((plan.project_id, freed));
                    }
                }

                Ok((actual_total_freed, per_project))
            })
            .await
            .map_err(|err| crate::AppHostError::Io {
                path: projects_dir,
                message: format!("file history reclaim task panicked: {err}"),
            })??;

        drop(guards);

        for (project_id, freed) in freed_by_project {
            self.service.record_history_removed(&project_id, freed);
        }

        Ok(total_freed)
    }
}
