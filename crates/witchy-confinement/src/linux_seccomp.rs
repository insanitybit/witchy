use std::collections::{BTreeMap, BTreeSet};
use std::convert::TryInto;

use seccompiler::{
    BpfProgram, SeccompAction, SeccompFilter, SeccompRule,
    apply_filter_all_threads,
};
use syscalls::Sysno;

use crate::{
    EnforcementError, EnforcementMode, Layer, LayerReport, LayerStatus, Policy,
    SyscallClass,
};

pub(crate) fn apply(
    policy: &Policy,
    mode: EnforcementMode,
) -> Result<LayerReport, EnforcementError> {
    if !policy.syscall_classes.contains(&SyscallClass::Base) {
        return Err(EnforcementError(
            "syscall confinement policy is missing the mandatory `base` promise".into(),
        ));
    }

    let denied = denied_syscalls(policy);
    let rules: BTreeMap<i64, Vec<SeccompRule>> = denied
        .iter()
        .map(|syscall| (i64::from(syscall.id()), Vec::new()))
        .collect();
    let target = std::env::consts::ARCH
        .try_into()
        .map_err(|error| enforcement_error("select target architecture", error))?;
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        target,
    )
    .map_err(|error| enforcement_error("compile promise-class filter", error))?;
    let program: BpfProgram = filter
        .try_into()
        .map_err(|error| enforcement_error("compile seccomp BPF", error))?;

    match apply_filter_all_threads(&program) {
        Ok(()) => Ok(LayerReport {
            layer: Layer::Syscalls,
            provider: "seccomp-bpf",
            status: LayerStatus::Enforced,
            detail: format!(
                "seccomp promise classes enforced across all threads ({} syscalls denied)",
                denied.len()
            ),
        }),
        Err(error) if mode == EnforcementMode::BestEffort => Ok(LayerReport {
            layer: Layer::Syscalls,
            provider: "seccomp-bpf",
            status: LayerStatus::Unavailable,
            detail: format!("seccomp filter could not be installed: {error}"),
        }),
        Err(error) => Err(enforcement_error("install seccomp filter", error)),
    }
}

fn denied_syscalls(policy: &Policy) -> BTreeSet<Sysno> {
    const GATED: [SyscallClass; 4] = [
        SyscallClass::FsOpen,
        SyscallClass::Network,
        SyscallClass::Listen,
        SyscallClass::Process,
    ];
    let mut denied = hard_denied_syscalls();
    for class in GATED {
        if !policy.syscall_classes.contains(&class) {
            denied.extend(class_syscalls(class));
        }
    }
    // A syscall may belong to more than one class (e.g. recvfrom/sendto are IPC
    // for `Process`'s spawn error-channel AND appear in `Network`). If ANY
    // granted class needs it, it must not be denied — so remove every granted
    // class's syscalls from the denial set after the union above. `hard_denied`
    // is never in a grantable class, so it is unaffected.
    for class in GATED {
        if policy.syscall_classes.contains(&class) {
            for syscall in class_syscalls(class) {
                denied.remove(&syscall);
            }
        }
    }
    denied
}

fn class_syscalls(class: SyscallClass) -> BTreeSet<Sysno> {
    match class {
        SyscallClass::Base => BTreeSet::new(),
        SyscallClass::FsOpen => fs_open_syscalls(),
        SyscallClass::Network => BTreeSet::from([
            Sysno::socket,
            Sysno::connect,
            Sysno::sendto,
            Sysno::recvfrom,
            Sysno::sendmsg,
            Sysno::recvmsg,
            Sysno::sendmmsg,
            Sysno::recvmmsg,
            Sysno::shutdown,
            Sysno::getsockname,
            Sysno::getpeername,
            Sysno::setsockopt,
            Sysno::getsockopt,
        ]),
        SyscallClass::Listen => BTreeSet::from([
            Sysno::bind,
            Sysno::listen,
            Sysno::accept,
            Sysno::accept4,
        ]),
        SyscallClass::Process => process_syscalls(),
    }
}

fn fs_open_syscalls() -> BTreeSet<Sysno> {
    let mut syscalls = BTreeSet::from([
        Sysno::openat,
        Sysno::openat2,
        Sysno::statx,
        Sysno::faccessat,
        Sysno::faccessat2,
        Sysno::readlinkat,
    ]);
    // The directory-relative stat syscall carries a different name per arch in
    // the `syscalls` table: `newfstatat` on x86_64, `fstatat` on aarch64.
    #[cfg(target_arch = "x86_64")]
    syscalls.insert(Sysno::newfstatat);
    #[cfg(target_arch = "aarch64")]
    syscalls.insert(Sysno::fstatat);
    #[cfg(target_arch = "x86_64")]
    syscalls.extend([
        Sysno::open,
        Sysno::creat,
        Sysno::stat,
        Sysno::lstat,
        Sysno::access,
        Sysno::readlink,
    ]);
    syscalls
}

fn process_syscalls() -> BTreeSet<Sysno> {
    // NOTE: `clone`/`clone3` are deliberately NOT gated here. Thread creation is
    // runtime infrastructure — the Wasmtime engine (and Witchy's own concurrency
    // runtime) must spawn threads to execute ANY guest, regardless of the guest's
    // capabilities, and threads inherit this same seccomp + Landlock confinement,
    // so they are not an escalation. The real process capability is launching a
    // NEW, unconfined executable, which is gated by `execve`/`execveat` (and the
    // bare `fork`/`vfork` copy primitives) below. Denying `clone` here made every
    // non-`Exec` program fail to start under enforced seccomp.
    // `mut` is used only on x86_64 (for the bare fork/vfork/pipe primitives that
    // do not exist on aarch64); allow the unused-mut on other arches.
    #[allow(unused_mut)]
    let mut syscalls = BTreeSet::from([
        Sysno::socketpair,
        // Rust's `std::process` spawn opens an AF_UNIX SOCK_SEQPACKET socketpair
        // as the child's exec-error channel and reads/writes it with
        // recvfrom/sendto. These are IPC over a private local pair, NOT network
        // access (real networking still needs `socket`+`connect`, gated by the
        // Network class). Gating them under Network made every `Exec` spawn from
        // a program without `Net` fail with "the CLOEXEC pipe failed: Operation
        // not permitted".
        Sysno::recvfrom,
        Sysno::sendto,
        Sysno::execve,
        Sysno::execveat,
        Sysno::wait4,
        Sysno::waitid,
        Sysno::pipe2,
    ]);
    #[cfg(target_arch = "x86_64")]
    syscalls.extend([Sysno::fork, Sysno::vfork, Sysno::pipe]);
    syscalls
}

fn hard_denied_syscalls() -> BTreeSet<Sysno> {
    BTreeSet::from([
        Sysno::io_uring_setup,
        Sysno::io_uring_enter,
        Sysno::io_uring_register,
        Sysno::bpf,
        Sysno::ptrace,
        Sysno::userfaultfd,
        Sysno::process_vm_readv,
        Sysno::process_vm_writev,
        Sysno::pidfd_getfd,
        Sysno::name_to_handle_at,
        Sysno::open_by_handle_at,
        Sysno::open_tree,
        Sysno::fsopen,
        Sysno::fsconfig,
        Sysno::fsmount,
        Sysno::fspick,
        Sysno::move_mount,
        Sysno::mount_setattr,
        Sysno::mount,
        Sysno::umount2,
        Sysno::pivot_root,
        Sysno::unshare,
        Sysno::setns,
    ])
}

fn enforcement_error(
    operation: &str,
    error: impl std::fmt::Display,
) -> EnforcementError {
    EnforcementError(format!("cannot {operation}: {error}"))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::FsScope;

    const EXEC_CHILD: &str = "WITCHY_SECCOMP_EXEC_CHILD";
    const SOCKET_CHILD: &str = "WITCHY_SECCOMP_SOCKET_CHILD";

    fn run_child(test: &str, marker: &str) -> std::process::Output {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(marker, "1")
            .output()
            .unwrap()
    }

    #[test]
    fn promise_sets_track_only_unarmed_classes() {
        let empty = denied_syscalls(&Policy::default());
        assert!(empty.contains(&Sysno::openat));
        assert!(empty.contains(&Sysno::socket));
        assert!(empty.contains(&Sysno::bind));
        assert!(empty.contains(&Sysno::execve));

        let mut granted = Policy::default();
        granted.add_fs_rule(
            "/tmp",
            FsScope::Tree,
            crate::FsAccess::new(true, false, false),
        );
        granted.network.connect_requested = true;
        granted.network.bind_requested = true;
        granted.syscall_classes.insert(SyscallClass::Process);
        granted.normalize_classes();
        let fully_granted = denied_syscalls(&granted);
        assert!(fully_granted.contains(&Sysno::io_uring_setup));
        assert!(fully_granted.contains(&Sysno::open_by_handle_at));
        assert!(!fully_granted.contains(&Sysno::openat));
        assert!(!fully_granted.contains(&Sysno::socket));
        assert!(!fully_granted.contains(&Sysno::socketpair));
        assert!(!fully_granted.contains(&Sysno::execve));
    }

    #[test]
    fn seccomp_denies_exec_without_process_promise() {
        if std::env::var_os(EXEC_CHILD).is_some() {
            let report = apply(&Policy::default(), EnforcementMode::Required)
                .unwrap();
            assert_eq!(report.status, LayerStatus::Enforced);
            let error = Command::new("/bin/true").status().unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            return;
        }

        let output = run_child(
            "linux_seccomp::tests::seccomp_denies_exec_without_process_promise",
            EXEC_CHILD,
        );
        assert!(
            output.status.success(),
            "child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn seccomp_denies_socket_without_network_promise() {
        if std::env::var_os(SOCKET_CHILD).is_some() {
            let report = apply(&Policy::default(), EnforcementMode::Required)
                .unwrap();
            assert_eq!(report.status, LayerStatus::Enforced);
            let error = std::net::TcpListener::bind(("127.0.0.1", 0))
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            return;
        }

        let output = run_child(
            "linux_seccomp::tests::seccomp_denies_socket_without_network_promise",
            SOCKET_CHILD,
        );
        assert!(
            output.status.success(),
            "child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
