use std::io;

#[cfg(target_os = "linux")]
use crate::backend::landlock_seccomp::PreparedLandlockSeccomp;
use crate::policy::ResourceLimits;

#[cfg(all(target_os = "linux", target_env = "gnu"))]
type RlimitResource = libc::__rlimit_resource_t;
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
type RlimitResource = libc::c_int;

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct BpfSlice {
    ptr: *const seccompiler::sock_filter,
    len: libc::c_ushort,
}

// SAFETY: `BpfSlice` points to the immutable `Vec<sock_filter>` owned by
// `PreparedLandlockSeccomp` in the parent across `fork()`/`exec()`, and is read
// without mutation in the child's `pre_exec` closure before `execve`.
#[cfg(target_os = "linux")]
unsafe impl Send for BpfSlice {}
// SAFETY: `BpfSlice` is read-only and never mutated across threads.
#[cfg(target_os = "linux")]
unsafe impl Sync for BpfSlice {}

#[cfg(target_os = "linux")]
#[repr(C)]
struct SockFprog {
    len: libc::c_ushort,
    filter: *const seccompiler::sock_filter,
}

fn apply_rlimit(resource: RlimitResource, value: u64) -> io::Result<()> {
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `current` is a valid mutable `libc::rlimit` passed to `getrlimit` for `resource`.
    let rc = unsafe { libc::getrlimit(resource, &raw mut current) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    let requested = value as libc::rlim_t;
    let capped = if current.rlim_max == libc::RLIM_INFINITY {
        requested
    } else {
        requested.min(current.rlim_max)
    };
    let next = libc::rlimit {
        rlim_cur: capped,
        rlim_max: capped,
    };
    // SAFETY: `next` is a valid initialized `libc::rlimit` whose values do not exceed `rlim_max`.
    let set_rc = unsafe { libc::setrlimit(resource, &raw const next) };
    if set_rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn configure_child_pre_exec(
    cmd: &mut tokio::process::Command,
    limits: &ResourceLimits,
    landlock_seccomp: Option<&PreparedLandlockSeccomp>,
) {
    let limits = limits.clone();
    let prepared_isolation = landlock_seccomp.map(|prep| {
        let bpf = prep.bpf_program();
        let len = libc::c_ushort::try_from(bpf.len()).unwrap_or(libc::c_ushort::MAX);
        (
            prep.raw_ruleset_fd(),
            BpfSlice {
                ptr: bpf.as_ptr(),
                len,
            },
        )
    });

    // SAFETY: The closure runs in the forked child before `exec` and invokes only
    // async-signal-safe OS syscalls (`setpgid`, `prctl`, `getrlimit`, `setrlimit`,
    // `landlock_restrict_self`, `seccomp`) with zero heap allocations.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }

            let _ = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);

            if let Some(cpu_secs) = limits.cpu_time_secs {
                apply_rlimit(libc::RLIMIT_CPU, cpu_secs)?;
            }
            if let Some(max_fsize) = limits.max_file_size_bytes {
                apply_rlimit(libc::RLIMIT_FSIZE, max_fsize)?;
            }
            if let Some(max_files) = limits.max_open_files {
                apply_rlimit(libc::RLIMIT_NOFILE, max_files)?;
            }
            if let Some(mem_bytes) = limits.memory_bytes {
                apply_rlimit(libc::RLIMIT_AS, mem_bytes)?;
            }
            if let Some(max_procs) = limits.max_processes {
                apply_rlimit(libc::RLIMIT_NPROC, max_procs)?;
            }

            if let Some((ruleset_fd, bpf_slice)) = prepared_isolation {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::syscall(
                    libc::SYS_landlock_restrict_self,
                    ruleset_fd,
                    0 as libc::c_uint,
                ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                let prog = SockFprog {
                    len: bpf_slice.len,
                    filter: bpf_slice.ptr,
                };
                if libc::syscall(
                    libc::SYS_seccomp,
                    libc::SECCOMP_SET_MODE_FILTER,
                    0 as libc::c_uint,
                    &raw const prog,
                ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
            }

            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn configure_child_pre_exec(cmd: &mut tokio::process::Command, limits: &ResourceLimits) {
    let limits = limits.clone();
    // SAFETY: The closure runs in the forked child before `exec` and invokes only
    // async-signal-safe OS syscalls (`setpgid`, `getrlimit`, `setrlimit`).
    unsafe {
        cmd.pre_exec(move || {
            // Put the child into its own process group (PGID == child PID) so
            // timeout, cancellation, and memory enforcement can kill the entire
            // descendant tree atomically.
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }

            if let Some(cpu_secs) = limits.cpu_time_secs {
                apply_rlimit(libc::RLIMIT_CPU, cpu_secs)?;
            }
            if let Some(max_fsize) = limits.max_file_size_bytes {
                apply_rlimit(libc::RLIMIT_FSIZE, max_fsize)?;
            }
            if let Some(max_files) = limits.max_open_files {
                apply_rlimit(libc::RLIMIT_NOFILE, max_files)?;
            }
            #[cfg(not(target_os = "macos"))]
            if let Some(mem_bytes) = limits.memory_bytes {
                apply_rlimit(libc::RLIMIT_AS, mem_bytes)?;
            }
            #[cfg(not(target_os = "macos"))]
            if let Some(max_procs) = limits.max_processes {
                apply_rlimit(libc::RLIMIT_NPROC, max_procs)?;
            }

            Ok(())
        });
    }
}

pub(super) fn kill_process_group(pid: u32) {
    let Ok(pgid) = libc::pid_t::try_from(pid) else {
        return;
    };
    if pgid <= 0 {
        return;
    }
    // SAFETY: `pgid` is a positive child PID created with `setpgid(0, 0)`, so
    // `-pgid` targets only that process group.
    unsafe {
        let _ = libc::kill(-pgid, libc::SIGKILL);
    }
}

#[cfg(target_os = "macos")]
fn query_pid_phys_footprint(pid: libc::pid_t) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: `info` is an adequately sized, zeroed `libc::rusage_info_v4` buffer matching
    // `libc::RUSAGE_INFO_V4`, and `proc_pid_rusage` fills it on 0 return.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: `proc_pid_rusage` returned 0, indicating `info` was initialized by the kernel.
    let initialized = unsafe { info.assume_init() };
    Some(initialized.ri_phys_footprint)
}

#[cfg(target_os = "macos")]
pub(super) fn sample_process_group_phys_footprint(pgid_u32: u32) -> Option<u64> {
    let pgid = libc::pid_t::try_from(pgid_u32).ok()?;
    if pgid <= 0 {
        return None;
    }

    // Query required buffer size first so large process groups are not truncated.
    // SAFETY: Passing null/0 queries the estimated byte count from `proc_listpgrppids`.
    let estimated_bytes = unsafe { libc::proc_listpgrppids(pgid, std::ptr::null_mut(), 0) };
    let pid_size = std::mem::size_of::<libc::pid_t>();
    let mut capacity = if estimated_bytes > 0 {
        ((estimated_bytes as usize) / pid_size).saturating_add(16)
    } else {
        32
    };

    let mut group_pids = Vec::new();
    for _ in 0..4 {
        let mut pids = vec![0 as libc::pid_t; capacity];
        let Ok(bytes_len) = libc::c_int::try_from(pids.len() * pid_size) else {
            break;
        };
        // SAFETY: `pids` provides `bytes_len` writable bytes of `pid_t` storage for `proc_listpgrppids`.
        let written_bytes = unsafe {
            libc::proc_listpgrppids(pgid, pids.as_mut_ptr().cast::<libc::c_void>(), bytes_len)
        };
        if written_bytes <= 0 {
            break;
        }
        let count = (written_bytes as usize) / pid_size;
        if count < pids.len() {
            pids.truncate(count);
            group_pids = pids;
            break;
        }
        // Buffer was filled; grow and retry in case more descendants spawned.
        pids.truncate(count);
        group_pids = pids;
        capacity = capacity.saturating_mul(2);
    }

    if !group_pids.contains(&pgid) {
        group_pids.push(pgid);
    }
    group_pids.sort_unstable();
    group_pids.dedup();

    let mut total: u64 = 0;
    let mut any = false;
    for pid in group_pids {
        if pid > 0
            && let Some(footprint) = query_pid_phys_footprint(pid)
        {
            total = total.saturating_add(footprint);
            any = true;
        }
    }
    any.then_some(total)
}
