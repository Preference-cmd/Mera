use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use jiff::Timestamp;
use mera_package_installer::{
    FsLock, FsTxError, atomic_promote_dir, atomic_write_file, sha256_hex,
};
use serde::{Deserialize, Serialize};

use super::pep723::{ScriptEnvError, ScriptEnvSpec, format_pep723_stub};
use crate::mod_manager::inventory::ModStorePaths;
use crate::mod_manager::uv::{detect_uv_runtime, resolve_uv_binary, venv_python_path};

/// Metadata file persisted inside `<runtime_root>/script-envs/<env_hash>/`.
pub const SCRIPT_ENV_METADATA_FILE: &str = ".mera-env.json";

static SCRIPT_ENV_STAGING_NONCE: AtomicU64 = AtomicU64::new(1);

/// Lightweight metadata persisted alongside each content-addressed script venv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptEnvMetadata {
    pub schema_version: u16,
    pub env_hash: String,
    pub python: String,
    pub target_triple: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub allow_source_build: bool,
    #[serde(default)]
    pub lock_sha256: Option<String>,
    pub created_at: String,
    pub last_used_at: String,
}

/// Resolved, cached script virtual environment ready for offline sandbox execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptEnvReady {
    pub env_hash: String,
    pub env_dir: PathBuf,
    pub venv_dir: PathBuf,
    pub python_executable: PathBuf,
    pub lock_sha256: Option<String>,
    pub read_only_paths: Vec<PathBuf>,
    pub metadata: ScriptEnvMetadata,
}

/// Offline inspection result from [`inspect_script_env`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptEnvLookup {
    Ready(Box<ScriptEnvReady>),
    NeedsPrepare {
        env_hash: String,
        env_dir: PathBuf,
        missing_uv: bool,
        missing_managed_python: bool,
        dependencies: Vec<String>,
    },
}

struct StagingCleanupGuard {
    dir: Option<PathBuf>,
}

impl StagingCleanupGuard {
    fn new(dir: PathBuf) -> Self {
        Self { dir: Some(dir) }
    }

    fn path(&self) -> &Path {
        self.dir.as_deref().expect("staging guard still active")
    }

    fn defuse(&mut self) {
        self.dir = None;
    }
}

impl Drop for StagingCleanupGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn dir_has_entries(path: &Path) -> bool {
    std::fs::read_dir(path)
        .ok()
        .and_then(|mut rd| rd.next())
        .is_some()
}

fn read_env_metadata(env_dir: &Path) -> Option<ScriptEnvMetadata> {
    let raw = std::fs::read(env_dir.join(SCRIPT_ENV_METADATA_FILE)).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn build_ready(
    paths: &ModStorePaths,
    env_dir: PathBuf,
    metadata: ScriptEnvMetadata,
) -> ScriptEnvReady {
    let venv_dir = env_dir.join("venv");
    let python_executable = venv_python_path(&venv_dir);
    let mut read_only_paths = vec![env_dir.clone()];
    let python_dir = paths.uv_python_dir();
    if python_dir.exists() {
        read_only_paths.push(python_dir);
    }
    ScriptEnvReady {
        env_hash: metadata.env_hash.clone(),
        env_dir,
        venv_dir,
        python_executable,
        lock_sha256: metadata.lock_sha256.clone(),
        read_only_paths,
        metadata,
    }
}

/// Inspect `<runtime_root>/script-envs/<env_hash>` purely offline without invoking
/// `uv` or performing any network I/O.
pub fn inspect_script_env(
    paths: &ModStorePaths,
    spec: &ScriptEnvSpec,
) -> Result<ScriptEnvLookup, ScriptEnvError> {
    let env_hash = spec.env_hash();
    let env_dir = paths.script_envs_dir().join(&env_hash);
    let venv_dir = env_dir.join("venv");
    let python_exe = venv_python_path(&venv_dir);

    if let Some(metadata) = read_env_metadata(&env_dir)
        && metadata.env_hash == env_hash
        && metadata.target_triple == spec.target_triple
        && metadata.python == spec.python
        && metadata.dependencies == spec.dependencies
        && metadata.allow_source_build == spec.allow_source_build
        && python_exe.is_file()
    {
        return Ok(ScriptEnvLookup::Ready(Box::new(build_ready(
            paths, env_dir, metadata,
        ))));
    }

    let uv_available = detect_uv_runtime(paths).available;
    let managed_python_present = dir_has_entries(&paths.uv_python_dir());
    Ok(ScriptEnvLookup::NeedsPrepare {
        env_hash,
        env_dir,
        missing_uv: !uv_available,
        missing_managed_python: !managed_python_present,
        dependencies: spec.dependencies.clone(),
    })
}

/// Update `last_used_at` in `.mera-env.json` for LRU cache accounting.
pub fn touch_script_env_last_used(
    paths: &ModStorePaths,
    env_hash: &str,
) -> Result<(), ScriptEnvError> {
    let meta_path = paths
        .script_envs_dir()
        .join(env_hash)
        .join(SCRIPT_ENV_METADATA_FILE);
    if !meta_path.is_file() {
        return Ok(());
    }
    let raw = std::fs::read(&meta_path).map_err(|e| ScriptEnvError::Io {
        path: meta_path.clone(),
        message: e.to_string(),
    })?;
    let mut metadata: ScriptEnvMetadata =
        serde_json::from_slice(&raw).map_err(|e| ScriptEnvError::PrepareFailed {
            reason: format!(
                "corrupt script environment metadata `{}`: {e}",
                meta_path.display()
            ),
        })?;
    metadata.last_used_at = Timestamp::now().to_string();
    let encoded =
        serde_json::to_vec_pretty(&metadata).map_err(|e| ScriptEnvError::PrepareFailed {
            reason: format!("failed to serialize script env metadata: {e}"),
        })?;
    atomic_write_file(&meta_path, &encoded).map_err(|e| ScriptEnvError::Io {
        path: meta_path,
        message: e.to_string(),
    })?;
    Ok(())
}

/// Explicitly install a managed Python distribution into `<runtime_root>/python` via `uv`.
pub async fn ensure_managed_python(
    paths: &ModStorePaths,
    python: &str,
) -> Result<(), ScriptEnvError> {
    let uv_bin = resolve_uv_binary(paths, "script").map_err(|e| ScriptEnvError::UvUnavailable {
        reason: e.to_string(),
    })?;
    let python_dir = paths.uv_python_dir();
    let cache_dir = paths.uv_cache_dir();
    std::fs::create_dir_all(&python_dir).map_err(|e| ScriptEnvError::Io {
        path: python_dir.clone(),
        message: e.to_string(),
    })?;
    std::fs::create_dir_all(&cache_dir).map_err(|e| ScriptEnvError::Io {
        path: cache_dir.clone(),
        message: e.to_string(),
    })?;

    let mut cmd = tokio::process::Command::new(&uv_bin);
    cmd.arg("python")
        .arg("install")
        .arg(python)
        .arg("--python-preference")
        .arg("only-managed")
        .env("UV_PYTHON_INSTALL_DIR", &python_dir)
        .env("UV_CACHE_DIR", &cache_dir);
    let out = cmd
        .output()
        .await
        .map_err(|e| ScriptEnvError::PythonUnavailable {
            python: python.to_owned(),
            reason: format!("failed to invoke `uv python install`: {e}"),
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(ScriptEnvError::PythonUnavailable {
            python: python.to_owned(),
            reason: stderr.trim().to_owned(),
        });
    }
    Ok(())
}

async fn run_uv_step(
    uv_bin: &Path,
    args: &[OsString],
    paths: &ModStorePaths,
    label: &str,
) -> Result<(), ScriptEnvError> {
    let mut cmd = tokio::process::Command::new(uv_bin);
    cmd.args(args)
        .env("UV_PYTHON_INSTALL_DIR", paths.uv_python_dir())
        .env("UV_CACHE_DIR", paths.uv_cache_dir())
        .env("UV_PYTHON_DOWNLOADS", "manual");
    let out = cmd
        .output()
        .await
        .map_err(|e| ScriptEnvError::PrepareFailed {
            reason: format!("failed to invoke `{label}`: {e}"),
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim()
        } else {
            stdout.trim()
        };
        return Err(ScriptEnvError::PrepareFailed {
            reason: format!("`{label}` failed: {detail}"),
        });
    }
    Ok(())
}

/// Phase 1 host-side preparation of a content-addressed read-only Python venv.
///
/// Never executes user script bodies: when third-party dependencies are needed,
/// emits an empty-body PEP 723 stub `deps.py`, locks via `uv lock --script`,
/// exports hashed requirements, and installs with `--require-hashes` and
/// `--no-build` (unless `spec.allow_source_build` is `true`).
pub async fn prepare_script_env(
    paths: &ModStorePaths,
    spec: &ScriptEnvSpec,
) -> Result<ScriptEnvReady, ScriptEnvError> {
    if let ScriptEnvLookup::Ready(ready) = inspect_script_env(paths, spec)? {
        let _ = touch_script_env_last_used(paths, &ready.env_hash);
        return Ok(*ready);
    }

    let env_hash = spec.env_hash();
    let lock_path = paths.script_env_lock_file(&env_hash);
    let _fs_lock = FsLock::acquire(&lock_path).map_err(|e| match e {
        FsTxError::LockContended { .. } => ScriptEnvError::LockContended {
            env_hash: env_hash.clone(),
        },
        other => ScriptEnvError::Io {
            path: lock_path.clone(),
            message: other.to_string(),
        },
    })?;

    if let ScriptEnvLookup::Ready(ready) = inspect_script_env(paths, spec)? {
        let _ = touch_script_env_last_used(paths, &ready.env_hash);
        return Ok(*ready);
    }

    let uv_bin = resolve_uv_binary(paths, "script").map_err(|e| ScriptEnvError::UvUnavailable {
        reason: e.to_string(),
    })?;

    for dir in [
        paths.uv_python_dir(),
        paths.uv_cache_dir(),
        paths.script_envs_dir(),
        paths.staging_root(),
    ] {
        std::fs::create_dir_all(&dir).map_err(|e| ScriptEnvError::Io {
            path: dir,
            message: e.to_string(),
        })?;
    }

    let nonce = SCRIPT_ENV_STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
    let staging_dir = paths
        .staging_root()
        .join(format!("script-env-{env_hash}-{nonce}"));
    std::fs::create_dir_all(&staging_dir).map_err(|e| ScriptEnvError::Io {
        path: staging_dir.clone(),
        message: e.to_string(),
    })?;
    let mut staging_guard = StagingCleanupGuard::new(staging_dir);

    let staged_root = staging_guard.path().to_path_buf();
    let venv_dir = staged_root.join("venv");

    let lock_sha256 = if spec.dependencies.is_empty() {
        let venv_args = vec![
            "venv".into(),
            venv_dir.clone().into_os_string(),
            "--clear".into(),
            "--python".into(),
            spec.python.clone().into(),
            "--python-preference".into(),
            "only-managed".into(),
        ];
        run_uv_step(&uv_bin, &venv_args, paths, "uv venv").await?;
        None
    } else {
        let deps_py = staged_root.join("deps.py");
        let stub = format_pep723_stub(spec.requires_python.as_deref(), &spec.dependencies);
        std::fs::write(&deps_py, stub.as_bytes()).map_err(|e| ScriptEnvError::Io {
            path: deps_py.clone(),
            message: e.to_string(),
        })?;

        let mut lock_args: Vec<OsString> = vec![
            "lock".into(),
            "--script".into(),
            deps_py.clone().into_os_string(),
            "--python".into(),
            spec.python.clone().into(),
            "--python-preference".into(),
            "only-managed".into(),
        ];
        if !spec.allow_source_build {
            lock_args.push("--no-build".into());
        }
        run_uv_step(&uv_bin, &lock_args, paths, "uv lock --script").await?;

        let lock_file = staged_root.join("deps.py.lock");
        let lock_bytes = std::fs::read(&lock_file).map_err(|e| ScriptEnvError::PrepareFailed {
            reason: format!(
                "`uv lock --script` did not produce `{}`: {e}",
                lock_file.display()
            ),
        })?;
        let lock_digest = sha256_hex(&lock_bytes);

        let venv_args = vec![
            "venv".into(),
            venv_dir.clone().into_os_string(),
            "--clear".into(),
            "--python".into(),
            spec.python.clone().into(),
            "--python-preference".into(),
            "only-managed".into(),
        ];
        run_uv_step(&uv_bin, &venv_args, paths, "uv venv").await?;

        let req_txt = staged_root.join("requirements.txt");
        let export_args = vec![
            "export".into(),
            "--script".into(),
            deps_py.into_os_string(),
            "--frozen".into(),
            "--format".into(),
            "requirements-txt".into(),
            "--output-file".into(),
            req_txt.clone().into_os_string(),
        ];
        run_uv_step(&uv_bin, &export_args, paths, "uv export --script").await?;

        let venv_py = venv_python_path(&venv_dir);
        let mut pip_args: Vec<OsString> = vec![
            "pip".into(),
            "install".into(),
            "--python".into(),
            venv_py.into_os_string(),
            "--require-hashes".into(),
            "-r".into(),
            req_txt.into_os_string(),
        ];
        if !spec.allow_source_build {
            pip_args.push("--no-build".into());
        }
        run_uv_step(&uv_bin, &pip_args, paths, "uv pip install").await?;

        Some(lock_digest)
    };

    let staged_python = venv_python_path(&venv_dir);
    if !staged_python.is_file() {
        return Err(ScriptEnvError::PrepareFailed {
            reason: format!(
                "prepared venv is missing Python interpreter at `{}`",
                staged_python.display()
            ),
        });
    }

    let now = Timestamp::now().to_string();
    let metadata = ScriptEnvMetadata {
        schema_version: 1,
        env_hash: env_hash.clone(),
        python: spec.python.clone(),
        target_triple: spec.target_triple.clone(),
        dependencies: spec.dependencies.clone(),
        allow_source_build: spec.allow_source_build,
        lock_sha256,
        created_at: now.clone(),
        last_used_at: now,
    };
    let meta_path = staged_root.join(SCRIPT_ENV_METADATA_FILE);
    let encoded_meta =
        serde_json::to_vec_pretty(&metadata).map_err(|e| ScriptEnvError::PrepareFailed {
            reason: format!("failed to serialize script env metadata: {e}"),
        })?;
    std::fs::write(&meta_path, encoded_meta).map_err(|e| ScriptEnvError::Io {
        path: meta_path,
        message: e.to_string(),
    })?;

    let env_dir = paths.script_envs_dir().join(&env_hash);
    atomic_promote_dir(&staged_root, &env_dir).map_err(|e| ScriptEnvError::Io {
        path: env_dir.clone(),
        message: e.to_string(),
    })?;
    staging_guard.defuse();

    Ok(build_ready(paths, env_dir, metadata))
}

/// Create the writable temporary subdirectories under `<scratch_dir>/_mera/tmp`
/// referenced by [`build_script_python_env`].
pub fn ensure_script_scratch_dirs(scratch_dir: &Path) -> Result<(), ScriptEnvError> {
    let tmp_root = scratch_dir.join("_mera").join("tmp");
    for dir in [
        tmp_root.clone(),
        tmp_root.join("home"),
        tmp_root.join("cache"),
        tmp_root.join("mpl"),
        scratch_dir.join("_mera").join("out"),
    ] {
        std::fs::create_dir_all(&dir).map_err(|e| ScriptEnvError::Io {
            path: dir,
            message: e.to_string(),
        })?;
    }
    Ok(())
}

/// Build the Phase 2 offline sandboxed Python environment variables.
///
/// Prevents bytecode/user-site writes into read-only mounts, forces safe `sys.path`
/// and UTF-8 I/O, redirects all home/temp/cache paths under `<scratch_dir>/_mera/tmp`,
/// and pins BLAS/OpenMP libraries to single-threaded deterministic execution.
#[must_use]
pub fn build_script_python_env(
    scratch_dir: &Path,
    venv_dir: &Path,
    uv_python_dir: Option<&Path>,
) -> Vec<(OsString, OsString)> {
    let tmp_root = scratch_dir.join("_mera").join("tmp");
    let home_dir = tmp_root.join("home");
    let cache_dir = tmp_root.join("cache");
    let mpl_dir = tmp_root.join("mpl");

    let mut env = vec![
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        ("PYTHONNOUSERSITE".into(), "1".into()),
        ("PYTHONSAFEPATH".into(), "1".into()),
        ("PYTHONUNBUFFERED".into(), "1".into()),
        ("PYTHONUTF8".into(), "1".into()),
        ("VIRTUAL_ENV".into(), venv_dir.as_os_str().to_os_string()),
        ("HOME".into(), home_dir.into_os_string()),
        ("TMPDIR".into(), tmp_root.as_os_str().to_os_string()),
        ("TEMP".into(), tmp_root.as_os_str().to_os_string()),
        ("TMP".into(), tmp_root.into_os_string()),
        ("XDG_CACHE_HOME".into(), cache_dir.into_os_string()),
        ("MPLCONFIGDIR".into(), mpl_dir.into_os_string()),
        ("OMP_NUM_THREADS".into(), "1".into()),
        ("OPENBLAS_NUM_THREADS".into(), "1".into()),
        ("MKL_NUM_THREADS".into(), "1".into()),
        ("NUMEXPR_NUM_THREADS".into(), "1".into()),
        ("UV_PYTHON_DOWNLOADS".into(), "never".into()),
    ];
    if let Some(python_dir) = uv_python_dir {
        env.push((
            "UV_PYTHON_INSTALL_DIR".into(),
            python_dir.as_os_str().to_os_string(),
        ));
    }
    env
}
