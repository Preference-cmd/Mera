use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use mera_core::command::CommandActor;
use mera_core::event::Timestamp;
use mera_core::model::FileSnapshotId;
use mera_core::project::WorkspaceFolder;

use super::scanner::{new_snapshot_id, verify_no_escaping_intermediate_symlinks};
use super::store::{
    append_index_lines_sync, blob_path, make_writable_perms, read_index_records_sync,
    read_scan_state_sync, remove_file_sync, store_blob_sync, write_scan_state_sync,
};
use super::types::{
    CachedFileState, FileEntryKind, FileSnapshotRecord, IndexLine, RestoreOutcome, SnapshotSource,
    TrashMover, file_history_caller_error, file_history_internal_error, metadata_mtime,
    normalize_rel_path,
};
use crate::{AppHostError, AppHostResult};

pub(crate) fn restore_snapshot_sync(
    history_dir: &Path,
    folders: &[WorkspaceFolder],
    snapshot_id: &FileSnapshotId,
    max_file_bytes: u64,
    actor: Option<CommandActor>,
    trash_mover: &dyn TrashMover,
) -> AppHostResult<(RestoreOutcome, u64)> {
    let records = read_index_records_sync(history_dir)?;
    let record = records
        .into_iter()
        .find(|r| &r.id == snapshot_id)
        .ok_or_else(|| {
            file_history_caller_error(
                "SNAPSHOT_NOT_FOUND",
                format!("unknown file snapshot `{snapshot_id}`"),
            )
        })?;

    if record.oversized {
        return Err(file_history_caller_error(
            "OVERSIZED_NOT_RESTORABLE",
            format!(
                "file `{}` ({} bytes) exceeded the maximum size limit and has no stored content",
                record.rel_path, record.size
            ),
        ));
    }

    let folder = folders
        .iter()
        .find(|f| f.id() == &record.folder_id)
        .ok_or_else(|| {
            file_history_caller_error(
                "UNKNOWN_FOLDER",
                format!(
                    "workspace folder `{}` is not registered on the project",
                    record.folder_id
                ),
            )
        })?;

    let rel_path = normalize_rel_path(&record.rel_path)?;
    let folder_root = Path::new(folder.path());
    if !folder_root.is_dir() {
        return Err(file_history_caller_error(
            "FOLDER_MISSING",
            format!(
                "workspace folder root `{}` does not exist or is not a directory",
                folder_root.display()
            ),
        ));
    }
    let canonical_root = std::fs::canonicalize(folder_root).map_err(|err| AppHostError::Io {
        path: folder_root.to_path_buf(),
        message: err.to_string(),
    })?;
    verify_no_escaping_intermediate_symlinks(folder_root, &canonical_root, &rel_path)?;

    // Verify source content exists BEFORE mutating any workspace file.
    let source_blob_path = match record.kind {
        FileEntryKind::File => {
            let hash = record.hash.as_deref().ok_or_else(|| {
                file_history_internal_error(
                    "BLOB_MISSING",
                    format!("snapshot `{snapshot_id}` has no content hash"),
                )
            })?;
            let path = blob_path(history_dir, hash);
            if !path.is_file() {
                return Err(file_history_internal_error(
                    "BLOB_MISSING",
                    format!(
                        "snapshot blob `{hash}` for `{rel_path}` is missing from history store"
                    ),
                ));
            }
            Some(path)
        }
        FileEntryKind::Symlink => {
            if record.symlink_target.is_none() {
                return Err(file_history_internal_error(
                    "SYMLINK_TARGET_MISSING",
                    format!("symlink snapshot `{snapshot_id}` has no recorded target"),
                ));
            }
            None
        }
        FileEntryKind::Deleted => None,
    };

    let target_path = folder_root.join(&rel_path);

    // Inspect current on-disk state and capture a PreRestore snapshot before mutating.
    let (pre_restore_record, added_blob_bytes) = capture_pre_restore_record(
        history_dir,
        folder,
        &rel_path,
        &target_path,
        max_file_bytes,
        actor,
    )?;
    let pre_restore_id = pre_restore_record.id.clone();

    // Perform the workspace restore mutation.
    let mut moved_to_trash = false;
    let mut scan_state = read_scan_state_sync(history_dir);
    let folder_key = folder.id().as_str().to_owned();

    match record.kind {
        FileEntryKind::File => {
            let blob = source_blob_path.as_deref().expect("verified blob path");
            let parent = target_path.parent().ok_or_else(|| {
                file_history_internal_error("INVALID_TARGET", "target path has no parent")
            })?;
            std::fs::create_dir_all(parent).map_err(|err| AppHostError::Io {
                path: parent.to_path_buf(),
                message: err.to_string(),
            })?;

            // If target currently exists as a directory, refuse to overwrite.
            if let Ok(meta) = std::fs::symlink_metadata(&target_path)
                && !meta.file_type().is_symlink()
                && meta.is_dir()
            {
                return Err(file_history_caller_error(
                    "TARGET_IS_DIRECTORY",
                    format!(
                        "cannot restore file over existing directory `{}`",
                        target_path.display()
                    ),
                ));
            }

            atomic_restore_blob_to_workspace(blob, &target_path)?;
            let new_meta =
                std::fs::symlink_metadata(&target_path).map_err(|err| AppHostError::Io {
                    path: target_path.clone(),
                    message: err.to_string(),
                })?;
            let (mtime_secs, mtime_nanos) = metadata_mtime(&new_meta);
            scan_state.entries.entry(folder_key).or_default().insert(
                rel_path.clone(),
                CachedFileState {
                    kind: FileEntryKind::File,
                    size: new_meta.len(),
                    mtime_secs,
                    mtime_nanos,
                    hash: record.hash.clone(),
                    oversized: false,
                    symlink_target: None,
                },
            );
        }
        FileEntryKind::Deleted => {
            match std::fs::symlink_metadata(&target_path) {
                Ok(meta) => {
                    if !meta.file_type().is_symlink() && meta.is_dir() {
                        return Err(file_history_caller_error(
                            "TARGET_IS_DIRECTORY",
                            format!(
                                "cannot move directory `{}` to trash during single-file restore",
                                target_path.display()
                            ),
                        ));
                    }
                    trash_mover.move_to_trash(&target_path).map_err(|err| {
                        file_history_internal_error(
                            "TRASH_FAILED",
                            format!("failed to move `{}` to trash: {err}", target_path.display()),
                        )
                    })?;
                    moved_to_trash = true;
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(AppHostError::Io {
                        path: target_path,
                        message: err.to_string(),
                    });
                }
            }
            if let Some(entries) = scan_state.entries.get_mut(&folder_key) {
                entries.remove(&rel_path);
            }
        }
        FileEntryKind::Symlink => {
            let link_target = record
                .symlink_target
                .as_deref()
                .expect("verified symlink target");
            let parent = target_path.parent().ok_or_else(|| {
                file_history_internal_error("INVALID_TARGET", "target path has no parent")
            })?;
            std::fs::create_dir_all(parent).map_err(|err| AppHostError::Io {
                path: parent.to_path_buf(),
                message: err.to_string(),
            })?;

            match std::fs::symlink_metadata(&target_path) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        std::fs::remove_file(&target_path).map_err(|err| AppHostError::Io {
                            path: target_path.clone(),
                            message: err.to_string(),
                        })?;
                    } else if meta.is_dir() {
                        return Err(file_history_caller_error(
                            "TARGET_IS_DIRECTORY",
                            format!(
                                "cannot replace directory `{}` with symlink",
                                target_path.display()
                            ),
                        ));
                    } else {
                        trash_mover.move_to_trash(&target_path).map_err(|err| {
                            file_history_internal_error(
                                "TRASH_FAILED",
                                format!(
                                    "failed to move `{}` to trash: {err}",
                                    target_path.display()
                                ),
                            )
                        })?;
                        moved_to_trash = true;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(AppHostError::Io {
                        path: target_path,
                        message: err.to_string(),
                    });
                }
            }

            create_symlink_sync(link_target, &target_path)?;
            let new_meta =
                std::fs::symlink_metadata(&target_path).map_err(|err| AppHostError::Io {
                    path: target_path.clone(),
                    message: err.to_string(),
                })?;
            let (mtime_secs, mtime_nanos) = metadata_mtime(&new_meta);
            scan_state.entries.entry(folder_key).or_default().insert(
                rel_path.clone(),
                CachedFileState {
                    kind: FileEntryKind::Symlink,
                    size: 0,
                    mtime_secs,
                    mtime_nanos,
                    hash: None,
                    oversized: false,
                    symlink_target: Some(link_target.to_owned()),
                },
            );
        }
    }

    append_index_lines_sync(
        history_dir,
        &[IndexLine::Snapshot(Box::new(pre_restore_record))],
    )?;
    write_scan_state_sync(history_dir, &scan_state)?;

    Ok((
        RestoreOutcome {
            restored_snapshot_id: record.id,
            pre_restore_snapshot_id: pre_restore_id,
            folder_id: record.folder_id,
            rel_path,
            kind: record.kind,
            moved_to_trash,
        },
        added_blob_bytes,
    ))
}

fn capture_pre_restore_record(
    history_dir: &Path,
    folder: &WorkspaceFolder,
    rel_path: &str,
    target_path: &Path,
    max_file_bytes: u64,
    actor: Option<CommandActor>,
) -> AppHostResult<(FileSnapshotRecord, u64)> {
    static PRE_RESTORE_NONCE: AtomicU64 = AtomicU64::new(1);
    let nonce = PRE_RESTORE_NONCE.fetch_add(1, Ordering::Relaxed);
    let now_ts = Timestamp::new(jiff::Timestamp::now().to_string());

    match std::fs::symlink_metadata(target_path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok((
            FileSnapshotRecord {
                id: new_snapshot_id(nonce),
                folder_id: folder.id().clone(),
                rel_path: rel_path.to_owned(),
                kind: FileEntryKind::Deleted,
                hash: None,
                size: 0,
                mtime_secs: 0,
                mtime_nanos: 0,
                recorded_at: now_ts,
                source: SnapshotSource::PreRestore,
                turn_id: None,
                actor,
                oversized: false,
                symlink_target: None,
                pinned: false,
                note: None,
            },
            0,
        )),
        Err(err) => Err(AppHostError::Io {
            path: target_path.to_path_buf(),
            message: err.to_string(),
        }),
        Ok(meta) if meta.file_type().is_symlink() => {
            let (mtime_secs, mtime_nanos) = metadata_mtime(&meta);
            let link_target = std::fs::read_link(target_path)
                .map_err(|err| AppHostError::Io {
                    path: target_path.to_path_buf(),
                    message: err.to_string(),
                })?
                .to_string_lossy()
                .into_owned();
            Ok((
                FileSnapshotRecord {
                    id: new_snapshot_id(nonce),
                    folder_id: folder.id().clone(),
                    rel_path: rel_path.to_owned(),
                    kind: FileEntryKind::Symlink,
                    hash: None,
                    size: 0,
                    mtime_secs,
                    mtime_nanos,
                    recorded_at: now_ts,
                    source: SnapshotSource::PreRestore,
                    turn_id: None,
                    actor,
                    oversized: false,
                    symlink_target: Some(link_target),
                    pinned: false,
                    note: None,
                },
                0,
            ))
        }
        Ok(meta) if meta.is_file() => {
            let (mtime_secs, mtime_nanos) = metadata_mtime(&meta);
            let size = meta.len();
            if size > max_file_bytes {
                Ok((
                    FileSnapshotRecord {
                        id: new_snapshot_id(nonce),
                        folder_id: folder.id().clone(),
                        rel_path: rel_path.to_owned(),
                        kind: FileEntryKind::File,
                        hash: None,
                        size,
                        mtime_secs,
                        mtime_nanos,
                        recorded_at: now_ts,
                        source: SnapshotSource::PreRestore,
                        turn_id: None,
                        actor,
                        oversized: true,
                        symlink_target: None,
                        pinned: false,
                        note: None,
                    },
                    0,
                ))
            } else {
                let (hash, actual_size, added_bytes) = store_blob_sync(history_dir, target_path)?;
                Ok((
                    FileSnapshotRecord {
                        id: new_snapshot_id(nonce),
                        folder_id: folder.id().clone(),
                        rel_path: rel_path.to_owned(),
                        kind: FileEntryKind::File,
                        hash: Some(hash),
                        size: actual_size,
                        mtime_secs,
                        mtime_nanos,
                        recorded_at: now_ts,
                        source: SnapshotSource::PreRestore,
                        turn_id: None,
                        actor,
                        oversized: false,
                        symlink_target: None,
                        pinned: false,
                        note: None,
                    },
                    added_bytes,
                ))
            }
        }
        Ok(_) => Err(file_history_caller_error(
            "TARGET_IS_DIRECTORY",
            format!(
                "cannot capture pre-restore snapshot for non-file `{}`",
                target_path.display()
            ),
        )),
    }
}

fn atomic_restore_blob_to_workspace(blob: &Path, target: &Path) -> AppHostResult<()> {
    let parent = target.parent().ok_or_else(|| {
        file_history_internal_error("INVALID_TARGET", "target path has no parent")
    })?;
    static RESTORE_TMP_NONCE: AtomicU64 = AtomicU64::new(1);
    let nonce = RESTORE_TMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let nanos = jiff::Timestamp::now().as_nanosecond();
    let tmp_path = parent.join(format!(".mera-restore-{nanos:x}-{nonce:x}.tmp"));

    if let Err(err) = reflink_copy::reflink_or_copy(blob, &tmp_path) {
        let _ = remove_file_sync(&tmp_path);
        return Err(AppHostError::Io {
            path: target.to_path_buf(),
            message: format!("failed to stage restored file: {err}"),
        });
    }

    if let Ok(meta) = std::fs::symlink_metadata(&tmp_path)
        && meta.permissions().readonly()
        && let Err(err) =
            std::fs::set_permissions(&tmp_path, make_writable_perms(meta.permissions()))
    {
        let _ = remove_file_sync(&tmp_path);
        return Err(AppHostError::Io {
            path: target.to_path_buf(),
            message: format!("failed to make restored file writable: {err}"),
        });
    }

    if let Err(err) = std::fs::rename(&tmp_path, target) {
        let _ = remove_file_sync(&tmp_path);
        return Err(AppHostError::Io {
            path: target.to_path_buf(),
            message: format!("failed to commit restored file: {err}"),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn create_symlink_sync(link_target: &str, link_path: &Path) -> AppHostResult<()> {
    std::os::unix::fs::symlink(link_target, link_path).map_err(|err| AppHostError::Io {
        path: link_path.to_path_buf(),
        message: format!("failed to create symlink: {err}"),
    })
}

#[cfg(not(unix))]
fn create_symlink_sync(link_target: &str, link_path: &Path) -> AppHostResult<()> {
    std::os::windows::fs::symlink_file(link_target, link_path).map_err(|err| AppHostError::Io {
        path: link_path.to_path_buf(),
        message: format!("failed to create symlink: {err}"),
    })
}
