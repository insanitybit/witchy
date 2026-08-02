//! Descriptor transport for artifacts, workers, and structured host boundaries.
//!
//! The bundle carries the canonical descriptor bytes themselves, not a second
//! reconstruction of their fields. Import therefore reuses [`LayoutInterner`]
//! validation and fails before a boundary can observe an unknown schema,
//! digest, dependency, or non-canonical encoding.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::{LAYOUT_SCHEMA_VERSION, LayoutError, LayoutId, LayoutInterner, LayoutKind};

const BUNDLE_MAGIC: &[u8; 4] = b"WLAB";
const BUNDLE_VERSION: u32 = 1;
const MAX_BUNDLE_DESCRIPTORS: usize = 1 << 20;
const MAX_DESCRIPTOR_BYTES: usize = 1 << 24;

/// A canonical, dependency-ordered layout graph plus the layouts used at one
/// artifact or host boundary. Roots are sorted and deduplicated so equivalent
/// contracts have byte-identical metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutBundle {
    descriptors: Vec<(LayoutId, Vec<u8>)>,
    roots: Vec<LayoutId>,
}

impl LayoutBundle {
    pub fn from_interner(
        interner: &LayoutInterner,
        roots: impl IntoIterator<Item = LayoutId>,
    ) -> Result<Self, LayoutTransportError> {
        let roots = roots.into_iter().collect::<BTreeSet<_>>();
        if roots.len() > MAX_BUNDLE_DESCRIPTORS {
            return Err(LayoutTransportError::Invalid("layout bundle root limit exceeded"));
        }
        let mut ordered = Vec::new();
        let mut visited = BTreeSet::new();
        let mut active = BTreeSet::new();
        for root in &roots {
            visit(*root, interner, &mut visited, &mut active, &mut ordered)?;
        }
        if ordered.len() > MAX_BUNDLE_DESCRIPTORS {
            return Err(LayoutTransportError::Invalid(
                "layout bundle descriptor limit exceeded",
            ));
        }
        if ordered.iter().any(|(_, bytes)| bytes.len() > MAX_DESCRIPTOR_BYTES) {
            return Err(LayoutTransportError::Invalid(
                "layout descriptor size limit exceeded",
            ));
        }
        Ok(Self { descriptors: ordered, roots: roots.into_iter().collect() })
    }

    pub fn roots(&self) -> &[LayoutId] {
        &self.roots
    }

    pub fn descriptors(&self) -> impl Iterator<Item = (LayoutId, &[u8])> {
        self.descriptors.iter().map(|(id, bytes)| (*id, bytes.as_slice()))
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(BUNDLE_MAGIC);
        push_u32(&mut bytes, BUNDLE_VERSION);
        push_u32(&mut bytes, LAYOUT_SCHEMA_VERSION);
        push_u32(
            &mut bytes,
            u32::try_from(self.descriptors.len()).expect("validated descriptor count fits u32"),
        );
        for (id, descriptor) in &self.descriptors {
            bytes.extend_from_slice(id.as_bytes());
            push_u32(
                &mut bytes,
                u32::try_from(descriptor.len()).expect("validated descriptor size fits u32"),
            );
            bytes.extend_from_slice(descriptor);
        }
        push_u32(
            &mut bytes,
            u32::try_from(self.roots.len()).expect("validated root count fits u32"),
        );
        for root in &self.roots {
            bytes.extend_from_slice(root.as_bytes());
        }
        bytes
    }

    /// Decode and validate a bundle into a fresh interner. Descriptors must be
    /// dependency-first; this prevents an artifact from relying on lookup order
    /// or lazily accepting a dangling layout reference.
    pub fn decode_canonical(
        bytes: &[u8],
    ) -> Result<(Self, LayoutInterner), LayoutTransportError> {
        let mut decoder = Decoder::new(bytes);
        if decoder.take(BUNDLE_MAGIC.len())? != BUNDLE_MAGIC {
            return Err(LayoutTransportError::Invalid("bad layout bundle magic"));
        }
        let version = decoder.u32()?;
        if version != BUNDLE_VERSION {
            return Err(LayoutTransportError::UnsupportedBundleVersion(version));
        }
        let schema = decoder.u32()?;
        if schema != LAYOUT_SCHEMA_VERSION {
            return Err(LayoutTransportError::UnsupportedLayoutSchema(schema));
        }
        let descriptor_count = decoder.count(MAX_BUNDLE_DESCRIPTORS)?;
        let mut interner = LayoutInterner::new();
        let mut ids = BTreeSet::new();
        for _ in 0..descriptor_count {
            let id = decoder.id()?;
            if !ids.insert(id) {
                return Err(LayoutTransportError::DuplicateDescriptor(id));
            }
            let length = decoder.count(MAX_DESCRIPTOR_BYTES)?;
            let descriptor = decoder.take(length)?.to_vec();
            interner.import_canonical(id, &descriptor)?;
        }
        let root_count = decoder.count(MAX_BUNDLE_DESCRIPTORS)?;
        let mut roots = Vec::with_capacity(root_count);
        for _ in 0..root_count {
            roots.push(decoder.id()?);
        }
        if !decoder.is_done() {
            return Err(LayoutTransportError::Invalid("trailing layout bundle bytes"));
        }
        if !roots.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(LayoutTransportError::Invalid(
                "layout bundle roots are not strictly sorted",
            ));
        }
        for root in &roots {
            if interner.get(*root).is_none() {
                return Err(LayoutTransportError::UnknownRoot(*root));
            }
        }
        let canonical = Self::from_interner(&interner, roots.iter().copied())?;
        if canonical.canonical_bytes() != bytes {
            return Err(LayoutTransportError::Invalid("non-canonical layout bundle encoding"));
        }
        Ok((canonical, interner))
    }
}

/// The only three outcomes permitted at a specialized structured host
/// boundary. `Marshal` remains explicit and countable; opt callers can reject
/// it rather than silently changing representation.
#[must_use = "a host layout decision must be emitted exactly, accounted as a marshal, or rejected"]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostLayoutDecision {
    Exact,
    Marshal {
        accepted: LayoutId,
        metric: HostMarshalMetric,
    },
    Reject,
}

/// The mandatory accounting attached to a registered marshal adapter.
///
/// A policy cannot authorize a reshape without naming the production counter
/// the adapter increments. The first host-boundary slice exposes only the
/// RFC-0111 reshaped-byte metric; additional accounting must be added here
/// before a new adapter can claim it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostMarshalMetric {
    ReshapedBytes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLayoutPolicy {
    exact: BTreeSet<LayoutId>,
    marshal: BTreeMap<LayoutId, (LayoutId, HostMarshalMetric)>,
}

impl HostLayoutPolicy {
    pub fn new(exact: impl IntoIterator<Item = LayoutId>) -> Self {
        Self { exact: exact.into_iter().collect(), marshal: BTreeMap::new() }
    }

    /// Register one checked, counted marshal adapter from the requested guest
    /// layout to the descriptor the host adapter actually consumes. Unknown
    /// layouts stay rejected; a policy cannot claim that an arbitrary
    /// descriptor is convertible without naming the exact accepted target and
    /// the counter the adapter increments. The target must also appear in this
    /// policy's exact set or the decision fails closed.
    pub fn with_counted_marshal(
        mut self,
        requested: LayoutId,
        accepted: LayoutId,
        metric: HostMarshalMetric,
    ) -> Self {
        self.marshal.insert(requested, (accepted, metric));
        self
    }

    /// Resolve a boundary only against the validated descriptor interner that
    /// produced the WIR layout. A naked digest is not authority to select a
    /// scalar-memory adapter: unknown IDs, including IDs fabricated for a
    /// capability/reference shape that the interner rejects, fail closed.
    pub fn decide(
        &self,
        layouts: &LayoutInterner,
        requested: LayoutId,
    ) -> HostLayoutDecision {
        if !scalar_storage_layout(layouts, requested) {
            return HostLayoutDecision::Reject;
        }
        if self.exact.contains(&requested) {
            HostLayoutDecision::Exact
        } else if let Some((accepted, metric)) = self
            .marshal
            .get(&requested)
            .filter(|(accepted, _)| {
                self.exact.contains(accepted) && scalar_storage_layout(layouts, *accepted)
            })
        {
            HostLayoutDecision::Marshal {
                accepted: *accepted,
                metric: *metric,
            }
        } else {
            HostLayoutDecision::Reject
        }
    }
}

/// Only the closed scalar-memory descriptor family may select these adapters.
/// Keeping this match exhaustive makes a future reference/capability layout
/// variant a compile-time integration task instead of silently authorizing it.
fn scalar_storage_layout(layouts: &LayoutInterner, id: LayoutId) -> bool {
    match layouts.get(id).map(|descriptor| descriptor.kind()) {
        Some(
            LayoutKind::Scalar(_)
            | LayoutKind::Tuple { .. }
            | LayoutKind::PackedRecord { .. }
            | LayoutKind::PackedList { .. }
            | LayoutKind::ClosedSum { .. },
        ) => true,
        None => false,
    }
}

impl Default for HostLayoutPolicy {
    fn default() -> Self {
        Self::new([])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutTransportError {
    Layout(LayoutError),
    UnsupportedBundleVersion(u32),
    UnsupportedLayoutSchema(u32),
    DuplicateDescriptor(LayoutId),
    UnknownRoot(LayoutId),
    Invalid(&'static str),
}

impl fmt::Display for LayoutTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Layout(error) => write!(formatter, "{error}"),
            Self::UnsupportedBundleVersion(version) => {
                write!(formatter, "unsupported layout bundle version {version}")
            }
            Self::UnsupportedLayoutSchema(version) => {
                write!(formatter, "unsupported layout schema {version}")
            }
            Self::DuplicateDescriptor(id) => write!(formatter, "duplicate layout `{id}`"),
            Self::UnknownRoot(id) => write!(formatter, "unknown layout root `{id}`"),
            Self::Invalid(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for LayoutTransportError {}

impl From<LayoutError> for LayoutTransportError {
    fn from(error: LayoutError) -> Self {
        Self::Layout(error)
    }
}

fn visit(
    id: LayoutId,
    interner: &LayoutInterner,
    visited: &mut BTreeSet<LayoutId>,
    active: &mut BTreeSet<LayoutId>,
    ordered: &mut Vec<(LayoutId, Vec<u8>)>,
) -> Result<(), LayoutTransportError> {
    if visited.contains(&id) {
        return Ok(());
    }
    let descriptor = interner.get(id).ok_or(LayoutTransportError::UnknownRoot(id))?;
    if !active.insert(id) {
        return Err(LayoutTransportError::Invalid("cyclic layout descriptor graph"));
    }
    let mut children = children(descriptor.kind());
    children.sort_unstable();
    children.dedup();
    for child in children {
        visit(child, interner, visited, active, ordered)?;
    }
    active.remove(&id);
    visited.insert(id);
    ordered.push((id, descriptor.canonical_bytes()));
    Ok(())
}

fn children(kind: &LayoutKind) -> Vec<LayoutId> {
    match kind {
        LayoutKind::Scalar(_) => Vec::new(),
        LayoutKind::Tuple { fields } | LayoutKind::PackedRecord { fields } => fields.clone(),
        LayoutKind::PackedList { element, .. } => vec![*element],
        LayoutKind::ClosedSum { variants } => variants.iter().flatten().copied().collect(),
    }
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], LayoutTransportError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(LayoutTransportError::Invalid("layout bundle length overflow"))?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(LayoutTransportError::Invalid("truncated layout bundle"))?;
        self.offset = end;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32, LayoutTransportError> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| LayoutTransportError::Invalid("truncated layout bundle integer"))?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn count(&mut self, maximum: usize) -> Result<usize, LayoutTransportError> {
        let value = usize::try_from(self.u32()?)
            .map_err(|_| LayoutTransportError::Invalid("layout bundle count overflow"))?;
        if value > maximum {
            return Err(LayoutTransportError::Invalid("layout bundle limit exceeded"));
        }
        Ok(value)
    }

    fn id(&mut self) -> Result<LayoutId, LayoutTransportError> {
        let bytes: [u8; 32] = self
            .take(32)?
            .try_into()
            .map_err(|_| LayoutTransportError::Invalid("truncated layout id"))?;
        Ok(LayoutId::from_bytes(bytes))
    }

    fn is_done(&self) -> bool {
        self.offset == self.bytes.len()
    }
}
