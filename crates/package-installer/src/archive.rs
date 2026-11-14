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
}
