use std::collections::BTreeMap;
use std::convert::TryInto;

use seccompiler::{SeccompAction, SeccompFilter, BpfProgram};
use tracing::info;

pub fn apply_seccomp() -> std::result::Result<(), String> {
    let blocked: &[i64] = &[
        libc::SYS_ptrace,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_reboot,
        libc::SYS_kexec_load,
        libc::SYS_kexec_file_load,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_userfaultfd,
        libc::SYS_perf_event_open,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_keyctl,
        libc::SYS_acct,
        libc::SYS_syslog,
        libc::SYS_lookup_dcookie,
    ];

    let rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> = blocked
        .iter()
        .map(|&syscall| (syscall, vec![]))
        .collect();

    let arch = std::env::consts::ARCH
        .try_into()
        .map_err(|e| format!("arch: {e}"))?;

    let filter: BpfProgram = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .map_err(|e| format!("seccomp filter build: {e}"))?
    .try_into()
    .map_err(|e| format!("seccomp compile: {e}"))?;

    seccompiler::apply_filter_all_threads(&filter)
        .map_err(|e| format!("seccomp apply: {e}"))?;

    info!("seccomp-bpf sandbox applied");
    Ok(())
}
