use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use mera_sandbox::{
    ResourceLimits, SandboxCancelToken, SandboxExecRequest, SandboxPolicy, TerminationReason,
    detect_sandbox_backend, run_sandboxed,
};

#[cfg(unix)]
fn process_is_alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(unix)]
fn wait_for_pid_file(path: &Path) -> i32 {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Ok(contents) = std::fs::read_to_string(path)
            && let Ok(pid) = contents.trim().parse::<i32>()
        {
            return pid;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for pid file at {}", path.display());
}

#[cfg(unix)]
fn wait_for_exit(pid: i32, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !process_is_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

#[cfg(unix)]
#[tokio::test]
async fn run_sandboxed_truncates_stdout_and_stderr_concurrently() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scratch = temp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    let policy = SandboxPolicy::for_mod(vec![], vec![scratch]).with_fail_closed(false);
    let mut req = SandboxExecRequest::new(policy, "/bin/sh")
        .with_args([
            OsString::from("-c"),
            OsString::from(
                "i=0; while [ $i -lt 500 ]; do printf 'OUT_0123456789ABCDEF\\n'; printf 'ERR_0123456789ABCDEF\\n' >&2; i=$((i+1)); done",
            ),
        ])
        .with_output_limits(256, 128);
    req.policy.limits.wall_timeout = Some(Duration::from_secs(10));

    let out = run_sandboxed(req).await.expect("run_sandboxed succeeds");
    assert_eq!(out.termination_reason, TerminationReason::Exited);
    assert!(out.exit_status.is_some_and(|s| s.success()));
    assert_eq!(out.stdout.len(), 256);
    assert_eq!(out.stderr.len(), 128);
    assert!(out.stdout_truncated);
    assert!(out.stderr_truncated);
    assert_eq!(out.backend, detect_sandbox_backend());
}

#[cfg(unix)]
#[tokio::test]
async fn run_sandboxed_wall_timeout_kills_process_group_including_grandchildren() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scratch = temp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let grandchild_pid_file = scratch.join("grandchild.pid");

    let mut policy = SandboxPolicy::for_mod(vec![], vec![scratch]).with_fail_closed(false);
    policy.limits = ResourceLimits {
        wall_timeout: Some(Duration::from_millis(300)),
        ..ResourceLimits::default()
    };

    let cmd = format!(
        "sleep 30 & echo $! > '{}'; wait",
        grandchild_pid_file.display()
    );
    let req = SandboxExecRequest::new(policy, "/bin/sh")
        .with_args([OsString::from("-c"), OsString::from(cmd)]);

    let started = std::time::Instant::now();
    let out = run_sandboxed(req)
        .await
        .expect("timeout run returns output");
    assert_eq!(out.termination_reason, TerminationReason::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "timeout kill took too long: {:?}",
        started.elapsed()
    );

    let grandchild_pid = wait_for_pid_file(&grandchild_pid_file);
    assert!(
        wait_for_exit(grandchild_pid, Duration::from_secs(2)),
        "grandchild pid {grandchild_pid} leaked after wall timeout"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn run_sandboxed_cancel_kills_process_group_including_grandchildren() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scratch = temp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let grandchild_pid_file = scratch.join("grandchild.pid");

    let policy = SandboxPolicy::for_mod(vec![], vec![scratch]).with_fail_closed(false);
    let cancel = SandboxCancelToken::new();
    let cmd = format!(
        "sleep 30 & echo $! > '{}'; wait",
        grandchild_pid_file.display()
    );
    let req = SandboxExecRequest::new(policy, "/bin/sh")
        .with_args([OsString::from("-c"), OsString::from(cmd)])
        .with_cancel(cancel.clone());

    let cancel_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
    });

    let out = run_sandboxed(req)
        .await
        .expect("cancelled run returns output");
    let _ = cancel_handle.await;
    assert_eq!(out.termination_reason, TerminationReason::Cancelled);

    let grandchild_pid = wait_for_pid_file(&grandchild_pid_file);
    assert!(
        wait_for_exit(grandchild_pid, Duration::from_secs(2)),
        "grandchild pid {grandchild_pid} leaked after cancellation"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn seatbelt_enforces_carveouts_deny_paths_and_ancestor_rename_lock() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ws = temp.path().join("ws");
    let sub = ws.join("sub");
    let git_dir = sub.join(".git");
    let future_ro = ws.join("future_ro");
    let secret_dir = ws.join("secret");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::create_dir_all(&secret_dir).unwrap();
    std::fs::write(git_dir.join("config"), "immutable\n").unwrap();
    std::fs::write(sub.join("sibling.txt"), "deletable\n").unwrap();
    std::fs::write(secret_dir.join("token.txt"), "top-secret\n").unwrap();

    let policy = SandboxPolicy::for_agent_workspace(
        vec![ws.clone()],
        vec![git_dir.clone(), future_ro.clone()],
        vec![secret_dir.clone()],
    );

    let script = format!(
        r#"
set -eu
echo "ok" > "{ws}/ok.txt"
rm "{sub}/sibling.txt"
if echo "tamper" >> "{git}/config" 2>/dev/null; then echo "FAIL_GIT_WRITE"; exit 10; fi
if mkdir "{future_ro}" 2>/dev/null; then echo "FAIL_FUTURE_RO_MKDIR"; exit 11; fi
if mv "{sub}" "{ws}/sub_moved" 2>/dev/null; then echo "FAIL_ANCESTOR_RENAME"; exit 12; fi
if cat "{secret}/token.txt" >/dev/null 2>&1; then echo "FAIL_DENY_READ"; exit 13; fi
if echo "leak" > "{secret}/evil.txt" 2>/dev/null; then echo "FAIL_DENY_WRITE"; exit 14; fi
echo "ALL_CARVEOUT_CHECKS_PASSED"
"#,
        ws = ws.display(),
        sub = sub.display(),
        git = git_dir.display(),
        future_ro = future_ro.display(),
        secret = secret_dir.display(),
    );

    let req = SandboxExecRequest::new(policy, "/bin/sh")
        .with_args([OsString::from("-c"), OsString::from(script)]);
    let out = run_sandboxed(req).await.expect("seatbelt test ran");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.termination_reason, TerminationReason::Exited);
    assert!(
        out.exit_status.is_some_and(|s| s.success()),
        "seatbelt check failed: status={:?}, stdout={stdout}, stderr={stderr}",
        out.exit_status
    );
    assert!(stdout.contains("ALL_CARVEOUT_CHECKS_PASSED"));
    assert_eq!(std::fs::read_to_string(ws.join("ok.txt")).unwrap(), "ok\n");
    assert_eq!(
        std::fs::read_to_string(git_dir.join("config")).unwrap(),
        "immutable\n"
    );
    assert!(!sub.join("sibling.txt").exists());
    assert!(!future_ro.exists());
    assert!(!secret_dir.join("evil.txt").exists());
}

#[cfg(target_os = "macos")]
fn resolve_macos_python() -> (std::path::PathBuf, Vec<std::path::PathBuf>) {
    if let Ok(out) = std::process::Command::new("xcrun")
        .args(["-f", "python3"])
        .output()
        && out.status.success()
    {
        let raw = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let py = std::path::PathBuf::from(raw);
        if py.is_file() {
            let mut ro_roots = Vec::new();
            for candidate in [
                "/Applications/Xcode.app/Contents/Developer",
                "/Library/Developer/CommandLineTools",
            ] {
                let p = std::path::PathBuf::from(candidate);
                if py.starts_with(&p) {
                    ro_roots.push(p);
                }
            }
            if ro_roots.is_empty()
                && let Some(parent) = py.parent().and_then(|p| p.parent())
            {
                ro_roots.push(parent.to_path_buf());
            }
            return (py, ro_roots);
        }
    }
    (std::path::PathBuf::from("/usr/bin/python3"), Vec::new())
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn seatbelt_script_step_runs_python_offline_and_blocks_outside_writes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scratch = temp.path().join("scratch");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let (python_bin, ro_roots) = resolve_macos_python();
    let mut policy = SandboxPolicy::for_script_step(ro_roots, scratch.clone());
    policy.limits.wall_timeout = Some(Duration::from_secs(10));

    let py_code = format!(
        r#"
import os, socket
with open('{scratch}/out.txt', 'w') as f:
    f.write('step-ok')
try:
    with open('{outside}/escape.txt', 'w') as f:
        f.write('bad')
    raise AssertionError('outside write succeeded')
except PermissionError:
    pass
try:
    s = socket.socket()
    s.settimeout(0.5)
    s.connect(('1.1.1.1', 80))
    raise AssertionError('network connect succeeded')
except OSError:
    pass
print('PYTHON_STEP_OK')
"#,
        scratch = scratch.display(),
        outside = outside.display(),
    );

    let req = SandboxExecRequest::new(policy, python_bin)
        .with_args([OsString::from("-c"), OsString::from(py_code)])
        .with_working_dir(&scratch)
        .with_env("TMPDIR", scratch.as_os_str());
    let out = run_sandboxed(req).await.expect("python script step ran");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.termination_reason, TerminationReason::Exited);
    assert!(
        out.exit_status.is_some_and(|s| s.success()),
        "python script step failed: status={:?}, stdout={stdout}, stderr={stderr}",
        out.exit_status
    );
    assert!(stdout.contains("PYTHON_STEP_OK"));
    assert_eq!(
        std::fs::read_to_string(scratch.join("out.txt")).unwrap(),
        "step-ok"
    );
    assert!(!outside.join("escape.txt").exists());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn seatbelt_memory_limit_kills_allocating_process_via_proc_pid_rusage() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scratch = temp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    let (python_bin, ro_roots) = resolve_macos_python();
    let mut policy = SandboxPolicy::for_script_step(ro_roots, scratch.clone());
    policy.limits.wall_timeout = Some(Duration::from_secs(10));
    policy.limits.memory_bytes = Some(24 * 1024 * 1024);

    // Touch 96 MiB of dirty pages so ri_phys_footprint reliably exceeds 24 MiB.
    let py_code = r#"
import time
buf = bytearray(96 * 1024 * 1024)
for off in range(0, len(buf), 4096):
    buf[off] = 1
time.sleep(10)
"#;

    let req = SandboxExecRequest::new(policy, python_bin)
        .with_args([OsString::from("-c"), OsString::from(py_code)])
        .with_working_dir(&scratch)
        .with_env("TMPDIR", scratch.as_os_str());
    let out = run_sandboxed(req).await.expect("memory limit test ran");
    assert_eq!(
        out.termination_reason,
        TerminationReason::MemoryLimitExceeded,
        "expected MemoryLimitExceeded, got {:?} (peak={:?}, stderr={})",
        out.termination_reason,
        out.peak_memory_bytes,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.peak_memory_bytes.is_some_and(|b| b > 24 * 1024 * 1024),
        "peak_memory_bytes={:?}",
        out.peak_memory_bytes
    );
}
