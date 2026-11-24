use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use sha2::{Digest, Sha256};

use super::types::{
    FileSnapshotRecord, IndexLine, ScanStateDoc, file_history_caller_error,
    file_history_internal_error,
};
use crate::{AppHostError, AppHostResult};

pub(crate) const INDEX_FILENAME: &str = "index.jsonl";
pub(crate) const SCAN_STATE_FILENAME: &str = "scan-state.json";
pub(crate) const IGNORE_FILENAME: &str = "ignore.txt";
pub(crate) const BLOBS_DIRNAME: &str = "blobs";

pub(crate) fn blobs_dir(history_dir: &Path) -> PathBuf {
    history_dir.join(BLOBS_DIRNAME)
}

pub(crate) fn blob_path(history_dir: &Path, hash: &str) -> PathBuf {
    let prefix = if hash.len() >= 2 { &hash[0..2] } else { "xx" };
    blobs_dir(history_dir).join(prefix).join(hash)
}

pub(crate) fn index_path(history_dir: &Path) -> PathBuf {
    history_dir.join(INDEX_FILENAME)
}

pub(crate) fn scan_state_path(history_dir: &Path) -> PathBuf {
    history_dir.join(SCAN_STATE_FILENAME)
}

pub(crate) fn ignore_rules_path(history_dir: &Path) -> PathBuf {
    history_dir.join(IGNORE_FILENAME)
}

#[cfg(unix)]
pub(crate) fn make_writable_perms(perms: std::fs::Permissions) -> std::fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = perms;
    perms.set_mode(perms.mode() | 0o200);
    perms
}

#[cfg(not(unix))]
pub(crate) fn make_writable_perms(perms: std::fs::Permissions) -> std::fs::Permissions {
    let mut perms = perms;
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    perms
}

pub(crate) fn remove_file_sync(path: &Path) -> bool {
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && meta.permissions().readonly()
    {
        let _ = std::fs::set_permissions(path, make_writable_perms(meta.permissions()));
    }
    std::fs::remove_file(path).is_ok()
}

pub(crate) fn atomic_write_sync(path: &Path, bytes: &[u8]) -> AppHostResult<()> {
    let parent = path.parent().ok_or_else(|| AppHostError::Io {
        path: path.to_path_buf(),
        message: "target path has no parent directory".to_owned(),
    })?;
    std::fs::create_dir_all(parent).map_err(|err| AppHostError::Io {
        path: parent.to_path_buf(),
        message: err.to_string(),
    })?;

    static TMP_NONCE: AtomicU64 = AtomicU64::new(1);
    let nonce = TMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let nanos = jiff::Timestamp::now().as_nanosecond();
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("tmp");
    let tmp_path = parent.join(format!(".{file_name}.tmp-{nanos:x}-{nonce:x}"));

    let write_res = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = remove_file_sync(&tmp_path);
        return Err(AppHostError::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        });
    }

    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = remove_file_sync(&tmp_path);
        return Err(AppHostError::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        });
    }
    Ok(())
}

/// Stream-hash `path` and ingest it into `history_dir/blobs/<xx>/<sha256>` using
/// `reflink_or_copy` to a temp file, atomic rename, and read-only permissions.
///
/// Returns `(sha256_hex, size, newly_written_blob_bytes)`.
pub(crate) fn store_blob_sync(
    history_dir: &Path,
    source_path: &Path,
) -> AppHostResult<(String, u64, u64)> {
    let blobs_root = blobs_dir(history_dir);
    std::fs::create_dir_all(&blobs_root).map_err(|err| AppHostError::Io {
        path: blobs_root.clone(),
        message: err.to_string(),
    })?;

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(1);
    let tmp_count = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_nanos = jiff::Timestamp::now().as_nanosecond();
    let tmp_file = blobs_root.join(format!(".tmp-{tmp_nanos:x}-{tmp_count:x}"));

    if let Err(err) = reflink_copy::reflink_or_copy(source_path, &tmp_file) {
        let _ = remove_file_sync(&tmp_file);
        return Err(AppHostError::Io {
            path: source_path.to_path_buf(),
            message: format!("failed to copy snapshot blob: {err}"),
        });
    }

    let (hash, size) = match stream_hash_and_size_sync(&tmp_file) {
        Ok(res) => res,
        Err(err) => {
            let _ = remove_file_sync(&tmp_file);
            return Err(err);
        }
    };

    let dest = blob_path(history_dir, &hash);
    if dest.exists() {
        let _ = remove_file_sync(&tmp_file);
        return Ok((hash, size, 0));
    }

    let Some(parent) = dest.parent() else {
        let _ = remove_file_sync(&tmp_file);
        return Err(file_history_internal_error(
            "IO",
            format!("blob destination has no parent: `{}`", dest.display()),
        ));
    };
    if let Err(err) = std::fs::create_dir_all(parent) {
        let _ = remove_file_sync(&tmp_file);
        return Err(AppHostError::Io {
            path: parent.to_path_buf(),
            message: err.to_string(),
        });
    }

    match std::fs::rename(&tmp_file, &dest) {
        Ok(()) => {
            if let Ok(meta) = std::fs::metadata(&dest) {
                let mut perms = meta.permissions();
                perms.set_readonly(true);
                let _ = std::fs::set_permissions(&dest, perms);
            }
            Ok((hash, size, size))
        }
        Err(err) => {
            let _ = remove_file_sync(&tmp_file);
            if dest.exists() {
                Ok((hash, size, 0))
            } else {
                Err(AppHostError::Io {
                    path: dest,
                    message: format!("failed to commit blob: {err}"),
                })
            }
        }
    }
}

pub(crate) fn stream_hash_and_size_sync(path: &Path) -> AppHostResult<(String, u64)> {
    let mut file = std::fs::File::open(path).map_err(|err| AppHostError::Io {
        path: path.to_path_buf(),
        message: format!("failed to open for hashing: {err}"),
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 16 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf).map_err(|err| AppHostError::Io {
            path: path.to_path_buf(),
            message: format!("failed to read for hashing: {err}"),
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size = size.saturating_add(n as u64);
    }
    Ok((format!("{:x}", hasher.finalize()), size))
}

fn terminate_partial_line_sync(file: &mut std::fs::File, path: &Path) -> AppHostResult<()> {
    let len = file
        .metadata()
        .map_err(|err| AppHostError::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?
        .len();
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))
        .map_err(|err| AppHostError::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last).map_err(|err| AppHostError::Io {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    if last[0] != b'\n' {
        file.write_all(b"\n").map_err(|err| AppHostError::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
    }
    Ok(())
}

pub(crate) fn append_index_lines_sync(
    history_dir: &Path,
    lines: &[IndexLine],
) -> AppHostResult<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let path = index_path(history_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| AppHostError::Io {
            path: parent.to_path_buf(),
            message: err.to_string(),
        })?;
    }

    let mut payload = String::new();
    for entry in lines {
        let line = serde_json::to_string(entry).map_err(|err| {
            file_history_internal_error(
                "SERIALIZATION",
                format!("failed to encode index line: {err}"),
            )
        })?;
        payload.push_str(&line);
        payload.push('\n');
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&path)
        .map_err(|err| AppHostError::Io {
            path: path.clone(),
            message: err.to_string(),
        })?;
    terminate_partial_line_sync(&mut file, &path)?;
    file.write_all(payload.as_bytes())
        .map_err(|err| AppHostError::Io {
            path: path.clone(),
            message: err.to_string(),
        })?;
    file.flush().map_err(|err| AppHostError::Io {
        path,
        message: err.to_string(),
    })?;
    Ok(())
}

/// Read `index.jsonl` and materialize pin updates onto snapshot records in append order.
///
/// A trailing line without a newline is ignored as an interrupted append, and
/// unparseable lines are skipped.
pub(crate) fn read_index_records_sync(
    history_dir: &Path,
) -> AppHostResult<Vec<FileSnapshotRecord>> {
    let path = index_path(history_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path).map_err(|err| AppHostError::Io {
        path: path.clone(),
        message: err.to_string(),
    })?;
    let completed = match content.rfind('\n') {
        Some(last_nl) => &content[..=last_nl],
        None => "",
    };

    let mut records: Vec<FileSnapshotRecord> = Vec::new();
    let mut index_by_id: HashMap<String, usize> = HashMap::new();

    for line in completed.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<IndexLine>(trimmed) else {
            continue;
        };
        match entry {
            IndexLine::Snapshot(record) => {
                let idx = records.len();
                index_by_id.insert(record.id.as_str().to_owned(), idx);
                records.push(*record);
            }
            IndexLine::SetPin { id, pinned, .. } => {
                if let Some(&idx) = index_by_id.get(id.as_str()) {
                    records[idx].pinned = pinned;
                }
            }
        }
    }

    Ok(records)
}

/// Atomically rewrite compacted `index.jsonl` containing only `records`.
pub(crate) fn rewrite_compacted_index_sync(
    history_dir: &Path,
    records: &[FileSnapshotRecord],
) -> AppHostResult<()> {
    let path = index_path(history_dir);
    if records.is_empty() && !path.exists() {
        return Ok(());
    }
    let mut bytes = Vec::new();
    for rec in records {
        let line =
            serde_json::to_string(&IndexLine::Snapshot(Box::new(rec.clone()))).map_err(|err| {
                file_history_internal_error(
                    "SERIALIZATION",
                    format!("failed to serialize record: {err}"),
                )
            })?;
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    atomic_write_sync(&path, &bytes)
}

pub(crate) fn read_scan_state_sync(history_dir: &Path) -> ScanStateDoc {
    let path = scan_state_path(history_dir);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return ScanStateDoc {
            schema_version: 1,
            entries: Default::default(),
        };
    };
    serde_json::from_str::<ScanStateDoc>(&raw).unwrap_or(ScanStateDoc {
        schema_version: 1,
        entries: Default::default(),
    })
}

pub(crate) fn write_scan_state_sync(history_dir: &Path, state: &ScanStateDoc) -> AppHostResult<()> {
    let path = scan_state_path(history_dir);
    let bytes = serde_json::to_vec(state).map_err(|err| {
        file_history_internal_error(
            "SERIALIZATION",
            format!("failed to serialize scan state: {err}"),
        )
    })?;
    atomic_write_sync(&path, &bytes)
}

pub(crate) fn read_ignore_rules_sync(history_dir: &Path) -> AppHostResult<String> {
    let path = ignore_rules_path(history_dir);
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(AppHostError::Io {
            path,
            message: err.to_string(),
        }),
    }
}

pub(crate) fn write_ignore_rules_sync(history_dir: &Path, rules: &str) -> AppHostResult<()> {
    compile_ignore_rules(rules)?;
    let path = ignore_rules_path(history_dir);
    atomic_write_sync(&path, rules.as_bytes())
}

pub(crate) fn compile_ignore_rules(rules: &str) -> AppHostResult<Gitignore> {
    let mut builder = GitignoreBuilder::new("");
    for line in rules.lines() {
        builder.add_line(None, line).map_err(|err| {
            file_history_caller_error("INVALID_IGNORE_RULE", format!("invalid ignore rule: {err}"))
        })?;
    }
    builder.build().map_err(|err| {
        file_history_caller_error("INVALID_IGNORE_RULE", format!("invalid ignore rule: {err}"))
    })
}

pub(crate) fn load_compiled_ignore_sync(history_dir: &Path) -> AppHostResult<Gitignore> {
    let rules = read_ignore_rules_sync(history_dir)?;
    compile_ignore_rules(&rules)
}

/// Delete all blob files under `history_dir/blobs/` whose hashes are not in `keep_hashes`,
/// and remove empty shard directories. Returns `(removed_blobs, freed_bytes)`.
pub(crate) fn gc_orphan_blobs_sync(
    history_dir: &Path,
    keep_hashes: &HashSet<String>,
) -> (usize, u64) {
    let root = blobs_dir(history_dir);
    let Ok(shards) = std::fs::read_dir(&root) else {
        return (0, 0);
    };
    let mut removed_blobs = 0usize;
    let mut freed_bytes = 0u64;

    for shard_entry in shards.flatten() {
        let shard_path = shard_entry.path();
        let Ok(shard_ft) = shard_entry.file_type() else {
            continue;
        };
        if shard_ft.is_symlink() {
            continue;
        }
        if !shard_ft.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&shard_path) else {
            continue;
        };
        for file_entry in files.flatten() {
            let file_path = file_entry.path();
            let Ok(file_ft) = file_entry.file_type() else {
                continue;
            };
            if file_ft.is_symlink() || !file_ft.is_file() {
                continue;
            }
            let hash_name = file_entry.file_name();
            let hash_str = hash_name.to_string_lossy();
            if keep_hashes.contains(hash_str.as_ref()) {
                continue;
            }
            let size = file_entry.metadata().map(|m| m.len()).unwrap_or(0);
            if remove_file_sync(&file_path) {
                removed_blobs += 1;
                freed_bytes = freed_bytes.saturating_add(size);
            }
        }
        if std::fs::read_dir(&shard_path)
            .map(|mut rd| rd.next().is_none())
            .unwrap_or(false)
        {
            let _ = std::fs::remove_dir(&shard_path);
        }
    }

    (removed_blobs, freed_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_history::types::{FileEntryKind, SnapshotSource};
    use mera_core::event::Timestamp;
    use mera_core::model::{FileSnapshotId, WorkspaceFolderId};

    #[test]
    fn index_jsonl_repairs_torn_tail_on_append_and_ignores_incomplete_trailing_line() {
        let temp = tempfile::tempdir().unwrap();
        let history_dir = temp.path().join("history");
        std::fs::create_dir_all(&history_dir).unwrap();

        // Simulate crash mid-append without trailing newline.
        std::fs::write(
            index_path(&history_dir),
            b"{\"op\":\"snapshot\",\"id\":\"snap-torn\"",
        )
        .unwrap();

        assert!(read_index_records_sync(&history_dir).unwrap().is_empty());

        let rec = FileSnapshotRecord {
            id: FileSnapshotId::new("snap-good-1"),
            folder_id: WorkspaceFolderId::new("folder-1"),
            rel_path: "a.txt".to_owned(),
            kind: FileEntryKind::File,
            hash: Some("deadbeef".to_owned()),
            size: 12,
            mtime_secs: 100,
            mtime_nanos: 0,
            recorded_at: Timestamp::new("2026-11-24T00:00:00Z"),
            source: SnapshotSource::Manual,
            turn_id: None,
            actor: None,
            oversized: false,
            symlink_target: None,
            pinned: false,
            note: None,
        };
        append_index_lines_sync(&history_dir, &[IndexLine::Snapshot(Box::new(rec.clone()))])
            .unwrap();
        append_index_lines_sync(
            &history_dir,
            &[IndexLine::SetPin {
                id: rec.id.clone(),
                pinned: true,
                updated_at: Timestamp::new("2026-11-24T00:00:01Z"),
            }],
        )
        .unwrap();

        let records = read_index_records_sync(&history_dir).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, rec.id);
        assert!(records[0].pinned);
    }
}
