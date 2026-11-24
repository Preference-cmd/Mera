use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::backend::bwrap::build_bwrap_args;
use crate::backend::seatbelt::build_seatbelt_spec;
use crate::backend::{
    SandboxBackend, resolve_sandbox_launch, resolve_sandbox_launch_with_override,
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
