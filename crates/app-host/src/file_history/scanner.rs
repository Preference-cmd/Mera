use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use ignore::gitignore::Gitignore;
use mera_core::event::Timestamp;
use mera_core::model::{FileSnapshotId, WorkspaceFolderId};
use mera_core::project::WorkspaceFolder;

use super::store::{
    append_index_lines_sync, blob_path, load_compiled_ignore_sync, read_index_records_sync,
    read_scan_state_sync, store_blob_sync, write_scan_state_sync,
};
use super::types::{
    CachedFileState, FileEntryKind, FileHistoryScanStatus, FileSnapshotRecord, IndexLine,
    ScanOptions, ScanSummary, SnapshotSource, file_history_caller_error, is_snapshot_protected,
    metadata_mtime, normalize_rel_path,
};
use crate::{AppHostError, AppHostResult};

pub(crate) fn is_fixed_skip_dir_name(name: &str) -> bool {
    matches!(name, ".git" | "node_modules")
}

pub(crate) fn is_fixed_skip_entry_name(name: &str) -> bool {
    name == ".DS_Store"
        || name.starts_with("~$")
        || name.ends_with(".tmp")
        || is_fixed_skip_dir_name(name)
}

pub(crate) fn is_fixed_skip_rel_path(rel_path: &str) -> bool {
    let mut parts = rel_path.split('/').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_some() {
            if is_fixed_skip_dir_name(part) {
                return true;
            }
        } else if is_fixed_skip_entry_name(part) {
            return true;
        }
    }
    false
}

pub(crate) fn is_rel_path_ignored(gitignore: &Gitignore, rel_path: &str, is_dir: bool) -> bool {
    if is_fixed_skip_rel_path(rel_path) {
        return true;
    }
    gitignore
        .matched_path_or_any_parents(Path::new(rel_path), is_dir)
        .is_ignore()
}

pub(crate) fn new_snapshot_id(nonce: u64) -> FileSnapshotId {
    let nanos = jiff::Timestamp::now().as_nanosecond();
    FileSnapshotId::new(format!("snap-{nanos:x}-{nonce:x}"))
}

fn next_id_nonce() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SNAPSHOT_COUNTER: AtomicU64 = AtomicU64::new(1);
    SNAPSHOT_COUNTER.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn scan_project_sync(
    history_dir: &Path,
    folders: &[WorkspaceFolder],
    max_file_bytes: u64,
    under_pressure: bool,
    options: &ScanOptions,
    live_status: Option<&Arc<Mutex<FileHistoryScanStatus>>>,
) -> AppHostResult<ScanSummary> {
    let gitignore = load_compiled_ignore_sync(history_dir)?;
    let existing_records = read_index_records_sync(history_dir)?;
    let mut latest_by_path: HashMap<(WorkspaceFolderId, String), FileSnapshotRecord> =
        HashMap::with_capacity(existing_records.len());
    for rec in existing_records {
        latest_by_path.insert((rec.folder_id.clone(), rec.rel_path.clone()), rec);
    }

    let mut scan_state = read_scan_state_sync(history_dir);
    let mut pending_lines: Vec<IndexLine> = Vec::new();
    let mut summary = ScanSummary::default();
    let protected_scan = is_snapshot_protected(options.source, options.pinned);
    let now_ts = Timestamp::new(jiff::Timestamp::now().to_string());

    for folder in folders {
        if options.cancel.is_cancelled() {
            summary.cancelled = true;
            break;
        }

        let folder_root = Path::new(folder.path());
        if !folder_root.is_dir() {
            continue;
        }

        let folder_key = folder.id().as_str().to_owned();
        let mut visited_in_folder: HashSet<String> = HashSet::new();
        let mut stack: Vec<(std::path::PathBuf, String)> =
            vec![(folder_root.to_path_buf(), String::new())];

        while let Some((dir_path, rel_prefix)) = stack.pop() {
            if options.cancel.is_cancelled() {
                summary.cancelled = true;
                break;
            }

            let Ok(read_dir) = std::fs::read_dir(&dir_path) else {
                continue;
            };
            let mut children: Vec<std::fs::DirEntry> = read_dir.flatten().collect();
            children.sort_by_key(|e| e.file_name());

            for entry in children {
                if options.cancel.is_cancelled() {
                    summary.cancelled = true;
                    break;
                }

                let file_name_os = entry.file_name();
                let Some(file_name) = file_name_os.to_str() else {
                    continue;
                };

                let entry_path = entry.path();
                let Ok(meta) = std::fs::symlink_metadata(&entry_path) else {
                    continue;
                };
                let is_symlink = meta.file_type().is_symlink();
                let is_dir = !is_symlink && meta.is_dir();

                let rel_path = if rel_prefix.is_empty() {
                    file_name.to_owned()
                } else {
                    format!("{rel_prefix}/{file_name}")
                };

                if is_dir {
                    if is_fixed_skip_dir_name(file_name)
                        || is_fixed_skip_entry_name(file_name)
                        || gitignore.matched(Path::new(&rel_path), true).is_ignore()
                    {
                        summary.skipped_ignored = summary.skipped_ignored.saturating_add(1);
                        continue;
                    }
                    stack.push((entry_path, rel_path));
                    continue;
                }

                if is_fixed_skip_entry_name(file_name)
                    || gitignore.matched(Path::new(&rel_path), false).is_ignore()
                {
                    summary.skipped_ignored = summary.skipped_ignored.saturating_add(1);
                    continue;
                }

                if !is_symlink && !meta.is_file() {
                    continue;
                }

                summary.visited_files = summary.visited_files.saturating_add(1);
                visited_in_folder.insert(rel_path.clone());

                let (mtime_secs, mtime_nanos) = metadata_mtime(&meta);
                let prev_state = scan_state
                    .entries
                    .get(&folder_key)
                    .and_then(|m| m.get(&rel_path))
                    .cloned();

                if is_symlink {
                    let target = match std::fs::read_link(&entry_path) {
                        Ok(t) => t.to_string_lossy().into_owned(),
                        Err(_) => continue,
                    };
                    let cache_hit = prev_state.as_ref().is_some_and(|cached| {
                        cached.kind == FileEntryKind::Symlink
                            && cached.mtime_secs == mtime_secs
                            && cached.mtime_nanos == mtime_nanos
                            && cached.symlink_target.as_deref() == Some(target.as_str())
                    });

                    if cache_hit {
                        if options.source == SnapshotSource::TurnStart {
                            let cached = prev_state.as_ref().expect("cache hit has prev_state");
                            let latest =
                                latest_by_path.get(&(folder.id().clone(), rel_path.clone()));
                            let already_turn_start = latest.is_some_and(|rec| {
                                rec.source == SnapshotSource::TurnStart
                                    && cached.matches_record(rec)
                            });
                            if !already_turn_start {
                                let rec = FileSnapshotRecord {
                                    id: new_snapshot_id(next_id_nonce()),
                                    folder_id: folder.id().clone(),
                                    rel_path: rel_path.clone(),
                                    kind: FileEntryKind::Symlink,
                                    hash: None,
                                    size: 0,
                                    mtime_secs,
                                    mtime_nanos,
                                    recorded_at: now_ts.clone(),
                                    source: SnapshotSource::TurnStart,
                                    turn_id: options.turn_id.clone(),
                                    actor: options.actor.clone(),
                                    oversized: false,
                                    symlink_target: Some(target),
                                    pinned: options.pinned,
                                    note: options.note.clone(),
                                };
                                latest_by_path
                                    .insert((folder.id().clone(), rel_path.clone()), rec.clone());
                                pending_lines.push(IndexLine::Snapshot(Box::new(rec)));
                                summary.recorded_snapshots =
                                    summary.recorded_snapshots.saturating_add(1);
                            }
                        }
                    } else if under_pressure && !protected_scan {
                        summary.skipped_over_budget = summary.skipped_over_budget.saturating_add(1);
                    } else {
                        let rec = FileSnapshotRecord {
                            id: new_snapshot_id(next_id_nonce()),
                            folder_id: folder.id().clone(),
                            rel_path: rel_path.clone(),
                            kind: FileEntryKind::Symlink,
                            hash: None,
                            size: 0,
                            mtime_secs,
                            mtime_nanos,
                            recorded_at: now_ts.clone(),
                            source: options.source,
                            turn_id: options.turn_id.clone(),
                            actor: options.actor.clone(),
                            oversized: false,
                            symlink_target: Some(target.clone()),
                            pinned: options.pinned,
                            note: options.note.clone(),
                        };
                        latest_by_path.insert((folder.id().clone(), rel_path.clone()), rec.clone());
                        pending_lines.push(IndexLine::Snapshot(Box::new(rec)));
                        scan_state
                            .entries
                            .entry(folder_key.clone())
                            .or_default()
                            .insert(
                                rel_path,
                                CachedFileState {
                                    kind: FileEntryKind::Symlink,
                                    size: 0,
                                    mtime_secs,
                                    mtime_nanos,
                                    hash: None,
                                    oversized: false,
                                    symlink_target: Some(target),
                                },
                            );
                        summary.recorded_snapshots = summary.recorded_snapshots.saturating_add(1);
                    }
                } else {
                    let size = meta.len();
                    if size > max_file_bytes {
                        summary.skipped_oversized = summary.skipped_oversized.saturating_add(1);
                        let cache_hit = prev_state.as_ref().is_some_and(|cached| {
                            cached.kind == FileEntryKind::File
                                && cached.oversized
                                && cached.size == size
                                && cached.mtime_secs == mtime_secs
                                && cached.mtime_nanos == mtime_nanos
                        });
                        if cache_hit {
                            if options.source == SnapshotSource::TurnStart {
                                let cached = prev_state.as_ref().expect("cache hit has prev_state");
                                let latest =
                                    latest_by_path.get(&(folder.id().clone(), rel_path.clone()));
                                let already_turn_start = latest.is_some_and(|rec| {
                                    rec.source == SnapshotSource::TurnStart
                                        && cached.matches_record(rec)
                                });
                                if !already_turn_start {
                                    let rec = FileSnapshotRecord {
                                        id: new_snapshot_id(next_id_nonce()),
                                        folder_id: folder.id().clone(),
                                        rel_path: rel_path.clone(),
                                        kind: FileEntryKind::File,
                                        hash: None,
                                        size,
                                        mtime_secs,
                                        mtime_nanos,
                                        recorded_at: now_ts.clone(),
                                        source: SnapshotSource::TurnStart,
                                        turn_id: options.turn_id.clone(),
                                        actor: options.actor.clone(),
                                        oversized: true,
                                        symlink_target: None,
                                        pinned: options.pinned,
                                        note: options.note.clone(),
                                    };
                                    latest_by_path.insert(
                                        (folder.id().clone(), rel_path.clone()),
                                        rec.clone(),
                                    );
                                    pending_lines.push(IndexLine::Snapshot(Box::new(rec)));
                                    summary.recorded_snapshots =
                                        summary.recorded_snapshots.saturating_add(1);
                                }
                            }
                        } else if under_pressure && !protected_scan {
                            summary.skipped_over_budget =
                                summary.skipped_over_budget.saturating_add(1);
                        } else {
                            let rec = FileSnapshotRecord {
                                id: new_snapshot_id(next_id_nonce()),
                                folder_id: folder.id().clone(),
                                rel_path: rel_path.clone(),
                                kind: FileEntryKind::File,
                                hash: None,
                                size,
                                mtime_secs,
                                mtime_nanos,
                                recorded_at: now_ts.clone(),
                                source: options.source,
                                turn_id: options.turn_id.clone(),
                                actor: options.actor.clone(),
                                oversized: true,
                                symlink_target: None,
                                pinned: options.pinned,
                                note: options.note.clone(),
                            };
                            latest_by_path
                                .insert((folder.id().clone(), rel_path.clone()), rec.clone());
                            pending_lines.push(IndexLine::Snapshot(Box::new(rec)));
                            scan_state
                                .entries
                                .entry(folder_key.clone())
                                .or_default()
                                .insert(
                                    rel_path,
                                    CachedFileState {
                                        kind: FileEntryKind::File,
                                        size,
                                        mtime_secs,
                                        mtime_nanos,
                                        hash: None,
                                        oversized: true,
                                        symlink_target: None,
                                    },
                                );
                            summary.recorded_snapshots =
                                summary.recorded_snapshots.saturating_add(1);
                        }
                    } else {
                        let cache_hit = prev_state.as_ref().is_some_and(|cached| {
                            cached.kind == FileEntryKind::File
                                && !cached.oversized
                                && cached.size == size
                                && cached.mtime_secs == mtime_secs
                                && cached.mtime_nanos == mtime_nanos
                                && cached
                                    .hash
                                    .as_deref()
                                    .is_some_and(|h| blob_path(history_dir, h).is_file())
                        });

                        if cache_hit {
                            if options.source == SnapshotSource::TurnStart {
                                let cached = prev_state.as_ref().expect("cache hit has prev_state");
                                let latest =
                                    latest_by_path.get(&(folder.id().clone(), rel_path.clone()));
                                let already_turn_start = latest.is_some_and(|rec| {
                                    rec.source == SnapshotSource::TurnStart
                                        && cached.matches_record(rec)
                                });
                                if !already_turn_start {
                                    let rec = FileSnapshotRecord {
                                        id: new_snapshot_id(next_id_nonce()),
                                        folder_id: folder.id().clone(),
                                        rel_path: rel_path.clone(),
                                        kind: FileEntryKind::File,
                                        hash: cached.hash.clone(),
                                        size,
                                        mtime_secs,
                                        mtime_nanos,
                                        recorded_at: now_ts.clone(),
                                        source: SnapshotSource::TurnStart,
                                        turn_id: options.turn_id.clone(),
                                        actor: options.actor.clone(),
                                        oversized: false,
                                        symlink_target: None,
                                        pinned: options.pinned,
                                        note: options.note.clone(),
                                    };
                                    latest_by_path.insert(
                                        (folder.id().clone(), rel_path.clone()),
                                        rec.clone(),
                                    );
                                    pending_lines.push(IndexLine::Snapshot(Box::new(rec)));
                                    summary.recorded_snapshots =
                                        summary.recorded_snapshots.saturating_add(1);
                                }
                            }
                        } else if under_pressure && !protected_scan {
                            summary.skipped_over_budget =
                                summary.skipped_over_budget.saturating_add(1);
                        } else {
                            let (hash, actual_size, added_bytes) =
                                store_blob_sync(history_dir, &entry_path)?;
                            summary.hashed_files = summary.hashed_files.saturating_add(1);
                            summary.added_blob_bytes =
                                summary.added_blob_bytes.saturating_add(added_bytes);
                            let rec = FileSnapshotRecord {
                                id: new_snapshot_id(next_id_nonce()),
                                folder_id: folder.id().clone(),
                                rel_path: rel_path.clone(),
                                kind: FileEntryKind::File,
                                hash: Some(hash.clone()),
                                size: actual_size,
                                mtime_secs,
                                mtime_nanos,
                                recorded_at: now_ts.clone(),
                                source: options.source,
                                turn_id: options.turn_id.clone(),
                                actor: options.actor.clone(),
                                oversized: false,
                                symlink_target: None,
                                pinned: options.pinned,
                                note: options.note.clone(),
                            };
                            latest_by_path
                                .insert((folder.id().clone(), rel_path.clone()), rec.clone());
                            pending_lines.push(IndexLine::Snapshot(Box::new(rec)));
                            scan_state
                                .entries
                                .entry(folder_key.clone())
                                .or_default()
                                .insert(
                                    rel_path,
                                    CachedFileState {
                                        kind: FileEntryKind::File,
                                        size,
                                        mtime_secs,
                                        mtime_nanos,
                                        hash: Some(hash),
                                        oversized: false,
                                        symlink_target: None,
                                    },
                                );
                            summary.recorded_snapshots =
                                summary.recorded_snapshots.saturating_add(1);
                        }
                    }
                }

                if summary.visited_files.is_multiple_of(64) {
                    update_live_counters(live_status, &summary);
                }
            }

            if summary.cancelled {
                break;
            }
        }

        // Skip deletion emission if cancelled so partial traversals never emit bogus deletes.
        if !summary.cancelled
            && let Some(folder_entries) = scan_state.entries.get_mut(&folder_key)
        {
            let missing_paths: Vec<String> = folder_entries
                .keys()
                .filter(|p| !visited_in_folder.contains(*p))
                .cloned()
                .collect();

            for rel_path in missing_paths {
                if is_rel_path_ignored(&gitignore, &rel_path, false) {
                    folder_entries.remove(&rel_path);
                    continue;
                }
                if under_pressure && !protected_scan {
                    summary.skipped_over_budget = summary.skipped_over_budget.saturating_add(1);
                    continue;
                }
                let rec = FileSnapshotRecord {
                    id: new_snapshot_id(next_id_nonce()),
                    folder_id: folder.id().clone(),
                    rel_path: rel_path.clone(),
                    kind: FileEntryKind::Deleted,
                    hash: None,
                    size: 0,
                    mtime_secs: 0,
                    mtime_nanos: 0,
                    recorded_at: now_ts.clone(),
                    source: options.source,
                    turn_id: options.turn_id.clone(),
                    actor: options.actor.clone(),
                    oversized: false,
                    symlink_target: None,
                    pinned: options.pinned,
                    note: options.note.clone(),
                };
                pending_lines.push(IndexLine::Snapshot(Box::new(rec)));
                folder_entries.remove(&rel_path);
                summary.deleted_files = summary.deleted_files.saturating_add(1);
                summary.recorded_snapshots = summary.recorded_snapshots.saturating_add(1);
            }
        }
    }

    if !pending_lines.is_empty() {
        append_index_lines_sync(history_dir, &pending_lines)?;
    }
    write_scan_state_sync(history_dir, &scan_state)?;
    update_live_counters(live_status, &summary);
    Ok(summary)
}

fn update_live_counters(
    live_status: Option<&Arc<Mutex<FileHistoryScanStatus>>>,
    summary: &ScanSummary,
) {
    let Some(status_lock) = live_status else {
        return;
    };
    if let Ok(mut guard) = status_lock.lock() {
        guard.visited_files = summary.visited_files;
        guard.hashed_files = summary.hashed_files;
        guard.recorded_snapshots = summary.recorded_snapshots;
        guard.skipped_ignored = summary.skipped_ignored;
        guard.skipped_oversized = summary.skipped_oversized;
        guard.skipped_over_budget = summary.skipped_over_budget;
        guard.deleted_files = summary.deleted_files;
        guard.cancelled = summary.cancelled;
    }
}

pub(crate) fn snapshot_single_file_sync(
    history_dir: &Path,
    folder: &WorkspaceFolder,
    raw_rel_path: &str,
    max_file_bytes: u64,
    under_pressure: bool,
    options: &ScanOptions,
) -> AppHostResult<(Option<FileSnapshotRecord>, u64)> {
    let rel_path = normalize_rel_path(raw_rel_path)?;
    let gitignore = load_compiled_ignore_sync(history_dir)?;
    if is_rel_path_ignored(&gitignore, &rel_path, false) {
        return Ok((None, 0));
    }

    let protected = is_snapshot_protected(options.source, options.pinned);
    if under_pressure && !protected {
        return Ok((None, 0));
    }

    let folder_root = Path::new(folder.path());
    let canonical_root = std::fs::canonicalize(folder_root).map_err(|err| AppHostError::Io {
        path: folder_root.to_path_buf(),
        message: err.to_string(),
    })?;
    verify_no_escaping_intermediate_symlinks(folder_root, &canonical_root, &rel_path)?;

    let target_path = folder_root.join(&rel_path);
    let meta = match std::fs::symlink_metadata(&target_path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, 0));
        }
        Err(err) => {
            return Err(AppHostError::Io {
                path: target_path,
                message: err.to_string(),
            });
        }
    };

    let now_ts = Timestamp::new(jiff::Timestamp::now().to_string());
    let (mtime_secs, mtime_nanos) = metadata_mtime(&meta);
    let mut scan_state = read_scan_state_sync(history_dir);
    let folder_key = folder.id().as_str().to_owned();

    let (record, cached, added_bytes) = if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&target_path)
            .map_err(|err| AppHostError::Io {
                path: target_path.clone(),
                message: err.to_string(),
            })?
            .to_string_lossy()
            .into_owned();
        let rec = FileSnapshotRecord {
            id: new_snapshot_id(next_id_nonce()),
            folder_id: folder.id().clone(),
            rel_path: rel_path.clone(),
            kind: FileEntryKind::Symlink,
            hash: None,
            size: 0,
            mtime_secs,
            mtime_nanos,
            recorded_at: now_ts,
            source: options.source,
            turn_id: options.turn_id.clone(),
            actor: options.actor.clone(),
            oversized: false,
            symlink_target: Some(target.clone()),
            pinned: options.pinned,
            note: options.note.clone(),
        };
        let cached = CachedFileState {
            kind: FileEntryKind::Symlink,
            size: 0,
            mtime_secs,
            mtime_nanos,
            hash: None,
            oversized: false,
            symlink_target: Some(target),
        };
        (rec, cached, 0)
    } else if meta.is_file() {
        let size = meta.len();
        if size > max_file_bytes {
            let rec = FileSnapshotRecord {
                id: new_snapshot_id(next_id_nonce()),
                folder_id: folder.id().clone(),
                rel_path: rel_path.clone(),
                kind: FileEntryKind::File,
                hash: None,
                size,
                mtime_secs,
                mtime_nanos,
                recorded_at: now_ts,
                source: options.source,
                turn_id: options.turn_id.clone(),
                actor: options.actor.clone(),
                oversized: true,
                symlink_target: None,
                pinned: options.pinned,
                note: options.note.clone(),
            };
            let cached = CachedFileState {
                kind: FileEntryKind::File,
                size,
                mtime_secs,
                mtime_nanos,
                hash: None,
                oversized: true,
                symlink_target: None,
            };
            (rec, cached, 0)
        } else {
            let prev = scan_state
                .entries
                .get(&folder_key)
                .and_then(|m| m.get(&rel_path));
            let cache_hit = prev.filter(|c| {
                c.kind == FileEntryKind::File
                    && !c.oversized
                    && c.size == size
                    && c.mtime_secs == mtime_secs
                    && c.mtime_nanos == mtime_nanos
                    && c.hash
                        .as_deref()
                        .is_some_and(|h| blob_path(history_dir, h).is_file())
            });
            let (hash, actual_size, added) = if let Some(cached) = cache_hit {
                (
                    cached.hash.clone().expect("cache hit has hash"),
                    cached.size,
                    0,
                )
            } else {
                store_blob_sync(history_dir, &target_path)?
            };
            let rec = FileSnapshotRecord {
                id: new_snapshot_id(next_id_nonce()),
                folder_id: folder.id().clone(),
                rel_path: rel_path.clone(),
                kind: FileEntryKind::File,
                hash: Some(hash.clone()),
                size: actual_size,
                mtime_secs,
                mtime_nanos,
                recorded_at: now_ts,
                source: options.source,
                turn_id: options.turn_id.clone(),
                actor: options.actor.clone(),
                oversized: false,
                symlink_target: None,
                pinned: options.pinned,
                note: options.note.clone(),
            };
            let cached = CachedFileState {
                kind: FileEntryKind::File,
                size,
                mtime_secs,
                mtime_nanos,
                hash: Some(hash),
                oversized: false,
                symlink_target: None,
            };
            (rec, cached, added)
        }
    } else {
        return Err(file_history_caller_error(
            "NOT_A_FILE",
            format!("path `{rel_path}` is not a regular file or symlink"),
        ));
    };

    append_index_lines_sync(
        history_dir,
        &[IndexLine::Snapshot(Box::new(record.clone()))],
    )?;
    scan_state
        .entries
        .entry(folder_key)
        .or_default()
        .insert(rel_path, cached);
    write_scan_state_sync(history_dir, &scan_state)?;

    Ok((Some(record), added_bytes))
}

pub(crate) fn verify_no_escaping_intermediate_symlinks(
    folder_root: &Path,
    canonical_root: &Path,
    rel_path: &str,
) -> AppHostResult<()> {
    let segments: Vec<&str> = rel_path.split('/').collect();
    let mut current = folder_root.to_path_buf();
    for segment in &segments[..segments.len().saturating_sub(1)] {
        current.push(segment);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    let resolved = std::fs::canonicalize(&current).map_err(|err| {
                        file_history_caller_error(
                            "SYMLINK_ESCAPE",
                            format!(
                                "intermediate symlink `{}` could not be resolved: {err}",
                                current.display()
                            ),
                        )
                    })?;
                    if !resolved.starts_with(canonical_root) {
                        return Err(file_history_caller_error(
                            "SYMLINK_ESCAPE",
                            format!(
                                "intermediate symlink `{}` escapes workspace folder root `{}`",
                                current.display(),
                                canonical_root.display()
                            ),
                        ));
                    }
                } else if !meta.is_dir() {
                    return Err(file_history_caller_error(
                        "INVALID_PARENT",
                        format!("parent `{}` is not a directory", current.display()),
                    ));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => {
                return Err(AppHostError::Io {
                    path: current,
                    message: err.to_string(),
                });
            }
        }
    }
    Ok(())
}
