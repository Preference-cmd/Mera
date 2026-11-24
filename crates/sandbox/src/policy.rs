use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::backend::SandboxBackend;

/// Workload category selecting sandbox defaults and enforcement rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfileKind {
    /// Long-lived external mod child process communicating over stdio JSON-RPC.
    Mod,
    /// Short-lived deterministic script step with zero network and strict limits.
    ScriptStep,
    /// Long-lived agent workspace child process with workspace write access and carve-outs.
    AgentWorkspace,
}

/// Network access policy enforced by the OS sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// All network access is blocked.
    #[default]
    Disabled,
    /// Only host/namespace loopback communication is permitted.
    LoopbackOnly,
    /// Unfiltered network access is permitted.
    FullAccess,
}

/// Resource limits applied to a sandboxed child process tree.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Wall-clock deadline before the entire process group is killed.
    pub wall_timeout: Option<Duration>,
    /// Kernel `RLIMIT_CPU` cap in seconds.
    pub cpu_time_secs: Option<u64>,
    /// Physical memory limit (polled on macOS; `RLIMIT_AS` on non-macOS Unix).
    pub memory_bytes: Option<u64>,
    /// Maximum single output file size in bytes (`RLIMIT_FSIZE`).
    pub max_file_size_bytes: Option<u64>,
    /// Maximum number of open file descriptors (`RLIMIT_NOFILE`).
    pub max_open_files: Option<u64>,
    /// Maximum number of processes (`RLIMIT_NPROC` on non-macOS Unix).
    pub max_processes: Option<u64>,
}

impl ResourceLimits {
    /// Default resource limits for deterministic single-step script execution:
    /// 30s wall timeout, 30s CPU time, 2 GiB memory, 256 MiB file size, 256 fds.
    #[must_use]
    pub fn script_step_default() -> Self {
        Self {
            wall_timeout: Some(Duration::from_secs(30)),
            cpu_time_secs: Some(30),
            memory_bytes: Some(2 * 1024 * 1024 * 1024),
            max_file_size_bytes: Some(256 * 1024 * 1024),
            max_open_files: Some(256),
            max_processes: Some(64),
        }
    }
}

/// Unified OS sandbox policy shared across Mod, ScriptStep, and AgentWorkspace profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicy {
    pub profile: SandboxProfileKind,
    pub read_only_paths: Vec<PathBuf>,
    pub read_write_paths: Vec<PathBuf>,
    /// Subpaths inside writable roots that remain readable but cannot be modified
    /// (for example `.git` and resolved worktree gitdirs).
    pub read_only_carveouts: Vec<PathBuf>,
    /// Subpaths where both reads and writes are denied (for example application data roots).
    pub deny_paths: Vec<PathBuf>,
    /// Optional `<workspace>/projects` root whose per-project `scratch/` directories are writable.
    pub scratch_projects_root: Option<PathBuf>,
    /// Whether read access extends across normal host directories (Mod host-path permission).
    pub allow_host_paths: bool,
    pub network: NetworkPolicy,
    /// Explicit environment variables injected after clearing the inherited environment.
    pub env_allowlist: Vec<(OsString, OsString)>,
    pub limits: ResourceLimits,
    /// When true, refusing to run without an OS-enforced sandbox instead of falling back.
    pub fail_closed: bool,
    /// Whether Linux may fall back to Landlock + seccomp when userns/bwrap is unavailable.
    pub allow_landlock_fallback: bool,
}

impl SandboxPolicy {
    /// Construct a restrictive policy for an external Mod process.
    #[must_use]
    pub fn for_mod(read_only_paths: Vec<PathBuf>, read_write_paths: Vec<PathBuf>) -> Self {
        Self {
            profile: SandboxProfileKind::Mod,
            read_only_paths,
            read_write_paths,
            read_only_carveouts: Vec::new(),
            deny_paths: Vec::new(),
            scratch_projects_root: None,
            allow_host_paths: false,
            network: NetworkPolicy::Disabled,
            env_allowlist: Vec::new(),
            limits: ResourceLimits::default(),
            fail_closed: false,
            allow_landlock_fallback: false,
        }
    }

    /// Construct a restrictive policy for a single mod package root and data directory.
    #[must_use]
    pub fn mod_restricted(package_root: PathBuf, data_dir: PathBuf) -> Self {
        Self::for_mod(vec![package_root], vec![data_dir])
    }

    /// Construct a fail-closed, offline policy for a deterministic script step.
    #[must_use]
    pub fn for_script_step(read_only_paths: Vec<PathBuf>, scratch_dir: PathBuf) -> Self {
        Self {
            profile: SandboxProfileKind::ScriptStep,
            read_only_paths,
            read_write_paths: vec![scratch_dir],
            read_only_carveouts: Vec::new(),
            deny_paths: Vec::new(),
            scratch_projects_root: None,
            allow_host_paths: false,
            network: NetworkPolicy::Disabled,
            env_allowlist: Vec::new(),
            limits: ResourceLimits::script_step_default(),
            fail_closed: true,
            allow_landlock_fallback: true,
        }
    }

    /// Construct a fail-closed policy for an agent workspace subprocess.
    #[must_use]
    pub fn for_agent_workspace(
        read_write_roots: Vec<PathBuf>,
        read_only_carveouts: Vec<PathBuf>,
        deny_paths: Vec<PathBuf>,
    ) -> Self {
        Self {
            profile: SandboxProfileKind::AgentWorkspace,
            read_only_paths: Vec::new(),
            read_write_paths: read_write_roots,
            read_only_carveouts,
            deny_paths,
            scratch_projects_root: None,
            allow_host_paths: true,
            network: NetworkPolicy::FullAccess,
            env_allowlist: Vec::new(),
            limits: ResourceLimits::default(),
            fail_closed: true,
            allow_landlock_fallback: false,
        }
    }

    #[must_use]
    pub fn with_network(mut self, network: NetworkPolicy) -> Self {
        self.network = network;
        self
    }

    #[must_use]
    pub fn with_allow_host_paths(mut self, allow: bool) -> Self {
        self.allow_host_paths = allow;
        self
    }

    #[must_use]
    pub fn with_scratch_projects_root(mut self, root: Option<PathBuf>) -> Self {
        self.scratch_projects_root = root;
        self
    }

    #[must_use]
    pub fn with_read_only_carveouts(mut self, carveouts: Vec<PathBuf>) -> Self {
        self.read_only_carveouts = carveouts;
        self
    }

    #[must_use]
    pub fn with_deny_paths(mut self, deny_paths: Vec<PathBuf>) -> Self {
        self.deny_paths = deny_paths;
        self
    }

    #[must_use]
    pub fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    #[must_use]
    pub fn with_fail_closed(mut self, fail_closed: bool) -> Self {
        self.fail_closed = fail_closed;
        self
    }

    #[must_use]
    pub fn with_landlock_fallback(mut self, allow: bool) -> Self {
        self.allow_landlock_fallback = allow;
        self
    }

    #[must_use]
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env_allowlist.push((key.into(), value.into()));
        self
    }

    #[must_use]
    pub fn with_envs(mut self, envs: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        self.env_allowlist.extend(envs);
        self
    }
}

/// Resolved command wrapper produced by [`crate::resolve_sandbox_launch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxLaunch {
    pub backend: SandboxBackend,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    /// Host placeholder paths created for non-existent mount targets that
    /// should be cleaned up once the sandboxed child exits.
    pub cleanup_paths: Vec<PathBuf>,
}

impl SandboxLaunch {
    /// Remove any synthetic host mount-point placeholders created for this launch.
    pub fn cleanup_synthetic_paths(&self) {
        for path in self.cleanup_paths.iter().rev() {
            if path.is_dir() {
                let _ = std::fs::remove_dir(path);
            } else if path.exists() {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}
