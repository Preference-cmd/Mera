use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::backend::{
    SandboxBackend, canonical_with_tail, expand_logical_and_canonical, normalize_lexical,
};
use crate::error::SandboxError;
use crate::policy::{NetworkPolicy, SandboxLaunch, SandboxPolicy, SandboxProfileKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeatbeltSpec {
    pub profile: String,
    pub params: Vec<(String, String)>,
}

fn validate_param_value(path: &Path) -> Result<String, SandboxError> {
    let raw = path.to_string_lossy();
    if raw.contains('\0') || raw.contains('\n') || raw.contains('\r') {
        return Err(SandboxError::ProfileSetupFailed {
            reason: format!(
                "seatbelt path contains disallowed control characters: `{}`",
                path.display()
            ),
        });
    }
    Ok(raw.into_owned())
}

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

fn push_param(params: &mut Vec<(String, String)>, key: String, value: String) -> String {
    params.push((key.clone(), value));
    key
}

fn paths_overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn collect_protected_ancestors(
    write_roots: &[PathBuf],
    protected_paths: &[PathBuf],
) -> Vec<PathBuf> {
    let mut ancestors = BTreeSet::new();
    for protected in protected_paths {
        for root in write_roots {
            if !protected.starts_with(root) {
                continue;
            }
            let mut cur = if protected == root {
                Some(root.as_path())
            } else {
                protected.parent()
            };
            while let Some(dir) = cur {
                if !dir.starts_with(root) {
                    break;
                }
                ancestors.insert(normalize_lexical(dir));
                ancestors.insert(canonical_with_tail(dir));
                if dir == root {
                    break;
                }
                cur = dir.parent();
            }
        }
    }
    ancestors.into_iter().collect()
}

pub(crate) fn build_seatbelt_spec(
    program: &Path,
    policy: &SandboxPolicy,
) -> Result<SeatbeltSpec, SandboxError> {
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
    read_roots.extend(policy.read_only_carveouts.iter().cloned());
    if let Some(projects_root) = &policy.scratch_projects_root {
        read_roots.push(projects_root.clone());
    }

    let read_paths = expand_logical_and_canonical(read_roots);
    let write_paths = expand_logical_and_canonical(policy.read_write_paths.iter().cloned());
    let ro_carveouts = expand_logical_and_canonical(policy.read_only_carveouts.iter().cloned());
    let deny_paths = expand_logical_and_canonical(policy.deny_paths.iter().cloned());

    let mut all_write_exclusions = Vec::with_capacity(ro_carveouts.len() + deny_paths.len());
    all_write_exclusions.extend(ro_carveouts.iter().cloned());
    all_write_exclusions.extend(deny_paths.iter().cloned());

    let mut ancestor_roots = write_paths.clone();
    if let Some(projects_root) = &policy.scratch_projects_root {
        ancestor_roots.push(normalize_lexical(projects_root));
        ancestor_roots.push(canonical_with_tail(projects_root));
    }
    let protected_ancestors = collect_protected_ancestors(&ancestor_roots, &all_write_exclusions);

    let mut params = Vec::new();
    let program_param = push_param(
        &mut params,
        "PROGRAM_PATH".to_owned(),
        validate_param_value(&canonical_with_tail(program))?,
    );

    let mut ro_carveout_params = Vec::with_capacity(ro_carveouts.len());
    for (idx, path) in ro_carveouts.iter().enumerate() {
        let key = push_param(
            &mut params,
            format!("RO_CARVEOUT_{idx}"),
            validate_param_value(path)?,
        );
        ro_carveout_params.push((path.clone(), key));
    }

    let mut deny_params = Vec::with_capacity(deny_paths.len());
    for (idx, path) in deny_paths.iter().enumerate() {
        let key = push_param(
            &mut params,
            format!("DENY_PATH_{idx}"),
            validate_param_value(path)?,
        );
        deny_params.push((path.clone(), key));
    }

    let mut sbpl = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec)\n\
         (allow process-fork)\n\
         (allow sysctl-read)\n\
         (allow signal (target self))\n\
         (allow file-read-metadata)\n",
    );

    match policy.profile {
        SandboxProfileKind::ScriptStep => {
            sbpl.push_str(
                "(allow ipc-posix-sem)\n\
                 (allow ipc-posix-shm-read* ipc-posix-shm-write-data (ipc-posix-name-regex #\"^/__KMP_REGISTERED_LIB_[0-9]+$\"))\n\
                 (allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\") (global-name \"com.apple.system.logger\") (global-name \"com.apple.system.notification_center\") (global-name \"com.apple.trustd.agent\") (global-name \"com.apple.SecurityServer\"))\n\
                 (deny mach-lookup (xpc-service-name-prefix \"\"))\n",
            );
        }
        SandboxProfileKind::Mod | SandboxProfileKind::AgentWorkspace => {
            sbpl.push_str("(allow ipc-posix-sem)\n(allow mach-lookup)\n");
        }
    }

    match policy.network {
        NetworkPolicy::Disabled => sbpl.push_str("(deny network*)\n"),
        NetworkPolicy::LoopbackOnly => sbpl.push_str(
            "(allow network-outbound (remote ip \"localhost:*\"))\n\
             (allow network-inbound (local ip \"localhost:*\"))\n\
             (allow network-bind (local ip \"localhost:*\"))\n",
        ),
        NetworkPolicy::FullAccess => sbpl.push_str("(allow network*)\n"),
    }

    if policy.allow_host_paths {
        sbpl.push_str("(allow file-read*)\n");
    } else {
        sbpl.push_str(
            "(allow file-read* (literal \"/\") (literal \"/private\") (literal \"/private/tmp\") (literal \"/tmp\")",
        );
        for (idx, path) in read_paths.iter().enumerate() {
            let key = push_param(
                &mut params,
                format!("READ_ROOT_{idx}"),
                validate_param_value(path)?,
            );
            sbpl.push_str(&format!(" (subpath (param \"{key}\"))"));
        }
        sbpl.push_str(&format!(" (literal (param \"{program_param}\")))\n"));
    }

    sbpl.push_str("(allow file-write* (literal \"/dev/null\") (literal \"/dev/dtracehelper\")");
    for (idx, path) in write_paths.iter().enumerate() {
        let key = push_param(
            &mut params,
            format!("WRITABLE_ROOT_{idx}"),
            validate_param_value(path)?,
        );
        let overlapping: Vec<&str> = ro_carveout_params
            .iter()
            .chain(deny_params.iter())
            .filter(|(excl, _)| paths_overlap(path, excl))
            .map(|(_, k)| k.as_str())
            .collect();
        if overlapping.is_empty() {
            sbpl.push_str(&format!(" (subpath (param \"{key}\"))"));
        } else {
            sbpl.push_str(&format!(" (require-all (subpath (param \"{key}\"))"));
            for excl_key in overlapping {
                sbpl.push_str(&format!(
                    " (require-not (literal (param \"{excl_key}\"))) (require-not (subpath (param \"{excl_key}\")))"
                ));
            }
            sbpl.push(')');
        }
    }
    if let Some(projects_root) = &policy.scratch_projects_root {
        let canonical = canonical_with_tail(projects_root);
        let regex_root = escape_sbpl_regex(&canonical.to_string_lossy());
        let escaped_regex = escape_sbpl_string(&format!("^{regex_root}/[^/]+/scratch(/|$)"));
        sbpl.push_str(&format!(" (regex #\"{escaped_regex}\")"));
    }
    sbpl.push_str(")\n");

    // SBPL evaluates matching rules last-match-wins, so carve-outs, deny paths,
    // ancestor directory unlink locks, and dangerous fcntl restrictions are
    // emitted at the end of the profile.
    if !ro_carveout_params.is_empty() {
        sbpl.push_str("(deny file-write*");
        for (_, key) in &ro_carveout_params {
            sbpl.push_str(&format!(
                " (literal (param \"{key}\")) (subpath (param \"{key}\"))"
            ));
        }
        sbpl.push_str(")\n");
    }

    if !deny_params.is_empty() {
        sbpl.push_str("(deny file-read* file-read-metadata file-write*");
        for (_, key) in &deny_params {
            sbpl.push_str(&format!(
                " (literal (param \"{key}\")) (subpath (param \"{key}\"))"
            ));
        }
        sbpl.push_str(")\n");
    }

    for (idx, anc) in protected_ancestors.iter().enumerate() {
        let key = push_param(
            &mut params,
            format!("PROTECTED_ANCESTOR_{idx}"),
            validate_param_value(anc)?,
        );
        sbpl.push_str(&format!(
            "(deny file-write-unlink (require-all (vnode-type DIRECTORY) (literal (param \"{key}\"))))\n"
        ));
    }

    // Block F_MAKECOMPRESSED (80) and F_TRANSFEREXTENTS (110), which can mutate
    // files through read-only file descriptors.
    sbpl.push_str("(deny system-fcntl (fcntl-command 80 110))\n");

    Ok(SeatbeltSpec {
        profile: sbpl,
        params,
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn resolve_seatbelt_launch(
    program: &Path,
    args: &[OsString],
    policy: &SandboxPolicy,
) -> Result<Option<SandboxLaunch>, SandboxError> {
    let seatbelt = Path::new("/usr/bin/sandbox-exec");
    if !seatbelt.is_file() {
        return Ok(None);
    }
    let spec = build_seatbelt_spec(program, policy)?;
    let mut wrapped_args = Vec::with_capacity(spec.params.len() * 2 + args.len() + 3);
    for (key, value) in &spec.params {
        wrapped_args.push(OsString::from("-D"));
        wrapped_args.push(OsString::from(format!("{key}={value}")));
    }
    wrapped_args.push(OsString::from("-p"));
    wrapped_args.push(OsString::from(spec.profile));
    wrapped_args.push(program.as_os_str().to_os_string());
    wrapped_args.extend(args.iter().cloned());

    Ok(Some(SandboxLaunch {
        backend: SandboxBackend::Seatbelt,
        program: seatbelt.to_path_buf(),
        args: wrapped_args,
        env: policy.env_allowlist.clone(),
        cleanup_paths: Vec::new(),
    }))
}
