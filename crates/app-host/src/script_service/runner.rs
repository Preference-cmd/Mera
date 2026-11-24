use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use mera_config::AppPaths;
use mera_core::diagnostic::Diagnostic;
use mera_core::model::ProjectId;
use mera_package_installer::FsLock;
use mera_sandbox::{
    ResourceLimits, SandboxCancelToken, SandboxExecRequest, SandboxPolicy, TerminationReason,
    run_sandboxed,
};
use mod_sdk::{OperationAnnotations, OperationSpec};
use serde::Deserialize;
use serde_json::{Value, json};

use super::approval::{
    ApprovedScriptRecord, OnlinePrepareReview, ScriptApprovalReview, ScriptApprovalStore,
    approval_required_diagnostic, compute_execution_digest, online_prepare_required_diagnostic,
    script_caller_error,
};
use crate::error::AppHostResult;
use crate::mod_manager::inventory::ModStorePaths;
use crate::operation_service::{KernelCall, KernelOp, MAX_INLINE_TEXT_BYTES, OperationService};
use crate::script_env::{
    ScriptEnvLookup, ScriptEnvReady, ScriptEnvSpec, build_script_python_env, ensure_managed_python,
    ensure_script_scratch_dirs, inspect_script_env, prepare_script_env, touch_script_env_last_used,
};

/// Default wall-clock and CPU timeout for `script.python` (seconds).
pub const DEFAULT_SCRIPT_TIMEOUT_SECS: u64 = 30;
/// Hard ceiling for `timeout_secs` on `script.python` (seconds).
pub const MAX_SCRIPT_TIMEOUT_SECS: u64 = 300;
/// Minimum configurable memory limit (16 MiB).
pub const MIN_SCRIPT_MEMORY_BYTES: u64 = 16 * 1024 * 1024;
/// Default physical memory limit (2 GiB).
pub const DEFAULT_SCRIPT_MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Maximum schema-accepted memory limit (8 GiB).
pub const MAX_SCRIPT_MEMORY_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Per-stream stdout/stderr capture ceiling (64 KiB).
pub const SCRIPT_STDIO_LIMIT_BYTES: usize = 64 * 1024;

const MERA_RUNNER_PY: &str = r#"import json
import os
import pathlib
import sys


class _MeraContext:
    def __init__(self, input_payload, assets, out_dir):
        self.input = input_payload
        self.assets = assets
        self.out_dir = pathlib.Path(out_dir)
        self._outputs = []

    def emit_output(self, name, path, media_type=None):
        raw_name = str(name).strip()
        if not raw_name:
            raise ValueError("output name must not be empty")
        candidate = pathlib.Path(path)
        if not candidate.is_absolute():
            candidate = self.out_dir / candidate
        resolved_out = self.out_dir.resolve()
        resolved_candidate = candidate.resolve()
        try:
            rel = resolved_candidate.relative_to(resolved_out)
        except ValueError as exc:
            raise ValueError(f"output path '{path}' escapes MERA_OUT_DIR") from exc
        entry = {"name": raw_name, "path": rel.as_posix()}
        if media_type is not None:
            entry["media_type"] = str(media_type)
        self._outputs.append(entry)
        return resolved_candidate


def _main():
    if len(sys.argv) < 2:
        raise RuntimeError("missing user script path")
    user_script_path = sys.argv[1]
    input_json_path = os.environ["MERA_INPUT_JSON"]
    out_dir = os.environ["MERA_OUT_DIR"]
    result_json_path = os.environ["MERA_RESULT_JSON"]

    with open(input_json_path, "r", encoding="utf-8") as fh:
        payload = json.load(fh)

    ctx = _MeraContext(
        payload.get("input", {}),
        payload.get("assets", {}),
        out_dir,
    )

    with open(user_script_path, "r", encoding="utf-8") as fh:
        source = fh.read()

    namespace = {
        "__name__": "__main__",
        "__file__": user_script_path,
    }
    compiled = compile(source, user_script_path, "exec")
    exec(compiled, namespace, namespace)

    main_fn = namespace.get("main")
    if callable(main_fn):
        ret = main_fn(ctx)
        user_wrote_result = (
            os.path.exists(result_json_path) and os.path.getsize(result_json_path) > 0
        )
        if ret is not None or ctx._outputs or not user_wrote_result:
            with open(result_json_path, "w", encoding="utf-8") as fh:
                json.dump({"result": ret, "outputs": ctx._outputs}, fh, ensure_ascii=False)


if __name__ == "__main__":
    _main()
"#;

/// Resolved Python interpreter descriptor returned by a [`ScriptPythonResolver`].
#[derive(Debug, Clone)]
pub struct ResolvedPythonInterpreter {
    pub executable: PathBuf,
    pub python_version: String,
    pub read_only_paths: Vec<PathBuf>,
    pub env_hash: String,
    pub venv_dir: Option<PathBuf>,
    pub lock_sha256: Option<String>,
}

/// Optional resolver hook allowing hosts or offline tests to supply a pre-built
/// or system Python interpreter without network downloads.
pub trait ScriptPythonResolver: Send + Sync {
    fn resolve(&self, spec: &ScriptEnvSpec) -> Option<ResolvedPythonInterpreter>;
}

/// Configuration options for [`ScriptService`].
#[derive(Clone)]
pub struct ScriptServiceOptions {
    pub python_resolver: Option<Arc<dyn ScriptPythonResolver>>,
    pub memory_limit_cap_bytes: Option<u64>,
    pub allow_landlock_fallback: bool,
}

impl Default for ScriptServiceOptions {
    fn default() -> Self {
        Self {
            python_resolver: None,
            memory_limit_cap_bytes: None,
            allow_landlock_fallback: true,
        }
    }
}

impl std::fmt::Debug for ScriptServiceOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptServiceOptions")
            .field("has_python_resolver", &self.python_resolver.is_some())
            .field("memory_limit_cap_bytes", &self.memory_limit_cap_bytes)
            .field("allow_landlock_fallback", &self.allow_landlock_fallback)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct ScriptInputArgs {
    code: String,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    python: Option<String>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    assets: Vec<ScriptInputAsset>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    memory_limit_bytes: Option<u64>,
    #[serde(default)]
    allow_source_build: bool,
}

#[derive(Debug, Deserialize)]
struct ScriptInputAsset {
    name: String,
    asset: String,
}

#[derive(Debug, Deserialize)]
struct RunnerResultFile {
    #[serde(default)]
    result: Value,
    #[serde(default)]
    outputs: Vec<RunnerOutputDecl>,
}

#[derive(Debug, Deserialize)]
struct RunnerOutputDecl {
    name: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    asset: Option<String>,
    #[serde(default)]
    media_type: Option<String>,
}

/// Kernel-managed service executing deterministic, single-step sandboxed Python
/// scripts (`script.python`) with two-stage approvals.
pub struct ScriptService {
    mod_paths: ModStorePaths,
    approvals: ScriptApprovalStore,
    python_resolver: RwLock<Option<Arc<dyn ScriptPythonResolver>>>,
    memory_limit_cap_bytes: Option<u64>,
    allow_landlock_fallback: bool,
}

impl std::fmt::Debug for ScriptService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptService")
            .field("mods_root", &self.mod_paths.mods_root())
            .field("memory_limit_cap_bytes", &self.memory_limit_cap_bytes)
            .field("allow_landlock_fallback", &self.allow_landlock_fallback)
            .finish_non_exhaustive()
    }
}

impl ScriptService {
    #[must_use]
    pub fn new(paths: AppPaths, mod_paths: ModStorePaths) -> Self {
        Self::with_options(paths, mod_paths, ScriptServiceOptions::default())
    }

    #[must_use]
    pub fn with_options(
        paths: AppPaths,
        mod_paths: ModStorePaths,
        options: ScriptServiceOptions,
    ) -> Self {
        Self {
            mod_paths,
            approvals: ScriptApprovalStore::new(paths),
            python_resolver: RwLock::new(options.python_resolver),
            memory_limit_cap_bytes: options.memory_limit_cap_bytes,
            allow_landlock_fallback: options.allow_landlock_fallback,
        }
    }

    /// Install or replace the optional Python interpreter resolver (primarily for tests).
    pub fn set_python_resolver(&self, resolver: Option<Arc<dyn ScriptPythonResolver>>) {
        *self
            .python_resolver
            .write()
            .expect("python_resolver lock poisoned") = resolver;
    }

    /// Build the [`OperationSpec`] for `script.python`.
    #[must_use]
    pub fn python_operation_spec() -> OperationSpec {
        OperationSpec {
            name: "script.python".to_owned(),
            description: "Execute a sandboxed, offline Python script step with deterministic inputs and declared outputs".to_owned(),
            input_schema: json!({
                "type": "object",
                "required": ["code"],
                "additionalProperties": false,
                "properties": {
                    "code": { "type": "string", "format": "inline-text" },
                    "dependencies": { "type": "array", "items": { "type": "string" } },
                    "python": { "type": "string" },
                    "input": { "type": "object" },
                    "assets": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string", "minLength": 1 },
                                "asset": { "type": "string", "format": "asset" }
                            },
                            "required": ["name", "asset"],
                            "additionalProperties": false
                        }
                    },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 300 },
                    "memory_limit_bytes": {
                        "type": "integer",
                        "minimum": 16777216,
                        "maximum": 8589934592u64
                    },
                    "allow_source_build": { "type": "boolean" }
                }
            }),
            output_schema: json!({
                "type": "object",
                "required": [
                    "result",
                    "outputs",
                    "stdout",
                    "stderr",
                    "stdout_truncated",
                    "stderr_truncated",
                    "env_hash",
                    "python",
                    "lock_sha256"
                ],
                "additionalProperties": false,
                "properties": {
                    "result": {},
                    "outputs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "asset": { "type": "string", "format": "asset" },
                                "media_type": { "type": "string" }
                            },
                            "required": ["name", "asset"],
                            "additionalProperties": false
                        }
                    },
                    "stdout": { "type": "string" },
                    "stderr": { "type": "string" },
                    "stdout_truncated": { "type": "boolean" },
                    "stderr_truncated": { "type": "boolean" },
                    "env_hash": { "type": "string" },
                    "python": { "type": "string" },
                    "lock_sha256": { "type": ["string", "null"] }
                }
            }),
            annotations: OperationAnnotations {
                read_only: false,
                destructive: false,
                idempotent: false,
                deterministic: true,
                open_world: false,
            },
        }
    }

    pub async fn approve_execution(
        &self,
        project_id: &ProjectId,
        exec_digest: &str,
    ) -> AppHostResult<()> {
        self.approvals
            .approve_execution(project_id, exec_digest)
            .await
    }

    pub async fn is_execution_approved(
        &self,
        project_id: &ProjectId,
        exec_digest: &str,
    ) -> AppHostResult<bool> {
        self.approvals
            .is_execution_approved(project_id, exec_digest)
            .await
    }

    pub async fn revoke_execution_approval(
        &self,
        project_id: &ProjectId,
        exec_digest: &str,
    ) -> AppHostResult<bool> {
        self.approvals
            .revoke_execution(project_id, exec_digest)
            .await
    }

    pub async fn list_approved_executions(
        &self,
        project_id: &ProjectId,
    ) -> AppHostResult<Vec<ApprovedScriptRecord>> {
        self.approvals.list_approved(project_id).await
    }

    #[must_use]
    pub fn pending_execution_review(
        &self,
        project_id: &ProjectId,
        exec_digest: &str,
    ) -> Option<ScriptApprovalReview> {
        self.approvals.pending_review(project_id, exec_digest)
    }

    pub fn approve_online_prepare(
        &self,
        project_id: &ProjectId,
        env_hash: &str,
    ) -> AppHostResult<()> {
        self.approvals.approve_online(project_id, env_hash)
    }

    #[must_use]
    pub fn is_online_prepare_approved(&self, project_id: &ProjectId, env_hash: &str) -> bool {
        self.approvals.is_online_approved(project_id, env_hash)
    }

    #[must_use]
    pub fn pending_online_prepare(
        &self,
        project_id: &ProjectId,
        env_hash: &str,
    ) -> Option<OnlinePrepareReview> {
        self.approvals.pending_online(project_id, env_hash)
    }

    fn custom_python_for(&self, spec: &ScriptEnvSpec) -> Option<ResolvedPythonInterpreter> {
        self.python_resolver
            .read()
            .expect("python_resolver lock poisoned")
            .as_ref()
            .and_then(|resolver| resolver.resolve(spec))
    }

    async fn resolve_runtime_for_call(
        &self,
        project_id: &ProjectId,
        spec: &ScriptEnvSpec,
    ) -> Result<ResolvedRuntime, Diagnostic> {
        if let Some(custom) = self.custom_python_for(spec) {
            return Ok(ResolvedRuntime::Custom(custom));
        }

        match inspect_script_env(&self.mod_paths, spec)
            .map_err(|e| script_caller_error("env_inspect", "ENV_INSPECT_FAILED", e.to_string()))?
        {
            ScriptEnvLookup::Ready(ready) => Ok(ResolvedRuntime::Managed(*ready)),
            ScriptEnvLookup::NeedsPrepare {
                env_hash,
                missing_uv,
                missing_managed_python,
                dependencies,
                ..
            } => {
                // Pure stdlib with `uv` + managed Python already installed locally is offline.
                let requires_network =
                    missing_uv || missing_managed_python || !dependencies.is_empty();
                if requires_network && !self.approvals.is_online_approved(project_id, &env_hash) {
                    let review = OnlinePrepareReview {
                        project_id: project_id.clone(),
                        env_hash: env_hash.clone(),
                        python: spec.python.clone(),
                        dependencies,
                        allow_source_build: spec.allow_source_build,
                        no_build: !spec.allow_source_build,
                        missing_uv,
                        missing_managed_python,
                    };
                    self.approvals.record_pending_online(review.clone());
                    return Err(online_prepare_required_diagnostic(&review));
                }

                if missing_managed_python {
                    ensure_managed_python(&self.mod_paths, &spec.python)
                        .await
                        .map_err(|e| {
                            script_caller_error("python_install", "PREPARE_FAILED", e.to_string())
                        })?;
                }
                let ready = prepare_script_env(&self.mod_paths, spec)
                    .await
                    .map_err(|e| script_caller_error("prepare", "PREPARE_FAILED", e.to_string()))?;
                Ok(ResolvedRuntime::Managed(ready))
            }
        }
    }

    async fn execute_python_call(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        let parsed: ScriptInputArgs = serde_json::from_value(call.invocation.input.clone())
            .map_err(|e| {
                script_caller_error(
                    "invalid_input",
                    "INVALID_INPUT",
                    format!("invalid script.python input: {e}"),
                )
            })?;

        let mut asset_map = BTreeMap::new();
        let mut asset_ro_paths = Vec::with_capacity(parsed.assets.len());
        for entry in parsed.assets {
            let trimmed_name = entry.name.trim();
            if trimmed_name.is_empty() {
                return Err(script_caller_error(
                    "invalid_asset_name",
                    "INVALID_INPUT",
                    "script asset entry `name` must not be empty",
                ));
            }
            if asset_map.contains_key(trimmed_name) {
                return Err(script_caller_error(
                    "duplicate_asset_name",
                    "INVALID_INPUT",
                    format!("duplicate script asset name `{trimmed_name}`"),
                ));
            }
            let trimmed_asset = entry.asset.trim();
            let asset_path = PathBuf::from(trimmed_asset);
            if trimmed_asset.is_empty() || !asset_path.is_absolute() {
                return Err(script_caller_error(
                    "invalid_asset_path",
                    "INVALID_INPUT",
                    format!(
                        "script asset `{trimmed_name}` must resolve to a non-empty absolute path"
                    ),
                ));
            }
            asset_ro_paths.push(asset_path);
            asset_map.insert(trimmed_name.to_owned(), entry.asset);
        }

        let spec = ScriptEnvSpec::resolve(
            &parsed.code,
            parsed.python.as_deref(),
            &parsed.dependencies,
            parsed.allow_source_build,
        )
        .map_err(|e| script_caller_error("invalid_spec", "INVALID_SPEC", e.to_string()))?;

        let resolved = self
            .resolve_runtime_for_call(&call.project_id, &spec)
            .await?;

        let (
            python_exe,
            python_version,
            env_hash,
            venv_dir,
            lock_sha256,
            mut env_ro_paths,
            uv_python_dir,
            env_lock_path,
        ) = match resolved {
            ResolvedRuntime::Managed(ready) => {
                let lock_path = self.mod_paths.script_env_lock_file(&ready.env_hash);
                let py_dir = self.mod_paths.uv_python_dir();
                (
                    ready.python_executable,
                    ready.metadata.python,
                    ready.env_hash,
                    ready.venv_dir,
                    ready.lock_sha256,
                    ready.read_only_paths,
                    py_dir.exists().then_some(py_dir),
                    Some(lock_path),
                )
            }
            ResolvedRuntime::Custom(custom) => {
                let venv_dir = custom.venv_dir.clone().unwrap_or_else(|| {
                    custom
                        .executable
                        .parent()
                        .unwrap_or(Path::new("/"))
                        .to_path_buf()
                });
                (
                    custom.executable,
                    custom.python_version,
                    custom.env_hash,
                    venv_dir,
                    custom.lock_sha256,
                    custom.read_only_paths,
                    None,
                    None,
                )
            }
        };

        let exec_digest = compute_execution_digest(
            &parsed.code,
            &spec.dependencies,
            &python_version,
            spec.allow_source_build,
            lock_sha256.as_deref(),
        );

        let approved = self
            .approvals
            .is_execution_approved(&call.project_id, &exec_digest)
            .await
            .map_err(|e| {
                script_caller_error("approval_load", "APPROVAL_STORE_ERROR", e.to_string())
            })?;
        if !approved {
            let review = ScriptApprovalReview {
                project_id: call.project_id.clone(),
                exec_digest,
                code: parsed.code,
                python: python_version,
                dependencies: spec.dependencies,
                allow_source_build: spec.allow_source_build,
                lock_sha256,
                offline_sandbox: true,
            };
            self.approvals.record_pending_review(review.clone());
            return Err(approval_required_diagnostic(&review));
        }

        let _shared_env_lock = match env_lock_path {
            Some(ref lock_path) => {
                let lock = FsLock::acquire_shared(lock_path).map_err(|e| {
                    script_caller_error("env_lock", "ENV_LOCK_ERROR", e.to_string())
                })?;
                let _ = touch_script_env_last_used(&self.mod_paths, &env_hash);
                Some(lock)
            }
            None => None,
        };

        let scratch_dir = call.invocation.output_dir.clone();
        ensure_script_scratch_dirs(&scratch_dir)
            .map_err(|e| script_caller_error("scratch_setup", "IO", e.to_string()))?;

        let meta_dir = scratch_dir.join("_mera");
        let input_json_path = meta_dir.join("input.json");
        let runner_py_path = meta_dir.join("runner.py");
        let user_script_path = meta_dir.join("user_script.py");
        let result_json_path = meta_dir.join("result.json");
        let out_dir = meta_dir.join("out");

        let input_payload = json!({
            "input": parsed.input.unwrap_or_else(|| json!({})),
            "assets": asset_map,
        });
        let input_bytes = serde_json::to_vec(&input_payload)
            .map_err(|e| script_caller_error("input_serialize", "INVALID_INPUT", e.to_string()))?;
        std::fs::write(&input_json_path, input_bytes)
            .and_then(|()| std::fs::write(&runner_py_path, MERA_RUNNER_PY))
            .and_then(|()| std::fs::write(&user_script_path, parsed.code.as_bytes()))
            .map_err(|e| {
                script_caller_error(
                    "scratch_write",
                    "IO",
                    format!("failed to stage script files: {e}"),
                )
            })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for ro_file in [&input_json_path, &runner_py_path, &user_script_path] {
                let _ = std::fs::set_permissions(ro_file, std::fs::Permissions::from_mode(0o444));
            }
        }

        let mut ro_paths = asset_ro_paths;
        ro_paths.append(&mut env_ro_paths);

        let timeout_secs = parsed
            .timeout_secs
            .unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECS)
            .clamp(1, MAX_SCRIPT_TIMEOUT_SECS);
        let mut memory_bytes = parsed
            .memory_limit_bytes
            .unwrap_or(DEFAULT_SCRIPT_MEMORY_BYTES)
            .clamp(MIN_SCRIPT_MEMORY_BYTES, MAX_SCRIPT_MEMORY_BYTES);
        if let Some(cap) = self.memory_limit_cap_bytes {
            memory_bytes = memory_bytes.min(cap);
        }

        let limits = ResourceLimits {
            wall_timeout: Some(Duration::from_secs(timeout_secs)),
            cpu_time_secs: Some(timeout_secs),
            memory_bytes: Some(memory_bytes),
            max_file_size_bytes: Some(256 * 1024 * 1024),
            max_open_files: Some(256),
            max_processes: Some(64),
        };

        let mut env_vars =
            build_script_python_env(&scratch_dir, &venv_dir, uv_python_dir.as_deref());
        env_vars.push((
            "MERA_INPUT_JSON".into(),
            input_json_path.as_os_str().to_os_string(),
        ));
        env_vars.push(("MERA_OUT_DIR".into(), out_dir.as_os_str().to_os_string()));
        env_vars.push((
            "MERA_RESULT_JSON".into(),
            result_json_path.as_os_str().to_os_string(),
        ));

        let policy = SandboxPolicy::for_script_step(ro_paths, scratch_dir.clone())
            .with_limits(limits)
            .with_landlock_fallback(self.allow_landlock_fallback)
            .with_envs(env_vars);

        let sandbox_cancel = SandboxCancelToken::new();
        if call.invocation.cancel.is_cancelled() {
            sandbox_cancel.cancel();
        }
        let bridge_in = call.invocation.cancel.clone();
        let bridge_out = sandbox_cancel.clone();
        let cancel_task = tokio::spawn(async move {
            bridge_in.cancelled().await;
            bridge_out.cancel();
        });

        let args: Vec<OsString> = vec![
            "-B".into(),
            "-P".into(),
            "-s".into(),
            runner_py_path.into_os_string(),
            user_script_path.into_os_string(),
        ];

        let exec_res = run_sandboxed(
            SandboxExecRequest::new(policy, python_exe)
                .with_args(args)
                .with_working_dir(&scratch_dir)
                .with_output_limits(SCRIPT_STDIO_LIMIT_BYTES, SCRIPT_STDIO_LIMIT_BYTES)
                .with_cancel(sandbox_cancel),
        )
        .await;
        cancel_task.abort();

        let run_out = exec_res.map_err(|e| {
            script_caller_error(
                "sandbox_failed",
                "SANDBOX_FAILED",
                format!("sandboxed Python execution failed to launch or run: {e}"),
            )
        })?;

        let stdout_str = String::from_utf8_lossy(&run_out.stdout).into_owned();
        let stderr_str = String::from_utf8_lossy(&run_out.stderr).into_owned();

        match run_out.termination_reason {
            TerminationReason::Exited => {
                if !run_out.exit_status.is_some_and(|s| s.success()) {
                    let code = run_out.exit_status.and_then(|s| s.code());
                    return Err(script_caller_error(
                        "nonzero_exit",
                        "EXECUTION_FAILED",
                        format!(
                            "python script exited with status {:?}: {}",
                            code,
                            stderr_str.trim()
                        ),
                    ));
                }
            }
            TerminationReason::TimedOut => {
                return Err(script_caller_error(
                    "timeout",
                    "TIMEOUT",
                    format!(
                        "python script exceeded {timeout_secs}s wall-clock timeout and was terminated"
                    ),
                ));
            }
            TerminationReason::MemoryLimitExceeded => {
                return Err(script_caller_error(
                    "memory_limit",
                    "MEMORY_LIMIT_EXCEEDED",
                    format!(
                        "python script exceeded {memory_bytes} byte memory limit and was terminated"
                    ),
                ));
            }
            TerminationReason::Cancelled => {
                return Err(script_caller_error(
                    "cancelled",
                    "CANCELLED",
                    "python script execution was cancelled",
                ));
            }
        }

        let (result_value, output_entries) =
            read_and_validate_result(&scratch_dir, &out_dir, &result_json_path)?;

        Ok(json!({
            "result": result_value,
            "outputs": output_entries,
            "stdout": stdout_str,
            "stderr": stderr_str,
            "stdout_truncated": run_out.stdout_truncated,
            "stderr_truncated": run_out.stderr_truncated,
            "env_hash": env_hash,
            "python": python_version,
            "lock_sha256": lock_sha256,
        }))
    }
}

enum ResolvedRuntime {
    Managed(ScriptEnvReady),
    Custom(ResolvedPythonInterpreter),
}

#[allow(clippy::result_large_err)]
fn read_and_validate_result(
    scratch_dir: &Path,
    out_dir: &Path,
    result_json_path: &Path,
) -> Result<(Value, Vec<Value>), Diagnostic> {
    if !result_json_path.exists() {
        return Ok((Value::Null, Vec::new()));
    }
    let meta = std::fs::symlink_metadata(result_json_path).map_err(|e| {
        script_caller_error(
            "result_stat",
            "INVALID_OUTPUT",
            format!("failed to inspect `_mera/result.json`: {e}"),
        )
    })?;
    if meta.file_type().is_symlink() {
        return Err(script_caller_error(
            "result_symlink",
            "ESCAPE_OUTPUT_DIR",
            "`_mera/result.json` must not be a symlink",
        ));
    }
    if meta.len() > MAX_INLINE_TEXT_BYTES as u64 {
        return Err(script_caller_error(
            "result_too_large",
            "RESULT_TOO_LARGE",
            format!(
                "`_mera/result.json` ({} bytes) exceeds {} byte limit",
                meta.len(),
                MAX_INLINE_TEXT_BYTES
            ),
        ));
    }
    let raw = std::fs::read(result_json_path).map_err(|e| {
        script_caller_error(
            "result_read",
            "INVALID_OUTPUT",
            format!("failed to read `_mera/result.json`: {e}"),
        )
    })?;
    if raw.is_empty() {
        return Ok((Value::Null, Vec::new()));
    }

    let parsed: RunnerResultFile = serde_json::from_slice(&raw).map_err(|e| {
        script_caller_error(
            "result_json",
            "INVALID_OUTPUT",
            format!("`_mera/result.json` is not valid structured JSON: {e}"),
        )
    })?;

    let serialized_result = serde_json::to_vec(&parsed.result)
        .map_err(|e| script_caller_error("result_serialize", "INVALID_OUTPUT", e.to_string()))?;
    if serialized_result.len() > MAX_INLINE_TEXT_BYTES {
        return Err(script_caller_error(
            "result_payload_too_large",
            "RESULT_TOO_LARGE",
            format!(
                "script JSON `result` ({} bytes) exceeds {} byte inline limit",
                serialized_result.len(),
                MAX_INLINE_TEXT_BYTES
            ),
        ));
    }

    let canon_scratch = std::fs::canonicalize(scratch_dir).map_err(|e| {
        script_caller_error(
            "scratch_canon",
            "IO",
            format!("failed to canonicalize scratch dir: {e}"),
        )
    })?;
    let canon_out_dir = std::fs::canonicalize(out_dir).map_err(|e| {
        script_caller_error(
            "out_dir_canon",
            "IO",
            format!("failed to canonicalize `_mera/out`: {e}"),
        )
    })?;

    let mut normalized_outputs = Vec::with_capacity(parsed.outputs.len());
    for decl in parsed.outputs {
        let name = decl.name.trim();
        if name.is_empty() {
            return Err(script_caller_error(
                "empty_output_name",
                "INVALID_OUTPUT",
                "script output entry `name` must not be empty",
            ));
        }
        let raw_path = decl
            .path
            .as_deref()
            .or(decl.asset.as_deref())
            .ok_or_else(|| {
                script_caller_error(
                    "missing_output_path",
                    "INVALID_OUTPUT",
                    format!("script output `{name}` is missing `path`"),
                )
            })?;

        let resolved = validate_output_file_in_out_dir(&canon_out_dir, out_dir, raw_path)?;
        let rel_to_scratch = resolved.strip_prefix(&canon_scratch).map_err(|_| {
            script_caller_error(
                "escape_scratch",
                "ESCAPE_OUTPUT_DIR",
                format!("output `{raw_path}` resolves outside scratch dir"),
            )
        })?;
        let rel_str = path_to_relative_utf8(rel_to_scratch)?;

        let mut obj = serde_json::Map::new();
        obj.insert("name".to_owned(), Value::String(name.to_owned()));
        obj.insert("asset".to_owned(), Value::String(rel_str));
        if let Some(mt) = decl.media_type.filter(|s| !s.trim().is_empty()) {
            obj.insert("media_type".to_owned(), Value::String(mt));
        }
        normalized_outputs.push(Value::Object(obj));
    }

    Ok((parsed.result, normalized_outputs))
}

#[allow(clippy::result_large_err)]
pub(crate) fn path_to_relative_utf8(rel_to_scratch: &Path) -> Result<String, Diagnostic> {
    let mut parts = Vec::new();
    for comp in rel_to_scratch.components() {
        match comp {
            Component::Normal(s) => {
                let segment = s.to_str().ok_or_else(|| {
                    script_caller_error(
                        "non_utf8_output_path",
                        "INVALID_OUTPUT",
                        "script output path contains non-UTF-8 characters",
                    )
                })?;
                parts.push(segment);
            }
            Component::CurDir => {}
            other => {
                return Err(script_caller_error(
                    "invalid_output_component",
                    "INVALID_OUTPUT",
                    format!("unexpected relative path component `{other:?}` in output path"),
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(script_caller_error(
            "empty_output_path",
            "INVALID_OUTPUT",
            "script output path resolved to an empty relative path",
        ));
    }
    Ok(parts.join("/"))
}

#[allow(clippy::result_large_err)]
pub(crate) fn validate_output_file_in_out_dir(
    canon_out_dir: &Path,
    out_dir: &Path,
    raw_path: &str,
) -> Result<PathBuf, Diagnostic> {
    if raw_path.is_empty() || raw_path.contains('\0') {
        return Err(script_caller_error(
            "invalid_output_path",
            "INVALID_OUTPUT",
            "script output path must not be empty",
        ));
    }

    let candidate_path = Path::new(raw_path);
    if candidate_path
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(script_caller_error(
            "escape_out_dir",
            "ESCAPE_OUTPUT_DIR",
            format!("script output path `{raw_path}` contains `..`"),
        ));
    }

    let joined = if candidate_path.is_absolute() {
        candidate_path.to_path_buf()
    } else {
        out_dir.join(candidate_path)
    };

    let meta = std::fs::symlink_metadata(&joined).map_err(|e| {
        script_caller_error(
            "missing_output_file",
            "INVALID_OUTPUT",
            format!("declared output `{raw_path}` not found under `_mera/out`: {e}"),
        )
    })?;
    if meta.file_type().is_symlink() {
        return Err(script_caller_error(
            "output_symlink",
            "ESCAPE_OUTPUT_DIR",
            format!("declared output `{raw_path}` must not be a symlink"),
        ));
    }

    let canonical = std::fs::canonicalize(&joined).map_err(|e| {
        script_caller_error(
            "canon_output_file",
            "INVALID_OUTPUT",
            format!("failed to resolve declared output `{raw_path}`: {e}"),
        )
    })?;
    if !canonical.starts_with(canon_out_dir) {
        return Err(script_caller_error(
            "escape_out_dir",
            "ESCAPE_OUTPUT_DIR",
            format!("declared output `{raw_path}` resolves outside `_mera/out`"),
        ));
    }
    if !canonical.is_file() {
        return Err(script_caller_error(
            "non_file_output",
            "INVALID_OUTPUT",
            format!("declared output `{raw_path}` is not a regular file"),
        ));
    }
    Ok(canonical)
}

/// [`KernelOp`] adapter exposing [`ScriptService`] as `script.python`.
pub struct ScriptPythonOp {
    spec: OperationSpec,
    service: Arc<ScriptService>,
}

impl ScriptPythonOp {
    #[must_use]
    pub fn new(service: Arc<ScriptService>) -> Self {
        Self {
            spec: ScriptService::python_operation_spec(),
            service,
        }
    }
}

#[async_trait]
impl KernelOp for ScriptPythonOp {
    fn spec(&self) -> &OperationSpec {
        &self.spec
    }

    async fn invoke(&self, call: KernelCall) -> Result<Value, Diagnostic> {
        self.service.execute_python_call(call).await
    }
}

/// Register kernel script operations (`script.python`) on `operation_service`.
pub fn register_script_ops(
    operation_service: &OperationService,
    script_service: Arc<ScriptService>,
) {
    operation_service.register_kernel_op(Arc::new(ScriptPythonOp::new(script_service)));
}
