use std::path::{Path, PathBuf};

use crate::backend::expand_logical_and_canonical;
use crate::error::SandboxError;
use crate::policy::{NetworkPolicy, SandboxPolicy, SandboxProfileKind};

/// Logical filesystem paths prepared for a Landlock ruleset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LandlockPathPlan {
    pub read_only_paths: Vec<PathBuf>,
    pub read_write_paths: Vec<PathBuf>,
}

/// High-level seccomp rule classification, exposed across platforms for unit testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeccompSyscallKind {
    IoUringSetup,
    IoUringEnter,
    IoUringRegister,
    Ptrace,
    ProcessVmReadv,
    ProcessVmWritev,
    Bpf,
    Mount,
    Unshare,
    KexecLoad,
    Socket,
    Socketpair,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeccompRuleSpec {
    AlwaysDeny(SeccompSyscallKind),
    DenyUnlessAfUnix(SeccompSyscallKind),
}

/// Validate whether `policy` can safely use the Linux Landlock + seccomp fallback.
///
/// Landlock filesystem rules are strictly additive allowlists and cannot carve out
/// read-only or denied subpaths inside a writable directory. Profiles that require
/// carve-outs (such as `AgentWorkspace`) or disable Landlock fallback must fail
/// closed when `bwrap` is unavailable.
pub(crate) fn check_landlock_eligibility(policy: &SandboxPolicy) -> Result<(), SandboxError> {
    if policy.profile == SandboxProfileKind::AgentWorkspace || !policy.allow_landlock_fallback {
        return Err(SandboxError::BackendUnavailable {
            profile: policy.profile,
            reason:
                "Linux Landlock fallback is disabled for this profile and bubblewrap (`bwrap`) is unavailable"
                    .to_owned(),
        });
    }
    if !policy.read_only_carveouts.is_empty() || !policy.deny_paths.is_empty() {
        return Err(SandboxError::BackendUnavailable {
            profile: policy.profile,
            reason:
                "Linux Landlock cannot enforce read_only_carveouts or deny_paths inside writable directories; bubblewrap (`bwrap`) is required"
                    .to_owned(),
        });
    }
    if policy.network == NetworkPolicy::LoopbackOnly {
        return Err(SandboxError::BackendUnavailable {
            profile: policy.profile,
            reason:
                "Linux Landlock + seccomp fallback does not support LoopbackOnly network filtering"
                    .to_owned(),
        });
    }
    Ok(())
}

/// Plan deduplicated read-only and read-write paths for Landlock enforcement.
pub(crate) fn plan_landlock_paths(program: &Path, policy: &SandboxPolicy) -> LandlockPathPlan {
    let mut ro_candidates = vec![
        PathBuf::from("/usr"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        PathBuf::from("/bin"),
        PathBuf::from("/sbin"),
        PathBuf::from("/etc"),
        PathBuf::from("/proc"),
        PathBuf::from("/dev"),
        PathBuf::from("/nix/store"),
    ];
    if policy.allow_host_paths {
        ro_candidates.extend([
            PathBuf::from("/home"),
            PathBuf::from("/mnt"),
            PathBuf::from("/media"),
        ]);
    }
    if let Some(parent) = program.parent()
        && !parent.as_os_str().is_empty()
    {
        ro_candidates.push(parent.to_path_buf());
    }
    ro_candidates.extend(policy.read_only_paths.iter().cloned());

    let mut rw_candidates = policy.read_write_paths.clone();
    if let Some(projects_root) = &policy.scratch_projects_root {
        rw_candidates.push(projects_root.clone());
    }
    rw_candidates.push(PathBuf::from("/dev/null"));

    LandlockPathPlan {
        read_only_paths: expand_logical_and_canonical(ro_candidates),
        read_write_paths: expand_logical_and_canonical(rw_candidates),
    }
}

/// Describe the seccomp-bpf rules enforced alongside Landlock.
pub(crate) fn describe_seccomp_rules(network: NetworkPolicy) -> Vec<SeccompRuleSpec> {
    let mut specs = vec![
        SeccompRuleSpec::AlwaysDeny(SeccompSyscallKind::IoUringSetup),
        SeccompRuleSpec::AlwaysDeny(SeccompSyscallKind::IoUringEnter),
        SeccompRuleSpec::AlwaysDeny(SeccompSyscallKind::IoUringRegister),
        SeccompRuleSpec::AlwaysDeny(SeccompSyscallKind::Ptrace),
        SeccompRuleSpec::AlwaysDeny(SeccompSyscallKind::ProcessVmReadv),
        SeccompRuleSpec::AlwaysDeny(SeccompSyscallKind::ProcessVmWritev),
        SeccompRuleSpec::AlwaysDeny(SeccompSyscallKind::Bpf),
        SeccompRuleSpec::AlwaysDeny(SeccompSyscallKind::Mount),
        SeccompRuleSpec::AlwaysDeny(SeccompSyscallKind::Unshare),
        SeccompRuleSpec::AlwaysDeny(SeccompSyscallKind::KexecLoad),
    ];
    if network == NetworkPolicy::Disabled {
        specs.push(SeccompRuleSpec::DenyUnlessAfUnix(
            SeccompSyscallKind::Socket,
        ));
        specs.push(SeccompRuleSpec::DenyUnlessAfUnix(
            SeccompSyscallKind::Socketpair,
        ));
    }
    specs
}

#[cfg(target_os = "linux")]
mod linux_impl {
    use std::collections::BTreeMap;
    use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};
    use std::path::Path;

    use landlock::{
        ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
        RulesetAttr, RulesetCreatedAttr,
    };
    use seccompiler::{
        BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule, TargetArch,
    };

    use super::{
        SeccompRuleSpec, SeccompSyscallKind, check_landlock_eligibility, describe_seccomp_rules,
        plan_landlock_paths,
    };
    use crate::error::SandboxError;
    use crate::policy::{NetworkPolicy, SandboxPolicy};

    /// Pre-allocated Landlock ruleset FD and compiled seccomp BPF program ready
    /// for zero-allocation installation inside `Command::pre_exec`.
    pub(crate) struct PreparedLandlockSeccomp {
        ruleset_fd: OwnedFd,
        bpf_program: BpfProgram,
    }

    impl PreparedLandlockSeccomp {
        pub(crate) fn raw_ruleset_fd(&self) -> RawFd {
            self.ruleset_fd.as_raw_fd()
        }

        pub(crate) fn bpf_program(&self) -> &BpfProgram {
            &self.bpf_program
        }
    }

    pub(crate) fn is_landlock_available() -> bool {
        let Ok(ruleset) = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(ABI::V1))
        else {
            return false;
        };
        ruleset.create().is_ok()
    }

    fn current_target_arch() -> Option<TargetArch> {
        #[cfg(target_arch = "x86_64")]
        {
            Some(TargetArch::x86_64)
        }
        #[cfg(target_arch = "aarch64")]
        {
            Some(TargetArch::aarch64)
        }
        #[cfg(target_arch = "riscv64")]
        {
            Some(TargetArch::riscv64)
        }
        #[cfg(not(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )))]
        {
            None
        }
    }

    #[allow(clippy::unnecessary_cast)]
    fn syscall_number(kind: SeccompSyscallKind) -> i64 {
        match kind {
            SeccompSyscallKind::IoUringSetup => libc::SYS_io_uring_setup as i64,
            SeccompSyscallKind::IoUringEnter => libc::SYS_io_uring_enter as i64,
            SeccompSyscallKind::IoUringRegister => libc::SYS_io_uring_register as i64,
            SeccompSyscallKind::Ptrace => libc::SYS_ptrace as i64,
            SeccompSyscallKind::ProcessVmReadv => libc::SYS_process_vm_readv as i64,
            SeccompSyscallKind::ProcessVmWritev => libc::SYS_process_vm_writev as i64,
            SeccompSyscallKind::Bpf => libc::SYS_bpf as i64,
            SeccompSyscallKind::Mount => libc::SYS_mount as i64,
            SeccompSyscallKind::Unshare => libc::SYS_unshare as i64,
            SeccompSyscallKind::KexecLoad => libc::SYS_kexec_load as i64,
            SeccompSyscallKind::Socket => libc::SYS_socket as i64,
            SeccompSyscallKind::Socketpair => libc::SYS_socketpair as i64,
        }
    }

    fn build_seccomp_bpf(policy: &SandboxPolicy) -> Result<BpfProgram, SandboxError> {
        let arch = current_target_arch().ok_or_else(|| SandboxError::BackendUnavailable {
            profile: policy.profile,
            reason: format!(
                "seccomp-bpf fallback is not supported on architecture `{}`",
                std::env::consts::ARCH
            ),
        })?;

        let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
        for spec in describe_seccomp_rules(policy.network) {
            match spec {
                SeccompRuleSpec::AlwaysDeny(kind) => {
                    rules.insert(syscall_number(kind), Vec::new());
                }
                SeccompRuleSpec::DenyUnlessAfUnix(kind) => {
                    let cond = SeccompCondition::new(
                        0,
                        SeccompCmpArgLen::Dword,
                        SeccompCmpOp::Ne,
                        libc::AF_UNIX as u64,
                    )
                    .map_err(|e| SandboxError::ProfileSetupFailed {
                        reason: format!("failed to build seccomp condition: {e}"),
                    })?;
                    let rule = SeccompRule::new(vec![cond]).map_err(|e| {
                        SandboxError::ProfileSetupFailed {
                            reason: format!("failed to build seccomp rule: {e}"),
                        }
                    })?;
                    rules.insert(syscall_number(kind), vec![rule]);
                }
            }
        }

        let filter = SeccompFilter::new(
            rules,
            SeccompAction::Allow,
            SeccompAction::Errno(libc::EPERM as u32),
            arch,
        )
        .map_err(|e| SandboxError::ProfileSetupFailed {
            reason: format!("failed to construct seccomp filter: {e}"),
        })?;

        BpfProgram::try_from(filter).map_err(|e| SandboxError::ProfileSetupFailed {
            reason: format!("failed to compile seccomp BPF program: {e}"),
        })
    }

    pub(crate) fn prepare_landlock_seccomp(
        program: &Path,
        policy: &SandboxPolicy,
    ) -> Result<PreparedLandlockSeccomp, SandboxError> {
        check_landlock_eligibility(policy)?;

        let mut ruleset = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(ABI::V1))
            .map_err(|e| SandboxError::BackendUnavailable {
                profile: policy.profile,
                reason: format!("Linux Landlock is unavailable on this kernel: {e}"),
            })?
            .set_compatibility(CompatLevel::BestEffort)
            .handle_access(AccessFs::Refer | AccessFs::Truncate | AccessFs::IoctlDev)
            .map_err(|e| SandboxError::ProfileSetupFailed {
                reason: format!("failed to configure Landlock filesystem rights: {e}"),
            })?;

        if policy.network == NetworkPolicy::Disabled {
            ruleset = ruleset
                .handle_access(AccessNet::BindTcp | AccessNet::ConnectTcp)
                .map_err(|e| SandboxError::ProfileSetupFailed {
                    reason: format!("failed to configure Landlock network rights: {e}"),
                })?;
        }

        let mut created = ruleset
            .create()
            .map_err(|e| SandboxError::BackendUnavailable {
                profile: policy.profile,
                reason: format!("failed to create Landlock ruleset: {e}"),
            })?;

        let plan = plan_landlock_paths(program, policy);
        let ro_dir_access = AccessFs::from_read(ABI::V5);
        let ro_file_access = ro_dir_access & AccessFs::from_file(ABI::V5);
        for path in &plan.read_only_paths {
            let Ok(meta) = std::fs::metadata(path) else {
                continue;
            };
            let Ok(fd) = PathFd::new(path) else {
                continue;
            };
            let access = if meta.is_dir() {
                ro_dir_access
            } else {
                ro_file_access
            };
            created = created
                .add_rule(PathBeneath::new(fd, access))
                .map_err(|e| SandboxError::ProfileSetupFailed {
                    reason: format!(
                        "failed to add read-only Landlock rule for `{}`: {e}",
                        path.display()
                    ),
                })?;
        }

        let rw_dir_access = AccessFs::from_all(ABI::V5);
        let rw_file_access = AccessFs::from_file(ABI::V5);
        for path in &plan.read_write_paths {
            let Ok(meta) = std::fs::metadata(path) else {
                continue;
            };
            let Ok(fd) = PathFd::new(path) else {
                continue;
            };
            let access = if meta.is_dir() {
                rw_dir_access
            } else {
                rw_file_access
            };
            created = created
                .add_rule(PathBeneath::new(fd, access))
                .map_err(|e| SandboxError::ProfileSetupFailed {
                    reason: format!(
                        "failed to add read-write Landlock rule for `{}`: {e}",
                        path.display()
                    ),
                })?;
        }

        let Some(ruleset_fd): Option<OwnedFd> = created.into() else {
            return Err(SandboxError::BackendUnavailable {
                profile: policy.profile,
                reason: "Landlock ruleset was not enforced by the running kernel".to_owned(),
            });
        };

        let bpf_program = build_seccomp_bpf(policy)?;

        Ok(PreparedLandlockSeccomp {
            ruleset_fd,
            bpf_program,
        })
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux_impl::{
    PreparedLandlockSeccomp, is_landlock_available, prepare_landlock_seccomp,
};
