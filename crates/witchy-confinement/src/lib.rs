//! Target-neutral platform-confinement policy.
//!
//! Capability providers describe concrete authority. This crate turns that
//! authority into one normalized policy consumed by independent native and web
//! enforcement providers. It deliberately has no compiler, runtime, Wasmtime,
//! browser, or operating-system dependency.

use std::collections::BTreeSet;
use std::path::PathBuf;

mod provider;
pub use provider::{
    EnforcementError, EnforcementReport, Layer, LayerReport, LayerStatus, apply,
};

#[cfg(target_os = "linux")]
mod linux;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EnforcementMode {
    /// Reusable library and test paths do not mutate process-wide policy.
    #[default]
    Disabled,
    /// Arm every available layer and report any unavailable or partial layer.
    BestEffort,
    /// Refuse launch unless every policy dimension has an enforcing provider.
    Required,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FsAccess {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl FsAccess {
    pub const fn new(read: bool, write: bool, execute: bool) -> Self {
        Self {
            read,
            write,
            execute,
        }
    }

    fn union(&mut self, other: Self) {
        self.read |= other.read;
        self.write |= other.write;
        self.execute |= other.execute;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FsScope {
    Tree,
    File,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsRule {
    pub path: PathBuf,
    pub scope: FsScope,
    pub access: FsAccess,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SyscallClass {
    Base,
    FsOpen,
    Network,
    Listen,
    Process,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NetworkPolicy {
    /// A connect-capable network root exists, even if its concrete allowlist is
    /// empty. Providers need this distinction from "no network grant."
    pub connect_requested: bool,
    /// A listen-capable network root exists, even if its concrete allowlist is
    /// empty.
    pub bind_requested: bool,
    pub connect_tcp_ports: BTreeSet<u16>,
    pub bind_tcp_ports: BTreeSet<u16>,
    /// Canonical Fetch origins are retained for CSP derivation. Native providers
    /// additionally project their TCP ports into `connect_tcp_ports`.
    pub fetch_origins: BTreeSet<String>,
    /// True when a granted transport cannot be represented by TCP-port Landlock
    /// rules (for example UDP or a Unix-domain socket). Required mode must not
    /// claim complete network enforcement for such a policy.
    pub has_unexpressed_transport: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Policy {
    pub filesystem: Vec<FsRule>,
    pub network: NetworkPolicy,
    pub syscall_classes: BTreeSet<SyscallClass>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            filesystem: Vec::new(),
            network: NetworkPolicy::default(),
            syscall_classes: BTreeSet::from([SyscallClass::Base]),
        }
    }
}

impl Policy {
    pub fn add_fs_rule(
        &mut self,
        path: impl Into<PathBuf>,
        scope: FsScope,
        access: FsAccess,
    ) {
        let path = path.into();
        if let Some(existing) = self
            .filesystem
            .iter_mut()
            .find(|rule| rule.path == path && rule.scope == scope)
        {
            existing.access.union(access);
            return;
        }
        self.filesystem.push(FsRule {
            path,
            scope,
            access,
        });
        self.filesystem.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| scope_order(left.scope).cmp(&scope_order(right.scope)))
        });
    }

    pub fn add_connect_target(&mut self, target: &str) {
        self.network.connect_requested = true;
        match tcp_port(target) {
            Some(port) => {
                self.network.connect_tcp_ports.insert(port);
            }
            None if is_non_tcp_target(target) => {
                self.network.has_unexpressed_transport = true;
            }
            None => {
                self.network.has_unexpressed_transport = true;
            }
        }
    }

    pub fn add_bind_target(&mut self, target: &str) {
        self.network.bind_requested = true;
        match tcp_port(target) {
            Some(port) => {
                self.network.bind_tcp_ports.insert(port);
            }
            None => {
                self.network.has_unexpressed_transport = true;
            }
        }
    }

    pub fn add_fetch_origin(&mut self, origin: &str) {
        self.network.connect_requested = true;
        self.network.fetch_origins.insert(origin.to_string());
        match origin_port(origin) {
            Some(port) => {
                self.network.connect_tcp_ports.insert(port);
            }
            None => {
                self.network.has_unexpressed_transport = true;
            }
        }
    }

    pub fn normalize_classes(&mut self) {
        if !self.filesystem.is_empty() {
            self.syscall_classes.insert(SyscallClass::FsOpen);
        }
        if self.network.connect_requested
            || self.network.bind_requested
            || self.network.has_unexpressed_transport
        {
            self.syscall_classes.insert(SyscallClass::Network);
        }
        if self.network.bind_requested {
            self.syscall_classes.insert(SyscallClass::Listen);
        }
    }
}

fn scope_order(scope: FsScope) -> u8 {
    match scope {
        FsScope::Tree => 0,
        FsScope::File => 1,
    }
}

fn is_non_tcp_target(target: &str) -> bool {
    target.starts_with("unix:")
        || target.starts_with("uds:")
        || target.starts_with("udp:")
}

fn tcp_port(target: &str) -> Option<u16> {
    if is_non_tcp_target(target) {
        return None;
    }
    let target = target
        .strip_prefix("tcp:")
        .or_else(|| target.strip_prefix("tls:"))
        .unwrap_or(target);
    target.rsplit_once(':')?.1.parse().ok()
}

fn origin_port(origin: &str) -> Option<u16> {
    let (scheme, authority) = origin.split_once("://")?;
    if let Some(port) = tcp_port(authority) {
        return Some(port);
    }
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_rules_union_without_losing_scope() {
        let mut policy = Policy::default();
        policy.add_fs_rule("/work", FsScope::Tree, FsAccess::new(true, false, false));
        policy.add_fs_rule("/work", FsScope::Tree, FsAccess::new(false, true, false));
        policy.add_fs_rule("/work", FsScope::File, FsAccess::new(true, false, false));
        assert_eq!(
            policy.filesystem,
            vec![
                FsRule {
                    path: PathBuf::from("/work"),
                    scope: FsScope::Tree,
                    access: FsAccess::new(true, true, false),
                },
                FsRule {
                    path: PathBuf::from("/work"),
                    scope: FsScope::File,
                    access: FsAccess::new(true, false, false),
                },
            ]
        );
    }

    #[test]
    fn network_targets_normalize_ports_and_record_gaps() {
        let mut policy = Policy::default();
        policy.add_connect_target("db.example:5432");
        policy.add_connect_target("tls:[::1]:8443");
        policy.add_bind_target("127.0.0.1:8080");
        policy.add_fetch_origin("https://api.example");
        policy.add_fetch_origin("http://localhost:3000");
        policy.add_connect_target("unix:/tmp/service.sock");
        policy.normalize_classes();

        assert_eq!(
            policy.network.connect_tcp_ports,
            BTreeSet::from([443, 3000, 5432, 8443])
        );
        assert_eq!(policy.network.bind_tcp_ports, BTreeSet::from([8080]));
        assert!(policy.network.has_unexpressed_transport);
        assert!(policy.syscall_classes.contains(&SyscallClass::Network));
        assert!(policy.syscall_classes.contains(&SyscallClass::Listen));
    }
}
