use landlock::{
    Access, AccessFs, AccessNet, ABI, CompatLevel, Compatible, NetPort,
    PathBeneath, PathFd, RestrictionStatus, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus,
};

use crate::{
    EnforcementError, EnforcementMode, EnforcementReport, FsAccess, FsScope,
    Layer, LayerReport, LayerStatus, Policy,
};

pub(crate) fn apply(
    policy: &Policy,
    mode: EnforcementMode,
) -> Result<EnforcementReport, EnforcementError> {
    if mode == EnforcementMode::Required && policy.network.has_unexpressed_transport {
        return Err(EnforcementError(
            "required TCP confinement cannot express a granted UDP or Unix-domain transport"
                .into(),
        ));
    }

    let compatibility = match mode {
        EnforcementMode::Required => CompatLevel::HardRequirement,
        EnforcementMode::BestEffort | EnforcementMode::Disabled => {
            CompatLevel::BestEffort
        }
    };
    let filesystem = apply_filesystem(policy, compatibility)?;
    let mut tcp = apply_tcp(policy, compatibility)?;
    if policy.network.has_unexpressed_transport && tcp.status == LayerStatus::Enforced {
        tcp.status = LayerStatus::Partial;
        tcp.detail =
            "Landlock enforces TCP ports; a granted UDP or Unix-domain transport remains host-layer only"
                .into();
    }
    let syscalls = apply_syscalls(policy, mode)?;
    let report = EnforcementReport {
        layers: vec![filesystem, tcp, syscalls],
    };
    if mode == EnforcementMode::Required && !report.fully_enforced() {
        return Err(EnforcementError(format!(
            "required platform confinement was not fully enforced: {:?}",
            report.layers
        )));
    }
    Ok(report)
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64"))]
fn apply_syscalls(
    policy: &Policy,
    mode: EnforcementMode,
) -> Result<LayerReport, EnforcementError> {
    crate::linux_seccomp::apply(policy, mode)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64")))]
fn apply_syscalls(
    _policy: &Policy,
    mode: EnforcementMode,
) -> Result<LayerReport, EnforcementError> {
    if mode == EnforcementMode::Required {
        return Err(EnforcementError(
            "required syscall confinement is unavailable on this Linux architecture".into(),
        ));
    }
    Ok(LayerReport {
        layer: Layer::Syscalls,
        provider: "none",
        status: LayerStatus::Unavailable,
        detail: "seccomp provider supports x86_64, aarch64, and riscv64".into(),
    })
}

fn apply_filesystem(
    policy: &Policy,
    compatibility: CompatLevel,
) -> Result<LayerReport, EnforcementError> {
    let abi = ABI::V3;
    let ruleset = Ruleset::default()
        .set_compatibility(compatibility)
        .handle_access(AccessFs::from_all(abi))
        .map_err(error)?;
    let mut created = ruleset.create().map_err(error)?;
    for rule in &policy.filesystem {
        let access = fs_access(rule.scope, rule.access);
        if access.is_empty() {
            continue;
        }
        let path = PathFd::new(&rule.path).map_err(error)?;
        created = created
            .add_rule(PathBeneath::new(path, access))
            .map_err(error)?;
    }
    let status = created.restrict_self().map_err(error)?;
    Ok(layer_report(Layer::Filesystem, status))
}

fn apply_tcp(
    policy: &Policy,
    compatibility: CompatLevel,
) -> Result<LayerReport, EnforcementError> {
    let abi = ABI::V4;
    let mut created = Ruleset::default()
        .set_compatibility(compatibility)
        .handle_access(AccessNet::from_all(abi))
        .map_err(error)?
        .create()
        .map_err(error)?;
    for port in &policy.network.connect_tcp_ports {
        created = created
            .add_rule(NetPort::new(*port, AccessNet::ConnectTcp))
            .map_err(error)?;
    }
    for port in &policy.network.bind_tcp_ports {
        created = created
            .add_rule(NetPort::new(*port, AccessNet::BindTcp))
            .map_err(error)?;
    }
    let status = created.restrict_self().map_err(error)?;
    Ok(layer_report(Layer::Tcp, status))
}

fn fs_access(scope: FsScope, access: FsAccess) -> landlock::BitFlags<AccessFs> {
    let abi = ABI::V3;
    let mut allowed = AccessFs::from_all(abi);
    if !access.read {
        allowed &= !AccessFs::from_read(abi);
    }
    if !access.write {
        allowed &= !AccessFs::from_write(abi);
    }
    if !access.execute {
        allowed &= !AccessFs::Execute;
    }
    if scope == FsScope::File {
        allowed &= AccessFs::ReadFile
            | AccessFs::WriteFile
            | AccessFs::Truncate
            | AccessFs::Execute;
    }
    allowed
}

fn layer_report(layer: Layer, status: RestrictionStatus) -> LayerReport {
    let (status, detail) = match (status.ruleset, status.no_new_privs) {
        (RulesetStatus::FullyEnforced, false) => (
            LayerStatus::Partial,
            "Landlock ruleset reported enforcement without no_new_privs".to_string(),
        ),
        (RulesetStatus::FullyEnforced, true) => (
            LayerStatus::Enforced,
            "Landlock ruleset fully enforced".to_string(),
        ),
        (RulesetStatus::PartiallyEnforced, _) => (
            LayerStatus::Partial,
            "Landlock ruleset partially enforced by this kernel".to_string(),
        ),
        (RulesetStatus::NotEnforced, _) => (
            LayerStatus::Unavailable,
            "Landlock ruleset is not enforced by this kernel".to_string(),
        ),
    };
    LayerReport {
        layer,
        provider: "landlock",
        status,
        detail,
    }
}

fn error(error: impl std::fmt::Display) -> EnforcementError {
    EnforcementError(format!("cannot apply Landlock confinement: {error}"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::*;

    const FS_CHILD: &str = "WITCHY_LANDLOCK_FS_CHILD";
    const TCP_CHILD: &str = "WITCHY_LANDLOCK_TCP_CHILD";

    fn scratch(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "witchy-landlock-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn run_child(test: &str, env: (&str, &Path)) -> std::process::Output {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(env.0, env.1)
            .output()
            .unwrap()
    }

    #[test]
    fn landlock_denies_an_ungranted_filesystem_path() {
        if let Some(root) = std::env::var_os(FS_CHILD) {
            let root = PathBuf::from(root);
            let allowed = root.join("allowed");
            let outside = root.join("outside");
            let mut policy = Policy::default();
            policy.add_fs_rule(
                &allowed,
                FsScope::Tree,
                FsAccess::new(true, false, false),
            );
            let report = apply(&policy, EnforcementMode::Required).unwrap();
            assert!(report.fully_enforced(), "{report:?}");
            assert_eq!(std::fs::read_to_string(allowed.join("value")).unwrap(), "allowed");
            let error = std::fs::read_to_string(outside.join("value")).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            return;
        }

        let root = scratch("fs");
        let allowed = root.join("allowed");
        let outside = root.join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(allowed.join("value"), "allowed").unwrap();
        std::fs::write(outside.join("value"), "outside").unwrap();
        let output = run_child(
            "linux::tests::landlock_denies_an_ungranted_filesystem_path",
            (FS_CHILD, &root),
        );
        std::fs::remove_dir_all(&root).unwrap();
        assert!(
            output.status.success(),
            "child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn landlock_denies_an_ungranted_tcp_port() {
        if let Some(port) = std::env::var_os(TCP_CHILD) {
            let port: u16 = port.to_string_lossy().parse().unwrap();
            let report = apply(&Policy::default(), EnforcementMode::Required).unwrap();
            assert!(report.fully_enforced(), "{report:?}");
            let error = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            return;
        }

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "linux::tests::landlock_denies_an_ungranted_tcp_port",
                "--nocapture",
            ])
            .env(TCP_CHILD, port.to_string())
            .output()
            .unwrap();
        drop(listener);
        assert!(
            output.status.success(),
            "child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
