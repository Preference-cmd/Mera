//! Optional portable `uv` runtime discovery, bootstrap, and isolated venv preparation.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mera_package_installer::{
    ExtractionLimits, atomic_promote_dir, extract_tar_gz_safely, sha256_hex,
};
use serde::Serialize;

use super::inventory::ModStorePaths;
use super::service::ModManagerError;

/// Optional environment override pointing directly at a `uv` executable.
pub const MERA_UV_BIN_ENV: &str = "MERA_UV_BIN";

static UV_STAGING_NONCE: AtomicU64 = AtomicU64::new(1);

/// Where the active `uv` executable was resolved from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UvSource {
    EnvOverride,
    ManagedPortable,
    SystemPath,
    Missing,
}

/// Availability status of the optional `uv` script runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UvRuntimeStatus {
    pub available: bool,
    pub source: UvSource,
    pub path: Option<String>,
    pub managed_dir: String,
}

/// Resolved command + args + env needed to launch a `uv` script mod.
#[derive(Debug, Clone)]
pub struct ResolvedUvCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub extra_env: Vec<(OsString, OsString)>,
}

fn uv_binary_name() -> &'static str {
    if cfg!(windows) { "uv.exe" } else { "uv" }
}

/// Probe `MERA_UV_BIN`, `<mods_root>/runtime/uv/`, and `PATH` for `uv`.
#[must_use]
pub fn detect_uv_runtime(paths: &ModStorePaths) -> UvRuntimeStatus {
    let managed_dir = paths.uv_dir();
    if let Some(override_path) = std::env::var_os(MERA_UV_BIN_ENV) {
        let candidate = PathBuf::from(override_path);
        if candidate.is_file() {
            return UvRuntimeStatus {
                available: true,
                source: UvSource::EnvOverride,
                path: Some(candidate.display().to_string()),
                managed_dir: managed_dir.display().to_string(),
            };
        }
    }

    let managed_bin = find_binary_under(&managed_dir, uv_binary_name());
    if let Some(bin) = managed_bin {
        return UvRuntimeStatus {
            available: true,
            source: UvSource::ManagedPortable,
            path: Some(bin.display().to_string()),
            managed_dir: managed_dir.display().to_string(),
        };
    }

    if let Some(system_bin) = find_on_path(uv_binary_name()) {
        return UvRuntimeStatus {
            available: true,
            source: UvSource::SystemPath,
            path: Some(system_bin.display().to_string()),
            managed_dir: managed_dir.display().to_string(),
        };
    }

    UvRuntimeStatus {
        available: false,
        source: UvSource::Missing,
        path: None,
        managed_dir: managed_dir.display().to_string(),
    }
}

/// Resolve the active `uv` binary path, returning an explicit error when unavailable.
pub fn resolve_uv_binary(paths: &ModStorePaths, mod_id: &str) -> Result<PathBuf, ModManagerError> {
    let status = detect_uv_runtime(paths);
    status
        .path
        .map(PathBuf::from)
        .ok_or_else(|| ModManagerError::SelfCheckFailed {
            mod_id: mod_id.to_owned(),
            reason: "this mod requires the optional `uv` script runtime, which is not installed; install portable `uv` in Settings -> Mods or place `uv` on PATH".to_owned(),
        })
}

/// Return the dedicated isolated virtual environment directory for `(mod_id, version)`.
#[must_use]
pub fn mod_venv_dir(paths: &ModStorePaths, mod_id: &str, version: &str) -> PathBuf {
    paths.runtime_root().join("envs").join(mod_id).join(version)
}

/// Prepare a `uv` mod's isolated virtual environment under `<mods_root>/runtime/envs/<id>/<ver>`
/// when `requirements` is declared (runs during install while online setup is permitted).
pub async fn prepare_uv_environment(
    paths: &ModStorePaths,
    mod_id: &str,
    version: &str,
    package_root: &Path,
    python: Option<&str>,
    requirements: Option<&str>,
) -> Result<(), ModManagerError> {
    let Some(req_rel) = requirements else {
        return Ok(());
    };
    let req_path = package_root.join(req_rel);
    if !req_path.is_file() {
        return Err(ModManagerError::SelfCheckFailed {
            mod_id: mod_id.to_owned(),
            reason: format!("uv requirements file missing at `{}`", req_path.display()),
        });
    }

    let uv_bin = resolve_uv_binary(paths, mod_id)?;
    let venv_dir = mod_venv_dir(paths, mod_id, version);
    if let Some(parent) = venv_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ModManagerError::Io {
            path: parent.to_path_buf(),
            message: e.to_string(),
        })?;
    }
    let cache_dir = paths.uv_cache_dir();
    std::fs::create_dir_all(&cache_dir).map_err(|e| ModManagerError::Io {
        path: cache_dir.clone(),
        message: e.to_string(),
    })?;

    let mut venv_cmd = tokio::process::Command::new(&uv_bin);
    venv_cmd
        .arg("venv")
        .arg(&venv_dir)
        .arg("--clear")
        .env("UV_CACHE_DIR", &cache_dir);
    if let Some(py) = python {
        venv_cmd.arg("--python").arg(py);
    }
    let venv_out = venv_cmd
        .output()
        .await
        .map_err(|e| ModManagerError::SelfCheckFailed {
            mod_id: mod_id.to_owned(),
            reason: format!("failed to invoke `uv venv`: {e}"),
        })?;
    if !venv_out.status.success() {
        let stderr = String::from_utf8_lossy(&venv_out.stderr);
        return Err(ModManagerError::SelfCheckFailed {
            mod_id: mod_id.to_owned(),
            reason: format!("`uv venv` failed: {}", stderr.trim()),
        });
    }

    let mut pip_cmd = tokio::process::Command::new(&uv_bin);
    pip_cmd
        .arg("pip")
        .arg("install")
        .arg("--python")
        .arg(venv_python_path(&venv_dir))
        .arg("-r")
        .arg(&req_path)
        .env("UV_CACHE_DIR", &cache_dir);
    let pip_out = pip_cmd
        .output()
        .await
        .map_err(|e| ModManagerError::SelfCheckFailed {
            mod_id: mod_id.to_owned(),
            reason: format!("failed to invoke `uv pip install`: {e}"),
        })?;
    if !pip_out.status.success() {
        let stderr = String::from_utf8_lossy(&pip_out.stderr);
        return Err(ModManagerError::SelfCheckFailed {
            mod_id: mod_id.to_owned(),
            reason: format!("`uv pip install` failed: {}", stderr.trim()),
        });
    }

    Ok(())
}

/// Resolve the concrete command used to spawn a `uv` script mod (under OS sandbox).
///
/// When an isolated venv exists under `<mods_root>/runtime/envs/<id>/<ver>`, invokes
/// the venv Python directly so the sandboxed mod needs no network access at runtime.
/// Otherwise runs `uv run --offline --no-project` with the mod's script.
pub fn resolve_uv_entrypoint_command(
    paths: &ModStorePaths,
    mod_id: &str,
    version: &str,
    package_root: &Path,
    script_rel: &str,
    python: Option<&str>,
    extra_args: &[String],
) -> Result<ResolvedUvCommand, ModManagerError> {
    let script_path = package_root.join(script_rel);
    if !script_path.is_file() {
        return Err(ModManagerError::SelfCheckFailed {
            mod_id: mod_id.to_owned(),
            reason: format!(
                "uv entrypoint script missing at `{}`",
                script_path.display()
            ),
        });
    }

    let venv_dir = mod_venv_dir(paths, mod_id, version);
    let venv_py = venv_python_path(&venv_dir);
    if venv_py.is_file() {
        let mut args = vec![script_path.into_os_string()];
        args.extend(extra_args.iter().map(OsString::from));
        return Ok(ResolvedUvCommand {
            program: venv_py,
            args,
            extra_env: vec![
                ("PYTHONUNBUFFERED".into(), "1".into()),
                ("VIRTUAL_ENV".into(), venv_dir.into_os_string()),
            ],
        });
    }

    let uv_bin = resolve_uv_binary(paths, mod_id)?;
    let mut args: Vec<OsString> = vec!["run".into(), "--no-project".into()];
    if let Some(py) = python {
        args.push("--python".into());
        args.push(py.into());
    }
    args.push(script_path.into_os_string());
    args.extend(extra_args.iter().map(OsString::from));

    Ok(ResolvedUvCommand {
        program: uv_bin,
        args,
        extra_env: vec![
            ("PYTHONUNBUFFERED".into(), "1".into()),
            ("UV_CACHE_DIR".into(), paths.uv_cache_dir().into_os_string()),
        ],
    })
}

/// Return the default official Astral `uv` `.tar.gz` archive URL for the current platform, if known.
#[must_use]
pub fn default_uv_archive_url() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(
            "https://github.com/astral-sh/uv/releases/latest/download/uv-aarch64-apple-darwin.tar.gz",
        ),
        ("macos", "x86_64") => Some(
            "https://github.com/astral-sh/uv/releases/latest/download/uv-x86_64-apple-darwin.tar.gz",
        ),
        ("linux", "x86_64") => Some(
            "https://github.com/astral-sh/uv/releases/latest/download/uv-x86_64-unknown-linux-gnu.tar.gz",
        ),
        ("linux", "aarch64") => Some(
            "https://github.com/astral-sh/uv/releases/latest/download/uv-aarch64-unknown-linux-gnu.tar.gz",
        ),
        _ => None,
    }
}

/// Download and install portable `uv` into `<mods_root>/runtime/uv/` from a `.tar.gz` archive.
pub async fn bootstrap_portable_uv(
    paths: &ModStorePaths,
    archive_url: &str,
    expected_sha256: Option<&str>,
) -> Result<UvRuntimeStatus, ModManagerError> {
    let parsed = super::sources::validate_remote_source_url(archive_url)?;
    let client = reqwest::Client::new();
    let resp = client
        .get(parsed)
        .send()
        .await
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("failed to download portable uv: {e}"),
        })?;
    if !resp.status().is_success() {
        return Err(ModManagerError::RemoteCatalog {
            message: format!("portable uv download returned HTTP {}", resp.status()),
        });
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ModManagerError::RemoteCatalog {
            message: format!("failed to read portable uv archive: {e}"),
        })?;

    if let Some(expected) = expected_sha256 {
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(expected.trim()) {
            return Err(ModManagerError::ArchiveInvalid {
                reason: format!("portable uv SHA-256 mismatch: expected {expected}, got {actual}"),
            });
        }
    }

    let nonce = UV_STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
    let staging = paths.staging_root().join(format!("uv-bootstrap-{nonce}"));
    std::fs::create_dir_all(&staging).map_err(|e| ModManagerError::Io {
        path: staging.clone(),
        message: e.to_string(),
    })?;

    let extract_res =
        extract_tar_gz_safely(&bytes, &staging, &ExtractionLimits::default(), &[], None);
    if let Err(err) = extract_res {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(ModManagerError::ArchiveInvalid {
            reason: format!("failed to extract portable uv archive: {err}"),
        });
    }

    let Some(extracted_bin) = find_binary_under(&staging, uv_binary_name()) else {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(ModManagerError::ArchiveInvalid {
            reason: "portable uv archive did not contain a `uv` binary".to_owned(),
        });
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&extracted_bin) {
            let _ = std::fs::set_permissions(
                &extracted_bin,
                std::fs::Permissions::from_mode(meta.permissions().mode() | 0o755),
            );
        }
    }
    #[cfg(not(unix))]
    let _ = extracted_bin;

    let target_dir = paths.uv_dir();
    atomic_promote_dir(&staging, &target_dir).map_err(|e| {
        let _ = std::fs::remove_dir_all(&staging);
        ModManagerError::Io {
            path: target_dir.clone(),
            message: e.to_string(),
        }
    })?;

    Ok(detect_uv_runtime(paths))
}

fn venv_python_path(venv_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python3")
    }
}

fn find_on_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn find_binary_under(root: &Path, binary_name: &str) -> Option<PathBuf> {
    let direct = root.join(binary_name);
    if direct.is_file() {
        return Some(direct);
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return None;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let nested = path.join(binary_name);
            if nested.is_file() {
                return Some(nested);
            }
            let bin_nested = path.join("bin").join(binary_name);
            if bin_nested.is_file() {
                return Some(bin_nested);
            }
        }
    }
    None
}
