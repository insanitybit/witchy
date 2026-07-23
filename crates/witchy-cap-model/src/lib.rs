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
            Self::Dir | Self::File => READ_WRITE_RIGHTS,
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
        assert_eq!(CapabilityKind::Console.right("Write"), None);
    }
}
