use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::Notify;

#[cfg(target_os = "linux")]
use crate::backend::landlock_seccomp::{PreparedLandlockSeccomp, prepare_landlock_seccomp};
#[cfg(windows)]
use crate::backend::windows_mxc;
use crate::backend::{SandboxBackend, detect_sandbox_backend_for, resolve_sandbox_launch};
use crate::error::SandboxError;
use crate::policy::SandboxPolicy;

#[cfg(unix)]
#[allow(unsafe_code)]
mod unix_sys;

/// Reason a sandboxed child execution ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminationReason {
    /// Process exited normally or via an unrelated signal.
    Exited,
    /// Exceeded configured wall-clock timeout.
    TimedOut,
    /// Exceeded configured physical memory limit.
    MemoryLimitExceeded,
    /// Aborted via [`SandboxCancelToken`].
    Cancelled,
}

/// Cooperative cancellation handle passed into [`SandboxExecRequest`].
#[derive(Debug, Clone, Default)]
pub struct SandboxCancelToken {
    inner: Arc<CancelInner>,
}

#[derive(Debug, Default)]
struct CancelInner {
    cancelled: AtomicBool,
    notify: Notify,
}

impl SandboxCancelToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::SeqCst) {
            self.inner.notify.notify_waiters();
        }
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.inner.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

/// Request parameters for executing a short-lived command inside the OS sandbox.
#[derive(Debug, Clone)]
pub struct SandboxExecRequest {
    pub policy: SandboxPolicy,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub working_dir: Option<PathBuf>,
    pub extra_env: Vec<(OsString, OsString)>,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub cancel: Option<SandboxCancelToken>,
}

impl SandboxExecRequest {
    /// Construct a request with default 64 KiB stdout/stderr capture bounds.
    #[must_use]
    pub fn new(policy: SandboxPolicy, program: impl Into<PathBuf>) -> Self {
        Self {
            policy,
            program: program.into(),
            args: Vec::new(),
            working_dir: None,
            extra_env: Vec::new(),
            max_stdout_bytes: 64 * 1024,
            max_stderr_bytes: 64 * 1024,
            cancel: None,
        }
    }

    #[must_use]
    pub fn with_args(mut self, args: impl IntoIterator<Item = OsString>) -> Self {
        self.args = args.into_iter().collect();
        self
    }

    #[must_use]
    pub fn with_working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    #[must_use]
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.extra_env.push((key.into(), value.into()));
        self
    }

    #[must_use]
    pub fn with_envs(mut self, envs: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        self.extra_env.extend(envs);
        self
    }

    #[must_use]
    pub fn with_output_limits(mut self, max_stdout_bytes: usize, max_stderr_bytes: usize) -> Self {
        self.max_stdout_bytes = max_stdout_bytes;
        self.max_stderr_bytes = max_stderr_bytes;
        self
    }

    #[must_use]
    pub fn with_cancel(mut self, cancel: SandboxCancelToken) -> Self {
        self.cancel = Some(cancel);
        self
    }
}

/// Bounded output, exit status, and termination reason returned by [`run_sandboxed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxRunOutput {
    pub backend: SandboxBackend,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub exit_status: Option<std::process::ExitStatus>,
    pub termination_reason: TerminationReason,
    pub peak_memory_bytes: Option<u64>,
}

struct PlaceholderGuard<'a>(&'a crate::policy::SandboxLaunch);

impl Drop for PlaceholderGuard<'_> {
    fn drop(&mut self) {
        self.0.cleanup_synthetic_paths();
    }
}

/// Execute `request` inside the resolved OS sandbox with process-group
/// isolation, resource limits, wall-clock timeout, cancellation, and bounded
/// concurrent stdout/stderr capture.
pub async fn run_sandboxed(request: SandboxExecRequest) -> Result<SandboxRunOutput, SandboxError> {
    if request
        .cancel
        .as_ref()
        .is_some_and(SandboxCancelToken::is_cancelled)
    {
        return Ok(SandboxRunOutput {
            backend: detect_sandbox_backend_for(request.policy.profile),
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            exit_status: None,
            termination_reason: TerminationReason::Cancelled,
            peak_memory_bytes: None,
        });
    }

    let mut effective_policy = request.policy.clone();
    effective_policy
        .env_allowlist
        .extend(request.extra_env.clone());

    #[cfg(windows)]
    {
        if windows_mxc::is_base_container_available() {
            return windows_mxc::run_windows_mxc(request, effective_policy).await;
        }
        if effective_policy.fail_closed {
            windows_mxc::probe_base_container(None, &effective_policy)?;
            return Err(SandboxError::BackendUnavailable {
                profile: effective_policy.profile,
                reason: windows_mxc::windows_base_container_unavailable_reason(
                    None,
                    None,
                    !effective_policy.deny_paths.is_empty(),
                    false,
                ),
            });
        }
    }

    #[cfg(target_os = "linux")]
    let (launch, prepared_landlock): (
        crate::policy::SandboxLaunch,
        Option<PreparedLandlockSeccomp>,
    ) = match resolve_sandbox_launch(&request.program, &request.args, &effective_policy) {
        Ok(launch) => (launch, None),
        Err(SandboxError::BackendUnavailable { .. }) => {
            let prepared = prepare_landlock_seccomp(&request.program, &effective_policy)?;
            (
                crate::policy::SandboxLaunch {
                    backend: SandboxBackend::LandlockSeccomp,
                    program: request.program.clone(),
                    args: request.args.clone(),
                    env: effective_policy.env_allowlist.clone(),
                    cleanup_paths: Vec::new(),
                },
                Some(prepared),
            )
        }
        Err(err) => return Err(err),
    };

    #[cfg(not(target_os = "linux"))]
    let launch = resolve_sandbox_launch(&request.program, &request.args, &effective_policy)?;

    let _placeholder_guard = PlaceholderGuard(&launch);

    let mut cmd = tokio::process::Command::new(&launch.program);
    cmd.args(&launch.args)
        .env_clear()
        .envs(launch.env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = &request.working_dir {
        cmd.current_dir(dir);
    }

    #[cfg(target_os = "linux")]
    unix_sys::configure_child_pre_exec(
        &mut cmd,
        &effective_policy.limits,
        prepared_landlock.as_ref(),
    );
    #[cfg(all(unix, not(target_os = "linux")))]
    unix_sys::configure_child_pre_exec(&mut cmd, &effective_policy.limits);

    let mut child = cmd.spawn().map_err(|source| SandboxError::SpawnFailed {
        program: request.program.clone(),
        source,
    })?;
    #[cfg(target_os = "linux")]
    drop(prepared_landlock);

    let child_pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_task = tokio::spawn(async move {
        match stdout {
            Some(pipe) => read_stream_bounded(pipe, request.max_stdout_bytes).await,
            None => (Vec::new(), false),
        }
    });
    let stderr_task = tokio::spawn(async move {
        match stderr {
            Some(pipe) => read_stream_bounded(pipe, request.max_stderr_bytes).await,
            None => (Vec::new(), false),
        }
    });

    let wall_sleep = effective_policy.limits.wall_timeout.map(tokio::time::sleep);
    tokio::pin!(wall_sleep);

    let cancel_token = request.cancel.clone();
    let memory_limit = effective_policy.limits.memory_bytes;
    let mut mem_interval = memory_limit.map(|_| tokio::time::interval(Duration::from_millis(25)));
    if let Some(interval) = mem_interval.as_mut() {
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    }

    let mut peak_memory_bytes: Option<u64> = None;
    let (termination_reason, exit_status) = loop {
        tokio::select! {
            wait_res = child.wait() => {
                let status = wait_res.map_err(|source| SandboxError::WaitFailed { source })?;
                break (TerminationReason::Exited, Some(status));
            }
            _ = async {
                if let Some(sleep) = wall_sleep.as_mut().as_pin_mut() {
                    sleep.await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                let status = kill_and_reap(&mut child, child_pid).await;
                break (TerminationReason::TimedOut, status);
            }
            _ = async {
                match &cancel_token {
                    Some(token) => token.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                let status = kill_and_reap(&mut child, child_pid).await;
                break (TerminationReason::Cancelled, status);
            }
            _ = async {
                match mem_interval.as_mut() {
                    Some(interval) => {
                        interval.tick().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {
                if let (Some(limit), Some(pid)) = (memory_limit, child_pid)
                    && let Some(current) = sample_memory_bytes(pid)
                {
                    peak_memory_bytes = Some(peak_memory_bytes.map_or(current, |prev| prev.max(current)));
                    if current > limit {
                        let status = kill_and_reap(&mut child, child_pid).await;
                        break (TerminationReason::MemoryLimitExceeded, status);
                    }
                }
            }
        }
    };

    let (stdout_bytes, stdout_truncated) =
        join_stream_task(stdout_task, termination_reason != TerminationReason::Exited).await;
    let (stderr_bytes, stderr_truncated) =
        join_stream_task(stderr_task, termination_reason != TerminationReason::Exited).await;

    Ok(SandboxRunOutput {
        backend: launch.backend,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
        stdout_truncated,
        stderr_truncated,
        exit_status,
        termination_reason,
        peak_memory_bytes,
    })
}

async fn join_stream_task(
    handle: tokio::task::JoinHandle<(Vec<u8>, bool)>,
    was_killed: bool,
) -> (Vec<u8>, bool) {
    if was_killed {
        let abort_handle = handle.abort_handle();
        match tokio::time::timeout(Duration::from_secs(2), handle).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => (Vec::new(), false),
            Err(_) => {
                abort_handle.abort();
                (Vec::new(), false)
            }
        }
    } else {
        handle.await.unwrap_or_else(|_| (Vec::new(), false))
    }
}

async fn kill_and_reap(
    child: &mut tokio::process::Child,
    child_pid: Option<u32>,
) -> Option<std::process::ExitStatus> {
    #[cfg(unix)]
    if let Some(pid) = child_pid {
        unix_sys::kill_process_group(pid);
    }
    #[cfg(not(unix))]
    let _ = child_pid;

    let _ = child.start_kill();
    match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(Ok(status)) => Some(status),
        _ => None,
    }
}

fn sample_memory_bytes(pid: u32) -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        unix_sys::sample_process_group_phys_footprint(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

async fn read_stream_bounded<R: AsyncRead + Unpin>(
    mut reader: R,
    max_bytes: usize,
) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;
    let mut buf = [0_u8; 8192];

    loop {
        match reader.read(&mut buf).await {
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
}
