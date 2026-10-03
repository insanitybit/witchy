//! Stable, dependency-bottom authority contracts for RFC-0163.
//!
//! These identifiers are part of the validating compiler's input language.
//! They are assigned explicitly: declaration order, Rust discriminants, and
//! source spellings are never security identities. The catalog describes the
//! semantic operation a Witchy program performs. A later ABI adapter maps each
//! operation to one or more concrete host imports.

use crate::{CapabilityKind, CapabilityRight, NET_RIGHTS, READ_WRITE_RIGHTS, SECRET_RIGHTS};

const NO_RIGHTS: &[CapabilityRight] = &[];
const READ: &[CapabilityRight] = &[CapabilityRight::Read];
const WRITE: &[CapabilityRight] = &[CapabilityRight::Write];
const CONNECT: &[CapabilityRight] = &[CapabilityRight::Connect];
const CONNECT_TCP: &[CapabilityRight] = &[CapabilityRight::Connect, CapabilityRight::Tcp];
const LISTEN_TCP: &[CapabilityRight] = &[CapabilityRight::Listen, CapabilityRight::Tcp];
const REVEAL: &[CapabilityRight] = &[CapabilityRight::Reveal, CapabilityRight::Seal];
const SEAL: &[CapabilityRight] = &[CapabilityRight::Seal];

/// A stable identifier for one built-in capability kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct CapabilityKindId(u16);

impl CapabilityKindId {
    pub const CONSOLE: Self = Self(0x0001);
    pub const CLOCK: Self = Self(0x0002);
    pub const RAND: Self = Self(0x0003);
    pub const ENV: Self = Self(0x0004);
    pub const SECRET: Self = Self(0x0005);
    pub const SECRET_STORE: Self = Self(0x0006);
    pub const DIR: Self = Self(0x0007);
    pub const FILE: Self = Self(0x0008);
    pub const NET: Self = Self(0x0009);
    pub const FETCH: Self = Self(0x000a);
    pub const EXEC: Self = Self(0x000b);

    pub const SOCKET: Self = Self(0x0101);
    pub const LISTENER: Self = Self(0x0102);

    pub const BUILD_OUT: Self = Self(0x0201);
    pub const BUILD_READ: Self = Self(0x0202);
    pub const BUILD_ENV: Self = Self(0x0203);
    pub const BUILD_NET: Self = Self(0x0204);
    pub const BUILD_EXEC: Self = Self(0x0205);

    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u16 {
        self.0
    }
}

/// A stable identifier for a right in a capability's right lattice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct CapabilityRightId(u16);

impl CapabilityRightId {
    pub const READ: Self = Self(0x0001);
    pub const WRITE: Self = Self(0x0002);
    pub const CONNECT: Self = Self(0x0003);
    pub const LISTEN: Self = Self(0x0004);
    pub const TCP: Self = Self(0x0005);
    pub const UDP: Self = Self(0x0006);
    pub const UDS: Self = Self(0x0007);
    pub const REVEAL: Self = Self(0x0008);
    pub const SEAL: Self = Self(0x0009);

    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u16 {
        self.0
    }
}

impl CapabilityKind {
    pub const fn stable_id(self) -> CapabilityKindId {
        match self {
            Self::Console => CapabilityKindId::CONSOLE,
            Self::Clock => CapabilityKindId::CLOCK,
            Self::Rand => CapabilityKindId::RAND,
            Self::Env => CapabilityKindId::ENV,
            Self::Secret => CapabilityKindId::SECRET,
            Self::SecretStore => CapabilityKindId::SECRET_STORE,
            Self::Dir => CapabilityKindId::DIR,
            Self::File => CapabilityKindId::FILE,
            Self::Net => CapabilityKindId::NET,
            Self::Fetch => CapabilityKindId::FETCH,
            Self::Exec => CapabilityKindId::EXEC,
            Self::Socket => CapabilityKindId::SOCKET,
            Self::Listener => CapabilityKindId::LISTENER,
            Self::BuildOut => CapabilityKindId::BUILD_OUT,
            Self::BuildRead => CapabilityKindId::BUILD_READ,
            Self::BuildEnv => CapabilityKindId::BUILD_ENV,
            Self::BuildNet => CapabilityKindId::BUILD_NET,
            Self::BuildExec => CapabilityKindId::BUILD_EXEC,
        }
    }
}

impl CapabilityRight {
    pub const fn stable_id(self) -> CapabilityRightId {
        match self {
            Self::Read => CapabilityRightId::READ,
            Self::Write => CapabilityRightId::WRITE,
            Self::Connect => CapabilityRightId::CONNECT,
            Self::Listen => CapabilityRightId::LISTEN,
            Self::Tcp => CapabilityRightId::TCP,
            Self::Udp => CapabilityRightId::UDP,
            Self::Uds => CapabilityRightId::UDS,
            Self::Reveal => CapabilityRightId::REVEAL,
            Self::Seal => CapabilityRightId::SEAL,
        }
    }

    /// Explicit canonical bit in [`RightsSet`]. This mapping is independent of
    /// both Rust discriminants and the stable numeric right ID.
    pub const fn canonical_bit(self) -> u16 {
        match self {
            Self::Read => 1 << 0,
            Self::Write => 1 << 1,
            Self::Connect => 1 << 2,
            Self::Listen => 1 << 3,
            Self::Tcp => 1 << 4,
            Self::Udp => 1 << 5,
            Self::Uds => 1 << 6,
            Self::Reveal => 1 << 7,
            Self::Seal => 1 << 8,
        }
    }
}

pub const fn capability_kind_from_id(id: CapabilityKindId) -> Option<CapabilityKind> {
    match id {
        CapabilityKindId::CONSOLE => Some(CapabilityKind::Console),
        CapabilityKindId::CLOCK => Some(CapabilityKind::Clock),
        CapabilityKindId::RAND => Some(CapabilityKind::Rand),
        CapabilityKindId::ENV => Some(CapabilityKind::Env),
        CapabilityKindId::SECRET => Some(CapabilityKind::Secret),
        CapabilityKindId::SECRET_STORE => Some(CapabilityKind::SecretStore),
        CapabilityKindId::DIR => Some(CapabilityKind::Dir),
        CapabilityKindId::FILE => Some(CapabilityKind::File),
        CapabilityKindId::NET => Some(CapabilityKind::Net),
        CapabilityKindId::FETCH => Some(CapabilityKind::Fetch),
        CapabilityKindId::EXEC => Some(CapabilityKind::Exec),
        CapabilityKindId::SOCKET => Some(CapabilityKind::Socket),
        CapabilityKindId::LISTENER => Some(CapabilityKind::Listener),
        CapabilityKindId::BUILD_OUT => Some(CapabilityKind::BuildOut),
        CapabilityKindId::BUILD_READ => Some(CapabilityKind::BuildRead),
        CapabilityKindId::BUILD_ENV => Some(CapabilityKind::BuildEnv),
        CapabilityKindId::BUILD_NET => Some(CapabilityKind::BuildNet),
        CapabilityKindId::BUILD_EXEC => Some(CapabilityKind::BuildExec),
        _ => None,
    }
}

pub const fn capability_right_from_id(id: CapabilityRightId) -> Option<CapabilityRight> {
    match id {
        CapabilityRightId::READ => Some(CapabilityRight::Read),
        CapabilityRightId::WRITE => Some(CapabilityRight::Write),
        CapabilityRightId::CONNECT => Some(CapabilityRight::Connect),
        CapabilityRightId::LISTEN => Some(CapabilityRight::Listen),
        CapabilityRightId::TCP => Some(CapabilityRight::Tcp),
        CapabilityRightId::UDP => Some(CapabilityRight::Udp),
        CapabilityRightId::UDS => Some(CapabilityRight::Uds),
        CapabilityRightId::REVEAL => Some(CapabilityRight::Reveal),
        CapabilityRightId::SEAL => Some(CapabilityRight::Seal),
        _ => None,
    }
}

const KNOWN_RIGHT_BITS: u16 = (1 << 9) - 1;

/// Failure to construct a canonical dynamic right set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightsSetError {
    UnknownBits(u16),
    RightNotAvailable {
        kind: CapabilityKind,
        right: CapabilityRight,
    },
    /// Serialized sets are already canonical. `from_rights` may normalize this
    /// source spelling, but a decoder must reject the noncanonical bit pattern.
    RevealWithoutSeal,
}

/// The one canonical dynamic representation of capability rights.
///
/// Static catalog rows retain readable slices, but source, WIR, certificate,
/// and final-byte checkers should exchange this validated closed bitset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct RightsSet(u16);

impl RightsSet {
    pub const EMPTY: Self = Self(0);

    /// Construct from a source-level collection. Secret Reveal is normalized
    /// to the closed `{Reveal, Seal}` set before validation.
    pub fn from_rights(
        kind: CapabilityKind,
        rights: &[CapabilityRight],
    ) -> Result<Self, RightsSetError> {
        let mut bits = 0;
        for right in rights {
            if !kind.rights().contains(right) {
                return Err(RightsSetError::RightNotAvailable {
                    kind,
                    right: *right,
                });
            }
            bits |= right.canonical_bit();
            if *right == CapabilityRight::Reveal {
                bits |= CapabilityRight::Seal.canonical_bit();
            }
        }
        Self::from_canonical_bits(kind, bits)
    }

    /// Decode an already-canonical bitset. Unknown bits and an unclosed Secret
    /// Reveal bit fail closed rather than being silently repaired.
    pub fn from_canonical_bits(kind: CapabilityKind, bits: u16) -> Result<Self, RightsSetError> {
        let unknown = bits & !KNOWN_RIGHT_BITS;
        if unknown != 0 {
            return Err(RightsSetError::UnknownBits(unknown));
        }
        if bits & CapabilityRight::Reveal.canonical_bit() != 0
            && bits & CapabilityRight::Seal.canonical_bit() == 0
        {
            return Err(RightsSetError::RevealWithoutSeal);
        }
        for right in [
            CapabilityRight::Read,
            CapabilityRight::Write,
            CapabilityRight::Connect,
            CapabilityRight::Listen,
            CapabilityRight::Tcp,
            CapabilityRight::Udp,
            CapabilityRight::Uds,
            CapabilityRight::Reveal,
            CapabilityRight::Seal,
        ] {
            if bits & right.canonical_bit() != 0 && !kind.rights().contains(&right) {
                return Err(RightsSetError::RightNotAvailable { kind, right });
            }
        }
        Ok(Self(bits))
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn contains(self, right: CapabilityRight) -> bool {
        self.0 & right.canonical_bit() != 0
    }

    pub fn is_well_formed_for(self, kind: CapabilityKind) -> bool {
        Self::from_canonical_bits(kind, self.0).is_ok()
    }

    /// Whether this held set authorizes every right in `required`.
    pub fn admits(self, kind: CapabilityKind, required: Self) -> bool {
        self.is_well_formed_for(kind)
            && required.is_well_formed_for(kind)
            && required.0 & !self.0 == 0
    }

    pub fn is_subset_of(self, kind: CapabilityKind, parent: Self) -> bool {
        parent.admits(kind, self)
    }

    pub fn is_monotone_narrowing(kind: CapabilityKind, parent: Self, child: Self) -> bool {
        child.is_subset_of(kind, parent)
    }
}

/// Representation of the receiver at the concrete host-call boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostReceiverEncoding {
    /// No runtime operand crosses the boundary. Validation must nevertheless
    /// prove the source parameter or root ghost that authorizes the call.
    ErasedGhost,
    /// An unforgeable host reference crosses the boundary.
    ExternRef,
}

/// Representation used when a capability must survive an internal suspension
/// or closure carrier. This is deliberately independent of host-call erasure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalCarrierEncoding {
    ExternRef,
}

/// A stable identifier for a host/compilation target domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct TargetDomainId(u16);

impl TargetDomainId {
    pub const NATIVE_RUNTIME: Self = Self(0x0001);
    pub const BROWSER_BASE: Self = Self(0x0002);
    pub const BROWSER_CAPABILITY_PROVIDER: Self = Self(0x0003);
    pub const NATIVE_BUILD_WORKER: Self = Self(0x0004);
    pub const INTERPRETER_ORACLE: Self = Self(0x7fff);

    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Diagnostic metadata for one target ID. `shippable` keeps the interpreter
/// oracle from being mistaken for a production admission target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetDomainContract {
    pub id: TargetDomainId,
    pub name: &'static str,
    pub shippable: bool,
}

pub const TARGET_DOMAIN_CONTRACTS: &[TargetDomainContract] = &[
    TargetDomainContract {
        id: TargetDomainId::NATIVE_RUNTIME,
        name: "native-runtime",
        shippable: true,
    },
    TargetDomainContract {
        id: TargetDomainId::BROWSER_BASE,
        name: "browser-base",
        shippable: true,
    },
    TargetDomainContract {
        id: TargetDomainId::BROWSER_CAPABILITY_PROVIDER,
        name: "browser-capability-provider",
        shippable: true,
    },
    TargetDomainContract {
        id: TargetDomainId::NATIVE_BUILD_WORKER,
        name: "native-build-worker",
        shippable: true,
    },
    TargetDomainContract {
        id: TargetDomainId::INTERPRETER_ORACLE,
        name: "interpreter-oracle",
        shippable: false,
    },
];

pub fn target_domain_contract(id: TargetDomainId) -> Option<&'static TargetDomainContract> {
    TARGET_DOMAIN_CONTRACTS
        .iter()
        .find(|contract| contract.id == id)
}

const NATIVE: &[TargetDomainId] = &[
    TargetDomainId::NATIVE_RUNTIME,
    TargetDomainId::INTERPRETER_ORACLE,
];
const NATIVE_AND_BROWSER_PROVIDER: &[TargetDomainId] = &[
    TargetDomainId::NATIVE_RUNTIME,
    TargetDomainId::BROWSER_CAPABILITY_PROVIDER,
    TargetDomainId::INTERPRETER_ORACLE,
];
const CONSOLE_WRITE_TARGETS: &[TargetDomainId] = &[
    TargetDomainId::NATIVE_RUNTIME,
    TargetDomainId::BROWSER_BASE,
    TargetDomainId::BROWSER_CAPABILITY_PROVIDER,
    TargetDomainId::INTERPRETER_ORACLE,
];
const BUILD: &[TargetDomainId] = &[
    TargetDomainId::NATIVE_BUILD_WORKER,
    TargetDomainId::INTERPRETER_ORACLE,
];

/// The kind of policy value carried by an authority identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityScopeKind {
    ConsoleStreams,
    Clock,
    Randomness,
    EnvironmentNames,
    SecretMaterial,
    SecretNames,
    DirectoryTree,
    FilePath,
    NetworkAddresses,
    FetchOrigins,
    ExecutablePrograms,
    ConnectedEndpoint,
    ListeningEndpoint,
    BuildOutputRoot,
    BuildReadRoots,
    BuildEnvironmentNames,
    BuildNetworkHosts,
    BuildTools,
}

/// Stable identity of a scope kind in canonical authority bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct AuthorityScopeKindId(u16);

impl AuthorityScopeKindId {
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u16 {
        self.0
    }
}

impl AuthorityScopeKind {
    pub const fn stable_id(self) -> AuthorityScopeKindId {
        match self {
            Self::ConsoleStreams => AuthorityScopeKindId(0x0001),
            Self::Clock => AuthorityScopeKindId(0x0002),
            Self::Randomness => AuthorityScopeKindId(0x0003),
            Self::EnvironmentNames => AuthorityScopeKindId(0x0004),
            Self::SecretMaterial => AuthorityScopeKindId(0x0005),
            Self::SecretNames => AuthorityScopeKindId(0x0006),
            Self::DirectoryTree => AuthorityScopeKindId(0x0007),
            Self::FilePath => AuthorityScopeKindId(0x0008),
            Self::NetworkAddresses => AuthorityScopeKindId(0x0009),
            Self::FetchOrigins => AuthorityScopeKindId(0x000a),
            Self::ExecutablePrograms => AuthorityScopeKindId(0x000b),
            Self::ConnectedEndpoint => AuthorityScopeKindId(0x0101),
            Self::ListeningEndpoint => AuthorityScopeKindId(0x0102),
            Self::BuildOutputRoot => AuthorityScopeKindId(0x0201),
            Self::BuildReadRoots => AuthorityScopeKindId(0x0202),
            Self::BuildEnvironmentNames => AuthorityScopeKindId(0x0203),
            Self::BuildNetworkHosts => AuthorityScopeKindId(0x0204),
            Self::BuildTools => AuthorityScopeKindId(0x0205),
        }
    }
}

pub const fn authority_scope_kind_from_id(id: AuthorityScopeKindId) -> Option<AuthorityScopeKind> {
    match id.0 {
        0x0001 => Some(AuthorityScopeKind::ConsoleStreams),
        0x0002 => Some(AuthorityScopeKind::Clock),
        0x0003 => Some(AuthorityScopeKind::Randomness),
        0x0004 => Some(AuthorityScopeKind::EnvironmentNames),
        0x0005 => Some(AuthorityScopeKind::SecretMaterial),
        0x0006 => Some(AuthorityScopeKind::SecretNames),
        0x0007 => Some(AuthorityScopeKind::DirectoryTree),
        0x0008 => Some(AuthorityScopeKind::FilePath),
        0x0009 => Some(AuthorityScopeKind::NetworkAddresses),
        0x000a => Some(AuthorityScopeKind::FetchOrigins),
        0x000b => Some(AuthorityScopeKind::ExecutablePrograms),
        0x0101 => Some(AuthorityScopeKind::ConnectedEndpoint),
        0x0102 => Some(AuthorityScopeKind::ListeningEndpoint),
        0x0201 => Some(AuthorityScopeKind::BuildOutputRoot),
        0x0202 => Some(AuthorityScopeKind::BuildReadRoots),
        0x0203 => Some(AuthorityScopeKind::BuildEnvironmentNames),
        0x0204 => Some(AuthorityScopeKind::BuildNetworkHosts),
        0x0205 => Some(AuthorityScopeKind::BuildTools),
        _ => None,
    }
}

/// Digest-sized identity for canonical restricted policy bytes. This layer
/// stores an identity supplied by the canonicalizer; it does not hash values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ScopeRestrictionId([u8; 32]);

impl ScopeRestrictionId {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Canonical scope value identity. Different restricted digests are not
/// assumed comparable; a typed refinement edge must justify that transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityScope {
    Unrestricted {
        kind: AuthorityScopeKind,
    },
    Restricted {
        kind: AuthorityScopeKind,
        restriction: ScopeRestrictionId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeKindMismatch {
    pub held: AuthorityScopeKind,
    pub required: AuthorityScopeKind,
}

impl AuthorityScope {
    pub const fn unrestricted(kind: AuthorityScopeKind) -> Self {
        Self::Unrestricted { kind }
    }

    pub const fn restricted(kind: AuthorityScopeKind, restriction: ScopeRestrictionId) -> Self {
        Self::Restricted { kind, restriction }
    }

    pub const fn kind(self) -> AuthorityScopeKind {
        match self {
            Self::Unrestricted { kind } | Self::Restricted { kind, .. } => kind,
        }
    }

    pub const fn restriction(self) -> Option<ScopeRestrictionId> {
        match self {
            Self::Unrestricted { .. } => None,
            Self::Restricted { restriction, .. } => Some(restriction),
        }
    }

    /// Whether `held` is at least as broad as `required` using identity alone.
    /// A differently restricted child needs a separately checked refinement
    /// edge; it is never guessed from opaque digests.
    pub fn admits(self, required: Self) -> Result<bool, ScopeKindMismatch> {
        if self.kind() != required.kind() {
            return Err(ScopeKindMismatch {
                held: self.kind(),
                required: required.kind(),
            });
        }
        Ok(match (self, required) {
            (Self::Unrestricted { .. }, _) => true,
            (
                Self::Restricted {
                    restriction: held, ..
                },
                Self::Restricted {
                    restriction: required,
                    ..
                },
            ) => held == required,
            (Self::Restricted { .. }, Self::Unrestricted { .. }) => false,
        })
    }

    pub fn is_monotone_narrowing(parent: Self, child: Self) -> Result<bool, ScopeKindMismatch> {
        parent.admits(child)
    }
}

/// How a capability kind may first enter the source authority environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootOriginPolicy {
    EntryParameter,
    /// A source entry parameter exists, but its host receiver is erased. The
    /// independent judgment must mint and propagate a ghost identity for it.
    GhostEntryParameter,
    DerivedOnly,
}

/// A typed, monotone transformation of an existing authority scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeNarrowingKind {
    RightsAscription,
    EnvOnly,
    ExecOnly,
    DirOnly,
    NetOnly,
    NetDeny,
    FetchOnly,
}

/// A typed authority-producing operation. Each result retains a unique parent
/// lineage; these tags describe the permitted parent/result kind relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivationKind {
    SecretStoreGet,
    SecretStoreRequire,
    DirSubtree,
    DirReadFile,
    DirWriteFile,
    NetFetch,
    NetConnect,
    NetConnectPinned,
    NetTryConnectPinned,
    NetListen,
    NetListenTls,
    ListenerAccept,
}

const NO_DERIVATIONS: &[DerivationKind] = &[];
const SECRET_DERIVATIONS: &[DerivationKind] = &[
    DerivationKind::SecretStoreGet,
    DerivationKind::SecretStoreRequire,
];
const DIR_DERIVATIONS: &[DerivationKind] = &[DerivationKind::DirSubtree];
const FILE_DERIVATIONS: &[DerivationKind] =
    &[DerivationKind::DirReadFile, DerivationKind::DirWriteFile];
const FETCH_DERIVATIONS: &[DerivationKind] = &[DerivationKind::NetFetch];
const SOCKET_DERIVATIONS: &[DerivationKind] = &[
    DerivationKind::NetConnect,
    DerivationKind::NetConnectPinned,
    DerivationKind::NetTryConnectPinned,
    DerivationKind::ListenerAccept,
];
const LISTENER_DERIVATIONS: &[DerivationKind] =
    &[DerivationKind::NetListen, DerivationKind::NetListenTls];

/// Static contract for a built-in capability kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityContract {
    pub kind: CapabilityKind,
    pub id: CapabilityKindId,
    pub rights: &'static [CapabilityRight],
    pub scope: AuthorityScopeKind,
    pub host_receiver: HostReceiverEncoding,
    pub internal_carrier: InternalCarrierEncoding,
    pub root_origin: RootOriginPolicy,
    pub permitted_derivations: &'static [DerivationKind],
}

macro_rules! capability {
    ($kind:ident, $rights:expr, $scope:ident, $host:ident, $root:ident, $derivations:expr) => {
        CapabilityContract {
            kind: CapabilityKind::$kind,
            id: CapabilityKind::$kind.stable_id(),
            rights: $rights,
            scope: AuthorityScopeKind::$scope,
            host_receiver: HostReceiverEncoding::$host,
            internal_carrier: InternalCarrierEncoding::ExternRef,
            root_origin: RootOriginPolicy::$root,
            permitted_derivations: $derivations,
        }
    };
}

pub const CAPABILITY_CONTRACTS: &[CapabilityContract] = &[
    capability!(
        Console,
        READ_WRITE_RIGHTS,
        ConsoleStreams,
        ErasedGhost,
        GhostEntryParameter,
        NO_DERIVATIONS
    ),
    capability!(
        Clock,
        NO_RIGHTS,
        Clock,
        ErasedGhost,
        GhostEntryParameter,
        NO_DERIVATIONS
    ),
    capability!(
        Rand,
        NO_RIGHTS,
        Randomness,
        ErasedGhost,
        GhostEntryParameter,
        NO_DERIVATIONS
    ),
    capability!(
        Env,
        NO_RIGHTS,
        EnvironmentNames,
        ExternRef,
        EntryParameter,
        NO_DERIVATIONS
    ),
    capability!(
        Secret,
        SECRET_RIGHTS,
        SecretMaterial,
        ExternRef,
        EntryParameter,
        SECRET_DERIVATIONS
    ),
    capability!(
        SecretStore,
        NO_RIGHTS,
        SecretNames,
        ErasedGhost,
        GhostEntryParameter,
        NO_DERIVATIONS
    ),
    capability!(
        Dir,
        READ_WRITE_RIGHTS,
        DirectoryTree,
        ExternRef,
        EntryParameter,
        DIR_DERIVATIONS
    ),
    capability!(
        File,
        READ_WRITE_RIGHTS,
        FilePath,
        ExternRef,
        EntryParameter,
        FILE_DERIVATIONS
    ),
    capability!(
        Net,
        NET_RIGHTS,
        NetworkAddresses,
        ExternRef,
        EntryParameter,
        NO_DERIVATIONS
    ),
    capability!(
        Fetch,
        NO_RIGHTS,
        FetchOrigins,
        ExternRef,
        EntryParameter,
        FETCH_DERIVATIONS
    ),
    capability!(
        Exec,
        NO_RIGHTS,
        ExecutablePrograms,
        ExternRef,
        EntryParameter,
        NO_DERIVATIONS
    ),
    capability!(
        Socket,
        NO_RIGHTS,
        ConnectedEndpoint,
        ExternRef,
        DerivedOnly,
        SOCKET_DERIVATIONS
    ),
    capability!(
        Listener,
        NO_RIGHTS,
        ListeningEndpoint,
        ExternRef,
        DerivedOnly,
        LISTENER_DERIVATIONS
    ),
    capability!(
        BuildOut,
        NO_RIGHTS,
        BuildOutputRoot,
        ErasedGhost,
        GhostEntryParameter,
        NO_DERIVATIONS
    ),
    capability!(
        BuildRead,
        NO_RIGHTS,
        BuildReadRoots,
        ErasedGhost,
        GhostEntryParameter,
        NO_DERIVATIONS
    ),
    capability!(
        BuildEnv,
        NO_RIGHTS,
        BuildEnvironmentNames,
        ErasedGhost,
        GhostEntryParameter,
        NO_DERIVATIONS
    ),
    capability!(
        BuildNet,
        NO_RIGHTS,
        BuildNetworkHosts,
        ErasedGhost,
        GhostEntryParameter,
        NO_DERIVATIONS
    ),
    capability!(
        BuildExec,
        NO_RIGHTS,
        BuildTools,
        ErasedGhost,
        GhostEntryParameter,
        NO_DERIVATIONS
    ),
];

pub fn capability_contract(kind: CapabilityKind) -> &'static CapabilityContract {
    CAPABILITY_CONTRACTS
        .iter()
        .find(|contract| contract.kind == kind)
        .expect("every built-in capability has an authority contract")
}

/// Whether a source entry root (and therefore a later ABI root-mint step) may
/// introduce this capability kind. Derived-only handles must fail this check.
pub fn root_origin_permits_entry(kind: CapabilityKind) -> bool {
    !matches!(
        capability_contract(kind).root_origin,
        RootOriginPolicy::DerivedOnly
    )
}

/// Whether one held right supplies one required right.
pub fn right_implies(
    kind: CapabilityKind,
    held: CapabilityRight,
    required: CapabilityRight,
) -> bool {
    held == required
        || matches!(
            (kind, held, required),
            (
                CapabilityKind::Secret,
                CapabilityRight::Reveal,
                CapabilityRight::Seal
            )
        )
}

pub fn rights_are_well_formed(kind: CapabilityKind, rights: &[CapabilityRight]) -> bool {
    for (index, right) in rights.iter().enumerate() {
        if !kind.rights().contains(right) || rights[index + 1..].contains(right) {
            return false;
        }
    }
    !rights.contains(&CapabilityRight::Reveal) || rights.contains(&CapabilityRight::Seal)
}

/// Whether `held` supplies every right in `required` under the capability's
/// lattice. Invalid or non-canonical sets fail closed.
pub fn rights_admit(
    kind: CapabilityKind,
    held: &[CapabilityRight],
    required: &[CapabilityRight],
) -> bool {
    rights_are_well_formed(kind, held)
        && rights_are_well_formed(kind, required)
        && required.iter().all(|required| {
            held.iter()
                .any(|held| right_implies(kind, *held, *required))
        })
}

pub fn rights_are_monotone_narrowing(
    kind: CapabilityKind,
    parent: &[CapabilityRight],
    child: &[CapabilityRight],
) -> bool {
    rights_admit(kind, parent, child)
}

/// Role played by one concrete ABI import in a semantic operation protocol.
/// Multiple imports may implement one [`AuthorityOperationContract`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportRole {
    RootMint,
    AuthorityAction,
    AuthorityAttenuation,
    AuthorityDerivation,
    ResultSize,
    ResultFill,
    PendingResultDrain,
    PendingListDrain,
    LaunchInput,
    PureInfrastructure,
    InternalService,
    RuntimeDiagnostic,
}

/// Shape of the multi-import host protocol implementing a semantic operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportProtocol {
    Direct,
    /// The authority import stages bytes; infrastructure drains them.
    PendingBytes,
    /// Separate authority imports compute size and fill caller-owned memory.
    SplitFill,
    /// The authority import stages a list; infrastructure drains it.
    PendingList,
}

/// A stable semantic operation identifier. Names are diagnostic only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct HostOperationId(u32);

impl HostOperationId {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

/// An additional capability-bearing operand required by an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityOperandRequirement {
    /// Zero-based index in the complete semantic call, including the receiver.
    pub operand_index: u8,
    pub kind: CapabilityKind,
    pub rights: &'static [CapabilityRight],
}

const NO_AUTHORITY_OPERANDS: &[AuthorityOperandRequirement] = &[];
const EXEC_DIR_REQUIREMENT: &[AuthorityOperandRequirement] = &[AuthorityOperandRequirement {
    operand_index: 1,
    kind: CapabilityKind::Dir,
    rights: READ,
}];
const TLS_SECRET_REQUIREMENT: &[AuthorityOperandRequirement] = &[AuthorityOperandRequirement {
    operand_index: 3,
    kind: CapabilityKind::Secret,
    rights: SEAL,
}];

/// How rights are assigned to a newly derived authority identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivedRights {
    NoRights,
    PreserveReceiver,
    Exact(&'static [CapabilityRight]),
    /// The named launch grant determines Reveal versus Seal. The result may
    /// never exceed that grant, even if source syntax names bare `Secret`.
    SecretGrantBounded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityDerivation {
    pub kind: DerivationKind,
    pub result_kind: CapabilityKind,
    pub result_rights: DerivedRights,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityOperationEffect {
    Use,
    Attenuate(ScopeNarrowingKind),
    Derive(AuthorityDerivation),
}

/// Typed events consumed by the independent source and final-WIR judgments.
/// `ReceiveRoot` is separate from a host ABI `mint_*` implementation step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityFlowEvent {
    ReceiveRoot(CapabilityKind),
    NarrowRights(CapabilityKind),
    NarrowScope(ScopeNarrowingKind),
    Derive(DerivationKind),
    Use(HostOperationId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityOperationContract {
    pub id: HostOperationId,
    /// Stable diagnostic spelling. The numeric ID remains the identity.
    pub name: &'static str,
    pub receiver: CapabilityKind,
    pub required_receiver_rights: &'static [CapabilityRight],
    pub additional_authority_operands: &'static [AuthorityOperandRequirement],
    pub effect: AuthorityOperationEffect,
    pub import_protocol: ImportProtocol,
    pub targets: &'static [TargetDomainId],
}

const fn use_op(
    id: u32,
    name: &'static str,
    receiver: CapabilityKind,
    rights: &'static [CapabilityRight],
    additional: &'static [AuthorityOperandRequirement],
    protocol: ImportProtocol,
    targets: &'static [TargetDomainId],
) -> AuthorityOperationContract {
    AuthorityOperationContract {
        id: HostOperationId::from_raw(id),
        name,
        receiver,
        required_receiver_rights: rights,
        additional_authority_operands: additional,
        effect: AuthorityOperationEffect::Use,
        import_protocol: protocol,
        targets,
    }
}

const fn attenuate_op(
    id: u32,
    name: &'static str,
    receiver: CapabilityKind,
    rights: &'static [CapabilityRight],
    narrowing: ScopeNarrowingKind,
    protocol: ImportProtocol,
    targets: &'static [TargetDomainId],
) -> AuthorityOperationContract {
    AuthorityOperationContract {
        id: HostOperationId::from_raw(id),
        name,
        receiver,
        required_receiver_rights: rights,
        additional_authority_operands: NO_AUTHORITY_OPERANDS,
        effect: AuthorityOperationEffect::Attenuate(narrowing),
        import_protocol: protocol,
        targets,
    }
}

const fn derive_op(
    id: u32,
    name: &'static str,
    receiver: CapabilityKind,
    rights: &'static [CapabilityRight],
    additional: &'static [AuthorityOperandRequirement],
    derivation: DerivationKind,
    result_kind: CapabilityKind,
    result_rights: DerivedRights,
    protocol: ImportProtocol,
    targets: &'static [TargetDomainId],
) -> AuthorityOperationContract {
    AuthorityOperationContract {
        id: HostOperationId::from_raw(id),
        name,
        receiver,
        required_receiver_rights: rights,
        additional_authority_operands: additional,
        effect: AuthorityOperationEffect::Derive(AuthorityDerivation {
            kind: derivation,
            result_kind,
            result_rights,
        }),
        import_protocol: protocol,
        targets,
    }
}

/// The complete receiver-qualified source authority surface: 54 capability
/// methods plus SecretStore.get/require and three Secret operations.
pub const AUTHORITY_OPERATIONS: &[AuthorityOperationContract] = &[
    use_op(
        0x0001_0001,
        "console.print",
        CapabilityKind::Console,
        WRITE,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        CONSOLE_WRITE_TARGETS,
    ),
    use_op(
        0x0001_0002,
        "console.read_line",
        CapabilityKind::Console,
        READ,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingBytes,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x0002_0001,
        "clock.now",
        CapabilityKind::Clock,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x0002_0002,
        "clock.now_monotonic",
        CapabilityKind::Clock,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x0003_0001,
        "rand.rand_u64",
        CapabilityKind::Rand,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE,
    ),
    use_op(
        0x0004_0001,
        "env.get_env",
        CapabilityKind::Env,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::SplitFill,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    attenuate_op(
        0x0004_0002,
        "env.only",
        CapabilityKind::Env,
        NO_RIGHTS,
        ScopeNarrowingKind::EnvOnly,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    derive_op(
        0x0005_0001,
        "secretstore.get",
        CapabilityKind::SecretStore,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        DerivationKind::SecretStoreGet,
        CapabilityKind::Secret,
        DerivedRights::SecretGrantBounded,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    derive_op(
        0x0005_0002,
        "secretstore.require",
        CapabilityKind::SecretStore,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        DerivationKind::SecretStoreRequire,
        CapabilityKind::Secret,
        DerivedRights::SecretGrantBounded,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x0006_0001,
        "crypto.sign",
        CapabilityKind::Secret,
        SEAL,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x0006_0002,
        "crypto.public_key",
        CapabilityKind::Secret,
        SEAL,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x0006_0003,
        "crypto.reveal",
        CapabilityKind::Secret,
        REVEAL,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingBytes,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x0007_0001,
        "exec.exec",
        CapabilityKind::Exec,
        NO_RIGHTS,
        EXEC_DIR_REQUIREMENT,
        ImportProtocol::PendingBytes,
        NATIVE,
    ),
    attenuate_op(
        0x0007_0002,
        "exec.only",
        CapabilityKind::Exec,
        NO_RIGHTS,
        ScopeNarrowingKind::ExecOnly,
        ImportProtocol::Direct,
        NATIVE,
    ),
    use_op(
        0x0008_0001,
        "build_out.write_out",
        CapabilityKind::BuildOut,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        BUILD,
    ),
    use_op(
        0x0009_0001,
        "build_read.read_build",
        CapabilityKind::BuildRead,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingBytes,
        BUILD,
    ),
    use_op(
        0x000a_0001,
        "build_env.get_build_env",
        CapabilityKind::BuildEnv,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::SplitFill,
        BUILD,
    ),
    use_op(
        0x000b_0001,
        "build_net.fetch_build",
        CapabilityKind::BuildNet,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingBytes,
        BUILD,
    ),
    use_op(
        0x000c_0001,
        "build_exec.run_tool",
        CapabilityKind::BuildExec,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingBytes,
        BUILD,
    ),
    use_op(
        0x000d_0001,
        "file.read",
        CapabilityKind::File,
        READ,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingBytes,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000d_0002,
        "file.write",
        CapabilityKind::File,
        WRITE,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    attenuate_op(
        0x000e_0001,
        "dir.only",
        CapabilityKind::Dir,
        READ,
        ScopeNarrowingKind::DirOnly,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_0002,
        "dir.list",
        CapabilityKind::Dir,
        READ,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingList,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_0003,
        "dir.read",
        CapabilityKind::Dir,
        READ,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingBytes,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_0004,
        "dir.exists",
        CapabilityKind::Dir,
        READ,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_0005,
        "dir.is_dir",
        CapabilityKind::Dir,
        READ,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    derive_op(
        0x000e_0006,
        "dir.subtree",
        CapabilityKind::Dir,
        READ,
        NO_AUTHORITY_OPERANDS,
        DerivationKind::DirSubtree,
        CapabilityKind::Dir,
        DerivedRights::PreserveReceiver,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_0007,
        "dir.make_dir",
        CapabilityKind::Dir,
        WRITE,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    derive_op(
        0x000e_0008,
        "dir.read_file",
        CapabilityKind::Dir,
        READ,
        NO_AUTHORITY_OPERANDS,
        DerivationKind::DirReadFile,
        CapabilityKind::File,
        DerivedRights::Exact(READ),
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    derive_op(
        0x000e_0009,
        "dir.write_file",
        CapabilityKind::Dir,
        WRITE,
        NO_AUTHORITY_OPERANDS,
        DerivationKind::DirWriteFile,
        CapabilityKind::File,
        DerivedRights::Exact(WRITE),
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_000a,
        "dir.write",
        CapabilityKind::Dir,
        WRITE,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_000b,
        "dir.append",
        CapabilityKind::Dir,
        WRITE,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_000c,
        "dir.create_new",
        CapabilityKind::Dir,
        WRITE,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_000d,
        "dir.replace",
        CapabilityKind::Dir,
        WRITE,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_000e,
        "dir.rename",
        CapabilityKind::Dir,
        WRITE,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_000f,
        "dir.read_bytes",
        CapabilityKind::Dir,
        READ,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingBytes,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_0010,
        "dir.write_bytes",
        CapabilityKind::Dir,
        WRITE,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_0011,
        "dir.append_bytes",
        CapabilityKind::Dir,
        WRITE,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x000e_0012,
        "dir.set_executable",
        CapabilityKind::Dir,
        WRITE,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    derive_op(
        0x000f_0001,
        "net.connect",
        CapabilityKind::Net,
        CONNECT_TCP,
        NO_AUTHORITY_OPERANDS,
        DerivationKind::NetConnect,
        CapabilityKind::Socket,
        DerivedRights::NoRights,
        ImportProtocol::Direct,
        NATIVE,
    ),
    derive_op(
        0x000f_0003,
        "net.listen",
        CapabilityKind::Net,
        LISTEN_TCP,
        NO_AUTHORITY_OPERANDS,
        DerivationKind::NetListen,
        CapabilityKind::Listener,
        DerivedRights::NoRights,
        ImportProtocol::Direct,
        NATIVE,
    ),
    derive_op(
        0x000f_0004,
        "net.listen_tls",
        CapabilityKind::Net,
        LISTEN_TCP,
        TLS_SECRET_REQUIREMENT,
        DerivationKind::NetListenTls,
        CapabilityKind::Listener,
        DerivedRights::NoRights,
        ImportProtocol::Direct,
        NATIVE,
    ),
    attenuate_op(
        0x000f_0005,
        "net.only",
        CapabilityKind::Net,
        NO_RIGHTS,
        ScopeNarrowingKind::NetOnly,
        ImportProtocol::Direct,
        NATIVE,
    ),
    derive_op(
        0x000f_0006,
        "net.fetch",
        CapabilityKind::Net,
        CONNECT_TCP,
        NO_AUTHORITY_OPERANDS,
        DerivationKind::NetFetch,
        CapabilityKind::Fetch,
        DerivedRights::NoRights,
        ImportProtocol::Direct,
        NATIVE,
    ),
    attenuate_op(
        0x000f_0007,
        "net.deny",
        CapabilityKind::Net,
        NO_RIGHTS,
        ScopeNarrowingKind::NetDeny,
        ImportProtocol::Direct,
        NATIVE,
    ),
    use_op(
        0x000f_0008,
        "net.resolve",
        CapabilityKind::Net,
        CONNECT,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingList,
        NATIVE,
    ),
    derive_op(
        0x000f_0009,
        "net.connect_pinned",
        CapabilityKind::Net,
        CONNECT_TCP,
        NO_AUTHORITY_OPERANDS,
        DerivationKind::NetConnectPinned,
        CapabilityKind::Socket,
        DerivedRights::NoRights,
        ImportProtocol::Direct,
        NATIVE,
    ),
    derive_op(
        0x000f_000a,
        "net.try_connect_pinned",
        CapabilityKind::Net,
        CONNECT_TCP,
        NO_AUTHORITY_OPERANDS,
        DerivationKind::NetTryConnectPinned,
        CapabilityKind::Socket,
        DerivedRights::NoRights,
        ImportProtocol::Direct,
        NATIVE,
    ),
    attenuate_op(
        0x0010_0001,
        "fetch.only",
        CapabilityKind::Fetch,
        NO_RIGHTS,
        ScopeNarrowingKind::FetchOnly,
        ImportProtocol::Direct,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    use_op(
        0x0010_0002,
        "fetch.send_raw",
        CapabilityKind::Fetch,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingBytes,
        NATIVE_AND_BROWSER_PROVIDER,
    ),
    derive_op(
        0x0011_0001,
        "listener.accept",
        CapabilityKind::Listener,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        DerivationKind::ListenerAccept,
        CapabilityKind::Socket,
        DerivedRights::NoRights,
        ImportProtocol::Direct,
        NATIVE,
    ),
    use_op(
        0x0011_0002,
        "listener.serve_pool",
        CapabilityKind::Listener,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE,
    ),
    use_op(
        0x0012_0001,
        "socket.send_line",
        CapabilityKind::Socket,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE,
    ),
    use_op(
        0x0012_0002,
        "socket.send_bytes",
        CapabilityKind::Socket,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE,
    ),
    use_op(
        0x0012_0003,
        "socket.recv_line",
        CapabilityKind::Socket,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingBytes,
        NATIVE,
    ),
    use_op(
        0x0012_0004,
        "socket.recv_all",
        CapabilityKind::Socket,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingBytes,
        NATIVE,
    ),
    use_op(
        0x0012_0005,
        "socket.recv_bytes",
        CapabilityKind::Socket,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::PendingBytes,
        NATIVE,
    ),
    use_op(
        0x0012_0006,
        "socket.close",
        CapabilityKind::Socket,
        NO_RIGHTS,
        NO_AUTHORITY_OPERANDS,
        ImportProtocol::Direct,
        NATIVE,
    ),
];

pub fn authority_operation(id: HostOperationId) -> Option<&'static AuthorityOperationContract> {
    AUTHORITY_OPERATIONS
        .iter()
        .find(|operation| operation.id == id)
}

pub fn authority_operation_named(name: &str) -> Option<&'static AuthorityOperationContract> {
    AUTHORITY_OPERATIONS
        .iter()
        .find(|operation| operation.name == name)
}
