use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use crate::backend::SandboxBackend;
use crate::backend::{canonical_with_tail, unique_canonical_paths};
use crate::error::SandboxError;
#[cfg(target_os = "linux")]
use crate::policy::SandboxLaunch;
use crate::policy::{NetworkPolicy, SandboxPolicy};

fn sort_shallow_to_deep(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut unique: Vec<PathBuf> = paths
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    unique.sort_by(|a, b| {
        a.components()
            .count()
            .cmp(&b.components().count())
            .then_with(|| a.cmp(b))
    });
    unique
}

fn append_bwrap_base_args(wrapped_args: &mut Vec<OsString>, check_host_exists: bool) {
    wrapped_args.extend([
        "--new-session".into(),
        "--die-with-parent".into(),
        "--unshare-user".into(),
        "--unshare-pid".into(),
        "--unshare-ipc".into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--clearenv".into(),
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        "--tmpfs".into(),
        "/tmp".into(),
    ]);
    for sys_dir in [
        "/usr",
        "/lib",
        "/lib64",
        "/bin",
        "/sbin",
        "/etc",
        "/nix/store",
    ] {
        if !check_host_exists || Path::new(sys_dir).exists() {
            wrapped_args.push("--ro-bind".into());
            wrapped_args.push(sys_dir.into());
            wrapped_args.push(sys_dir.into());
        }
    }
    if check_host_exists && Path::new("/run/WSL").exists() {
        wrapped_args.extend([
            "--perms".into(),
            "000".into(),
            "--tmpfs".into(),
            "/run/WSL".into(),
            "--remount-ro".into(),
            "/run/WSL".into(),
        ]);
    }
}

pub(crate) fn build_bwrap_args(
    program: &Path,
    args: &[OsString],
    policy: &SandboxPolicy,
    check_host_exists: bool,
    supports_perms: bool,
) -> Result<Vec<OsString>, SandboxError> {
    let mut wrapped_args: Vec<OsString> = Vec::new();
    append_bwrap_base_args(&mut wrapped_args, check_host_exists);

    if policy.allow_host_paths {
        for host_root in ["/home", "/mnt", "/media"] {
            if !check_host_exists || Path::new(host_root).exists() {
                wrapped_args.push("--ro-bind".into());
                wrapped_args.push(host_root.into());
                wrapped_args.push(host_root.into());
            }
        }
    }

    let mut ro_paths = policy.read_only_paths.clone();
    if let Some(parent) = program.parent()
        && !parent.as_os_str().is_empty()
    {
        ro_paths.push(parent.to_path_buf());
    }
    for path in sort_shallow_to_deep(unique_canonical_paths(ro_paths)) {
        if !check_host_exists || path.exists() {
            wrapped_args.push("--ro-bind".into());
            wrapped_args.push(path.as_os_str().to_os_string());
            wrapped_args.push(path.as_os_str().to_os_string());
        }
    }

    let mut rw_paths = policy.read_write_paths.clone();
    if let Some(projects_root) = &policy.scratch_projects_root {
        rw_paths.push(projects_root.clone());
    }
    for path in sort_shallow_to_deep(unique_canonical_paths(rw_paths)) {
        if !check_host_exists || path.exists() {
            wrapped_args.push("--bind".into());
            wrapped_args.push(path.as_os_str().to_os_string());
            wrapped_args.push(path.as_os_str().to_os_string());
        }
    }

    // Apply read-only carve-outs shallow-to-deep after parent `--bind` mounts so
    // nested subpaths override their writable parent cleanly.
    for path in sort_shallow_to_deep(unique_canonical_paths(
        policy.read_only_carveouts.iter().cloned(),
    )) {
        if !check_host_exists || path.exists() {
            wrapped_args.push("--ro-bind".into());
            wrapped_args.push(path.as_os_str().to_os_string());
            wrapped_args.push(path.as_os_str().to_os_string());
        }
    }

    // Mask denied paths after writable and read-only mounts using a read-only
    // tmpfs (with `--perms 000` when supported by bwrap >= 0.7) for directories,
    // or `/dev/null` for files.
    for path in sort_shallow_to_deep(unique_canonical_paths(policy.deny_paths.iter().cloned())) {
        if !check_host_exists || path.is_dir() {
            if supports_perms {
                wrapped_args.push("--perms".into());
                wrapped_args.push("000".into());
            }
            wrapped_args.push("--tmpfs".into());
            wrapped_args.push(path.as_os_str().to_os_string());
            wrapped_args.push("--remount-ro".into());
            wrapped_args.push(path.as_os_str().to_os_string());
        } else if path.exists() {
            wrapped_args.push("--ro-bind".into());
            wrapped_args.push("/dev/null".into());
            wrapped_args.push(path.as_os_str().to_os_string());
        }
    }

    match policy.network {
        NetworkPolicy::Disabled | NetworkPolicy::LoopbackOnly => {
            wrapped_args.push("--unshare-net".into());
        }
        NetworkPolicy::FullAccess => {}
    }

    for (key, value) in &policy.env_allowlist {
        wrapped_args.push("--setenv".into());
        wrapped_args.push(key.clone());
        wrapped_args.push(value.clone());
    }

    wrapped_args.push("--".into());
    wrapped_args.push(program.as_os_str().to_os_string());
    wrapped_args.extend(args.iter().cloned());

    Ok(wrapped_args)
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub(crate) struct BwrapCapabilities {
    pub binary: PathBuf,
    pub supports_perms: bool,
}

#[cfg(target_os = "linux")]
fn find_bwrap_binary() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("MERA_BWRAP_BIN") {
        let candidate = PathBuf::from(explicit);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let system_bwrap = Path::new("/usr/bin/bwrap");
    if system_bwrap.is_file() {
        return Some(system_bwrap.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("bwrap");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(target_os = "linux")]
pub(crate) fn probe_bwrap_binary(bwrap: &Path, with_perms: bool) -> bool {
    let mut args = Vec::new();
    append_bwrap_base_args(&mut args, true);
    if with_perms {
        args.extend([
            "--perms".into(),
            "000".into(),
            "--tmpfs".into(),
            "/tmp/mera-bwrap-probe".into(),
            "--remount-ro".into(),
            "/tmp/mera-bwrap-probe".into(),
        ]);
    }
    args.push("--unshare-net".into());
    args.push("--".into());
    if let Some(true_bin) = ["/bin/true", "/usr/bin/true"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
    {
        args.push(true_bin.into());
    } else {
        args.push("/bin/sh".into());
        args.push("-c".into());
        args.push("true".into());
    }

    std::process::Command::new(bwrap)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
pub(crate) fn usable_bwrap_capabilities() -> Option<BwrapCapabilities> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static PROBE_CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<bool>>>> = OnceLock::new();

    let bwrap = find_bwrap_binary()?;
    let key = canonical_with_tail(&bwrap);
    let cache = PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some(&cached) = guard.get(&key)
    {
        return cached.map(|supports_perms| BwrapCapabilities {
            binary: bwrap,
            supports_perms,
        });
    }

    let result = if probe_bwrap_binary(&bwrap, false) {
        Some(probe_bwrap_binary(&bwrap, true))
    } else {
        None
    };
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, result);
    }
    result.map(|supports_perms| BwrapCapabilities {
        binary: bwrap,
        supports_perms,
    })
}

#[cfg(target_os = "linux")]
fn ensure_placeholder_dir(target: &Path, writable_roots: &[PathBuf]) -> Vec<PathBuf> {
    if target.exists() {
        return Vec::new();
    }
    let inside_writable = writable_roots.iter().any(|root| target.starts_with(root));
    if !inside_writable {
        return Vec::new();
    }
    let mut created = Vec::new();
    let mut missing = Vec::new();
    let mut cur = Some(target);
    while let Some(p) = cur {
        if p.exists() {
            break;
        }
        missing.push(p.to_path_buf());
        cur = p.parent();
    }
    for dir in missing.into_iter().rev() {
        if std::fs::create_dir(&dir).is_ok() {
            created.push(dir);
        } else {
            break;
        }
    }
    created
}

#[cfg(target_os = "linux")]
pub(crate) fn resolve_bwrap_launch(
    program: &Path,
    args: &[OsString],
    policy: &SandboxPolicy,
) -> Result<Option<SandboxLaunch>, SandboxError> {
    let Some(caps) = usable_bwrap_capabilities() else {
        return Ok(None);
    };

    let writable_roots = unique_canonical_paths(
        policy
            .read_write_paths
            .iter()
            .cloned()
            .chain(policy.scratch_projects_root.iter().cloned()),
    );
    let mut cleanup_paths = Vec::new();
    for protected in policy
        .read_only_carveouts
        .iter()
        .chain(policy.deny_paths.iter())
    {
        let resolved = canonical_with_tail(protected);
        cleanup_paths.extend(ensure_placeholder_dir(&resolved, &writable_roots));
    }

    let wrapped_args = build_bwrap_args(program, args, policy, true, caps.supports_perms)?;

    Ok(Some(SandboxLaunch {
        backend: SandboxBackend::Bubblewrap,
        program: caps.binary,
        args: wrapped_args,
        env: policy.env_allowlist.clone(),
        cleanup_paths,
    }))
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
fn _touch_canonical_import(path: &Path) -> PathBuf {
    canonical_with_tail(path)
}
