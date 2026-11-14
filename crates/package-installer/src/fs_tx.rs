use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;

#[derive(Debug)]
pub enum FsTxError {
    LockContended {
        path: PathBuf,
    },
    Io {
        path: PathBuf,
        message: String,
    },
    PromoteFailed {
        from: PathBuf,
        to: PathBuf,
        message: String,
    },
}

impl fmt::Display for FsTxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockContended { path } => {
                write!(f, "install lock already held at `{}`", path.display())
            }
            Self::Io { path, message } => {
                write!(f, "I/O error at `{}`: {message}", path.display())
            }
            Self::PromoteFailed { from, to, message } => {
                write!(
                    f,
                    "failed to promote `{}` to `{}`: {message}",
                    from.display(),
                    to.display()
                )
            }
        }
    }
}

impl std::error::Error for FsTxError {}

pub type FsTxResult<T> = Result<T, FsTxError>;

/// Non-blocking exclusive advisory lock on a path.
pub struct FsLock {
    file: fs::File,
}

impl FsLock {
    pub fn acquire(lock_path: &Path) -> FsTxResult<Self> {
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(|e| FsTxError::Io {
                path: parent.to_path_buf(),
                message: format!("failed to create lock directory: {e}"),
            })?;
        }
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|e| FsTxError::Io {
                path: lock_path.to_path_buf(),
                message: format!("failed to open lock file: {e}"),
            })?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { file }),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(FsTxError::LockContended {
                path: lock_path.to_path_buf(),
            }),
            Err(e) => Err(FsTxError::Io {
                path: lock_path.to_path_buf(),
                message: format!("failed to acquire lock: {e}"),
            }),
        }
    }
}

impl Drop for FsLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Atomically write `bytes` to `path` via a sibling `.tmp` file, `sync_all`, and `rename`.
pub fn atomic_write_file(path: &Path, bytes: &[u8]) -> FsTxResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| FsTxError::Io {
            path: parent.to_path_buf(),
            message: format!("create_dir_all: {e}"),
        })?;
    }
    let tmp = temp_sibling_path(path, "tmp");
    {
        let mut f = fs::File::create(&tmp).map_err(|e| FsTxError::Io {
            path: tmp.clone(),
            message: format!("create temp: {e}"),
        })?;
        f.write_all(bytes).map_err(|e| FsTxError::Io {
            path: tmp.clone(),
            message: format!("write: {e}"),
        })?;
        f.sync_all().map_err(|e| FsTxError::Io {
            path: tmp.clone(),
            message: format!("sync_all: {e}"),
        })?;
    }
    fs::rename(&tmp, path).map_err(|e| FsTxError::Io {
        path: path.to_path_buf(),
        message: format!("rename: {e}"),
    })?;
    sync_parent(path)
}

/// Atomically promote a staged directory into `target_path`, backing up any
/// prior directory and restoring it if the rename fails.
pub fn atomic_promote_dir(staging_path: &Path, target_path: &Path) -> FsTxResult<()> {
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent).map_err(|e| FsTxError::Io {
            path: parent.to_path_buf(),
            message: format!("failed to create target parent: {e}"),
        })?;
    }

    let backup = temp_sibling_path(target_path, "bak");
    if target_path.exists() {
        if backup.exists() {
            let _ = fs::remove_dir_all(&backup);
        }
        fs::rename(target_path, &backup).map_err(|e| FsTxError::PromoteFailed {
            from: staging_path.to_path_buf(),
            to: target_path.to_path_buf(),
            message: format!("failed to back up existing target: {e}"),
        })?;
    }

    sync_tree(staging_path)?;

    if let Err(e) = fs::rename(staging_path, target_path) {
        if backup.exists() {
            let _ = fs::rename(&backup, target_path);
        }
        return Err(FsTxError::PromoteFailed {
            from: staging_path.to_path_buf(),
            to: target_path.to_path_buf(),
            message: format!("rename failed: {e}"),
        });
    }
    sync_parent(target_path)?;
    if backup.exists() {
        let _ = fs::remove_dir_all(&backup);
    }
    Ok(())
}

/// Recursively `sync_all` all files and directories under `path`.
pub fn sync_tree(path: &Path) -> FsTxResult<()> {
    for entry in fs::read_dir(path).map_err(|error| FsTxError::Io {
        path: path.to_path_buf(),
        message: format!("failed to read staging tree for sync: {error}"),
    })? {
        let entry = entry.map_err(|error| FsTxError::Io {
            path: path.to_path_buf(),
            message: format!("failed to read staging entry for sync: {error}"),
        })?;
        let entry_path = entry.path();
        if entry
            .file_type()
            .map_err(|error| FsTxError::Io {
                path: entry_path.clone(),
                message: error.to_string(),
            })?
            .is_dir()
        {
            sync_tree(&entry_path)?;
        } else {
            fs::File::open(&entry_path)
                .and_then(|file| file.sync_all())
                .map_err(|error| FsTxError::Io {
                    path: entry_path,
                    message: format!("failed to sync staged file: {error}"),
                })?;
        }
    }
    sync_directory(path)
}

fn temp_sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".{suffix}"));
    path.with_file_name(name)
}

fn sync_parent(path: &Path) -> FsTxResult<()> {
    path.parent().map_or(Ok(()), sync_directory)
}

fn sync_directory(path: &Path) -> FsTxResult<()> {
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| FsTxError::Io {
                path: path.to_path_buf(),
                message: format!("failed to sync directory: {error}"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_promote_replaces_existing_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        let target = tmp.path().join("target/pkg");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("file.txt"), b"v2").unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("file.txt"), b"v1").unwrap();

        atomic_promote_dir(&staging, &target).unwrap();
        assert!(!staging.exists());
        assert_eq!(fs::read(target.join("file.txt")).unwrap(), b"v2");
    }
}
