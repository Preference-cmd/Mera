use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::SandboxError;
use crate::policy::{NetworkPolicy, SandboxPolicy};

/// Network posture projected onto Windows MXC `BaseContainer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WindowsMxcNetworkMode {
    Disabled,
    LoopbackOnly,
    FullAccess,
}

/// Platform-neutral containment plan translated into an MXC `ContainerRequest` on Windows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WindowsMxcPlan {
    pub command_line: String,
    pub working_directory: Option<String>,
    pub readonly_paths: Vec<String>,
    pub readwrite_paths: Vec<String>,
    pub denied_paths: Vec<String>,
    pub network_mode: WindowsMxcNetworkMode,
    pub timeout_ms: Option<u32>,
    pub env: Vec<(String, String)>,
    pub inherit_default_env: bool,
    pub use_base_container_only: bool,
}

/// Quote a single Windows command-line argument using standard `CommandLineToArgvW` rules.
pub(crate) fn quote_windows_arg(arg: &OsStr) -> Result<String, SandboxError> {
    let raw = arg
        .to_str()
        .ok_or_else(|| SandboxError::ProfileSetupFailed {
            reason: "Windows sandbox command arguments must be valid UTF-8".to_owned(),
        })?;
    if raw.contains('\0') {
        return Err(SandboxError::ProfileSetupFailed {
            reason: "Windows sandbox command argument contains NUL byte".to_owned(),
        });
    }
    if !raw.is_empty()
        && !raw
            .bytes()
            .any(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'"'))
    {
        return Ok(raw.to_owned());
    }

    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in raw.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            other => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(other);
            }
        }
    }
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
    Ok(out)
}

/// Format `(program, args)` into a Windows `CreateProcessW` command line string.
pub(crate) fn format_windows_command_line(
    program: &Path,
    args: &[OsString],
) -> Result<String, SandboxError> {
    if program.as_os_str().is_empty() {
        return Err(SandboxError::InvalidPolicy {
            reason: "sandbox launch program path must not be empty".to_owned(),
        });
    }
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(quote_windows_arg(program.as_os_str())?);
    for arg in args {
        parts.push(quote_windows_arg(arg.as_os_str())?);
    }
    Ok(parts.join(" "))
}

fn collect_utf8_paths(
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Vec<String>, SandboxError> {
    let mut dedup = BTreeSet::new();
    for path in paths {
        if path.as_os_str().is_empty() {
            continue;
        }
        let s = path
            .to_str()
            .ok_or_else(|| SandboxError::ProfileSetupFailed {
                reason: format!("Windows MXC path must be valid UTF-8: `{}`", path.display()),
            })?;
        dedup.insert(s.to_owned());
    }
    Ok(dedup.into_iter().collect())
}

/// Convert `SandboxPolicy` and launch parameters into a [`WindowsMxcPlan`].
///
/// Only `BaseContainer` (`Containment::ProcessContainer`) is ever targeted;
/// AppContainer fallback is never emitted.
pub(crate) fn build_windows_mxc_plan(
    program: &Path,
    args: &[OsString],
    working_dir: Option<&Path>,
    policy: &SandboxPolicy,
) -> Result<WindowsMxcPlan, SandboxError> {
    let command_line = format_windows_command_line(program, args)?;
    let working_directory = match working_dir {
        Some(dir) if !dir.as_os_str().is_empty() => {
            let s = dir
                .to_str()
                .ok_or_else(|| SandboxError::ProfileSetupFailed {
                    reason: format!(
                        "Windows MXC working directory must be valid UTF-8: `{}`",
                        dir.display()
                    ),
                })?;
            Some(s.to_owned())
        }
        _ => None,
    };

    let mut ro_inputs = policy.read_only_paths.clone();
    if let Some(parent) = program.parent()
        && !parent.as_os_str().is_empty()
    {
        ro_inputs.push(parent.to_path_buf());
    }
    ro_inputs.extend(policy.read_only_carveouts.iter().cloned());

    let mut rw_inputs = policy.read_write_paths.clone();
    if let Some(projects_root) = &policy.scratch_projects_root {
        rw_inputs.push(projects_root.clone());
    }

    let readonly_paths = collect_utf8_paths(ro_inputs)?;
    let readwrite_paths = collect_utf8_paths(rw_inputs)?;
    let denied_paths = collect_utf8_paths(policy.deny_paths.iter().cloned())?;

    let network_mode = match policy.network {
        NetworkPolicy::Disabled => WindowsMxcNetworkMode::Disabled,
        NetworkPolicy::LoopbackOnly => WindowsMxcNetworkMode::LoopbackOnly,
        NetworkPolicy::FullAccess => WindowsMxcNetworkMode::FullAccess,
    };

    let timeout_ms = policy
        .limits
        .wall_timeout
        .map(|d| d.as_millis().min(u128::from(u32::MAX)) as u32);

    let mut env = Vec::with_capacity(policy.env_allowlist.len());
    for (k, v) in &policy.env_allowlist {
        let ks = k.to_str().ok_or_else(|| SandboxError::ProfileSetupFailed {
            reason: "Windows environment variable key must be valid UTF-8".to_owned(),
        })?;
        let vs = v.to_str().ok_or_else(|| SandboxError::ProfileSetupFailed {
            reason: "Windows environment variable value must be valid UTF-8".to_owned(),
        })?;
        env.push((ks.to_owned(), vs.to_owned()));
    }

    Ok(WindowsMxcPlan {
        command_line,
        working_directory,
        readonly_paths,
        readwrite_paths,
        denied_paths,
        network_mode,
        timeout_ms,
        env,
        // Layer explicit entries over the OS user-profile block so required Windows
        // variables (SystemDrive, SystemRoot, LOCALAPPDATA) remain valid.
        inherit_default_env: true,
        use_base_container_only: true,
    })
}

/// Return denied paths whose parent directories do not yet exist on disk.
pub(crate) fn missing_deny_parents(denied_paths: &[String]) -> Vec<PathBuf> {
    let mut missing = Vec::new();
    for denied in denied_paths {
        let path = Path::new(denied);
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            missing.push(parent.to_path_buf());
        }
    }
    missing
}

/// Drop guard removing only newly created denied-path parent directories in reverse order.
#[derive(Debug, Default)]
pub(crate) struct DeniedParentCleanupGuard(pub(crate) Vec<PathBuf>);

impl Drop for DeniedParentCleanupGuard {
    fn drop(&mut self) {
        for path in self.0.iter().rev() {
            let _ = std::fs::remove_dir(path);
        }
    }
}

/// Create absent parent directories for denied paths inside writable mounts and
/// return a [`DeniedParentCleanupGuard`] containing only the directories newly
/// created by this call.
pub(crate) fn prepare_denied_parents_if_needed(
    denied_paths: &[String],
    readwrite_paths: &[String],
) -> Result<DeniedParentCleanupGuard, SandboxError> {
    let mut guard = DeniedParentCleanupGuard::default();
    for missing_parent in missing_deny_parents(denied_paths) {
        let inside_writable = readwrite_paths
            .iter()
            .any(|root| missing_parent.starts_with(Path::new(root)));
        if !inside_writable {
            continue;
        }

        let mut to_create = Vec::new();
        let mut cursor = Some(missing_parent.as_path());
        while let Some(dir) = cursor {
            if dir.as_os_str().is_empty() || dir.exists() {
                break;
            }
            to_create.push(dir.to_path_buf());
            cursor = dir.parent();
        }

        for dir in to_create.into_iter().rev() {
            match std::fs::create_dir(&dir) {
                Ok(()) => guard.0.push(dir),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    return Err(SandboxError::ProfileSetupFailed {
                        reason: format!(
                            "failed to prepare denied path parent `{}`: {e}",
                            dir.display()
                        ),
                    });
                }
            }
        }
    }
    Ok(guard)
}

/// Structured user-facing diagnostic explaining why MXC `BaseContainer` is unavailable.
pub(crate) fn windows_base_container_unavailable_reason(
    probe_error: Option<&str>,
    tier: Option<&str>,
    deny_paths_required: bool,
    supports_deny_paths: bool,
) -> String {
    if let Some(tier_name) = tier
        && tier_name != "base-container"
    {
        return format!(
            "Windows MXC reported tier `{tier_name}`, but AppContainer fallback is forbidden; Windows 11 24H2 (build 26100.9278+) or later with BaseContainer (PSEC) is required"
        );
    }
    if deny_paths_required && !supports_deny_paths {
        return "Windows MXC BaseContainer on this OS build does not support native deniedPaths enforcement; Windows 11 24H2 (build 26100.9278+) or later is required".to_owned();
    }
    match probe_error {
        Some(err) if !err.trim().is_empty() => format!(
            "Windows MXC BaseContainer (PSEC) is unavailable ({err}); Windows 11 24H2 (build 26100.9278+) or later is required (AppContainer fallback is disabled)"
        ),
        _ => "Windows MXC BaseContainer (PSEC) is unavailable; Windows 11 24H2 (build 26100.9278+) or later is required (AppContainer fallback is disabled)".to_owned(),
    }
}

/// Evaluate Windows `mxc_sdk::v1::probe` output against `policy`.
///
/// Rejects any non-`base-container` tier (never falling back to AppContainer).
pub(crate) fn evaluate_probe_for_policy(
    tier: Option<&str>,
    api_present: bool,
    supports_deny_paths: bool,
    probe_error: Option<&str>,
    policy: &SandboxPolicy,
) -> Result<(), SandboxError> {
    let deny_paths_required = !policy.deny_paths.is_empty();
    let usable = api_present
        && tier == Some("base-container")
        && probe_error.is_none()
        && (!deny_paths_required || supports_deny_paths);

    if usable {
        return Ok(());
    }

    Err(SandboxError::BackendUnavailable {
        profile: policy.profile,
        reason: windows_base_container_unavailable_reason(
            probe_error,
            tier,
            deny_paths_required,
            supports_deny_paths,
        ),
    })
}

#[cfg(windows)]
mod windows_impl {
    use std::io::Read;
    use std::os::windows::process::ExitStatusExt;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    use mxc_sdk::v1::configs::ProcessContainerConfig;
    use mxc_sdk::v1::{
        self, ContainerRequest, Containment, FilesystemPolicy, NetworkAction, NetworkEgressPolicy,
        NetworkIngressPolicy, NetworkPeerPolicy, NetworkPolicy as MxcNetworkPolicy,
        NetworkRulePolicy, SpawnOptions, UiPolicy, WaitResult,
    };

    use super::{
        WindowsMxcNetworkMode, WindowsMxcPlan, build_windows_mxc_plan, evaluate_probe_for_policy,
        prepare_denied_parents_if_needed,
    };
    use crate::backend::SandboxBackend;
    use crate::error::SandboxError;
    use crate::policy::SandboxPolicy;
    use crate::runner::{SandboxExecRequest, SandboxRunOutput, TerminationReason};

    fn base_containment() -> Containment {
        Containment::ProcessContainer(ProcessContainerConfig::default())
    }

    fn build_mxc_network(mode: WindowsMxcNetworkMode) -> MxcNetworkPolicy {
        match mode {
            WindowsMxcNetworkMode::Disabled => MxcNetworkPolicy {
                egress: Some(NetworkEgressPolicy {
                    default: Some(NetworkAction::Deny),
                    ..Default::default()
                }),
                ingress: Some(NetworkIngressPolicy {
                    default: Some(NetworkAction::Deny),
                    host_loopback: Some(NetworkAction::Deny),
                }),
                runtime_config: None,
            },
            WindowsMxcNetworkMode::LoopbackOnly => MxcNetworkPolicy {
                egress: Some(NetworkEgressPolicy {
                    default: Some(NetworkAction::Deny),
                    allow: Some(vec![NetworkRulePolicy {
                        to: Some(vec![
                            NetworkPeerPolicy::new("127.0.0.0/8"),
                            NetworkPeerPolicy::new("::1/128"),
                        ]),
                        ports: None,
                    }]),
                    deny: None,
                }),
                ingress: Some(NetworkIngressPolicy {
                    default: Some(NetworkAction::Deny),
                    host_loopback: Some(NetworkAction::Allow),
                }),
                runtime_config: None,
            },
            WindowsMxcNetworkMode::FullAccess => MxcNetworkPolicy {
                egress: Some(NetworkEgressPolicy {
                    default: Some(NetworkAction::Allow),
                    ..Default::default()
                }),
                ingress: Some(NetworkIngressPolicy {
                    default: Some(NetworkAction::Allow),
                    host_loopback: Some(NetworkAction::Allow),
                }),
                runtime_config: None,
            },
        }
    }

    pub(crate) fn to_mxc_container_request(plan: &WindowsMxcPlan) -> ContainerRequest {
        let mut req = ContainerRequest::new(plan.command_line.clone());
        req.containment = base_containment();
        req.filesystem = Some(FilesystemPolicy {
            readwrite_paths: plan.readwrite_paths.clone(),
            readonly_paths: plan.readonly_paths.clone(),
            denied_paths: plan.denied_paths.clone(),
            clear_policy_on_exit: Some(true),
        });
        req.network = Some(build_mxc_network(plan.network_mode));
        req.ui = Some(UiPolicy::default());
        req.timeout_ms = plan.timeout_ms;
        req.working_directory = plan.working_directory.clone();
        req.environment = Some(plan.env.clone());
        req.inherit_default_environment = Some(plan.inherit_default_env);
        req
    }

    static BASE_CONTAINER_AVAILABLE: OnceLock<bool> = OnceLock::new();

    pub(crate) fn is_base_container_available() -> bool {
        *BASE_CONTAINER_AVAILABLE.get_or_init(|| {
            let mut req = ContainerRequest::new("cmd.exe /c exit 0");
            req.containment = base_containment();
            let Ok(probe) = v1::probe(Some(&req)) else {
                return false;
            };
            probe.tier == Some("base-container")
                && probe.error.is_none()
                && probe.probes.base_container_api_present
        })
    }

    pub(crate) fn probe_base_container(
        req: Option<&ContainerRequest>,
        policy: &SandboxPolicy,
    ) -> Result<(), SandboxError> {
        let probe = v1::probe(req).map_err(|e| SandboxError::BackendUnavailable {
            profile: policy.profile,
            reason: format!("Windows MXC probe failed: {e}"),
        })?;
        evaluate_probe_for_policy(
            probe.tier,
            probe.probes.base_container_api_present,
            probe.probes.base_container_supports_deny_paths,
            probe.error.as_deref(),
            policy,
        )
    }

    fn spawn_bounded_reader(
        stream: Option<Box<dyn Read + Send>>,
        max_bytes: usize,
    ) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
        std::thread::spawn(move || {
            let Some(mut reader) = stream else {
                return (Vec::new(), false);
            };
            let mut out = Vec::new();
            let mut truncated = false;
            let mut buf = [0_u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if out.len() < max_bytes {
                            let remaining = max_bytes - out.len();
                            let take = remaining.min(n);
                            out.extend_from_slice(&buf[..take]);
                            if n > remaining {
                                truncated = true;
                            }
                        } else {
                            truncated = true;
                        }
                    }
                    Err(_) => break,
                }
            }
            (out, truncated)
        })
    }

    pub(crate) async fn run_windows_mxc(
        request: SandboxExecRequest,
        effective_policy: SandboxPolicy,
    ) -> Result<SandboxRunOutput, SandboxError> {
        let plan = build_windows_mxc_plan(
            &request.program,
            &request.args,
            request.working_dir.as_deref(),
            &effective_policy,
        )?;
        let _denied_parent_guard =
            prepare_denied_parents_if_needed(&plan.denied_paths, &plan.readwrite_paths)?;

        let mxc_request = to_mxc_container_request(&plan);
        probe_base_container(Some(&mxc_request), &effective_policy)?;

        let program_for_err = request.program.clone();
        let mut proc =
            tokio::task::spawn_blocking(move || v1::spawn(mxc_request, SpawnOptions::default()))
                .await
                .map_err(|e| SandboxError::SpawnFailed {
                    program: program_for_err.clone(),
                    source: std::io::Error::other(format!("MXC spawn task panicked: {e}")),
                })?
                .map_err(|e| SandboxError::SpawnFailed {
                    program: program_for_err,
                    source: std::io::Error::other(e.to_string()),
                })?;

        let stdout_closer = proc.stdout_closer();
        let stderr_closer = proc.stderr_closer();
        let stdout_handle = spawn_bounded_reader(proc.take_stdout(), request.max_stdout_bytes);
        let stderr_handle = spawn_bounded_reader(proc.take_stderr(), request.max_stderr_bytes);

        let proc_arc = Arc::new(Mutex::new(proc));
        let wall_sleep = effective_policy.limits.wall_timeout.map(tokio::time::sleep);
        tokio::pin!(wall_sleep);
        let cancel_token = request.cancel.clone();

        let mut poll_interval = tokio::time::interval(Duration::from_millis(20));
        poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let termination_reason = loop {
            tokio::select! {
                _ = poll_interval.tick() => {
                    let poll_res = {
                        let mut guard = proc_arc.lock().expect("mxc process lock poisoned");
                        guard.try_wait()
                    };
                    match poll_res {
                        Ok(Some(_)) => break TerminationReason::Exited,
                        Ok(None) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                            break TerminationReason::TimedOut;
                        }
                        Err(source) => {
                            return Err(SandboxError::WaitFailed { source });
                        }
                    }
                }
                _ = async {
                    if let Some(sleep) = wall_sleep.as_mut().as_pin_mut() {
                        sleep.await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    let proc_kill = Arc::clone(&proc_arc);
                    let _ = tokio::task::spawn_blocking(move || {
                        let mut guard = proc_kill.lock().expect("mxc process lock poisoned");
                        let _ = guard.kill_for_timeout();
                    })
                    .await;
                    break TerminationReason::TimedOut;
                }
                _ = async {
                    match &cancel_token {
                        Some(token) => token.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    let proc_kill = Arc::clone(&proc_arc);
                    let _ = tokio::task::spawn_blocking(move || {
                        let mut guard = proc_kill.lock().expect("mxc process lock poisoned");
                        let _ = guard.kill();
                    })
                    .await;
                    break TerminationReason::Cancelled;
                }
            }
        };

        let proc_wait = Arc::clone(&proc_arc);
        let wait_res = tokio::task::spawn_blocking(move || {
            let mut guard = proc_wait.lock().expect("mxc process lock poisoned");
            guard.wait()
        })
        .await
        .map_err(|e| SandboxError::WaitFailed {
            source: std::io::Error::other(format!("MXC wait task panicked: {e}")),
        })?;

        if termination_reason != TerminationReason::Exited {
            if let Some(closer) = stdout_closer {
                closer.close();
            }
            if let Some(closer) = stderr_closer {
                closer.close();
            }
        }

        let (final_reason, exit_status) = match (termination_reason, wait_res) {
            (TerminationReason::Exited, Ok(WaitResult::Exited(code))) => (
                TerminationReason::Exited,
                Some(std::process::ExitStatus::from_raw(code as u32)),
            ),
            (TerminationReason::Exited, Ok(WaitResult::TimedOut)) => {
                (TerminationReason::TimedOut, None)
            }
            (other, Ok(WaitResult::Exited(code))) => {
                (other, Some(std::process::ExitStatus::from_raw(code as u32)))
            }
            (other, Ok(WaitResult::TimedOut)) => (other, None),
            (TerminationReason::Exited, Err(source)) => {
                return Err(SandboxError::WaitFailed { source });
            }
            (other, Err(_)) => (other, None),
        };

        let (stdout, stdout_truncated) = tokio::task::spawn_blocking(move || {
            stdout_handle.join().unwrap_or_else(|_| (Vec::new(), false))
        })
        .await
        .unwrap_or_else(|_| (Vec::new(), false));
        let (stderr, stderr_truncated) = tokio::task::spawn_blocking(move || {
            stderr_handle.join().unwrap_or_else(|_| (Vec::new(), false))
        })
        .await
        .unwrap_or_else(|_| (Vec::new(), false));

        Ok(SandboxRunOutput {
            backend: SandboxBackend::WindowsMxc,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
            exit_status,
            termination_reason: final_reason,
            peak_memory_bytes: None,
        })
    }
}

#[cfg(windows)]
pub(crate) use windows_impl::{is_base_container_available, probe_base_container, run_windows_mxc};
