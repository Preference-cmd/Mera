use std::fs;
use std::path::{Path, PathBuf};

use reimagine_package_installer::{FsLock, FsTxError, atomic_promote_dir, atomic_write_file};
use serde::{Deserialize, Serialize};
use tracing;

use super::error::{InstallError, InstallResult};
use crate::WorkerStorePaths;

/// Journal states for the installation transaction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum JournalState {
    /// Staging directory created, download/extraction in progress.
    Staging,
    /// Package verified and self-check passed.
    Verified,
    /// Atomic promotion in progress.
    Promoting,
    /// Installation complete.
    Complete,
    /// Installation failed and should be rolled back.
    Failed(String),
}

/// An intent journal entry for tracking installation progress.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallJournal {
    pub installation_id: String,
    pub staging_path: PathBuf,
    pub target_path: PathBuf,
    pub state: JournalState,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub record: Option<crate::InstallationRecord>,
}

/// Filesystem-based lock for mutual exclusion among concurrent installer processes.
pub struct InstallLock {
    _inner: FsLock,
}

impl InstallLock {
    pub fn acquire(store_paths: &WorkerStorePaths) -> InstallResult<Self> {
        let lock_path = store_paths.install_lock_path();
        let inner = FsLock::acquire(&lock_path).map_err(|error| match error {
            FsTxError::LockContended { .. } => InstallError::LockContended,
            FsTxError::Io { path, message } => InstallError::Io { path, message },
            FsTxError::PromoteFailed { from, to, message } => {
                InstallError::PromoteFailed { from, to, message }
            }
        })?;
        tracing::info!("acquired install lock at `{}`", lock_path.display());
        Ok(Self { _inner: inner })
    }
}

/// Journal manager for crash-recoverable installation transactions.
pub struct JournalManager;

impl JournalManager {
    /// Create a new journal entry in the `Staging` state.
    pub fn begin(
        store_paths: &WorkerStorePaths,
        installation_id: &str,
        staging_path: &Path,
        target_path: &Path,
    ) -> InstallResult<InstallJournal> {
        let now = iso_now();
        let journal = InstallJournal {
            installation_id: installation_id.to_string(),
            staging_path: staging_path.to_path_buf(),
            target_path: target_path.to_path_buf(),
            state: JournalState::Staging,
            created_at: now.clone(),
            updated_at: now,
            record: None,
        };

        Self::write_journal(store_paths, &journal)?;
        Ok(journal)
    }

    /// Transition journal to `Verified`.
    pub fn mark_verified(
        store_paths: &WorkerStorePaths,
        journal: &mut InstallJournal,
        record: crate::InstallationRecord,
    ) -> InstallResult<()> {
        journal.state = JournalState::Verified;
        journal.record = Some(record);
        journal.updated_at = iso_now();
        Self::write_journal(store_paths, journal)
    }

    /// Transition journal to `Promoting`.
    pub fn mark_promoting(
        store_paths: &WorkerStorePaths,
        journal: &mut InstallJournal,
    ) -> InstallResult<()> {
        journal.state = JournalState::Promoting;
        journal.updated_at = iso_now();
        Self::write_journal(store_paths, journal)
    }

    /// Transition journal to `Complete`.
    pub fn mark_complete(
        store_paths: &WorkerStorePaths,
        journal: &mut InstallJournal,
    ) -> InstallResult<()> {
        journal.state = JournalState::Complete;
        journal.updated_at = iso_now();
        Self::write_journal(store_paths, journal)
    }

    /// Mark the journal as failed.
    pub fn mark_failed(
        store_paths: &WorkerStorePaths,
        journal: &mut InstallJournal,
        reason: &str,
    ) -> InstallResult<()> {
        journal.state = JournalState::Failed(reason.to_string());
        journal.updated_at = iso_now();
        Self::write_journal(store_paths, journal)
    }

    /// Read the journal for a given installation.
    pub fn read_journal(
        store_paths: &WorkerStorePaths,
        installation_id: &str,
    ) -> InstallResult<Option<InstallJournal>> {
        let path = store_paths.intent_path(installation_id);
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)
            .map_err(|_e| InstallError::JournalCorrupt { path: path.clone() })?;

        serde_json::from_str(&content)
            .map(Some)
            .map_err(|_e| InstallError::JournalCorrupt { path })
    }

    /// Recover from a previous incomplete installation.
    ///
    /// Based on the journal state:
    /// - `Staging`: discard the staging directory
    /// - `Verified`: retry promotion, then mark complete
    /// - `Promoting`: check if target exists, promote if not, mark complete
    /// - `Complete`: nothing to do
    /// - `Failed`: discard staging
    pub fn recover(
        _store_paths: &WorkerStorePaths,
        journal: &InstallJournal,
    ) -> InstallResult<RecoveryAction> {
        match &journal.state {
            JournalState::Staging => {
                tracing::info!("recovering from Staging: cleaning up staging");
                let _ = fs::remove_dir_all(&journal.staging_path);
                Ok(RecoveryAction::CleanupStaging)
            }
            JournalState::Verified => {
                tracing::info!("recovering from Verified: retrying promotion");
                Ok(RecoveryAction::RetryPromote)
            }
            JournalState::Promoting => {
                if journal.target_path.exists() {
                    tracing::info!("recovering from Promoting: target exists, completing");
                    Ok(RecoveryAction::Complete)
                } else if journal.staging_path.exists() {
                    tracing::info!("recovering from Promoting: retrying promotion");
                    Ok(RecoveryAction::RetryPromote)
                } else {
                    tracing::info!("recovering from Promoting: nothing to promote, marking failed");
                    Ok(RecoveryAction::Failed)
                }
            }
            JournalState::Complete => {
                tracing::info!("recovering from Complete: installation already done");
                Ok(RecoveryAction::AlreadyComplete)
            }
            JournalState::Failed(_) => {
                tracing::info!("recovering from Failed: cleaning up staging");
                let _ = fs::remove_dir_all(&journal.staging_path);
                Ok(RecoveryAction::CleanupStaging)
            }
        }
    }

    fn write_journal(
        store_paths: &WorkerStorePaths,
        journal: &InstallJournal,
    ) -> InstallResult<()> {
        let path = store_paths.intent_path(&journal.installation_id);
        let content = serde_json::to_string_pretty(journal)
            .map_err(|_e| InstallError::JournalCorrupt { path: path.clone() })?;

        atomic_write_file(&path, content.as_bytes()).map_err(|error| match error {
            FsTxError::LockContended { .. } => InstallError::LockContended,
            FsTxError::Io { path, message } => InstallError::Io { path, message },
            FsTxError::PromoteFailed { from, to, message } => {
                InstallError::PromoteFailed { from, to, message }
            }
        })
    }
}

/// Action to take after journal recovery.
#[derive(Debug, PartialEq)]
pub enum RecoveryAction {
    /// Clean up the staging directory (installation failed).
    CleanupStaging,
    /// Retry promotion from staging to target.
    RetryPromote,
    /// Mark the installation as complete.
    Complete,
    /// Already complete, nothing to do.
    AlreadyComplete,
    /// Mark the installation as failed.
    Failed,
}

/// Atomic promotion engine.
pub struct PromotionEngine;

impl PromotionEngine {
    /// Promote the staged worker to the target installation directory.
    pub fn promote(staging_path: &Path, target_path: &Path) -> InstallResult<()> {
        atomic_promote_dir(staging_path, target_path).map_err(|error| match error {
            FsTxError::LockContended { .. } => InstallError::LockContended,
            FsTxError::Io { path, message } => InstallError::Io { path, message },
            FsTxError::PromoteFailed { from, to, message } => {
                InstallError::PromoteFailed { from, to, message }
            }
        })?;

        tracing::info!(
            "promoted worker from `{}` to `{}`",
            staging_path.display(),
            target_path.display()
        );

        Ok(())
    }

    /// Remove an installed worker directory.
    pub fn remove_installed(target_path: &Path) -> InstallResult<()> {
        if !target_path.exists() {
            return Ok(());
        }

        match fs::remove_dir_all(target_path) {
            Ok(()) => {
                tracing::info!("removed installed worker at `{}`", target_path.display());
                Ok(())
            }
            Err(e) => {
                tracing::warn!(
                    "failed to remove installed worker at `{}`: {e}",
                    target_path.display()
                );
                Err(InstallError::Io {
                    path: target_path.to_path_buf(),
                    message: e.to_string(),
                })
            }
        }
    }
}

fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(tmp: &tempfile::TempDir) -> WorkerStorePaths {
        WorkerStorePaths::new(tmp.path().to_path_buf())
    }

    fn test_record() -> crate::InstallationRecord {
        crate::InstallationRecord {
            installation_id: crate::WorkerInstallationId("test-worker".to_owned()),
            version: "1.0.0".to_owned(),
            identity: crate::ExpectedWorkerIdentity {
                backend_instance_id: crate::BackendInstanceId("burn:wgpu:default".to_owned()),
                installation_id: crate::WorkerInstallationId("test-worker".to_owned()),
                backend_kind: "burn".to_owned(),
                target: "test-target".to_owned(),
                manifest_digest: "digest".to_owned(),
            },
            installed_at: chrono::Utc::now(),
            install_path: "/installed/test-worker".to_owned(),
            manifest_profile: None,
            protocol_version_min: 1,
            protocol_version_max: 1,
        }
    }

    #[test]
    fn lock_acquire_and_release() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(&tmp);

        {
            let _lock = InstallLock::acquire(&paths).expect("acquire lock");
            assert!(paths.install_lock_path().exists());

            // Second acquire should fail
            let second = InstallLock::acquire(&paths);
            assert!(second.is_err());
            // Drop lock
        }

        // After drop, lock should be released
        let lock = InstallLock::acquire(&paths).expect("re-acquire lock after drop");
        drop(lock);
    }

    #[test]
    fn journal_begin_and_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(&tmp);

        let mut journal = JournalManager::begin(
            &paths,
            "test-worker",
            Path::new("/staging/test-worker"),
            Path::new("/installed/test-worker"),
        )
        .expect("begin journal");

        assert_eq!(journal.state, JournalState::Staging);

        JournalManager::mark_verified(&paths, &mut journal, test_record()).expect("mark verified");
        assert_eq!(journal.state, JournalState::Verified);

        JournalManager::mark_complete(&paths, &mut journal).expect("mark complete");
        assert_eq!(journal.state, JournalState::Complete);

        // Verify persistence
        let loaded = JournalManager::read_journal(&paths, "test-worker")
            .expect("read journal")
            .expect("journal exists");
        assert_eq!(loaded.state, JournalState::Complete);
    }

    #[test]
    fn promote_creates_target() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        let target = tmp.path().join("installed");

        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("burn-worker"), b"binary").unwrap();

        PromotionEngine::promote(&staging, &target).expect("promote");
        assert!(target.join("burn-worker").exists());
        assert!(!staging.exists());
    }

    #[test]
    fn journal_recovery_staging_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(&tmp);

        let staging = tmp.path().join("staging");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("test.bin"), b"data").unwrap();

        let journal = InstallJournal {
            installation_id: "test".to_string(),
            staging_path: staging.clone(),
            target_path: tmp.path().join("target"),
            state: JournalState::Staging,
            created_at: iso_now(),
            updated_at: iso_now(),
            record: None,
        };

        let action = JournalManager::recover(&paths, &journal).expect("recover");
        assert_eq!(action, RecoveryAction::CleanupStaging);
        assert!(!staging.exists());
    }
}
