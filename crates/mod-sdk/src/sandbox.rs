//! OS-level process sandboxing wrapper for out-of-process mods.
//!
//! Derives a least-privilege execution wrapper from declared
//! [`ModPermissions`](crate::package::ModPermissions):
//! - macOS: Seatbelt (`/usr/bin/sandbox-exec`) with dynamic SBPL policy
//! - Linux: Bubblewrap (`bwrap`) with `--unshare-net` and explicit bind mounts
//! - Other platforms or constrained environments: honest `EnvClearedOnly`
//!   fallback via `cmd.env_clear()`

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Operating-system sandbox mechanism enforcing a mod child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackend {
    Seatbelt,
    Bubblewrap,
    EnvClearedOnly,
}

impl SandboxBackend {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Seatbelt => "seatbelt",
            Self::Bubblewrap => "bubblewrap",
            Self::EnvClearedOnly => "env_cleared_only",
        }
    }

    #[must_use]
    pub const fn is_os_enforced(self) -> bool {
        matches!(self, Self::Seatbelt | Self::Bubblewrap)
    }
}

/// Sandbox policy applied when spawning a mod child process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModSandboxPolicy {
    pub mod_id: String,
    pub allow_network: bool,
    pub allow_host_paths: bool,
    pub read_only_paths: Vec<PathBuf>,
    pub read_write_paths: Vec<PathBuf>,
    /// Optional `<workspace>/projects` root whose per-project `scratch/` subdirectories
    /// are writable by mod operations producing asset outputs.
    pub scratch_projects_root: Option<PathBuf>,
}

impl ModSandboxPolicy {
    /// Construct a restrictive policy for `mod_id` permitting read-only access
    /// to `package_root` and read-write access only to `data_dir`.
    #[must_use]
    pub fn restricted(mod_id: impl Into<String>, package_root: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            mod_id: mod_id.into(),
            allow_network: false,
            allow_host_paths: false,
            read_only_paths: vec![package_root],
            read_write_paths: vec![data_dir],
            scratch_projects_root: None,
        }
    }
}

/// Resolved command wrapper produced by [`resolve_sandbox_launch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxLaunch {
    pub backend: SandboxBackend,
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

/// Probe which sandbox backend this host will enforce for mod processes.
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
        if usable_bwrap_binary().is_some() {
            return SandboxBackend::Bubblewrap;
        }
        SandboxBackend::EnvClearedOnly
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        SandboxBackend::EnvClearedOnly
    }
}

/// Wrap `(program, args)` in the active OS sandbox for `policy`, falling back
/// cleanly to [`SandboxBackend::EnvClearedOnly`] when no OS sandbox driver is present.
#[must_use]
pub fn resolve_sandbox_launch(
    program: &Path,
    args: &[OsString],
    policy: &ModSandboxPolicy,
) -> SandboxLaunch {
    if sandbox_disabled_by_env() {
        return env_cleared_launch(program, args);
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(launch) = resolve_seatbelt_launch(program, args, policy) {
            return launch;
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(launch) = resolve_bwrap_launch(program, args, policy) {
            return launch;
        }
    }

    env_cleared_launch(program, args)
}

fn env_cleared_launch(program: &Path, args: &[OsString]) -> SandboxLaunch {
    SandboxLaunch {
        backend: SandboxBackend::EnvClearedOnly,
        program: program.to_path_buf(),
        args: args.to_vec(),
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

fn canonical_or_raw(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn unique_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut set = BTreeSet::new();
    for path in paths {
        if !path.as_os_str().is_empty() {
            set.insert(canonical_or_raw(&path));
        }
    }
    set.into_iter().collect()
}

#[cfg(any(target_os = "macos", test))]
fn escape_sbpl_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' | '\0' => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(any(target_os = "macos", test))]
fn escape_sbpl_regex(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for ch in value.chars() {
        if matches!(
            ch,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|'
        ) {
            out.push('\\');
        }
        if !matches!(ch, '\n' | '\r' | '\0') {
            out.push(ch);
        }
    }
    out
}

#[cfg(any(target_os = "macos", test))]
fn build_seatbelt_profile(program: &Path, policy: &ModSandboxPolicy) -> String {
    let mut read_roots = vec![
        PathBuf::from("/System"),
        PathBuf::from("/usr"),
        PathBuf::from("/bin"),
        PathBuf::from("/sbin"),
        PathBuf::from("/Library"),
        PathBuf::from("/dev"),
        PathBuf::from("/private/var/db"),
        PathBuf::from("/private/etc"),
        PathBuf::from("/etc"),
    ];
    if let Some(parent) = program.parent()
        && !parent.as_os_str().is_empty()
    {
        read_roots.push(parent.to_path_buf());
    }
    read_roots.extend(policy.read_only_paths.iter().cloned());
    read_roots.extend(policy.read_write_paths.iter().cloned());
    if let Some(projects_root) = &policy.scratch_projects_root {
        read_roots.push(projects_root.clone());
    }
    let read_paths = unique_paths(read_roots);
    let write_paths = unique_paths(policy.read_write_paths.iter().cloned());

    let mut sbpl = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec)\n\
         (allow process-fork)\n\
         (allow sysctl-read)\n\
         (allow mach-lookup)\n\
         (allow signal (target self))\n\
         (allow file-read-metadata)\n",
    );

    if policy.allow_host_paths {
        sbpl.push_str("(allow file-read*)\n");
    } else {
        sbpl.push_str(
            "(allow file-read* (literal \"/\") (literal \"/private\") (literal \"/private/tmp\") (literal \"/tmp\")",
        );
        for path in &read_paths {
            let escaped = escape_sbpl_string(&path.to_string_lossy());
            sbpl.push_str(&format!(" (subpath \"{escaped}\")"));
        }
        let escaped_program = escape_sbpl_string(&canonical_or_raw(program).to_string_lossy());
        sbpl.push_str(&format!(" (literal \"{escaped_program}\"))\n"));
    }

    sbpl.push_str("(allow file-write* (literal \"/dev/null\") (literal \"/dev/dtracehelper\")");
    for path in &write_paths {
        let escaped = escape_sbpl_string(&path.to_string_lossy());
        sbpl.push_str(&format!(" (subpath \"{escaped}\")"));
    }
    if let Some(projects_root) = &policy.scratch_projects_root {
        let canonical = canonical_or_raw(projects_root);
        let regex_root = escape_sbpl_regex(&canonical.to_string_lossy());
        let escaped_regex = escape_sbpl_string(&format!("^{regex_root}/[^/]+/scratch(/|$)"));
        sbpl.push_str(&format!(" (regex #\"{escaped_regex}\")"));
    }
    sbpl.push_str(")\n");

    if policy.allow_network {
        sbpl.push_str("(allow network*)\n");
    } else {
        sbpl.push_str("(deny network*)\n");
    }

    sbpl
}

#[cfg(target_os = "macos")]
fn resolve_seatbelt_launch(
    program: &Path,
    args: &[OsString],
    policy: &ModSandboxPolicy,
) -> Option<SandboxLaunch> {
    let seatbelt = Path::new("/usr/bin/sandbox-exec");
    if !seatbelt.is_file() {
        return None;
    }
    let profile = build_seatbelt_profile(program, policy);
    let mut wrapped_args = Vec::with_capacity(args.len() + 3);
    wrapped_args.push(OsString::from("-p"));
    wrapped_args.push(OsString::from(profile));
    wrapped_args.push(program.as_os_str().to_os_string());
    wrapped_args.extend(args.iter().cloned());

    Some(SandboxLaunch {
        backend: SandboxBackend::Seatbelt,
        program: seatbelt.to_path_buf(),
        args: wrapped_args,
    })
}

#[cfg(target_os = "linux")]
fn find_bwrap_binary() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("MERA_BWRAP_BIN") {
        let candidate = PathBuf::from(explicit);
        if candidate.is_file() {
            return Some(candidate);
        }
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
fn append_bwrap_base_args(wrapped_args: &mut Vec<OsString>) {
    wrapped_args.extend([
        "--die-with-parent".into(),
        "--clearenv".into(),
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        "--tmpfs".into(),
        "/tmp".into(),
    ]);
    for sys_dir in ["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc"] {
        if Path::new(sys_dir).exists() {
            wrapped_args.push("--ro-bind".into());
            wrapped_args.push(sys_dir.into());
            wrapped_args.push(sys_dir.into());
        }
    }
}

#[cfg(target_os = "linux")]
fn probe_bwrap_binary(bwrap: &Path) -> bool {
    let mut args = Vec::new();
    append_bwrap_base_args(&mut args);
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
fn usable_bwrap_binary() -> Option<PathBuf> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static PROBE_CACHE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();

    let bwrap = find_bwrap_binary()?;
    let key = canonical_or_raw(&bwrap);
    let cache = PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some(&usable) = guard.get(&key)
    {
        return usable.then_some(bwrap);
    }

    let usable = probe_bwrap_binary(&bwrap);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, usable);
    }
    usable.then_some(bwrap)
}

#[cfg(target_os = "linux")]
fn resolve_bwrap_launch(
    program: &Path,
    args: &[OsString],
    policy: &ModSandboxPolicy,
) -> Option<SandboxLaunch> {
    let bwrap = usable_bwrap_binary()?;
    let mut wrapped_args: Vec<OsString> = Vec::new();
    append_bwrap_base_args(&mut wrapped_args);
    if policy.allow_host_paths {
        for host_root in ["/home", "/mnt", "/media"] {
            if Path::new(host_root).exists() {
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
    for path in unique_paths(ro_paths) {
        if path.exists() {
            wrapped_args.push("--ro-bind".into());
            wrapped_args.push(path.as_os_str().to_os_string());
            wrapped_args.push(path.as_os_str().to_os_string());
        }
    }
    let mut rw_paths = policy.read_write_paths.clone();
    if let Some(projects_root) = &policy.scratch_projects_root {
        rw_paths.push(projects_root.clone());
    }
    for path in unique_paths(rw_paths) {
        if path.exists() {
            wrapped_args.push("--bind".into());
            wrapped_args.push(path.as_os_str().to_os_string());
            wrapped_args.push(path.as_os_str().to_os_string());
        }
    }
    if !policy.allow_network {
        wrapped_args.push("--unshare-net".into());
    }

    wrapped_args.push("--".into());
    wrapped_args.push(program.as_os_str().to_os_string());
    wrapped_args.extend(args.iter().cloned());

    Some(SandboxLaunch {
        backend: SandboxBackend::Bubblewrap,
        program: bwrap,
        args: wrapped_args,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seatbelt_profile_denies_network_unless_permitted_and_restricts_writes() {
        let mut policy = ModSandboxPolicy::restricted(
            "demo",
            PathBuf::from("/opt/mods/demo"),
            PathBuf::from("/tmp/mod-data/demo"),
        );
        let locked = build_seatbelt_profile(Path::new("/opt/mods/demo/bin/demo"), &policy);
        assert!(locked.contains("(deny default)"));
        assert!(locked.contains("(deny network*)"));
        assert!(locked.contains("(allow file-write* (literal \"/dev/null\")"));
        assert!(locked.contains("/opt/mods/demo"));
        assert!(locked.contains("/tmp/mod-data/demo"));

        policy.allow_network = true;
        let net = build_seatbelt_profile(Path::new("/opt/mods/demo/bin/demo"), &policy);
        assert!(net.contains("(allow network*)"));
        assert!(!net.contains("(deny network*)"));
    }

    #[test]
    fn sbpl_string_escaping_strips_quotes_and_control_newlines() {
        let escaped = escape_sbpl_string("/tmp/a\"b\\c\nd");
        assert_eq!(escaped, "/tmp/a\\\"b\\\\cd");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bwrap_probe_rejects_unusable_driver_binary() {
        let false_bin = ["/bin/false", "/usr/bin/false"]
            .into_iter()
            .find(|path| Path::new(path).is_file())
            .expect("false binary exists on linux");
        assert!(!probe_bwrap_binary(Path::new(false_bin)));
    }
}
