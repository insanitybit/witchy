//! Dependency-bottom vocabulary for Witchy's built-in capabilities.
//!
//! Syntax resolution, type checking, footprint analysis, runtime providers,
//! launch tooling, and host menus all consume this catalog. Keeping the model
//! free of AST and runtime dependencies prevents those consumers from growing
//! private capability-name and rights tables.

#![deny(unsafe_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityClass {
    Host,
    Derived,
    Build,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityRight {
    Read,
    Write,
    Connect,
    Listen,
    Tcp,
    Udp,
    Uds,
}

impl CapabilityRight {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::Write => "Write",
            Self::Connect => "Connect",
            Self::Listen => "Listen",
            Self::Tcp => "Tcp",
            Self::Udp => "Udp",
            Self::Uds => "Uds",
        }
    }
}

pub const READ_WRITE_RIGHTS: &[CapabilityRight] =
    &[CapabilityRight::Read, CapabilityRight::Write];
pub const NET_VERB_RIGHTS: &[CapabilityRight] =
    &[CapabilityRight::Connect, CapabilityRight::Listen];
pub const NET_TRANSPORT_RIGHTS: &[CapabilityRight] =
    &[CapabilityRight::Tcp, CapabilityRight::Udp, CapabilityRight::Uds];
pub const NET_RIGHTS: &[CapabilityRight] = &[
    CapabilityRight::Connect,
    CapabilityRight::Listen,
    CapabilityRight::Tcp,
    CapabilityRight::Udp,
    CapabilityRight::Uds,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityKind {
    Console,
    Clock,
    Rand,
    Env,
    Secret,
    SecretStore,
    Dir,
    File,
    Net,
    Fetch,
    Exec,
    Socket,
    Listener,
    BuildOut,
    BuildRead,
    BuildEnv,
    BuildNet,
    BuildExec,
}

impl CapabilityKind {
    pub const ALL: &'static [Self] = &[
        Self::Console,
        Self::Clock,
        Self::Rand,
        Self::Env,
        Self::Secret,
        Self::SecretStore,
        Self::Dir,
        Self::File,
        Self::Net,
        Self::Fetch,
        Self::Exec,
        Self::Socket,
        Self::Listener,
        Self::BuildOut,
        Self::BuildRead,
        Self::BuildEnv,
        Self::BuildNet,
        Self::BuildExec,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Console => "Console",
            Self::Clock => "Clock",
            Self::Rand => "Rand",
            Self::Env => "Env",
            Self::Secret => "Secret",
            Self::SecretStore => "SecretStore",
            Self::Dir => "Dir",
            Self::File => "File",
            Self::Net => "Net",
            Self::Fetch => "Fetch",
            Self::Exec => "Exec",
            Self::Socket => "Socket",
            Self::Listener => "Listener",
            Self::BuildOut => "BuildOut",
            Self::BuildRead => "BuildRead",
            Self::BuildEnv => "BuildEnv",
            Self::BuildNet => "BuildNet",
            Self::BuildExec => "BuildExec",
        }
    }

    pub const fn class(self) -> CapabilityClass {
        match self {
            Self::Socket | Self::Listener => CapabilityClass::Derived,
            Self::BuildOut
            | Self::BuildRead
            | Self::BuildEnv
            | Self::BuildNet
            | Self::BuildExec => CapabilityClass::Build,
            _ => CapabilityClass::Host,
        }
    }

    pub const fn rights(self) -> &'static [CapabilityRight] {
        match self {
            Self::Console | Self::Dir | Self::File => READ_WRITE_RIGHTS,
            Self::Net => NET_RIGHTS,
            _ => &[],
        }
    }

    pub fn right(self, name: &str) -> Option<CapabilityRight> {
        self.rights().iter().copied().find(|right| right.name() == name)
    }

    /// Ordinary capability values have arity zero. Rights-bearing capability
    /// syntax is checked by the dedicated marker validator rather than generic
    /// type arity checking.
    pub const fn builtin_arity(self) -> Option<usize> {
        if self.rights().is_empty() { Some(0) } else { None }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Console" => Some(Self::Console),
            "Clock" => Some(Self::Clock),
            "Rand" => Some(Self::Rand),
            "Env" => Some(Self::Env),
            "Secret" => Some(Self::Secret),
            "SecretStore" => Some(Self::SecretStore),
            "Dir" => Some(Self::Dir),
            "File" => Some(Self::File),
            "Net" => Some(Self::Net),
            "Fetch" => Some(Self::Fetch),
            "Exec" => Some(Self::Exec),
            "Socket" => Some(Self::Socket),
            "Listener" => Some(Self::Listener),
            "BuildOut" => Some(Self::BuildOut),
            "BuildRead" => Some(Self::BuildRead),
            "BuildEnv" => Some(Self::BuildEnv),
            "BuildNet" => Some(Self::BuildNet),
            "BuildExec" => Some(Self::BuildExec),
            _ => None,
        }
    }
}

pub fn is_capability_type_name(name: &str) -> bool {
    CapabilityKind::from_name(name).is_some()
}

pub fn is_host_capability(name: &str) -> bool {
    CapabilityKind::from_name(name)
        .is_some_and(|kind| kind.class() == CapabilityClass::Host)
}

pub fn is_build_capability(name: &str) -> bool {
    CapabilityKind::from_name(name)
        .is_some_and(|kind| kind.class() == CapabilityClass::Build)
}

const DIR_DENY_ALL: &str = "\u{0}";

/// Narrow a directory entry policy by intersecting each constrained dimension.
///
/// The empty policy is unrestricted. Recognized dimensions are represented as
/// newline-separated `dimension:value` rows. An invalid or disjoint refinement
/// fails closed to an unrepresentable sentinel accepted by [`dir_admits`].
pub fn dir_only(current: &str, refine: &str) -> String {
    if refine.is_empty() {
        return current.to_string();
    }
    use std::collections::{BTreeMap, BTreeSet};

    fn group(value: &str) -> BTreeMap<&str, BTreeSet<&str>> {
        let mut grouped: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for pattern in value.split('\n') {
            if let Some((dimension, _)) = pattern.split_once(':') {
                grouped.entry(dimension).or_default().insert(pattern);
            }
        }
        grouped
    }

    let refinement = group(refine);
    if refinement.is_empty() {
        return DIR_DENY_ALL.to_string();
    }
    if current.is_empty() {
        return refine.to_string();
    }
    let current = group(current);
    let mut narrowed = BTreeSet::new();
    for (dimension, patterns) in &current {
        if !refinement.contains_key(dimension) {
            narrowed.extend(patterns.iter().copied());
        }
    }
    for (dimension, patterns) in &refinement {
        match current.get(dimension) {
            Some(existing) => {
                let intersection: BTreeSet<&str> =
                    patterns.intersection(existing).copied().collect();
                if intersection.is_empty() {
                    return DIR_DENY_ALL.to_string();
                }
                narrowed.extend(intersection);
            }
            None => narrowed.extend(patterns.iter().copied()),
        }
    }
    narrowed.into_iter().collect::<Vec<_>>().join("\n")
}

/// Whether a normalized entry is admitted by a directory entry policy.
pub fn dir_admits(policy: &str, name: &str, is_dir: bool) -> bool {
    if policy.is_empty() {
        return true;
    }
    let (mut has_extension, mut extension_allowed) = (false, false);
    let (mut has_kind, mut kind_allowed) = (false, false);
    for pattern in policy.split('\n') {
        if pattern == DIR_DENY_ALL {
            return false;
        }
        if let Some(extension) = pattern.strip_prefix("ext:") {
            has_extension = true;
            if is_dir || name.ends_with(extension) {
                extension_allowed = true;
            }
        } else if let Some(kind) = pattern.strip_prefix("kind:") {
            has_kind = true;
            if (kind == "dir") == is_dir {
                kind_allowed = true;
            }
        }
    }
    (!has_extension || extension_allowed) && (!has_kind || kind_allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_names_are_unique_and_round_trip() {
        let mut names = std::collections::BTreeSet::new();
        for kind in CapabilityKind::ALL {
            assert!(names.insert(kind.name()), "duplicate capability {}", kind.name());
            assert_eq!(CapabilityKind::from_name(kind.name()), Some(*kind));
        }
    }

    #[test]
    fn rights_belong_only_to_their_declared_capability() {
        assert_eq!(CapabilityKind::Dir.right("Read"), Some(CapabilityRight::Read));
        assert_eq!(CapabilityKind::Net.right("Connect"), Some(CapabilityRight::Connect));
        assert_eq!(CapabilityKind::Net.right("Read"), None);
        assert_eq!(CapabilityKind::Console.right("Read"), Some(CapabilityRight::Read));
        assert_eq!(CapabilityKind::Console.right("Write"), Some(CapabilityRight::Write));
    }

    #[test]
    fn directory_policy_refinement_is_monotone_and_fail_closed() {
        assert_eq!(
            dir_only("kind:file\next:.txt", "ext:.txt"),
            "ext:.txt\nkind:file"
        );
        assert!(!dir_admits(
            &dir_only("ext:.txt", "ext:.md"),
            "note.txt",
            false
        ));
        assert!(!dir_admits(&dir_only("", "garbled"), "anything", true));
        assert!(dir_admits("ext:.txt", "nested", true));
        assert!(!dir_admits("kind:file", "nested", true));
    }
}
