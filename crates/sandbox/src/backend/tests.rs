use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::backend::bwrap::build_bwrap_args;
use crate::backend::landlock_seccomp::{
    SeccompRuleSpec, SeccompSyscallKind, check_landlock_eligibility, describe_seccomp_rules,
    plan_landlock_paths,
};
use crate::backend::seatbelt::build_seatbelt_spec;
use crate::backend::windows_mxc::{
    WindowsMxcNetworkMode, build_windows_mxc_plan, evaluate_probe_for_policy,
    format_windows_command_line, missing_deny_parents, quote_windows_arg,
};
use crate::backend::{
    SandboxBackend, detect_sandbox_backend_for, resolve_sandbox_launch,
    resolve_sandbox_launch_with_override,
};
use crate::error::SandboxError;
use crate::policy::{NetworkPolicy, SandboxPolicy, SandboxProfileKind};

#[test]
fn sandbox_backend_serde_and_labels_stay_compatible() {
    let cases = [
        (SandboxBackend::Seatbelt, "seatbelt", true),
        (SandboxBackend::Bubblewrap, "bubblewrap", true),
        (SandboxBackend::LandlockSeccomp, "landlock_seccomp", true),
        (SandboxBackend::WindowsMxc, "windows_mxc", true),
        (SandboxBackend::EnvClearedOnly, "env_cleared_only", false),
    ];
    for (variant, label, enforced) in cases {
        assert_eq!(variant.as_str(), label);
        assert_eq!(variant.is_os_enforced(), enforced);
        let serialized = serde_json::to_string(&variant).expect("serialize backend");
        assert_eq!(serialized, format!("\"{label}\""));
        let roundtrip: SandboxBackend =
            serde_json::from_str(&serialized).expect("deserialize backend");
        assert_eq!(roundtrip, variant);
    }
}

#[test]
fn seatbelt_spec_uses_macro_params_and_hardening_rules() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ws = temp.path().join("ws");
    let sub = ws.join("sub");
    let git = sub.join(".git");
    let secret = ws.join("secret");
    std::fs::create_dir_all(&git).unwrap();
    std::fs::create_dir_all(&secret).unwrap();

    let policy = SandboxPolicy::for_agent_workspace(
        vec![ws.clone()],
        vec![git.clone()],
        vec![secret.clone()],
    );
    let spec = build_seatbelt_spec(Path::new("/usr/bin/python3"), &policy).expect("spec");

    assert!(spec.profile.contains("(deny default)"));
    assert!(
        spec.profile
            .contains("(deny system-fcntl (fcntl-command 80 110))")
    );
    assert!(spec.profile.contains("(deny file-write*"));
    assert!(
        spec.profile
            .contains("(deny file-read* file-read-metadata file-write*")
    );
    assert!(spec.profile.contains(
        "(deny file-write-unlink (require-all (vnode-type DIRECTORY) (literal (param \"PROTECTED_ANCESTOR_0\"))))"
    ));

    // Raw user paths must be bound via -D macro params, not pasted verbatim into SBPL.
    assert!(!spec.profile.contains(&git.to_string_lossy().to_string()));
    assert!(!spec.profile.contains(&secret.to_string_lossy().to_string()));
    assert!(
        spec.params
            .iter()
            .any(|(k, _)| k.starts_with("RO_CARVEOUT_"))
    );
    assert!(spec.params.iter().any(|(k, _)| k.starts_with("DENY_PATH_")));
    assert!(
        spec.params
            .iter()
            .any(|(k, _)| k.starts_with("PROTECTED_ANCESTOR_"))
    );

    let fcntl_pos = spec
        .profile
        .rfind("(deny system-fcntl (fcntl-command 80 110))")
        .unwrap();
    let deny_write_pos = spec.profile.rfind("(deny file-write*").unwrap();
    let allow_write_pos = spec.profile.find("(allow file-write*").unwrap();
    assert!(allow_write_pos < deny_write_pos);
    assert!(deny_write_pos < fcntl_pos);
}

#[test]
fn seatbelt_script_step_narrows_mach_lookup_and_blocks_network() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scratch = temp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    let script_policy = SandboxPolicy::for_script_step(vec![], scratch.clone());
    let spec = build_seatbelt_spec(Path::new("/usr/bin/python3"), &script_policy).expect("spec");
    assert!(spec.profile.contains("(deny network*)"));
    assert!(!spec.profile.contains("(allow mach-lookup)\n"));
    assert!(
        spec.profile
            .contains("(deny mach-lookup (xpc-service-name-prefix \"\"))")
    );
    assert!(
        spec.profile
            .contains("com.apple.system.opendirectoryd.libinfo")
    );
    assert!(spec.profile.contains("__KMP_REGISTERED_LIB_"));

    let loopback_policy = SandboxPolicy::mod_restricted(
        PathBuf::from("/opt/mods/demo"),
        PathBuf::from("/tmp/mod-data/demo"),
    )
    .with_network(NetworkPolicy::LoopbackOnly);
    let loopback_spec = build_seatbelt_spec(Path::new("/opt/mods/demo/bin/demo"), &loopback_policy)
        .expect("loopback spec");
    assert!(loopback_spec.profile.contains("(allow mach-lookup)\n"));
    assert!(
        loopback_spec
            .profile
            .contains("(allow network-outbound (remote ip \"localhost:*\"))")
    );
}

#[test]
fn seatbelt_rejects_control_characters_in_paths() {
    let policy = SandboxPolicy::for_script_step(vec![], PathBuf::from("/tmp/bad\npath"));
    let err = build_seatbelt_spec(Path::new("/usr/bin/true"), &policy).unwrap_err();
    assert!(matches!(err, SandboxError::ProfileSetupFailed { .. }));
}

#[test]
fn bwrap_args_include_hardening_and_shallow_to_deep_carveouts() {
    let policy = SandboxPolicy::for_agent_workspace(
        vec![PathBuf::from("/workspace")],
        vec![
            PathBuf::from("/workspace/packages/sub/.git"),
            PathBuf::from("/workspace/.git"),
        ],
        vec![PathBuf::from("/workspace/.mera-data")],
    )
    .with_network(NetworkPolicy::Disabled)
    .with_env("FOO", "BAR");

    let args = build_bwrap_args(
        Path::new("/usr/bin/agent"),
        &[OsString::from("--help")],
        &policy,
        false,
        true,
    )
    .expect("bwrap args");
    let strs: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    assert!(strs.contains(&"--new-session".to_owned()));
    assert!(strs.contains(&"--die-with-parent".to_owned()));
    assert!(strs.contains(&"--unshare-pid".to_owned()));
    assert!(strs.contains(&"--unshare-net".to_owned()));
    let cap_idx = strs.iter().position(|s| s == "--cap-drop").unwrap();
    assert_eq!(strs.get(cap_idx + 1).map(String::as_str), Some("ALL"));

    let bind_ws = strs
        .windows(3)
        .position(|w| w == ["--bind", "/workspace", "/workspace"])
        .expect("writable workspace bind");
    let ro_git = strs
        .windows(3)
        .position(|w| w == ["--ro-bind", "/workspace/.git", "/workspace/.git"])
        .expect("shallow git carveout");
    let ro_nested_git = strs
        .windows(3)
        .position(|w| {
            w == [
                "--ro-bind",
                "/workspace/packages/sub/.git",
                "/workspace/packages/sub/.git",
            ]
        })
        .expect("deep git carveout");
    let deny_data = strs
        .windows(6)
        .position(|w| {
            w == [
                "--perms",
                "000",
                "--tmpfs",
                "/workspace/.mera-data",
                "--remount-ro",
                "/workspace/.mera-data",
            ]
        })
        .expect("deny tmpfs mask");

    assert!(bind_ws < ro_git);
    assert!(ro_git < ro_nested_git);
    assert!(ro_nested_git < deny_data);

    let setenv_idx = strs.iter().position(|s| s == "--setenv").unwrap();
    assert_eq!(strs.get(setenv_idx + 1).map(String::as_str), Some("FOO"));
    assert_eq!(strs.get(setenv_idx + 2).map(String::as_str), Some("BAR"));
}

#[test]
fn bwrap_args_omit_perms_flag_when_older_bwrap_lacks_support() {
    let policy = SandboxPolicy::for_agent_workspace(
        vec![PathBuf::from("/workspace")],
        vec![],
        vec![PathBuf::from("/workspace/.mera-data")],
    );

    let args = build_bwrap_args(Path::new("/usr/bin/agent"), &[], &policy, false, false)
        .expect("bwrap args without --perms");
    let strs: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    assert!(!strs.contains(&"--perms".to_owned()));
    assert!(strs.windows(4).any(|w| {
        w == [
            "--tmpfs",
            "/workspace/.mera-data",
            "--remount-ro",
            "/workspace/.mera-data",
        ]
    }));
}

#[test]
fn resolve_sandbox_launch_rejects_empty_program() {
    let policy = SandboxPolicy::for_script_step(vec![], PathBuf::from("/tmp/scratch"));
    let err = resolve_sandbox_launch(Path::new(""), &[], &policy).unwrap_err();
    assert!(matches!(err, SandboxError::InvalidPolicy { .. }));
}

#[test]
fn fail_closed_refuses_downgrade_and_mod_env_toggle_does_not_affect_script_step() {
    let strict = SandboxPolicy::for_script_step(vec![], PathBuf::from("/tmp/scratch"));
    let err = resolve_sandbox_launch_with_override(
        Path::new("/usr/bin/python3"),
        &[],
        &strict,
        false,
        true,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        SandboxError::BackendUnavailable {
            profile: SandboxProfileKind::ScriptStep,
            ..
        }
    ));

    // Even when `MERA_MOD_SANDBOX` disables mod sandboxing, ScriptStep and
    // AgentWorkspace ignore it and continue resolving the OS sandbox normally.
    if crate::detect_sandbox_backend().is_os_enforced() {
        let launch = resolve_sandbox_launch_with_override(
            Path::new("/usr/bin/python3"),
            &[],
            &strict,
            true,
            false,
        )
        .expect("ScriptStep ignores mod-only env disable");
        assert!(launch.backend.is_os_enforced());
    }

    let permissive =
        SandboxPolicy::mod_restricted(PathBuf::from("/opt/mod"), PathBuf::from("/tmp/data"));
    let launch = resolve_sandbox_launch_with_override(
        Path::new("/opt/mod/bin"),
        &[OsString::from("--flag")],
        &permissive,
        true,
        false,
    )
    .expect("non-fail-closed mod falls back to env_cleared_only");
    assert_eq!(launch.backend, SandboxBackend::EnvClearedOnly);
    assert!(!launch.backend.is_os_enforced());
    assert_eq!(launch.program, PathBuf::from("/opt/mod/bin"));
}

#[test]
fn landlock_eligibility_enforces_script_step_boundary_and_rejects_carveouts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ws = temp.path().join("ws");
    let scratch = temp.path().join("scratch");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&scratch).unwrap();

    // Clean ScriptStep is eligible for Landlock + seccomp fallback.
    let script_policy = SandboxPolicy::for_script_step(vec![ws.clone()], scratch.clone());
    assert!(check_landlock_eligibility(&script_policy).is_ok());

    // AgentWorkspace is never eligible, even if carve-out lists happen to be empty.
    let agent_empty = SandboxPolicy::for_agent_workspace(vec![ws.clone()], vec![], vec![]);
    assert!(matches!(
        check_landlock_eligibility(&agent_empty),
        Err(SandboxError::BackendUnavailable {
            profile: SandboxProfileKind::AgentWorkspace,
            ..
        })
    ));

    // Any read_only_carveouts or deny_paths disqualifies Landlock.
    let with_carveout = SandboxPolicy::for_script_step(vec![], scratch.clone())
        .with_read_only_carveouts(vec![ws.join(".git")]);
    assert!(matches!(
        check_landlock_eligibility(&with_carveout),
        Err(SandboxError::BackendUnavailable { .. })
    ));

    let with_deny = SandboxPolicy::for_script_step(vec![], scratch.clone())
        .with_deny_paths(vec![ws.join(".secret")]);
    assert!(matches!(
        check_landlock_eligibility(&with_deny),
        Err(SandboxError::BackendUnavailable { .. })
    ));

    // LoopbackOnly is not supported by Landlock + seccomp fallback.
    let loopback =
        SandboxPolicy::for_script_step(vec![], scratch).with_network(NetworkPolicy::LoopbackOnly);
    assert!(matches!(
        check_landlock_eligibility(&loopback),
        Err(SandboxError::BackendUnavailable { .. })
    ));
}

#[test]
fn landlock_path_plan_and_seccomp_rules_cover_required_boundaries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let py_dir = temp.path().join("python/bin");
    let input_dir = temp.path().join("inputs");
    let scratch = temp.path().join("scratch");
    std::fs::create_dir_all(&py_dir).unwrap();
    std::fs::create_dir_all(&input_dir).unwrap();
    std::fs::create_dir_all(&scratch).unwrap();

    let py_bin = py_dir.join("python3");
    let policy = SandboxPolicy::for_script_step(vec![input_dir.clone()], scratch.clone());
    let plan = plan_landlock_paths(&py_bin, &policy);

    assert!(plan.read_only_paths.iter().any(|p| p == &py_dir));
    assert!(plan.read_only_paths.iter().any(|p| p == &input_dir));
    assert!(plan.read_only_paths.iter().any(|p| p == Path::new("/usr")));
    assert!(plan.read_write_paths.iter().any(|p| p == &scratch));
    assert!(
        plan.read_write_paths
            .iter()
            .any(|p| p == Path::new("/dev/null"))
    );

    let offline_rules = describe_seccomp_rules(NetworkPolicy::Disabled);
    assert!(offline_rules.contains(&SeccompRuleSpec::AlwaysDeny(
        SeccompSyscallKind::IoUringSetup
    )));
    assert!(offline_rules.contains(&SeccompRuleSpec::AlwaysDeny(SeccompSyscallKind::Ptrace)));
    assert!(offline_rules.contains(&SeccompRuleSpec::AlwaysDeny(SeccompSyscallKind::Bpf)));
    assert!(offline_rules.contains(&SeccompRuleSpec::DenyUnlessAfUnix(
        SeccompSyscallKind::Socket
    )));
    assert!(offline_rules.contains(&SeccompRuleSpec::DenyUnlessAfUnix(
        SeccompSyscallKind::Socketpair
    )));

    let online_rules = describe_seccomp_rules(NetworkPolicy::FullAccess);
    assert!(!online_rules.contains(&SeccompRuleSpec::DenyUnlessAfUnix(
        SeccompSyscallKind::Socket
    )));
}

#[test]
fn windows_command_line_quoting_handles_spaces_quotes_and_backslashes() {
    use std::ffi::OsStr;

    assert_eq!(quote_windows_arg(OsStr::new("plain")).unwrap(), "plain");
    assert_eq!(quote_windows_arg(OsStr::new("")).unwrap(), "\"\"");
    assert_eq!(
        quote_windows_arg(OsStr::new(r"C:\Program Files\Python\python.exe")).unwrap(),
        r#""C:\Program Files\Python\python.exe""#
    );
    assert_eq!(
        quote_windows_arg(OsStr::new(r#"a"b\c\"#)).unwrap(),
        r#""a\"b\c\\""#
    );

    let cmd = format_windows_command_line(
        Path::new(r"C:\Program Files\app\tool.exe"),
        &[
            OsString::from("--msg"),
            OsString::from("hello world"),
            OsString::from(""),
        ],
    )
    .unwrap();
    assert_eq!(
        cmd,
        r#""C:\Program Files\app\tool.exe" --msg "hello world" """#
    );
}

#[test]
fn windows_mxc_plan_maps_carveouts_denies_network_and_limits() {
    let mut policy = SandboxPolicy::for_agent_workspace(
        vec![PathBuf::from(r"C:\ws")],
        vec![PathBuf::from(r"C:\ws\.git")],
        vec![PathBuf::from(r"C:\ws\.mera-data")],
    )
    .with_network(NetworkPolicy::Disabled)
    .with_env("MERA_KEY", "VAL");
    policy.read_only_paths = vec![PathBuf::from(r"C:\tools")];
    policy.limits.wall_timeout = Some(std::time::Duration::from_secs(30));

    let plan = build_windows_mxc_plan(
        Path::new(r"C:\tools\agent.exe"),
        &[OsString::from("--flag")],
        Some(Path::new(r"C:\ws")),
        &policy,
    )
    .expect("build windows mxc plan");

    assert!(plan.use_base_container_only);
    assert!(plan.inherit_default_env);
    assert_eq!(plan.command_line, r"C:\tools\agent.exe --flag");
    assert_eq!(plan.working_directory.as_deref(), Some(r"C:\ws"));
    assert_eq!(plan.readwrite_paths, vec![r"C:\ws"]);
    assert!(plan.readonly_paths.contains(&r"C:\ws\.git".to_owned()));
    assert!(plan.readonly_paths.contains(&r"C:\tools".to_owned()));
    assert_eq!(plan.denied_paths, vec![r"C:\ws\.mera-data"]);
    assert_eq!(plan.network_mode, WindowsMxcNetworkMode::Disabled);
    assert_eq!(plan.timeout_ms, Some(30_000));
    assert_eq!(plan.env, vec![("MERA_KEY".to_owned(), "VAL".to_owned())]);

    let loopback_plan = build_windows_mxc_plan(
        Path::new("/opt/tools/agent.exe"),
        &[],
        None,
        &policy.clone().with_network(NetworkPolicy::LoopbackOnly),
    )
    .unwrap();
    assert!(
        loopback_plan
            .readonly_paths
            .contains(&"/opt/tools".to_owned())
    );
    assert_eq!(
        loopback_plan.network_mode,
        WindowsMxcNetworkMode::LoopbackOnly
    );
}

#[test]
fn windows_mxc_probe_evaluator_rejects_appcontainer_and_legacy_windows() {
    let script_policy = SandboxPolicy::for_script_step(vec![], PathBuf::from(r"C:\scratch"));
    let agent_policy = SandboxPolicy::for_agent_workspace(
        vec![PathBuf::from(r"C:\ws")],
        vec![PathBuf::from(r"C:\ws\.git")],
        vec![PathBuf::from(r"C:\ws\.mera-data")],
    );

    // Reject AppContainer tier outright.
    let err = evaluate_probe_for_policy(Some("appcontainer"), true, true, None, &script_policy)
        .unwrap_err();
    assert!(matches!(err, SandboxError::BackendUnavailable { .. }));
    assert!(err.to_string().contains("AppContainer"));
    assert!(err.to_string().contains("26100.9278"));

    // Reject legacy Windows without PSEC API.
    let err =
        evaluate_probe_for_policy(None, false, false, Some("PSEC unavailable"), &script_policy)
            .unwrap_err();
    assert!(err.to_string().contains("26100.9278"));

    // Reject missing deniedPaths support when policy requires deny_paths.
    let err = evaluate_probe_for_policy(Some("base-container"), true, false, None, &agent_policy)
        .unwrap_err();
    assert!(err.to_string().contains("26100.9278"));

    // Accept full BaseContainer support.
    assert!(
        evaluate_probe_for_policy(Some("base-container"), true, true, None, &agent_policy).is_ok()
    );
}

#[test]
fn windows_missing_deny_parents_detects_absent_parent_directories() {
    let temp = tempfile::tempdir().expect("tempdir");
    let existing_parent = temp.path().join("present");
    std::fs::create_dir_all(&existing_parent).unwrap();

    let ok_target = existing_parent.join("deny_me");
    let missing_target = temp.path().join("missing_parent").join("deny_me");

    let missing = missing_deny_parents(&[
        ok_target.to_string_lossy().into_owned(),
        missing_target.to_string_lossy().into_owned(),
    ]);
    assert_eq!(missing, vec![temp.path().join("missing_parent")]);
}

#[test]
fn windows_prepare_denied_parents_tracks_only_new_dirs_and_cleans_up_on_drop() {
    let temp = tempfile::tempdir().expect("tempdir");
    let rw_root = temp.path().join("workspace");
    let pre_existing = rw_root.join("existing");
    std::fs::create_dir_all(&pre_existing).unwrap();

    let denied_inside_new = pre_existing.join("nested").join("deep").join(".env");
    let denied_outside_rw = temp.path().join("outside").join("secret").join(".env");

    let rw_str = rw_root.to_string_lossy().into_owned();
    let guard = crate::backend::windows_mxc::prepare_denied_parents_if_needed(
        &[
            denied_inside_new.to_string_lossy().into_owned(),
            denied_outside_rw.to_string_lossy().into_owned(),
        ],
        &[rw_str],
    )
    .expect("prepare parents");

    assert_eq!(
        guard.0,
        vec![
            pre_existing.join("nested"),
            pre_existing.join("nested/deep")
        ]
    );
    assert!(pre_existing.join("nested/deep").exists());
    assert!(!temp.path().join("outside").exists());

    drop(guard);

    assert!(pre_existing.exists());
    assert!(!pre_existing.join("nested").exists());
}

#[test]
fn detect_sandbox_backend_for_returns_expected_profile_backend() {
    let mod_backend = detect_sandbox_backend_for(SandboxProfileKind::Mod);
    let script_backend = detect_sandbox_backend_for(SandboxProfileKind::ScriptStep);
    let agent_backend = detect_sandbox_backend_for(SandboxProfileKind::AgentWorkspace);

    #[cfg(target_os = "macos")]
    {
        assert_eq!(mod_backend, SandboxBackend::Seatbelt);
        assert_eq!(script_backend, SandboxBackend::Seatbelt);
        assert_eq!(agent_backend, SandboxBackend::Seatbelt);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (mod_backend, script_backend, agent_backend);
    }
}
