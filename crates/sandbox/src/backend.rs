#[cfg(any(target_os = "macos", target_os = "linux", test))]
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::SandboxError;
use crate::policy::{SandboxLaunch, SandboxPolicy, SandboxProfileKind};

#[cfg(any(target_os = "linux", test))]
mod bwrap;
#[cfg(any(target_os = "macos", test))]
mod seatbelt;
#[cfg(test)]
mod tests;

/// Operating-system sandbox mechanism enforcing a child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackend {
    Seatbelt,
    Bubblewrap,
    LandlockSeccomp,
    WindowsMxc,
    EnvClearedOnly,
}

impl SandboxBackend {
    /// Stable snake_case identifier matching serialized UI and DTO fields.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Seatbelt => "seatbelt",
            Self::Bubblewrap => "bubblewrap",
            Self::LandlockSeccomp => "landlock_seccomp",
            Self::WindowsMxc => "windows_mxc",
            Self::EnvClearedOnly => "env_cleared_only",
        }
    }

    /// Returns `true` when this backend enforces isolation at the OS kernel level.
    #[must_use]
    pub const fn is_os_enforced(self) -> bool {
        matches!(
            self,
            Self::Seatbelt | Self::Bubblewrap | Self::LandlockSeccomp | Self::WindowsMxc
        )
    }
}

/// Probe which OS sandbox backend this host will enforce for child processes.
#[must_use]
pub fn detect_sandbox_backend() -> SandboxBackend {
    if sandbox_disabled_by_env() {
        return SandboxBackend::EnvClearedOnly;
    }
    #[cfg(target_os = "macos")]
    {
        if Path::new("/usr/bin/sandbox-exec").is_file() {
            return SandboxBackend::Seatbelt;
        }
        SandboxBackend::EnvClearedOnly
    }
    #[cfg(target_os = "linux")]
    {
        if bwrap::usable_bwrap_capabilities().is_some() {
            return SandboxBackend::Bubblewrap;
        }
        SandboxBackend::EnvClearedOnly
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        SandboxBackend::EnvClearedOnly
    }
}

/// Resolve `(program, args)` into a [`SandboxLaunch`] according to `policy`.
///
/// When `policy.fail_closed` is `true` and no suitable OS sandbox backend is
/// available, returns [`SandboxError::BackendUnavailable`] instead of silently
/// degrading to [`SandboxBackend::EnvClearedOnly`].
pub fn resolve_sandbox_launch(
    program: &Path,
    args: &[OsString],
    policy: &SandboxPolicy,
) -> Result<SandboxLaunch, SandboxError> {
    resolve_sandbox_launch_with_override(program, args, policy, sandbox_disabled_by_env(), false)
}

#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    allow(unused_variables)
)]
pub(crate) fn resolve_sandbox_launch_with_override(
    program: &Path,
    args: &[OsString],
    policy: &SandboxPolicy,
    mod_sandbox_disabled: bool,
    force_backend_unavailable: bool,
) -> Result<SandboxLaunch, SandboxError> {
    if program.as_os_str().is_empty() {
        return Err(SandboxError::InvalidPolicy {
            reason: "sandbox launch program path must not be empty".to_owned(),
        });
    }

    // `MERA_MOD_SANDBOX` is a Mod-only development toggle and must never
    // weaken or disable `ScriptStep` or `AgentWorkspace`.
    if mod_sandbox_disabled && policy.profile == SandboxProfileKind::Mod {
        if policy.fail_closed {
            return Err(SandboxError::BackendUnavailable {
                profile: policy.profile,
                reason: "OS sandbox is disabled via MERA_MOD_SANDBOX while fail_closed is enabled"
                    .to_owned(),
            });
        }
        return Ok(env_cleared_launch(program, args, policy));
    }

    if force_backend_unavailable {
        if policy.fail_closed {
            return Err(SandboxError::BackendUnavailable {
                profile: policy.profile,
                reason: "no OS-enforced sandbox backend is available".to_owned(),
            });
        }
        return Ok(env_cleared_launch(program, args, policy));
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(launch) = seatbelt::resolve_seatbelt_launch(program, args, policy)? {
            return Ok(launch);
        }
        if policy.fail_closed {
            return Err(SandboxError::BackendUnavailable {
                profile: policy.profile,
                reason: "macOS Seatbelt (/usr/bin/sandbox-exec) is unavailable".to_owned(),
            });
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(launch) = bwrap::resolve_bwrap_launch(program, args, policy)? {
            return Ok(launch);
        }
        if policy.fail_closed {
            return Err(SandboxError::BackendUnavailable {
                profile: policy.profile,
                reason:
                    "Linux bubblewrap (`bwrap`) is unavailable or unprivileged user namespaces are restricted"
                        .to_owned(),
            });
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        if policy.fail_closed {
            return Err(SandboxError::BackendUnavailable {
                profile: policy.profile,
                reason: "no OS-enforced sandbox backend is available on this platform".to_owned(),
            });
        }
    }

    Ok(env_cleared_launch(program, args, policy))
}

fn env_cleared_launch(program: &Path, args: &[OsString], policy: &SandboxPolicy) -> SandboxLaunch {
    SandboxLaunch {
        backend: SandboxBackend::EnvClearedOnly,
        program: program.to_path_buf(),
        args: args.to_vec(),
        env: policy.env_allowlist.clone(),
        cleanup_paths: Vec::new(),
    }
}

fn sandbox_disabled_by_env() -> bool {
    std::env::var("MERA_MOD_SANDBOX")
        .map(|val| {
            let trimmed = val.trim();
            trimmed.eq_ignore_ascii_case("off")
                || trimmed.eq_ignore_ascii_case("false")
                || trimmed == "0"
        })
        .unwrap_or(false)
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub(crate) fn canonical_with_tail(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let mut tail_parts = Vec::new();
    let mut current = path;
    while let Some(name) = current.file_name() {
        tail_parts.push(name.to_os_string());
        let Some(parent) = current.parent() else {
            break;
        };
        if parent.as_os_str().is_empty() {
            break;
        }
        if let Ok(mut base) = std::fs::canonicalize(parent) {
            for part in tail_parts.into_iter().rev() {
                base.push(part);
            }
            return base;
        }
        current = parent;
    }
    path.to_path_buf()
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub(crate) fn normalize_lexical(path: &Path) -> PathBuf {
    path.components().collect()
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn unique_canonical_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut set = BTreeSet::new();
    for path in paths {
        if !path.as_os_str().is_empty() {
            set.insert(canonical_with_tail(&path));
        }
    }
    set.into_iter().collect()
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub(crate) fn expand_logical_and_canonical(
    paths: impl IntoIterator<Item = PathBuf>,
) -> Vec<PathBuf> {
    let mut set = BTreeSet::new();
    for path in paths {
        if path.as_os_str().is_empty() {
            continue;
        }
        set.insert(normalize_lexical(&path));
        set.insert(canonical_with_tail(&path));
    }
    set.into_iter().collect()
}
