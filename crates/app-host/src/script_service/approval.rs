use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use jiff::Timestamp;
use mera_config::{AppPaths, atomic_write};
use mera_core::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSourceName, DiagnosticTarget,
    DiagnosticTargetDomain,
};
use mera_core::model::{DiagnosticId, ProjectId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppHostError, AppHostResult};

/// Filename holding persisted per-project approved `exec_digest` records.
pub const SCRIPT_APPROVALS_FILENAME: &str = "script-approvals.json";

/// Schema version for `script-approvals.json`.
pub const SCRIPT_APPROVALS_SCHEMA_VERSION: u16 = 1;

/// Persisted approval record for one `(code, deps, python, allow_source_build, lock_sha256)` digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedScriptRecord {
    pub exec_digest: String,
    pub python: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub allow_source_build: bool,
    #[serde(default)]
    pub lock_sha256: Option<String>,
    pub approved_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ScriptApprovalsDocument {
    schema_version: u16,
    #[serde(default)]
    approved: BTreeMap<String, ApprovedScriptRecord>,
}

impl Default for ScriptApprovalsDocument {
    fn default() -> Self {
        Self {
            schema_version: SCRIPT_APPROVALS_SCHEMA_VERSION,
            approved: BTreeMap::new(),
        }
    }
}

/// Full review metadata surfaced when first-run script execution requires approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptApprovalReview {
    pub project_id: ProjectId,
    pub exec_digest: String,
    pub code: String,
    pub python: String,
    pub dependencies: Vec<String>,
    pub allow_source_build: bool,
    pub lock_sha256: Option<String>,
    pub offline_sandbox: bool,
}

/// Review metadata surfaced when online dependency preparation requires HITL confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnlinePrepareReview {
    pub project_id: ProjectId,
    pub env_hash: String,
    pub python: String,
    pub dependencies: Vec<String>,
    pub allow_source_build: bool,
    pub no_build: bool,
    pub missing_uv: bool,
    pub missing_managed_python: bool,
}

/// Compute the deterministic SHA-256 execution digest binding script code,
/// canonical dependencies, Python version, source-build flag, and lock hash.
#[must_use]
pub fn compute_execution_digest(
    code: &str,
    dependencies: &[String],
    python: &str,
    allow_source_build: bool,
    lock_sha256: Option<&str>,
) -> String {
    #[derive(Serialize)]
    struct CanonicalDigestPayload<'a> {
        schema_version: u16,
        code: &'a str,
        dependencies: &'a [String],
        python: &'a str,
        allow_source_build: bool,
        lock_sha256: Option<&'a str>,
    }

    let normalized_code = code.replace("\r\n", "\n");
    let payload = CanonicalDigestPayload {
        schema_version: 1,
        code: &normalized_code,
        dependencies,
        python,
        allow_source_build,
        lock_sha256,
    };
    let bytes = serde_json::to_vec(&payload).expect("digest payload serialization is infallible");
    hex::encode(Sha256::digest(bytes))
}

pub(crate) fn is_valid_sha256_hex(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn script_caller_error(
    id_suffix: &str,
    code: &str,
    message: impl Into<String>,
) -> Diagnostic {
    Diagnostic::new(
        DiagnosticId::new(format!("script:{id_suffix}")),
        DiagnosticCode::new(format!("SCRIPT/{code}")),
        DiagnosticSeverity::Error,
        DiagnosticSourceName::new("script_service"),
        message,
        DiagnosticTarget::new(DiagnosticTargetDomain::new("op.script.python")),
    )
}

pub(crate) fn approval_required_diagnostic(review: &ScriptApprovalReview) -> Diagnostic {
    let summary = serde_json::json!({
        "exec_digest": review.exec_digest,
        "python": review.python,
        "dependencies": review.dependencies,
        "allow_source_build": review.allow_source_build,
        "lock_sha256": review.lock_sha256,
        "offline_sandbox": review.offline_sandbox,
        "code_bytes": review.code.len(),
    });
    script_caller_error(
        &format!("approval:{}", review.exec_digest),
        "APPROVAL_REQUIRED",
        format!("script execution requires explicit human approval: {summary}"),
    )
}

pub(crate) fn online_prepare_required_diagnostic(review: &OnlinePrepareReview) -> Diagnostic {
    let summary = serde_json::json!({
        "env_hash": review.env_hash,
        "python": review.python,
        "dependencies": review.dependencies,
        "allow_source_build": review.allow_source_build,
        "no_build": review.no_build,
        "missing_uv": review.missing_uv,
        "missing_managed_python": review.missing_managed_python,
    });
    script_caller_error(
        &format!("online_prepare:{}", review.env_hash),
        "ONLINE_PREPARE_REQUIRED",
        format!("online dependency preparation requires human confirmation: {summary}"),
    )
}

pub(crate) struct ScriptApprovalStore {
    paths: AppPaths,
    project_locks: Mutex<HashMap<ProjectId, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    pending_reviews: RwLock<HashMap<(ProjectId, String), ScriptApprovalReview>>,
    pending_online: RwLock<HashMap<(ProjectId, String), OnlinePrepareReview>>,
    approved_online: RwLock<HashSet<(ProjectId, String)>>,
}

impl ScriptApprovalStore {
    pub(crate) fn new(paths: AppPaths) -> Self {
        Self {
            paths,
            project_locks: Mutex::new(HashMap::new()),
            pending_reviews: RwLock::new(HashMap::new()),
            pending_online: RwLock::new(HashMap::new()),
            approved_online: RwLock::new(HashSet::new()),
        }
    }

    fn approvals_path(&self, project_id: &ProjectId) -> PathBuf {
        self.paths
            .project_dir(project_id.as_str())
            .join(SCRIPT_APPROVALS_FILENAME)
    }

    fn project_lock(&self, project_id: &ProjectId) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.project_locks.lock().expect("project locks poisoned");
        locks
            .entry(project_id.clone())
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn load_doc(&self, path: &Path) -> AppHostResult<ScriptApprovalsDocument> {
        if !path.exists() {
            return Ok(ScriptApprovalsDocument::default());
        }
        let raw = std::fs::read(path).map_err(|e| AppHostError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let doc: ScriptApprovalsDocument =
            serde_json::from_slice(&raw).map_err(|e| AppHostError::Io {
                path: path.to_path_buf(),
                message: format!("corrupt script approvals file: {e}"),
            })?;
        if doc.schema_version != SCRIPT_APPROVALS_SCHEMA_VERSION {
            return Err(AppHostError::Io {
                path: path.to_path_buf(),
                message: format!(
                    "unsupported script approvals schema version {}",
                    doc.schema_version
                ),
            });
        }
        for (key, rec) in &doc.approved {
            if key != &rec.exec_digest || !is_valid_sha256_hex(key) {
                return Err(AppHostError::Io {
                    path: path.to_path_buf(),
                    message: format!("invalid or tampered approval entry `{key}`"),
                });
            }
        }
        Ok(doc)
    }

    pub(crate) fn record_pending_review(&self, review: ScriptApprovalReview) {
        self.pending_reviews
            .write()
            .expect("pending_reviews lock poisoned")
            .insert(
                (review.project_id.clone(), review.exec_digest.clone()),
                review,
            );
    }

    pub(crate) fn pending_review(
        &self,
        project_id: &ProjectId,
        exec_digest: &str,
    ) -> Option<ScriptApprovalReview> {
        self.pending_reviews
            .read()
            .expect("pending_reviews lock poisoned")
            .get(&(project_id.clone(), exec_digest.to_ascii_lowercase()))
            .cloned()
    }

    pub(crate) fn list_pending_reviews(&self, project_id: &ProjectId) -> Vec<ScriptApprovalReview> {
        let mut items: Vec<ScriptApprovalReview> = self
            .pending_reviews
            .read()
            .expect("pending_reviews lock poisoned")
            .iter()
            .filter(|((pid, _), _)| pid == project_id)
            .map(|(_, review)| review.clone())
            .collect();
        items.sort_by(|a, b| a.exec_digest.cmp(&b.exec_digest));
        items
    }

    pub(crate) fn dismiss_pending_review(&self, project_id: &ProjectId, exec_digest: &str) -> bool {
        self.pending_reviews
            .write()
            .expect("pending_reviews lock poisoned")
            .remove(&(project_id.clone(), exec_digest.trim().to_ascii_lowercase()))
            .is_some()
    }

    pub(crate) fn record_pending_online(&self, review: OnlinePrepareReview) {
        self.pending_online
            .write()
            .expect("pending_online lock poisoned")
            .insert((review.project_id.clone(), review.env_hash.clone()), review);
    }

    pub(crate) fn pending_online(
        &self,
        project_id: &ProjectId,
        env_hash: &str,
    ) -> Option<OnlinePrepareReview> {
        self.pending_online
            .read()
            .expect("pending_online lock poisoned")
            .get(&(project_id.clone(), env_hash.to_ascii_lowercase()))
            .cloned()
    }

    pub(crate) fn list_pending_online(&self, project_id: &ProjectId) -> Vec<OnlinePrepareReview> {
        let mut items: Vec<OnlinePrepareReview> = self
            .pending_online
            .read()
            .expect("pending_online lock poisoned")
            .iter()
            .filter(|((pid, _), _)| pid == project_id)
            .map(|(_, review)| review.clone())
            .collect();
        items.sort_by(|a, b| a.env_hash.cmp(&b.env_hash));
        items
    }

    pub(crate) fn dismiss_pending_online(&self, project_id: &ProjectId, env_hash: &str) -> bool {
        self.pending_online
            .write()
            .expect("pending_online lock poisoned")
            .remove(&(project_id.clone(), env_hash.trim().to_ascii_lowercase()))
            .is_some()
    }

    pub(crate) fn is_online_approved(&self, project_id: &ProjectId, env_hash: &str) -> bool {
        self.approved_online
            .read()
            .expect("approved_online lock poisoned")
            .contains(&(project_id.clone(), env_hash.to_ascii_lowercase()))
    }

    pub(crate) fn approve_online(
        &self,
        project_id: &ProjectId,
        env_hash: &str,
    ) -> AppHostResult<()> {
        let normalized = env_hash.trim().to_ascii_lowercase();
        if !is_valid_sha256_hex(&normalized) {
            return Err(AppHostError::Io {
                path: self.approvals_path(project_id),
                message: format!("invalid SHA-256 env_hash `{env_hash}`"),
            });
        }
        self.approved_online
            .write()
            .expect("approved_online lock poisoned")
            .insert((project_id.clone(), normalized.clone()));
        self.pending_online
            .write()
            .expect("pending_online lock poisoned")
            .remove(&(project_id.clone(), normalized));
        Ok(())
    }

    pub(crate) async fn is_execution_approved(
        &self,
        project_id: &ProjectId,
        exec_digest: &str,
    ) -> AppHostResult<bool> {
        let normalized = exec_digest.trim().to_ascii_lowercase();
        if !is_valid_sha256_hex(&normalized) {
            return Ok(false);
        }
        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;
        let doc = self.load_doc(&self.approvals_path(project_id))?;
        Ok(doc.approved.contains_key(&normalized))
    }

    pub(crate) async fn approve_execution(
        &self,
        project_id: &ProjectId,
        exec_digest: &str,
    ) -> AppHostResult<()> {
        let normalized = exec_digest.trim().to_ascii_lowercase();
        let path = self.approvals_path(project_id);
        if !is_valid_sha256_hex(&normalized) {
            return Err(AppHostError::Io {
                path,
                message: format!("invalid SHA-256 execution digest `{exec_digest}`"),
            });
        }

        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;
        let mut doc = self.load_doc(&path)?;
        let pending = self.pending_review(project_id, &normalized);
        let record = ApprovedScriptRecord {
            exec_digest: normalized.clone(),
            python: pending
                .as_ref()
                .map(|r| r.python.clone())
                .unwrap_or_default(),
            dependencies: pending
                .as_ref()
                .map(|r| r.dependencies.clone())
                .unwrap_or_default(),
            allow_source_build: pending.as_ref().is_some_and(|r| r.allow_source_build),
            lock_sha256: pending.and_then(|r| r.lock_sha256),
            approved_at: Timestamp::now().to_string(),
        };
        doc.approved.insert(normalized.clone(), record);
        let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| AppHostError::Io {
            path: path.clone(),
            message: format!("failed to serialize script approvals: {e}"),
        })?;
        atomic_write(&path, bytes)
            .await
            .map_err(AppHostError::BootstrapConfig)?;
        self.pending_reviews
            .write()
            .expect("pending_reviews lock poisoned")
            .remove(&(project_id.clone(), normalized));
        Ok(())
    }

    pub(crate) async fn revoke_execution(
        &self,
        project_id: &ProjectId,
        exec_digest: &str,
    ) -> AppHostResult<bool> {
        let normalized = exec_digest.trim().to_ascii_lowercase();
        let path = self.approvals_path(project_id);
        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;
        let mut doc = self.load_doc(&path)?;
        let removed = doc.approved.remove(&normalized).is_some();
        if removed {
            let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| AppHostError::Io {
                path: path.clone(),
                message: format!("failed to serialize script approvals: {e}"),
            })?;
            atomic_write(&path, bytes)
                .await
                .map_err(AppHostError::BootstrapConfig)?;
        }
        Ok(removed)
    }

    pub(crate) async fn list_approved(
        &self,
        project_id: &ProjectId,
    ) -> AppHostResult<Vec<ApprovedScriptRecord>> {
        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;
        let doc = self.load_doc(&self.approvals_path(project_id))?;
        Ok(doc.approved.into_values().collect())
    }
}
