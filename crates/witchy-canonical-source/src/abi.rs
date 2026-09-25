//! Canonical source-level callable ABI identities: the tagged, deterministic
//! description of a checked callable's parameters, result and target policy,
//! and the manifest digest bound into the checked-source witness. The checker
//! derives entries from its typing tables (`witchy_types::canonical_abi`);
//! this module only defines and encodes them (RFC-0170).

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalScalarKind {
    Int,
    Float,
    Duration,
    Bool,
    Unit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalCapabilityKind {
    Console,
    Clock,
    Rand,
    Env,
    Secret,
    Exec,
    Fetch,
    Dir,
    File,
    Net,
    Socket,
    Listener,
    BuildOut,
    BuildRead,
    BuildEnv,
    BuildNet,
    BuildExec,
    SecretStore,
}

/// A structural identity for a checked source type used by a reference or
/// aggregate ABI entry.  It is not a WIR layout identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalTypeKey([u8; 32]);

impl CanonicalTypeKey {
    #[doc(hidden)]
    pub fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalReferenceKind {
    String,
    Bytes,
    Aggregate(CanonicalTypeKey),
    Capability(CanonicalCapabilityKind),
    /// A local rooted-view lane.  The payload key binds the checked payload
    /// identity; the carrier kind says how the retained root/projection is
    /// represented.  This is deliberately not an open, separately compiled
    /// reference ABI.
    RootedView {
        payload: CanonicalTypeKey,
        carrier: CanonicalRootedViewCarrier,
    },
}

/// Physical representation of an authenticated rooted view. The structural
/// payload key binds the exact checked source type within each class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalRootedViewCarrier {
    /// An authenticated scalar place carrying an Int snapshot.
    ScalarPlace,
    /// An immutable scalar copied at view formation.
    InlineScalar,
    /// A retained List object and an authenticated start/length projection.
    RetainedProjection,
    /// A retained immutable String, List, tuple, dictionary, or record root.
    RetainedRoot,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalValueKind {
    Scalar(CanonicalScalarKind),
    Reference(CanonicalReferenceKind),
    Callable(CanonicalCallableAbiKey),
    Value(CanonicalTypeKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalParameterConvention {
    Let,
    Borrow,
    Var,
    Own,
    VarUnique,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalParameterAbi {
    pub convention: CanonicalParameterConvention,
    pub value: CanonicalValueKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalResultKind {
    Scalar(CanonicalScalarKind),
    Reference(CanonicalReferenceKind),
    Value(CanonicalValueKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalCallableQualifiers {
    pub pure: bool,
    pub once: bool,
}

/// A source-structural local target.  The ordinal is assigned only from the
/// final checked function sequence, never from a function spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalLocalTarget {
    pub function_ordinal: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalTargetPolicy {
    ClosedLocal(CanonicalLocalTarget),
    OpenAbi,
}

/// Hash identity of a complete source-level callable ABI shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalCallableAbiKey([u8; 32]);

impl CanonicalCallableAbiKey {
    #[doc(hidden)]
    pub fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalCallableAbi {
    pub key: CanonicalCallableAbiKey,
    pub parameters: Vec<CanonicalParameterAbi>,
    pub result: CanonicalResultKind,
    pub qualifiers: CanonicalCallableQualifiers,
    pub target_policy: CanonicalTargetPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalCallableAbiManifest {
    entries: Vec<CanonicalCallableAbi>,
    digest: [u8; 32],
}

impl CanonicalCallableAbiManifest {
    /// The manifest of a checked source whose runtime declarations could not
    /// be encoded (an RFC-0165 obligation recorded by the pipeline). It binds
    /// no callable and its digest is the digest of no entries, so nothing can
    /// mistake it for a derived manifest of the same program.
    pub fn empty() -> Self {
        let entries = Vec::new();
        let digest = manifest_digest(&entries);
        Self { entries, digest }
    }

    /// The manifest of exactly these entries, in the order given; the checker
    /// supplies them sorted.
    pub fn from_entries(entries: Vec<CanonicalCallableAbi>) -> Self {
        let digest = manifest_digest(&entries);
        Self { entries, digest }
    }

    pub fn entries(&self) -> &[CanonicalCallableAbi] {
        &self.entries
    }

    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalAbiError {
    NonConcrete(String),
    Unsupported(String),
    InvalidCallable(String),
}

impl fmt::Display for CanonicalAbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonConcrete(context) => write!(f, "canonical callable ABI is not concrete: {context}"),
            Self::Unsupported(context) => write!(f, "canonical callable ABI has unsupported type: {context}"),
            Self::InvalidCallable(context) => write!(f, "canonical callable ABI is invalid: {context}"),
        }
    }
}

impl std::error::Error for CanonicalAbiError {}

#[doc(hidden)]
pub fn encode_callable_parts(parameters: &[CanonicalParameterAbi], result: &CanonicalResultKind, qualifiers: CanonicalCallableQualifiers) -> Vec<u8> {
    let mut bytes = b"witchy.canonical-callable-abi.v1".to_vec();
    put_len(&mut bytes, parameters.len());
    for parameter in parameters {
        bytes.push(match parameter.convention { CanonicalParameterConvention::Let => 0, CanonicalParameterConvention::Borrow => 1, CanonicalParameterConvention::Var => 2, CanonicalParameterConvention::Own => 3, CanonicalParameterConvention::VarUnique => 4 });
        encode_value(&mut bytes, &parameter.value);
    }
    encode_result(&mut bytes, result);
    bytes.push(u8::from(qualifiers.pure));
    bytes.push(u8::from(qualifiers.once));
    bytes
}

fn encode_value(bytes: &mut Vec<u8>, value: &CanonicalValueKind) {
    match value {
        CanonicalValueKind::Scalar(kind) => { bytes.push(0); bytes.push(*kind as u8); }
        CanonicalValueKind::Reference(kind) => { bytes.push(1); encode_reference(bytes, kind); }
        CanonicalValueKind::Callable(key) => { bytes.push(2); bytes.extend_from_slice(&key.0); }
        CanonicalValueKind::Value(key) => { bytes.push(3); bytes.extend_from_slice(&key.0); }
    }
}

fn encode_result(bytes: &mut Vec<u8>, result: &CanonicalResultKind) {
    match result {
        CanonicalResultKind::Scalar(kind) => { bytes.push(0); bytes.push(*kind as u8); }
        CanonicalResultKind::Reference(kind) => { bytes.push(1); encode_reference(bytes, kind); }
        CanonicalResultKind::Value(value) => { bytes.push(2); encode_value(bytes, value); }
    }
}

fn encode_reference(bytes: &mut Vec<u8>, kind: &CanonicalReferenceKind) {
    match kind {
        CanonicalReferenceKind::String => bytes.push(0),
        CanonicalReferenceKind::Bytes => bytes.push(1),
        CanonicalReferenceKind::Aggregate(key) => { bytes.push(2); bytes.extend_from_slice(&key.0); }
        CanonicalReferenceKind::Capability(kind) => { bytes.push(3); bytes.push(*kind as u8); }
        CanonicalReferenceKind::RootedView { payload, carrier } => {
            bytes.push(4);
            bytes.extend_from_slice(&payload.0);
            bytes.push(*carrier as u8);
        }
    }
}

fn manifest_digest(entries: &[CanonicalCallableAbi]) -> [u8; 32] {
    let mut bytes = b"witchy.canonical-callable-abi-manifest.v1".to_vec();
    put_len(&mut bytes, entries.len());
    for entry in entries {
        bytes.extend_from_slice(&entry.key.0);
        let policy = match entry.target_policy { CanonicalTargetPolicy::ClosedLocal(target) => { bytes.push(0); target.function_ordinal } CanonicalTargetPolicy::OpenAbi => { bytes.push(1); 0 } };
        bytes.extend_from_slice(&policy.to_le_bytes());
    }
    *blake3::hash(&bytes).as_bytes()
}

#[doc(hidden)]
pub fn put_len(bytes: &mut Vec<u8>, length: usize) {
    bytes.extend_from_slice(&(u32::try_from(length).expect("validated canonical ABI length")).to_le_bytes());
}
