use std::collections::HashSet;
use std::fmt;
use std::io::Read;
use std::path::{Component, Path};

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

/// Limits enforced when decompressing and extracting `.tar.gz` archives.
#[derive(Clone, Debug)]
pub struct ExtractionLimits {
    pub max_entries: usize,
    pub max_expanded_size: u64,
    pub max_entry_size: u64,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_expanded_size: 2 * 1024 * 1024 * 1024, // 2 GiB
            max_entry_size: 512 * 1024 * 1024,         // 512 MiB
        }
    }
}

#[derive(Debug)]
pub enum ArchiveError {
    EntryCountLimit { max: usize, actual: usize },
    ExpandedSizeLimit { max: u64, actual: u64 },
    InvalidPath { entry: String },
    PathTraversal { entry: String },
    SymlinkRejected { entry: String },
    HardlinkRejected { entry: String },
    DuplicatePath { entry: String },
    UnexpectedFileType { entry: String, kind: String },
    HashMismatch { entry: String },
    Io { message: String },
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EntryCountLimit { max, actual } => {
                write!(f, "archive entry count {actual} exceeds limit {max}")
            }
            Self::ExpandedSizeLimit { max, actual } => {
                write!(f, "archive expanded size {actual} exceeds limit {max}")
            }
            Self::InvalidPath { entry } => write!(f, "invalid archive entry path: `{entry}`"),
            Self::PathTraversal { entry } => {
                write!(f, "archive entry path traverses outside staging: `{entry}`")
            }
            Self::SymlinkRejected { entry } => {
                write!(f, "symlink not allowed in package archive: `{entry}`")
            }
            Self::HardlinkRejected { entry } => {
                write!(f, "hardlink not allowed in package archive: `{entry}`")
            }
            Self::DuplicatePath { entry } => write!(f, "duplicate archive entry: `{entry}`"),
            Self::UnexpectedFileType { entry, kind } => {
                write!(f, "unexpected {kind} entry in archive: `{entry}`")
            }
            Self::HashMismatch { entry } => {
                write!(f, "hash mismatch for extracted entry: `{entry}`")
            }
            Self::Io { message } => write!(f, "archive I/O error: {message}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

impl From<std::io::Error> for ArchiveError {
    fn from(error: std::io::Error) -> Self {
        Self::Io {
            message: error.to_string(),
        }
    }
}

pub type ArchiveResult<T> = Result<T, ArchiveError>;

#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn verify_file_sha256(path: &Path, expected_sha256: &str) -> ArchiveResult<()> {
    let data = std::fs::read(path)?;
    if sha256_hex(&data) != expected_sha256 {
        return Err(ArchiveError::HashMismatch {
            entry: path.to_string_lossy().into_owned(),
        });
    }
    Ok(())
}

pub fn validate_relative_path(path: &str) -> ArchiveResult<()> {
    let path_ref = Path::new(path);
    if path.is_empty() || path_ref.is_absolute() {
        return Err(ArchiveError::InvalidPath {
            entry: path.to_owned(),
        });
    }
    if path_ref.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(ArchiveError::PathTraversal {
            entry: path.to_owned(),
        });
    }
    Ok(())
}

/// Decompress a gzip stream enforcing `max_expanded_size + archive_metadata_allowance`.
pub fn decompress_gzip(data: &[u8], limits: &ExtractionLimits) -> ArchiveResult<Vec<u8>> {
    const ARCHIVE_METADATA_ALLOWANCE: u64 = 16 * 1024 * 1024;
    let limit = limits
        .max_expanded_size
        .saturating_add(ARCHIVE_METADATA_ALLOWANCE);
    let decoder = GzDecoder::new(data);
    let mut decompressed = Vec::new();
    decoder
        .take(limit.saturating_add(1))
        .read_to_end(&mut decompressed)?;
    if decompressed.len() as u64 > limit {
        return Err(ArchiveError::ExpandedSizeLimit {
            max: limits.max_expanded_size,
            actual: decompressed.len() as u64,
        });
    }
    Ok(decompressed)
}

/// Read the first regular-file entry whose normalized path matches `target_path`.
pub fn read_tar_member(
    tar_bytes: &[u8],
    target_path: &str,
    strip_prefix: Option<&str>,
    max_bytes: u64,
) -> ArchiveResult<Option<Vec<u8>>> {
    let mut archive = tar::Archive::new(tar_bytes);
    for entry in archive.entries()? {
        let entry = entry?;
        let raw_path = entry.path()?.to_string_lossy().into_owned();
        let relative = normalize_entry_path(&raw_path, strip_prefix);
        if relative != target_path || !entry.header().entry_type().is_file() {
            continue;
        }
        let mut content = Vec::new();
        entry
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut content)?;
        if content.len() as u64 > max_bytes {
            return Err(ArchiveError::ExpandedSizeLimit {
                max: max_bytes,
                actual: content.len() as u64,
            });
        }
        return Ok(Some(content));
    }
    Ok(None)
}

/// Extract a raw `.tar` buffer safely into `dest_dir`, skipping `skip_paths`.
pub fn extract_tar_safely(
    tar_bytes: &[u8],
    dest_dir: &Path,
    limits: &ExtractionLimits,
    skip_paths: &[&str],
    strip_prefix: Option<&str>,
) -> ArchiveResult<HashSet<String>> {
    let mut archive = tar::Archive::new(tar_bytes);
    let mut extracted_count = 0usize;
    let mut total_size = 0u64;
    let mut seen_paths = HashSet::new();

    for entry in archive.entries()? {
        let entry = entry?;
        let raw_path = entry.path()?.to_string_lossy().into_owned();
        if raw_path.is_empty() {
            continue;
        }

        let relative_path = normalize_entry_path(&raw_path, strip_prefix).to_owned();
        if relative_path.is_empty() || relative_path == "." {
            continue;
        }
        if skip_paths.contains(&relative_path.as_str()) {
            continue;
        }

        validate_relative_path(&relative_path)?;

        let entry_type = entry.header().entry_type();
        if entry_type == tar::EntryType::Symlink {
            return Err(ArchiveError::SymlinkRejected {
                entry: relative_path,
            });
        }
        if entry_type == tar::EntryType::Link {
            return Err(ArchiveError::HardlinkRejected {
                entry: relative_path,
            });
        }
        if entry_type == tar::EntryType::Directory {
            std::fs::create_dir_all(dest_dir.join(&relative_path))?;
            continue;
        }
        if !entry_type.is_file() {
            return Err(ArchiveError::UnexpectedFileType {
                entry: relative_path,
                kind: format!("{entry_type:?}"),
            });
        }
        if !seen_paths.insert(relative_path.clone()) {
            return Err(ArchiveError::DuplicatePath {
                entry: relative_path,
            });
        }

        extracted_count += 1;
        if extracted_count > limits.max_entries {
            return Err(ArchiveError::EntryCountLimit {
                max: limits.max_entries,
                actual: extracted_count,
            });
        }

        let declared_size = entry.header().size()?;
        let mode = entry.header().mode()?;
        if declared_size > limits.max_entry_size {
            return Err(ArchiveError::ExpandedSizeLimit {
                max: limits.max_entry_size,
                actual: declared_size,
            });
        }

        let mut data = Vec::new();
        entry
            .take(limits.max_entry_size.saturating_add(1))
            .read_to_end(&mut data)?;
        if data.len() as u64 > limits.max_entry_size {
            return Err(ArchiveError::ExpandedSizeLimit {
                max: limits.max_entry_size,
                actual: data.len() as u64,
            });
        }

        total_size = total_size.saturating_add(data.len() as u64);
        if total_size > limits.max_expanded_size {
            return Err(ArchiveError::ExpandedSizeLimit {
                max: limits.max_expanded_size,
                actual: total_size,
            });
        }

        let target = dest_dir.join(&relative_path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, &data)?;

        if mode & 0o111 != 0 {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))?;
            }
        }
    }

    Ok(seen_paths)
}

/// Decompress `.tar.gz` bytes and extract safely into `dest_dir`.
pub fn extract_tar_gz_safely(
    archive_bytes: &[u8],
    dest_dir: &Path,
    limits: &ExtractionLimits,
    skip_paths: &[&str],
    strip_prefix: Option<&str>,
) -> ArchiveResult<HashSet<String>> {
    let tar_bytes = decompress_gzip(archive_bytes, limits)?;
    extract_tar_safely(&tar_bytes, dest_dir, limits, skip_paths, strip_prefix)
}

/// Extract a `.zip` archive safely into `dest_dir`, enforcing path safety,
/// symlink rejection, and streaming decompression byte/entry limits.
pub fn extract_zip_safely(
    archive_bytes: &[u8],
    dest_dir: &Path,
    limits: &ExtractionLimits,
) -> ArchiveResult<HashSet<String>> {
    let cursor = std::io::Cursor::new(archive_bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|err| ArchiveError::Io {
        message: err.to_string(),
    })?;
    archive
        .validate_header_consistency()
        .map_err(|err| ArchiveError::Io {
            message: err.to_string(),
        })?;

    let max_dir_entries = limits.max_entries.saturating_mul(8).max(1024);
    let mut dir_count = 0usize;
    let mut extracted_count = 0usize;
    let mut total_size = 0u64;
    let mut seen_paths = HashSet::new();

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|err| ArchiveError::Io {
            message: err.to_string(),
        })?;
        let raw_name = entry
            .name()
            .map_err(|err| ArchiveError::Io {
                message: err.to_string(),
            })?
            .into_owned();
        if raw_name.is_empty() {
            continue;
        }
        if raw_name.contains('\0') || raw_name.contains('\\') {
            return if raw_name.contains("..") {
                Err(ArchiveError::PathTraversal { entry: raw_name })
            } else {
                Err(ArchiveError::InvalidPath { entry: raw_name })
            };
        }

        let is_dir = entry.is_dir() || raw_name.ends_with('/');
        let trimmed_prefix = raw_name.strip_prefix("./").unwrap_or(&raw_name);
        let relative_path = if is_dir {
            trimmed_prefix.trim_end_matches('/').to_owned()
        } else {
            trimmed_prefix.to_owned()
        };
        if relative_path.is_empty() || relative_path == "." {
            continue;
        }

        validate_relative_path(&relative_path)?;

        let unix_mode = entry.unix_mode();
        if entry.is_symlink() || unix_mode.is_some_and(|mode| (mode & 0o170000) == 0o120000) {
            return Err(ArchiveError::SymlinkRejected {
                entry: relative_path,
            });
        }
        if let Some(mode) = unix_mode {
            let file_type = mode & 0o170000;
            if file_type != 0 && file_type != 0o100000 && file_type != 0o040000 {
                return Err(ArchiveError::UnexpectedFileType {
                    entry: relative_path,
                    kind: format!("unix_mode(0o{mode:o})"),
                });
            }
        }

        if is_dir {
            dir_count += 1;
            if dir_count > max_dir_entries {
                return Err(ArchiveError::EntryCountLimit {
                    max: limits.max_entries,
                    actual: dir_count,
                });
            }
            std::fs::create_dir_all(dest_dir.join(&relative_path))?;
            continue;
        }

        if !seen_paths.insert(relative_path.clone()) {
            return Err(ArchiveError::DuplicatePath {
                entry: relative_path,
            });
        }

        extracted_count += 1;
        if extracted_count > limits.max_entries {
            return Err(ArchiveError::EntryCountLimit {
                max: limits.max_entries,
                actual: extracted_count,
            });
        }

        let declared_size = entry.size();
        if declared_size > limits.max_entry_size {
            return Err(ArchiveError::ExpandedSizeLimit {
                max: limits.max_entry_size,
                actual: declared_size,
            });
        }

        let remaining_total = limits.max_expanded_size.saturating_sub(total_size);
        let read_cap = limits.max_entry_size.min(remaining_total).saturating_add(1);
        let mut data = Vec::new();
        (&mut entry).take(read_cap).read_to_end(&mut data)?;

        let actual_len = data.len() as u64;
        if actual_len > limits.max_entry_size {
            return Err(ArchiveError::ExpandedSizeLimit {
                max: limits.max_entry_size,
                actual: actual_len,
            });
        }

        total_size = total_size.saturating_add(actual_len);
        if total_size > limits.max_expanded_size {
            return Err(ArchiveError::ExpandedSizeLimit {
                max: limits.max_expanded_size,
                actual: total_size,
            });
        }

        let target = dest_dir.join(&relative_path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, &data)?;

        if let Some(mode) = unix_mode
            && mode & 0o111 != 0
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Keep only regular permission bits (strip setuid/setgid/sticky) and ensure owner rw.
                let safe_mode = (mode & 0o777) | 0o600;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(safe_mode))?;
            }
        }
    }

    Ok(seen_paths)
}

fn normalize_entry_path<'a>(raw_path: &'a str, strip_prefix: Option<&str>) -> &'a str {
    let trimmed = raw_path.strip_prefix("./").unwrap_or(raw_path);
    match strip_prefix {
        Some(prefix) => trimmed.strip_prefix(prefix).unwrap_or(trimmed),
        None => trimmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_tar_gz(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::fast());
            let mut builder = tar::Builder::new(gz);
            for (path, data, mode) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(*mode);
                header.set_cksum();
                builder.append_data(&mut header, *path, *data).unwrap();
            }
            builder.finish().unwrap();
        }
        buf
    }

    #[test]
    fn extracts_and_preserves_executable_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = build_tar_gz(&[("bin/tool", b"#!/bin/sh\n", 0o755)]);
        let extracted = extract_tar_gz_safely(
            &archive,
            tmp.path(),
            &ExtractionLimits::default(),
            &[],
            None,
        )
        .unwrap();
        assert!(extracted.contains("bin/tool"));
        assert!(tmp.path().join("bin/tool").is_file());
    }

    #[test]
    fn rejects_path_traversal() {
        let err = validate_relative_path("../escape").unwrap_err();
        assert!(matches!(err, ArchiveError::PathTraversal { .. }));
    }

    fn build_zip_with_attrs(entries: &[(&str, &[u8], Option<u32>)]) -> Vec<u8> {
        use std::io::Write;
        use zip::CompressionMethod;
        use zip::write::SimpleFileOptions;

        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buf);
            for (path, data, mode) in entries {
                let mut opts =
                    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
                if let Some(m) = mode {
                    opts = opts.external_attributes(m << 16);
                }
                writer.start_file(*path, opts).unwrap();
                writer.write_all(data).unwrap();
            }
            writer.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn extract_zip_safely_extracts_and_strips_special_bits() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = build_zip_with_attrs(&[
            ("uv-win/uv.exe", b"mz-binary", Some(0o104755)),
            ("uv-win/README.txt", b"hello", Some(0o100644)),
        ]);
        let extracted =
            extract_zip_safely(&archive, tmp.path(), &ExtractionLimits::default()).unwrap();
        assert!(extracted.contains("uv-win/uv.exe"));
        assert!(extracted.contains("uv-win/README.txt"));
        assert_eq!(
            std::fs::read(tmp.path().join("uv-win/uv.exe")).unwrap(),
            b"mz-binary"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(tmp.path().join("uv-win/uv.exe"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o7777, 0o755);
        }
    }

    #[test]
    fn extract_zip_safely_rejects_zip_slip_and_backslash_traversal() {
        for bad_name in ["../escape.txt", "/etc/passwd", r"sub\..\..\escape.txt"] {
            let tmp = tempfile::tempdir().unwrap();
            let archive = build_zip_with_attrs(&[(bad_name, b"evil", None)]);
            let err = extract_zip_safely(&archive, tmp.path(), &ExtractionLimits::default())
                .expect_err("must reject unsafe zip path");
            assert!(
                matches!(
                    err,
                    ArchiveError::PathTraversal { .. } | ArchiveError::InvalidPath { .. }
                ),
                "unexpected error for {bad_name}: {err:?}"
            );
        }
    }

    #[test]
    fn extract_zip_safely_rejects_symlinks() {
        use zip::write::SimpleFileOptions;

        let tmp = tempfile::tempdir().unwrap();
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buf);
            writer
                .add_symlink("link", "/etc/passwd", SimpleFileOptions::default())
                .unwrap();
            writer.finish().unwrap();
        }
        let archive = buf.into_inner();
        let err = extract_zip_safely(&archive, tmp.path(), &ExtractionLimits::default())
            .expect_err("must reject symlink entry");
        assert!(matches!(err, ArchiveError::SymlinkRejected { .. }));
    }

    #[test]
    fn extract_zip_safely_enforces_entry_count_and_streaming_size_limits() {
        let tmp = tempfile::tempdir().unwrap();
        let many = build_zip_with_attrs(&[
            ("a.txt", b"a", None),
            ("b.txt", b"b", None),
            ("c.txt", b"c", None),
        ]);
        let err = extract_zip_safely(
            &many,
            tmp.path(),
            &ExtractionLimits {
                max_entries: 2,
                ..ExtractionLimits::default()
            },
        )
        .expect_err("must enforce max_entries");
        assert!(matches!(err, ArchiveError::EntryCountLimit { .. }));

        // Highly compressible payload exceeding max_entry_size.
        let payload = vec![0u8; 8192];
        let bomb = build_zip_with_attrs(&[("bomb.bin", &payload, None)]);
        assert!(bomb.len() < 1024);
        let err = extract_zip_safely(
            &bomb,
            tmp.path(),
            &ExtractionLimits {
                max_entries: 10,
                max_entry_size: 1024,
                max_expanded_size: 100_000,
            },
        )
        .expect_err("must reject oversized entry");
        assert!(matches!(err, ArchiveError::ExpandedSizeLimit { .. }));

        // Also reject when header uncompressed_size is spoofed down to 1 byte.
        let mut spoofed = bomb;
        let orig_le = (payload.len() as u32).to_le_bytes();
        let fake_le = 1u32.to_le_bytes();
        let mut patched = 0usize;
        for i in 0..=spoofed.len().saturating_sub(4) {
            if spoofed[i..i + 4] == orig_le {
                spoofed[i..i + 4].copy_from_slice(&fake_le);
                patched += 1;
            }
        }
        assert!(patched >= 2);
        extract_zip_safely(
            &spoofed,
            tmp.path(),
            &ExtractionLimits {
                max_entries: 10,
                max_entry_size: 1024,
                max_expanded_size: 100_000,
            },
        )
        .expect_err("must reject spoofed-size deflate entry");
        assert!(!tmp.path().join("bomb.bin").exists());

        // Multiple valid-sized entries whose sum exceeds max_expanded_size.
        let multi =
            build_zip_with_attrs(&[("x.bin", &[1u8; 600], None), ("y.bin", &[2u8; 600], None)]);
        let err = extract_zip_safely(
            &multi,
            tmp.path(),
            &ExtractionLimits {
                max_entries: 10,
                max_entry_size: 1000,
                max_expanded_size: 1000,
            },
        )
        .expect_err("must enforce max_expanded_size across entries");
        assert!(matches!(err, ArchiveError::ExpandedSizeLimit { .. }));
    }
}
