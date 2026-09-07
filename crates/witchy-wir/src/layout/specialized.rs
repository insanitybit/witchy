//! RFC-0111 canonical layouts for closed specialized values.
//!
//! Logical type resolution stays upstream. WIR consumes a typed resolver and
//! walks the actual resolved AST fields itself, so callers never provide child
//! layout ids for nominal fields. Descriptors are immutable products of that
//! walk or of the validated canonical artifact decoder.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use witchy_syntax::ast::{effective_type_def_params, Type, TypeDef, TypeQual};

const LAYOUT_MAGIC: &[u8; 4] = b"WLAY";
const MAX_DESCRIPTOR_ITEMS: usize = 1 << 20;

/// Canonical descriptor encoding version. Changing an existing tag or its
/// semantics increments this number. Additive kinds use an unassigned tag so
/// canonical bytes and `LayoutId`s for the existing RFC-0111 graph stay exact.
pub const LAYOUT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayoutId([u8; 32]);

impl LayoutId {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        let mut result = String::with_capacity(64);
        for byte in self.0 {
            use fmt::Write as _;
            write!(&mut result, "{byte:02x}").expect("writing to String cannot fail");
        }
        result
    }
}

impl fmt::Debug for LayoutId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "LayoutId({})", self.to_hex())
    }
}

impl fmt::Display for LayoutId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// The physical-layout portion of a callable signature.
///
/// `None` means the value uses its ordinary scalar/reference ABI. `Some(id)`
/// means callers and callees must agree on the exact canonical descriptor
/// before the logical signature is considered link-compatible. Ownership and
/// access remain separate facts (RFC-0110); later first-class-call lowering can
/// pair this value with the access signature without reconstructing either.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CallableLayoutSignature {
    parameters: Vec<Option<LayoutId>>,
    result: Option<LayoutId>,
}

impl CallableLayoutSignature {
    pub fn new(parameters: Vec<Option<LayoutId>>, result: Option<LayoutId>) -> Self {
        Self { parameters, result }
    }

    pub fn parameters(&self) -> &[Option<LayoutId>] {
        &self.parameters
    }

    pub fn result(&self) -> Option<LayoutId> {
        self.result
    }

    pub fn has_specialized_layout(&self) -> bool {
        self.result.is_some() || self.parameters.iter().any(Option::is_some)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarKind {
    Bool,
    Int,
    Float,
    Duration,
    U32,
    Tag8,
    Tag16,
    Tag32,
    /// An untyped byte lane. Unlike `Bool` and the tag kinds, every bit pattern
    /// is valid and loading it grants no higher-level value authority.
    RawByte,
    /// The widened eight-byte carrier used by the ordinary Bool-list ABI.
    ///
    /// This is representation-only: it is not a source Bool (whose canonical
    /// scalar layout is one byte), and its identity grants no Bool value-range
    /// authority by itself.
    BoolValueSlot,
    /// One opaque 128-bit SIMD lane. Its bytes carry no reference authority.
    V128,
    /// The four-byte lane that carries a function-table index: slot 0 of a
    /// closure object. It is an ordinary lane -- a plain typed load and store
    /// reach it -- but its value type is not derivable from the lane alone:
    /// the module's closure-layout registry names the signature the index
    /// must have. It is never a list element in v1.
    Callable,
}

impl ScalarKind {
    /// Whether ordinary lowering may load or store this scalar directly.
    ///
    /// The RFC-0162 representation-only kinds answer `false`: they are
    /// described by SafeWIR and reached through the checked place builder,
    /// never by a plain typed load. Exhaustive on purpose, so a new scalar
    /// must decide rather than inherit `true`.
    pub fn is_ordinary_lowering_target(self) -> bool {
        match self {
            Self::Bool
            | Self::Int
            | Self::Float
            | Self::Duration
            | Self::U32
            | Self::Tag8
            | Self::Tag16
            | Self::Tag32
            | Self::Callable => true,
            Self::RawByte | Self::BoolValueSlot | Self::V128 => false,
        }
    }

    pub const fn size(self) -> u32 {
        match self {
            Self::Bool | Self::Tag8 | Self::RawByte => 1,
            Self::Tag16 => 2,
            Self::U32 | Self::Tag32 | Self::Callable => 4,
            Self::Int | Self::Float | Self::Duration | Self::BoolValueSlot => 8,
            Self::V128 => 16,
        }
    }

    pub const fn alignment(self) -> u32 {
        self.size()
    }

    const fn is_source_value(self) -> bool {
        matches!(self, Self::Bool | Self::Int | Self::Float | Self::Duration)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutSize {
    Fixed(u32),
    Dynamic { base: u32, stride: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Scalar(ScalarKind),
    Inline(LayoutId),
    Pointer {
        pointee: LayoutId,
        slot: PointerSlot,
    },
}

/// The representation contract carried by a linear-memory pointer slot.
///
/// This is descriptor metadata, not validator state: even `Owned` must be
/// authenticated against a live object and ownership fact before use. `Raw`
/// deliberately cannot establish object provenance or ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PointerSlot {
    Owned,
    Borrowed,
    Raw,
}

impl PointerSlot {
    pub const fn may_establish_object_provenance(self) -> bool {
        matches!(self, Self::Owned | Self::Borrowed)
    }

    pub const fn is_owning(self) -> bool {
        matches!(self, Self::Owned)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutField {
    offset: u32,
    kind: FieldKind,
}

impl LayoutField {
    /// Allocation-relative byte offset. For an exposed-root descriptor this is
    /// never relative to the exposed runtime handle, so prefix fields retain a
    /// nonnegative range even when they are observed at `handle - n`.
    pub fn offset(&self) -> u32 {
        self.offset
    }

    pub fn kind(&self) -> FieldKind {
        self.kind
    }
}

/// Canonical allocation-relative metadata for a closed-sum discriminant.
///
/// Values are minted only by [`LayoutInterner::closed_sum_tag_metadata`] after
/// the complete descriptor has reproduced from its authenticated kind graph.
/// Callers cannot substitute a tag width, scalar kind, or offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosedSumTagMetadata {
    layout: LayoutId,
    offset: u32,
    width: u32,
    kind: FieldKind,
}

impl ClosedSumTagMetadata {
    pub fn layout(&self) -> LayoutId {
        self.layout
    }

    pub fn offset(&self) -> u32 {
        self.offset
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn kind(&self) -> FieldKind {
        self.kind
    }
}

/// Canonical allocation-relative metadata for one selected sum field.
///
/// `child_layout` is the exact inline child for [`LayoutKind::ClosedSum`] and
/// the exact widened value/pointer carrier for
/// [`LayoutKind::RuntimeClosedSum`]. The parent and ordinals remain attached so
/// metadata from another sum or variant cannot be confused at later sealed
/// boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosedSumVariantFieldMetadata {
    layout: LayoutId,
    variant: usize,
    field: usize,
    child_layout: LayoutId,
    offset: u32,
    width: u32,
    kind: FieldKind,
}

impl ClosedSumVariantFieldMetadata {
    pub fn layout(&self) -> LayoutId {
        self.layout
    }

    pub fn variant(&self) -> usize {
        self.variant
    }

    pub fn field(&self) -> usize {
        self.field
    }

    pub fn child_layout(&self) -> LayoutId {
        self.child_layout
    }

    pub fn offset(&self) -> u32 {
        self.offset
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn kind(&self) -> FieldKind {
        self.kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantLayout {
    fields: Vec<LayoutField>,
    payload_size: u32,
    payload_alignment: u32,
}

impl VariantLayout {
    pub fn fields(&self) -> &[LayoutField] {
        &self.fields
    }

    pub fn payload_size(&self) -> u32 {
        self.payload_size
    }

    pub fn payload_alignment(&self) -> u32 {
        self.payload_alignment
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RcHeader {
    Required,
    Elided,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderLayout {
    None,
    PackedList {
        rc: RcHeader,
        length_offset: u32,
        capacity_offset: u32,
        data_offset: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipPosition {
    RootBuffer,
    Field(Vec<u32>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationShape {
    None,
    Scalar(ScalarKind),
    Fields(Vec<LayoutId>),
    Variants {
        tag: ScalarKind,
        variants: Vec<Vec<LayoutId>>,
    },
    PackedElements {
        element: LayoutId,
        stride: u32,
    },
}

/// One checked slot in front of an exposed runtime handle. Callers select only
/// the scalar or exact pointee identity; physical offsets, padding, and the
/// resulting anchor are derived by the interner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePrefixSlot {
    Scalar(ScalarKind),
    Pointer {
        pointee: LayoutId,
        semantics: PointerSlot,
    },
}

/// One section of a section-major layout.
///
/// Byte base is `constant_base + count_multiple * count`; slot `i` within the
/// section is `element_width * i` beyond that. Every field is a descriptor fact,
/// so a projection derives an address from these rather than from caller
/// arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionSpec {
    pub constant_base: u32,
    pub count_multiple: u32,
    pub element_width: u32,
}

/// Section ordinals for the Swiss carrier, in physical order.
pub const CARRIER_CONTROL: u32 = 0;
pub const CARRIER_CONTROL_MIRROR: u32 = 1;
pub const CARRIER_ENTRY_INDEX: u32 = 2;
pub const CARRIER_KEY: u32 = 3;
pub const CARRIER_VALUE: u32 = 4;
pub const CARRIER_ORDER: u32 = 5;

/// The carrier is `[u32 slots][ctrl: slots + 16][index: slots * 4]
/// [keys: slots * 8][values: slots * 8][order: slots * 4]`.
///
/// The control region carries a sixteen-byte mirrored tail so a SIMD group load
/// at any slot stays in bounds; the mirror is addressed as its own section
/// because its base is `4 + slots` rather than `4`.
const RUNTIME_DICTIONARY_SECTIONS: &[SectionSpec] = &[
    // control bytes: 4 + 1 * i
    SectionSpec { constant_base: 4, count_multiple: 0, element_width: 1 },
    // mirrored control tail: 4 + slots + i
    SectionSpec { constant_base: 4, count_multiple: 1, element_width: 1 },
    // entry indices: 20 + slots + 4 * i
    SectionSpec { constant_base: 20, count_multiple: 1, element_width: 4 },
    // key slots: 20 + slots * 5 + 8 * i
    SectionSpec { constant_base: 20, count_multiple: 5, element_width: 8 },
    // value slots: 20 + slots * 13 + 8 * i
    SectionSpec { constant_base: 20, count_multiple: 13, element_width: 8 },
    // insertion order: 20 + slots * 21 + 4 * i
    SectionSpec { constant_base: 20, count_multiple: 21, element_width: 4 },
];

/// Where the runtime-visible object handle lies within its allocation.
/// Descriptor fields always remain relative to the allocation base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootAnchor {
    AllocationBase,
    Exposed { allocation_offset: u32 },
}

impl RootAnchor {
    pub const fn allocation_offset(self) -> u32 {
        match self {
            Self::AllocationBase => 0,
            Self::Exposed { allocation_offset } => allocation_offset,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationShapes {
    copy: OperationShape,
    duplicate: OperationShape,
    drop: OperationShape,
    equality: OperationShape,
    render: OperationShape,
    serialization: OperationShape,
}

impl OperationShapes {
    pub fn copy(&self) -> &OperationShape {
        &self.copy
    }

    pub fn duplicate(&self) -> &OperationShape {
        &self.duplicate
    }

    pub fn drop(&self) -> &OperationShape {
        &self.drop
    }

    pub fn equality(&self) -> &OperationShape {
        &self.equality
    }

    pub fn render(&self) -> &OperationShape {
        &self.render
    }

    pub fn serialization(&self) -> &OperationShape {
        &self.serialization
    }
}

/// The minimal child identity needed to reconstruct every derived descriptor
/// invariant. Offsets, padding, operation shapes, and headers are not supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutKind {
    Scalar(ScalarKind),
    Tuple { fields: Vec<LayoutId> },
    PackedRecord { fields: Vec<LayoutId> },
    PackedList { element: LayoutId, rc: RcHeader },
    ClosedSum { variants: Vec<Vec<LayoutId>> },
    /// A packed `[length: u32][elements...]` runtime buffer. The payload starts
    /// at byte four without inserting natural-alignment padding; this describes
    /// both legacy byte buffers and legacy eight-byte value-slot lists.
    RuntimeBuffer { element: LayoutId },
    /// Headerless byte storage returned only by the authenticated scratch
    /// kernel. The payload begins at the object root and has exact extent
    /// `count * 1`; it is deliberately distinct from `[length][payload]`.
    ScratchBuffer { element: LayoutId },
    /// A packed `[length: u32][widened pointer slots...]` runtime buffer.
    /// Each pointer occupies the low four bytes of one eight-byte value slot;
    /// the remaining bytes are representation padding and grant no authority.
    RuntimePointerBuffer {
        pointee: LayoutId,
        semantics: PointerSlot,
    },
    /// The dictionary's exposed ordered body: `[count: u32][key, value rows...]`.
    /// Key and value identities are exact eight-byte slot carriers and the row
    /// stride is fixed by the runtime ABI rather than supplied by callers.
    RuntimeDictionaryBody {
        key: LayoutId,
        value: LayoutId,
        /// The exact row identity, so an indexed projection has an element to
        /// resolve through. Minted by `intern_runtime_dictionary_body`; callers
        /// do not supply it, and it is part of the canonical identity because a
        /// row's shape is what an `Element` step means here.
        row: LayoutId,
    },
    /// One dense dictionary row: `[key slot][value slot]`, sixteen bytes.
    ///
    /// Alignment is FOUR, not the eight its slots suggest. Rows begin at the
    /// body's base of four and step by sixteen, so a row address is 4-aligned;
    /// claiming eight would misstate the ABI. `RuntimeDictionaryBody` already
    /// makes exactly this call, and this follows it.
    RuntimeDictionaryRow {
        key: LayoutId,
        value: LayoutId,
    },
    /// The non-owning Swiss acceleration carrier reached through the raw hidden
    /// dictionary prefix pointer. Its exact extent is `20 + 25 * slot_count`;
    /// segment projections remain a named runtime-catalog concern.
    RuntimeDictionaryIndex {
        key: LayoutId,
        value: LayoutId,
    },
    /// One exact source scalar carried in the legacy eight-byte aggregate lane.
    /// The child identity distinguishes same-width values such as `Int` and
    /// `Float`; the descriptor owns the widened physical lane.
    RuntimeValueSlot {
        value: LayoutId,
    },
    /// One exact linear-memory pointer carried in the low four bytes of the
    /// legacy eight-byte aggregate lane. The high four bytes are padding and
    /// grant no authority.
    RuntimePointerValueSlot {
        pointee: LayoutId,
        semantics: PointerSlot,
    },
    /// The ordinary slot-backed sum ABI: `[tag: u32][eight-byte lanes...]`.
    /// Every non-empty variant child is one canonical runtime value-slot
    /// descriptor, so payload identity and pointer ownership survive erasure.
    RuntimeClosedSum {
        variants: Vec<Vec<LayoutId>>,
    },
    /// Ordered typed captures for the Wasm-GC closure environment.
    ///
    /// This is an identity descriptor, not linear-memory storage. Captures are
    /// canonical runtime value-slot descriptors, but this descriptor has no
    /// byte fields or byte extent because the encoder emits a typed GC struct.
    ClosureEnvironment {
        captures: Vec<LayoutId>,
    },
    /// A checked allocation prefix followed by an exposed runtime object. The
    /// prefix slots are packed canonically and the body starts at the derived
    /// [`RootAnchor`]; no signed or caller-authored field offset is accepted.
    ExposedRoot {
        body: LayoutId,
        prefix: Vec<RuntimePrefixSlot>,
        anchor_bias: u32,
    },
}

impl LayoutKind {
    /// Whether an ordinary inline field carrying this layout may be loaded,
    /// stored, or byte-copied by the pre-SafeWIR lowering paths.
    ///
    /// The RFC-0162 runtime layouts answer `false`. They are reachable as
    /// `FieldKind::Inline` children -- a runtime value slot carries its payload
    /// that way -- so a consumer that refuses only `FieldKind::Pointer` and the
    /// representation-only scalars would still see an ordinary inline field and
    /// emit a plain load, store, or `memory.copy` of the wrong shape. Every
    /// such consumer asks this first and declines.
    ///
    /// Exhaustive on purpose: a new layout kind must decide whether ordinary
    /// lowering may touch it rather than inherit `true`.
    /// The element layout of a dynamically sized, packed element sequence.
    ///
    /// `PackedList` and `RuntimeBuffer` are the same physical shape -- a length
    /// word followed by `stride`-sized elements -- and every consumer that
    /// indexes one must treat them alike. Returning the element from a single
    /// place keeps the builder's projection guard and the validator's Element
    /// and Slice arms from drifting apart, which is how one of them would come
    /// to accept a shape the other rejects.
    pub fn packed_element_layout(&self) -> Option<LayoutId> {
        match self {
            Self::PackedList { element, .. } | Self::RuntimeBuffer { element } => Some(*element),
            // A dense body is a packed run of equal-stride rows, so it indexes
            // like any other packed sequence; the row carries the key/value
            // offsets a following `Field` step reads.
            Self::RuntimeDictionaryBody { row, .. } => Some(*row),
            _ => None,
        }
    }

    /// The section table of a SECTION-MAJOR layout, or `None`.
    ///
    /// A packed sequence puts every field of element `i` together, so one stride
    /// and one base describe it. A section-major layout is the transpose: it
    /// stores all of one field, then all of the next. The Swiss carrier is that
    /// shape -- every control byte, then every entry index, then every key slot --
    /// so a section's base is a function of the ELEMENT COUNT rather than a
    /// descriptor constant, and no `Element`/`Field` chain can name a slot in it.
    ///
    /// Each section's byte base is `constant_base + count_multiple * count`, and
    /// slot `i` within it sits at `element_width * i` from there. The carrier's
    /// five sections plus its mirrored control tail therefore sum to the
    /// descriptor's own `Dynamic { base: 20, stride: 25 }`: the constants agree
    /// with the size, which is what makes this a derivation and not a second
    /// opinion.
    pub fn sections(&self) -> Option<&'static [SectionSpec]> {
        match self {
            Self::RuntimeDictionaryIndex { .. } => Some(RUNTIME_DICTIONARY_SECTIONS),
            _ => None,
        }
    }

    pub fn is_ordinary_inline_target(&self) -> bool {
        match self {
            Self::Scalar(scalar) => scalar.is_ordinary_lowering_target(),
            Self::Tuple { .. }
            | Self::PackedRecord { .. }
            | Self::PackedList { .. }
            | Self::ClosedSum { .. } => true,
            Self::RuntimeBuffer { .. }
            | Self::ScratchBuffer { .. }
            | Self::RuntimePointerBuffer { .. }
            | Self::RuntimeDictionaryBody { .. }
            | Self::RuntimeDictionaryRow { .. }
            | Self::RuntimeDictionaryIndex { .. }
            | Self::RuntimeValueSlot { .. }
            | Self::RuntimePointerValueSlot { .. }
            | Self::RuntimeClosedSum { .. }
            | Self::ClosureEnvironment { .. }
            | Self::ExposedRoot { .. } => false,
        }
    }
}

/// An immutable validated physical descriptor. There is no public field or raw
/// constructor; values come from [`LayoutInterner::intern_type`] or the checked
/// canonical decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutDescriptor {
    schema_version: u32,
    kind: LayoutKind,
    size: LayoutSize,
    alignment: u32,
    fields: Vec<LayoutField>,
    variants: Vec<VariantLayout>,
    ownership: Vec<OwnershipPosition>,
    header: HeaderLayout,
    anchor: RootAnchor,
    operations: OperationShapes,
}

impl LayoutDescriptor {
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn kind(&self) -> &LayoutKind {
        &self.kind
    }

    pub fn size(&self) -> LayoutSize {
        self.size
    }

    pub fn alignment(&self) -> u32 {
        self.alignment
    }

    pub fn fields(&self) -> &[LayoutField] {
        &self.fields
    }

    pub fn variant_layouts(&self) -> &[VariantLayout] {
        &self.variants
    }

    pub fn ownership(&self) -> &[OwnershipPosition] {
        &self.ownership
    }

    pub fn header(&self) -> HeaderLayout {
        self.header
    }

    pub fn anchor(&self) -> RootAnchor {
        self.anchor
    }

    pub fn operations(&self) -> &OperationShapes {
        &self.operations
    }

    /// The sole descriptor wire representation, shared by caches, artifacts,
    /// workers, and host boundary metadata.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = Encoder::default();
        encoder.bytes(LAYOUT_MAGIC);
        encoder.u32(self.schema_version);
        encoder.descriptor(self);
        encoder.output
    }

    pub fn layout_id(&self) -> LayoutId {
        LayoutId(sha256(&self.canonical_bytes()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    Owning,
    BorrowedView,
    Function,
    ExternRef,
    GcReference,
    Capability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageClass {
    OwningReference,
    BorrowedView,
    FunctionReference,
    ExternRef,
    GcReference,
    CapabilityReference,
}

impl ReferenceKind {
    pub const fn storage_class(self) -> StorageClass {
        match self {
            Self::Owning => StorageClass::OwningReference,
            Self::BorrowedView => StorageClass::BorrowedView,
            Self::Function => StorageClass::FunctionReference,
            Self::ExternRef => StorageClass::ExternRef,
            Self::GcReference => StorageClass::GcReference,
            Self::Capability => StorageClass::CapabilityReference,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathSegment {
    Root(String),
    Field(String),
    Variant(String),
    Tuple(usize),
    ListElement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutPath(Vec<PathSegment>);

impl LayoutPath {
    fn root(ty: &Type) -> Self {
        let name = match ty {
            Type::Named(name, _) => name.clone(),
            Type::Tuple(_) => "<tuple>".to_owned(),
            Type::Qualified(_, inner) => return Self::root(inner),
            Type::Fn(..) => "<function>".to_owned(),
            Type::Dyn(name, _) => format!("dyn {name}"),
            Type::RecordCompose { .. } => "<record composition>".to_owned(),
            Type::Slice(_) => "<slice>".to_owned(),
        };
        Self(vec![PathSegment::Root(name)])
    }

    fn field(&self, name: String) -> Self {
        let mut path = self.clone();
        path.0.push(PathSegment::Field(name));
        path
    }

    fn variant(&self, name: String) -> Self {
        let mut path = self.clone();
        path.0.push(PathSegment::Variant(name));
        path
    }

    fn tuple(&self, index: usize) -> Self {
        let mut path = self.clone();
        path.0.push(PathSegment::Tuple(index));
        path
    }

    fn list_element(&self) -> Self {
        let mut path = self.clone();
        path.0.push(PathSegment::ListElement);
        path
    }
}

impl fmt::Display for LayoutPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for segment in &self.0 {
            match segment {
                PathSegment::Root(name) => formatter.write_str(name)?,
                PathSegment::Field(name) => write!(formatter, ".{name}")?,
                PathSegment::Variant(name) => write!(formatter, "::{name}")?,
                PathSegment::Tuple(index) => write!(formatter, ".{index}")?,
                PathSegment::ListElement => formatter.write_str(".element")?,
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    UnresolvedType {
        path: LayoutPath,
        name: String,
    },
    ResolvedTypeMismatch {
        path: LayoutPath,
        expected: String,
        actual: String,
    },
    TypeArgumentCount {
        path: LayoutPath,
        expected: usize,
        actual: usize,
    },
    UnsupportedType {
        path: LayoutPath,
        reason: &'static str,
    },
    NotPackedRecord {
        path: LayoutPath,
        name: String,
    },
    CapabilityDefinition {
        path: LayoutPath,
        name: String,
    },
    InvalidClosedSum {
        path: LayoutPath,
        name: String,
    },
    InlineNominalCycle {
        path: LayoutPath,
        definition: String,
    },
    ReferenceNotInline {
        path: LayoutPath,
        kind: ReferenceKind,
        class: StorageClass,
    },
    UnknownLayout(LayoutId),
    DynamicInlineField(LayoutId),
    ArithmeticOverflow,
    UnsupportedSchema {
        found: u32,
    },
    Decode {
        offset: usize,
        reason: &'static str,
    },
    TrailingBytes {
        offset: usize,
    },
    NonCanonicalEncoding,
    DescriptorInvariant(&'static str),
    DigestMismatch {
        expected: LayoutId,
        actual: LayoutId,
    },
    DigestCollision(LayoutId),
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnresolvedType { path, name } => {
                write!(formatter, "{path}: unresolved closed layout type `{name}`")
            }
            Self::ResolvedTypeMismatch {
                path,
                expected,
                actual,
            } => {
                write!(
                    formatter,
                    "{path}: resolver returned `{actual}` for `{expected}`"
                )
            }
            Self::TypeArgumentCount {
                path,
                expected,
                actual,
            } => {
                write!(
                    formatter,
                    "{path}: expected {expected} closed type arguments, got {actual}"
                )
            }
            Self::UnsupportedType { path, reason } => write!(formatter, "{path}: {reason}"),
            Self::NotPackedRecord { path, name } => {
                write!(
                    formatter,
                    "{path}: `{name}` is not a one-variant packed record"
                )
            }
            Self::CapabilityDefinition { path, name } => {
                write!(
                    formatter,
                    "{path}: capability `{name}` requires reference-safe storage"
                )
            }
            Self::InvalidClosedSum { path, name } => {
                write!(formatter, "{path}: `{name}` is not a non-empty closed sum")
            }
            Self::InlineNominalCycle { path, definition } => {
                write!(
                    formatter,
                    "{path}: inline layout cycle re-enters `{definition}`"
                )
            }
            Self::ReferenceNotInline { path, kind, class } => {
                write!(
                    formatter,
                    "{path}: {kind:?} is {class:?} and cannot be stored inline"
                )
            }
            Self::UnknownLayout(id) => write!(formatter, "unknown child layout `{id}`"),
            Self::DynamicInlineField(id) => {
                write!(formatter, "dynamic layout `{id}` cannot be stored inline")
            }
            Self::ArithmeticOverflow => formatter.write_str("physical layout size overflow"),
            Self::UnsupportedSchema { found } => {
                write!(formatter, "unsupported layout schema {found}")
            }
            Self::Decode { offset, reason } => {
                write!(formatter, "invalid layout bytes at {offset}: {reason}")
            }
            Self::TrailingBytes { offset } => {
                write!(formatter, "trailing layout bytes at {offset}")
            }
            Self::NonCanonicalEncoding => formatter.write_str("non-canonical layout encoding"),
            Self::DescriptorInvariant(reason) => {
                write!(formatter, "invalid layout descriptor: {reason}")
            }
            Self::DigestMismatch { expected, actual } => {
                write!(
                    formatter,
                    "layout digest mismatch: expected {expected}, got {actual}"
                )
            }
            Self::DigestCollision(id) => write!(formatter, "layout digest collision for `{id}`"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// A type checker's closed, typed decision for a named logical type. The
/// interner still walks and substitutes every actual [`TypeDef`] field itself.
pub enum ResolvedNamed<'a> {
    Scalar(ScalarKind),
    PackedRecord(&'a TypeDef),
    PackedList { rc: RcHeader },
    ClosedSum(&'a TypeDef),
    Reference(ReferenceKind),
}

/// Authoritative bridge from resolved AST identity into physical classes. This
/// is intentionally typed: no child layout ids or field arrays cross it.
pub trait ClosedTypeResolver {
    fn resolve_named<'a>(&'a self, name: &str, arguments: &[Type]) -> Option<ResolvedNamed<'a>>;
}

#[derive(Debug, Default)]
pub struct LayoutInterner {
    descriptors: BTreeMap<LayoutId, LayoutDescriptor>,
}

impl LayoutInterner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.descriptors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }

    pub fn get(&self, id: LayoutId) -> Option<&LayoutDescriptor> {
        self.descriptors.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (LayoutId, &LayoutDescriptor)> {
        self.descriptors
            .iter()
            .map(|(id, descriptor)| (*id, descriptor))
    }

    /// Return the exact canonical discriminant lane for one closed sum.
    ///
    /// Both source-inline and runtime slot-backed sums enter through this one
    /// query. The descriptor is reproduced before metadata is returned, so a
    /// decoded or internally corrupted field table cannot mint tag authority.
    pub fn closed_sum_tag_metadata(
        &self,
        layout: LayoutId,
    ) -> Result<ClosedSumTagMetadata, LayoutError> {
        let (descriptor, _) = self.checked_closed_sum_descriptor(layout)?;
        let [tag] = descriptor.fields.as_slice() else {
            return Err(LayoutError::DescriptorInvariant(
                "closed-sum tag metadata is malformed",
            ));
        };
        if !matches!(tag.kind, FieldKind::Scalar(_)) {
            return Err(LayoutError::DescriptorInvariant(
                "closed-sum tag metadata is malformed",
            ));
        }
        Ok(ClosedSumTagMetadata {
            layout,
            offset: tag.offset,
            width: self.field_width(tag.kind)?,
            kind: tag.kind,
        })
    }

    /// Return one exact selected variant field from the canonical graph.
    ///
    /// The caller supplies only parent identity and ordinals. For a generic
    /// closed sum, scalar children retain both their canonical child id and
    /// scalar physical kind; fixed aggregates retain their inline child id.
    /// Runtime sums return their exact eight-byte value/pointer carrier id.
    pub fn closed_sum_variant_field_metadata(
        &self,
        layout: LayoutId,
        variant: usize,
        field: usize,
    ) -> Result<ClosedSumVariantFieldMetadata, LayoutError> {
        let (descriptor, variants) = self.checked_closed_sum_descriptor(layout)?;
        let children = variants.get(variant).ok_or(
            LayoutError::DescriptorInvariant("closed-sum variant index is out of range"),
        )?;
        let child_layout = *children.get(field).ok_or(
            LayoutError::DescriptorInvariant("closed-sum field index is out of range"),
        )?;
        let physical = descriptor
            .variants
            .get(variant)
            .and_then(|selected| selected.fields.get(field))
            .ok_or(LayoutError::DescriptorInvariant(
                "closed-sum variant field metadata is malformed",
            ))?;
        let child = self
            .descriptors
            .get(&child_layout)
            .ok_or(LayoutError::UnknownLayout(child_layout))?;
        let expected_kind = match &descriptor.kind {
            LayoutKind::ClosedSum { .. } => match &child.kind {
                LayoutKind::Scalar(scalar) => FieldKind::Scalar(*scalar),
                _ => FieldKind::Inline(child_layout),
            },
            LayoutKind::RuntimeClosedSum { .. } => FieldKind::Inline(child_layout),
            _ => {
                return Err(LayoutError::DescriptorInvariant(
                    "sum field metadata requires a closed-sum layout",
                ));
            }
        };
        if physical.kind != expected_kind {
            return Err(LayoutError::DescriptorInvariant(
                "closed-sum variant field metadata is malformed",
            ));
        }
        Ok(ClosedSumVariantFieldMetadata {
            layout,
            variant,
            field,
            child_layout,
            offset: physical.offset,
            width: self.field_width(physical.kind)?,
            kind: physical.kind,
        })
    }

    fn checked_closed_sum_descriptor(
        &self,
        layout: LayoutId,
    ) -> Result<(&LayoutDescriptor, &[Vec<LayoutId>]), LayoutError> {
        let descriptor = self
            .descriptors
            .get(&layout)
            .ok_or(LayoutError::UnknownLayout(layout))?;
        let variants = match &descriptor.kind {
            LayoutKind::ClosedSum { variants }
            | LayoutKind::RuntimeClosedSum { variants } => variants.as_slice(),
            _ => {
                return Err(LayoutError::DescriptorInvariant(
                    "sum metadata requires a closed-sum layout",
                ));
            }
        };
        self.validate_descriptor(descriptor)?;
        if variants.len() != descriptor.variants.len() {
            return Err(LayoutError::DescriptorInvariant(
                "closed-sum variant metadata is malformed",
            ));
        }
        Ok((descriptor, variants))
    }

    fn field_width(&self, kind: FieldKind) -> Result<u32, LayoutError> {
        match kind {
            FieldKind::Scalar(scalar) => Ok(scalar.size()),
            FieldKind::Pointer { .. } => Ok(4),
            FieldKind::Inline(child) => match self
                .descriptors
                .get(&child)
                .ok_or(LayoutError::UnknownLayout(child))?
                .size
            {
                LayoutSize::Fixed(size) => Ok(size),
                LayoutSize::Dynamic { .. } => Err(LayoutError::DynamicInlineField(child)),
            },
        }
    }

    /// Return the exact live byte extent for one selected closed-sum variant.
    ///
    /// A descriptor's fixed `size` is the maximum carrier size across every
    /// variant. It is therefore not valid evidence that bytes belonging only
    /// to a larger variant are live after a smaller discriminant is selected.
    /// This query derives the selected extent from the authenticated tag and
    /// field graph, including inline child sizes, then preserves the common
    /// carrier alignment. It deliberately does not trust `payload_size` as an
    /// independently authorizing width.
    pub fn variant_live_extent(
        &self,
        layout: LayoutId,
        variant: usize,
    ) -> Result<u32, LayoutError> {
        let descriptor = self
            .descriptors
            .get(&layout)
            .ok_or(LayoutError::UnknownLayout(layout))?;
        if !matches!(
            &descriptor.kind,
            LayoutKind::ClosedSum { .. } | LayoutKind::RuntimeClosedSum { .. }
        ) {
            return Err(LayoutError::DescriptorInvariant(
                "variant extent requires a closed-sum layout",
            ));
        }
        let selected = descriptor.variants.get(variant).ok_or(
            LayoutError::DescriptorInvariant("closed-sum variant index is out of range"),
        )?;

        let mut live_end = 0u32;
        for field in descriptor.fields.iter().chain(&selected.fields) {
            let width = match field.kind {
                FieldKind::Scalar(scalar) => scalar.size(),
                FieldKind::Pointer { .. } => 4,
                FieldKind::Inline(child) => match self
                    .descriptors
                    .get(&child)
                    .ok_or(LayoutError::UnknownLayout(child))?
                    .size
                {
                    LayoutSize::Fixed(size) => size,
                    LayoutSize::Dynamic { .. } => {
                        return Err(LayoutError::DynamicInlineField(child));
                    }
                },
            };
            live_end = live_end.max(
                field
                    .offset
                    .checked_add(width)
                    .ok_or(LayoutError::ArithmeticOverflow)?,
            );
        }
        align_up(live_end, descriptor.alignment)
    }

    /// Resolve and intern one complete closed logical type. Nominal children are
    /// derived from substituted `TypeDef` fields, never accepted as caller ids.
    pub fn intern_type(
        &mut self,
        ty: &Type,
        resolver: &impl ClosedTypeResolver,
    ) -> Result<LayoutId, LayoutError> {
        let path = LayoutPath::root(ty);
        self.intern_resolved(ty, resolver, &path, &mut BTreeSet::new())
    }

    /// Intern one representation-only scalar through the same checked catalog
    /// path as source-derived layouts. Source lowering should continue to use
    /// [`Self::intern_type`]; this entry exists for runtime ABI primitives such
    /// as raw bytes and SIMD chunks.
    pub fn intern_scalar(&mut self, scalar: ScalarKind) -> Result<LayoutId, LayoutError> {
        self.intern_kind(LayoutKind::Scalar(scalar))
    }

    /// Intern the canonical packed `[u32 length][elements...]` runtime buffer.
    /// Only exact scalar element descriptors are accepted in this foundational
    /// form. Aggregate/runtime container classes require a later named contract
    /// rather than smuggling an arbitrary stride through this API.
    pub fn intern_runtime_buffer(&mut self, element: LayoutId) -> Result<LayoutId, LayoutError> {
        self.intern_kind(LayoutKind::RuntimeBuffer { element })
    }

    /// Intern a source packed list: `[u32 length][u32 capacity][elements...]`.
    ///
    /// `intern_type` already reaches this kind, but only with a
    /// `ClosedTypeResolver` and a source `Type` in hand. The WIR helper library
    /// has neither -- it builds functions from scratch -- and the shape it needs
    /// is fixed, so it gets the same direct entry point the runtime kinds have.
    /// Offsets, stride, and the header remain derived by the descriptor.
    pub fn intern_packed_list(
        &mut self,
        element: LayoutId,
        rc: RcHeader,
    ) -> Result<LayoutId, LayoutError> {
        self.intern_kind(LayoutKind::PackedList { element, rc })
    }

    /// Intern the canonical headerless scratch-byte carrier. Runtime catalog
    /// validation restricts `element` to the representation-only RawByte
    /// scalar; ordinary source lowering has no entry point that mints it.
    pub fn intern_scratch_buffer(&mut self, element: LayoutId) -> Result<LayoutId, LayoutError> {
        let descriptor = self
            .get(element)
            .ok_or(LayoutError::UnknownLayout(element))?;
        if descriptor.kind() != &LayoutKind::Scalar(ScalarKind::RawByte) {
            return Err(LayoutError::DescriptorInvariant(
                "scratch buffer construction requires the exact RawByte scalar",
            ));
        }
        self.intern_kind(LayoutKind::ScratchBuffer { element })
    }



    /// Intern the canonical `[u32 length][8-byte widened pointer slots...]`
    /// runtime buffer. The pointee and slot semantics are part of the canonical
    /// descriptor identity; callers cannot supply a width, stride, or root.
    pub fn intern_runtime_pointer_buffer(
        &mut self,
        pointee: LayoutId,
        semantics: PointerSlot,
    ) -> Result<LayoutId, LayoutError> {
        self.intern_kind(LayoutKind::RuntimePointerBuffer { pointee, semantics })
    }

    /// Intern the exact ordered dictionary body. Both carriers must be exact
    /// fixed eight-byte runtime value slots; pointer-bearing slots retain their
    /// pointee and ownership semantics in their canonical child descriptor.
    /// Intern one dense dictionary row. The body constructor mints this itself;
    /// it is public so a row identity can be recovered independently.
    pub fn intern_runtime_dictionary_row(
        &mut self,
        key: LayoutId,
        value: LayoutId,
    ) -> Result<LayoutId, LayoutError> {
        self.intern_kind(LayoutKind::RuntimeDictionaryRow { key, value })
    }

    pub fn intern_runtime_dictionary_body(
        &mut self,
        key: LayoutId,
        value: LayoutId,
    ) -> Result<LayoutId, LayoutError> {
        let row = self.intern_runtime_dictionary_row(key, value)?;
        self.intern_kind(LayoutKind::RuntimeDictionaryBody { key, value, row })
    }

    /// Intern the exact raw Swiss carrier paired with one dictionary body.
    pub fn intern_runtime_dictionary_index(
        &mut self,
        key: LayoutId,
        value: LayoutId,
    ) -> Result<LayoutId, LayoutError> {
        self.intern_kind(LayoutKind::RuntimeDictionaryIndex { key, value })
    }

    /// Intern one exact source scalar in the ordinary eight-byte value lane.
    pub fn intern_runtime_value_slot(
        &mut self,
        value: LayoutId,
    ) -> Result<LayoutId, LayoutError> {
        self.intern_kind(LayoutKind::RuntimeValueSlot { value })
    }

    /// Intern one ownership-aware pointer in the ordinary eight-byte value lane.
    pub fn intern_runtime_pointer_value_slot(
        &mut self,
        pointee: LayoutId,
        semantics: PointerSlot,
    ) -> Result<LayoutId, LayoutError> {
        self.intern_kind(LayoutKind::RuntimePointerValueSlot { pointee, semantics })
    }

    /// Intern a fixed record of already-interned field layouts: scalars and
    /// owned pointer slots. The canonical layout catalog builds heap
    /// aggregates (records, sums, `Option`/`Result`) this way because a
    /// reference field cannot be reached through a type resolver.
    pub fn intern_packed_record(&mut self, fields: Vec<LayoutId>) -> Result<LayoutId, LayoutError> {
        self.intern_kind(LayoutKind::PackedRecord { fields })
    }

    /// Intern one ordinary slot-backed closed sum. Empty variants are retained
    /// exactly; non-empty children must already be canonical runtime value slots.
    pub fn intern_runtime_closed_sum(
        &mut self,
        variants: Vec<Vec<LayoutId>>,
    ) -> Result<LayoutId, LayoutError> {
        self.intern_kind(LayoutKind::RuntimeClosedSum { variants })
    }

    /// Intern the ordered capture identity of one typed Wasm-GC environment.
    /// No linear-memory bytes are described or introduced by this operation.
    pub fn intern_closure_environment(
        &mut self,
        captures: Vec<LayoutId>,
    ) -> Result<LayoutId, LayoutError> {
        self.intern_kind(LayoutKind::ClosureEnvironment { captures })
    }

    /// Intern an allocation prefix whose body is addressed through an exposed
    /// runtime handle. Prefix offsets and the anchor are derived; callers cannot
    /// supply either or construct an unchecked [`LayoutDescriptor`].
    pub fn intern_exposed_root(
        &mut self,
        body: LayoutId,
        prefix: Vec<RuntimePrefixSlot>,
    ) -> Result<LayoutId, LayoutError> {
        let body_alignment = self
            .descriptors
            .get(&body)
            .ok_or(LayoutError::UnknownLayout(body))?
            .alignment;
        let anchor_bias = derive_anchor_bias(&prefix, body_alignment, |id| {
            self.descriptors.get(&id)
        })?;
        self.intern_kind(LayoutKind::ExposedRoot {
            body,
            prefix,
            anchor_bias,
        })
    }

    /// Decode and fully validate canonical bytes without mutating the interner.
    /// Every referenced child must already be present.
    pub fn decode_canonical(&self, bytes: &[u8]) -> Result<LayoutDescriptor, LayoutError> {
        let descriptor = decode_descriptor(bytes)?;
        if descriptor.canonical_bytes() != bytes {
            return Err(LayoutError::NonCanonicalEncoding);
        }
        self.validate_descriptor(&descriptor)?;
        Ok(descriptor)
    }

    pub fn validate_canonical(&self, expected: LayoutId, bytes: &[u8]) -> Result<(), LayoutError> {
        let descriptor = self.decode_canonical(bytes)?;
        let actual = descriptor.layout_id();
        if actual != expected {
            return Err(LayoutError::DigestMismatch { expected, actual });
        }
        Ok(())
    }

    /// Validate and import an artifact/host descriptor. Child descriptors must
    /// be imported first, giving the descriptor graph a deterministic order.
    pub fn import_canonical(
        &mut self,
        expected: LayoutId,
        bytes: &[u8],
    ) -> Result<LayoutId, LayoutError> {
        let descriptor = self.decode_canonical(bytes)?;
        let actual = descriptor.layout_id();
        if actual != expected {
            return Err(LayoutError::DigestMismatch { expected, actual });
        }
        if let Some(existing) = self.descriptors.get(&actual) {
            if existing != &descriptor {
                return Err(LayoutError::DigestCollision(actual));
            }
            return Ok(actual);
        }
        self.descriptors.insert(actual, descriptor);
        Ok(actual)
    }

    fn intern_resolved(
        &mut self,
        ty: &Type,
        resolver: &impl ClosedTypeResolver,
        path: &LayoutPath,
        active_nominals: &mut BTreeSet<String>,
    ) -> Result<LayoutId, LayoutError> {
        match ty {
            Type::Qualified(TypeQual::LegacyBorrow(_), _) => {
                Err(reference_error(path, ReferenceKind::BorrowedView))
            }
            Type::Qualified(_, inner) => {
                self.intern_resolved(inner, resolver, path, active_nominals)
            }
            Type::Tuple(fields) => {
                let mut children = Vec::with_capacity(fields.len());
                for (index, field) in fields.iter().enumerate() {
                    children.push(self.intern_resolved(
                        field,
                        resolver,
                        &path.tuple(index),
                        active_nominals,
                    )?);
                }
                self.intern_kind(LayoutKind::Tuple { fields: children })
            }
            Type::Fn(..) => Err(reference_error(path, ReferenceKind::Function)),
            Type::Dyn(..) => Err(reference_error(path, ReferenceKind::GcReference)),
            Type::Slice(..) => Err(reference_error(path, ReferenceKind::BorrowedView)),
            Type::RecordCompose { .. } => Err(LayoutError::UnsupportedType {
                path: path.clone(),
                reason: "structural record composition must be normalized before layout",
            }),
            Type::Named(name, arguments) => {
                let resolved = resolver.resolve_named(name, arguments).ok_or_else(|| {
                    LayoutError::UnresolvedType {
                        path: path.clone(),
                        name: name.clone(),
                    }
                })?;
                match resolved {
                    ResolvedNamed::Scalar(scalar) => {
                        if !arguments.is_empty() {
                            return Err(LayoutError::TypeArgumentCount {
                                path: path.clone(),
                                expected: 0,
                                actual: arguments.len(),
                            });
                        }
                        if !scalar.is_source_value() {
                            return Err(LayoutError::UnsupportedType {
                                path: path.clone(),
                                reason: "internal header/tag scalar cannot be a source field",
                            });
                        }
                        self.intern_kind(LayoutKind::Scalar(scalar))
                    }
                    ResolvedNamed::PackedList { rc } => {
                        if arguments.len() != 1 {
                            return Err(LayoutError::TypeArgumentCount {
                                path: path.clone(),
                                expected: 1,
                                actual: arguments.len(),
                            });
                        }
                        let element = self.intern_resolved(
                            &arguments[0],
                            resolver,
                            &path.list_element(),
                            active_nominals,
                        )?;
                        self.intern_kind(LayoutKind::PackedList { element, rc })
                    }
                    ResolvedNamed::Reference(kind) => Err(reference_error(path, kind)),
                    ResolvedNamed::PackedRecord(definition) => {
                        self.check_definition_identity(name, definition, path)?;
                        if definition.is_capability {
                            return Err(LayoutError::CapabilityDefinition {
                                path: path.clone(),
                                name: definition.name.clone(),
                            });
                        }
                        if !definition.packed || definition.variants.len() != 1 {
                            return Err(LayoutError::NotPackedRecord {
                                path: path.clone(),
                                name: definition.name.clone(),
                            });
                        }
                        if !active_nominals.insert(definition.name.clone()) {
                            return Err(LayoutError::InlineNominalCycle {
                                path: path.clone(),
                                definition: definition.name.clone(),
                            });
                        }
                        let result = (|| {
                            let variants = instantiate_fields(definition, arguments, path)?;
                            let variant = &definition.variants[0];
                            let mut children = Vec::with_capacity(variants[0].len());
                            for (index, field) in variants[0].iter().enumerate() {
                                let field_name = variant
                                    .field_names
                                    .get(index)
                                    .cloned()
                                    .unwrap_or_else(|| index.to_string());
                                children.push(self.intern_resolved(
                                    field,
                                    resolver,
                                    &path.field(field_name),
                                    active_nominals,
                                )?);
                            }
                            self.intern_kind(LayoutKind::PackedRecord { fields: children })
                        })();
                        active_nominals.remove(&definition.name);
                        result
                    }
                    ResolvedNamed::ClosedSum(definition) => {
                        self.check_definition_identity(name, definition, path)?;
                        if definition.is_capability {
                            return Err(LayoutError::CapabilityDefinition {
                                path: path.clone(),
                                name: definition.name.clone(),
                            });
                        }
                        if definition.variants.is_empty() {
                            return Err(LayoutError::InvalidClosedSum {
                                path: path.clone(),
                                name: definition.name.clone(),
                            });
                        }
                        if !active_nominals.insert(definition.name.clone()) {
                            return Err(LayoutError::InlineNominalCycle {
                                path: path.clone(),
                                definition: definition.name.clone(),
                            });
                        }
                        let result = (|| {
                            let fields = instantiate_fields(definition, arguments, path)?;
                            let mut variants = Vec::with_capacity(fields.len());
                            for (variant_index, variant_fields) in fields.iter().enumerate() {
                                let variant = &definition.variants[variant_index];
                                let variant_path = path.variant(variant.name.clone());
                                let mut children = Vec::with_capacity(variant_fields.len());
                                for (field_index, field) in variant_fields.iter().enumerate() {
                                    let field_name = variant
                                        .field_names
                                        .get(field_index)
                                        .cloned()
                                        .unwrap_or_else(|| field_index.to_string());
                                    children.push(self.intern_resolved(
                                        field,
                                        resolver,
                                        &variant_path.field(field_name),
                                        active_nominals,
                                    )?);
                                }
                                variants.push(children);
                            }
                            self.intern_kind(LayoutKind::ClosedSum { variants })
                        })();
                        active_nominals.remove(&definition.name);
                        result
                    }
                }
            }
        }
    }

    fn check_definition_identity(
        &self,
        expected: &str,
        definition: &TypeDef,
        path: &LayoutPath,
    ) -> Result<(), LayoutError> {
        if definition.name != expected {
            return Err(LayoutError::ResolvedTypeMismatch {
                path: path.clone(),
                expected: expected.to_owned(),
                actual: definition.name.clone(),
            });
        }
        Ok(())
    }

    fn intern_kind(&mut self, kind: LayoutKind) -> Result<LayoutId, LayoutError> {
        let descriptor = build_descriptor(kind, |id| self.descriptors.get(&id))?;
        // This is deliberately a release-mode check. No id is computed until
        // the complete descriptor reproduces from its minimal kind and children.
        self.validate_descriptor(&descriptor)?;
        let id = descriptor.layout_id();
        if let Some(existing) = self.descriptors.get(&id) {
            if existing != &descriptor {
                return Err(LayoutError::DigestCollision(id));
            }
            return Ok(id);
        }
        self.descriptors.insert(id, descriptor);
        Ok(id)
    }

    fn validate_descriptor(&self, descriptor: &LayoutDescriptor) -> Result<(), LayoutError> {
        if descriptor.schema_version != LAYOUT_SCHEMA_VERSION {
            return Err(LayoutError::UnsupportedSchema {
                found: descriptor.schema_version,
            });
        }
        validate_descriptor_item_counts(descriptor)?;
        if descriptor.alignment == 0 || !descriptor.alignment.is_power_of_two() {
            return Err(LayoutError::DescriptorInvariant(
                "alignment must be a non-zero power of two",
            ));
        }
        let expected = build_descriptor(descriptor.kind.clone(), |id| self.descriptors.get(&id))?;
        if descriptor != &expected {
            return Err(LayoutError::DescriptorInvariant(
                "size, alignment, fields, header, ownership, or operation shape disagrees with kind",
            ));
        }
        Ok(())
    }
}

fn validate_descriptor_item_counts(descriptor: &LayoutDescriptor) -> Result<(), LayoutError> {
    fn check(count: usize) -> Result<(), LayoutError> {
        if count > MAX_DESCRIPTOR_ITEMS || u32::try_from(count).is_err() {
            Err(LayoutError::DescriptorInvariant(
                "descriptor item count exceeds canonical limit",
            ))
        } else {
            Ok(())
        }
    }

    match &descriptor.kind {
        LayoutKind::Scalar(_)
        | LayoutKind::PackedList { .. }
        | LayoutKind::RuntimeBuffer { .. }
        | LayoutKind::ScratchBuffer { .. }
        | LayoutKind::RuntimePointerBuffer { .. }
        | LayoutKind::RuntimeDictionaryBody { .. }
        | LayoutKind::RuntimeDictionaryRow { .. }
        | LayoutKind::RuntimeDictionaryIndex { .. }
        | LayoutKind::RuntimeValueSlot { .. }
        | LayoutKind::RuntimePointerValueSlot { .. } => {}
        LayoutKind::Tuple { fields } | LayoutKind::PackedRecord { fields } => {
            check(fields.len())?;
        }
        LayoutKind::ClosedSum { variants }
        | LayoutKind::RuntimeClosedSum { variants } => {
            check(variants.len())?;
            for variant in variants {
                check(variant.len())?;
            }
        }
        LayoutKind::ClosureEnvironment { captures } => check(captures.len())?,
        LayoutKind::ExposedRoot { prefix, .. } => check(prefix.len())?,
    }
    check(descriptor.fields.len())?;
    check(descriptor.variants.len())?;
    for variant in &descriptor.variants {
        check(variant.fields.len())?;
    }
    check(descriptor.ownership.len())?;
    for position in &descriptor.ownership {
        if let OwnershipPosition::Field(path) = position {
            check(path.len())?;
        }
    }
    for operation in [
        &descriptor.operations.copy,
        &descriptor.operations.duplicate,
        &descriptor.operations.drop,
        &descriptor.operations.equality,
        &descriptor.operations.render,
        &descriptor.operations.serialization,
    ] {
        match operation {
            OperationShape::None
            | OperationShape::Scalar(_)
            | OperationShape::PackedElements { .. } => {}
            OperationShape::Fields(fields) => check(fields.len())?,
            OperationShape::Variants { variants, .. } => {
                check(variants.len())?;
                for variant in variants {
                    check(variant.len())?;
                }
            }
        }
    }
    Ok(())
}

fn reference_error(path: &LayoutPath, kind: ReferenceKind) -> LayoutError {
    LayoutError::ReferenceNotInline {
        path: path.clone(),
        kind,
        class: kind.storage_class(),
    }
}

fn instantiate_fields(
    definition: &TypeDef,
    arguments: &[Type],
    path: &LayoutPath,
) -> Result<Vec<Vec<Type>>, LayoutError> {
    let parameters = effective_type_def_params(definition);
    if arguments.len() != parameters.len() {
        return Err(LayoutError::TypeArgumentCount {
            path: path.clone(),
            expected: parameters.len(),
            actual: arguments.len(),
        });
    }
    let bindings = parameters
        .into_iter()
        .zip(arguments.iter().cloned())
        .collect::<BTreeMap<_, _>>();
    Ok(definition
        .variants
        .iter()
        .map(|variant| {
            variant
                .fields
                .iter()
                .map(|field| substitute_type(field, &bindings))
                .collect()
        })
        .collect())
}

fn substitute_type(ty: &Type, bindings: &BTreeMap<String, Type>) -> Type {
    match ty {
        Type::Named(name, arguments) if arguments.is_empty() => {
            bindings.get(name).cloned().unwrap_or_else(|| ty.clone())
        }
        Type::Named(name, arguments) => Type::Named(
            name.clone(),
            arguments
                .iter()
                .map(|argument| substitute_type(argument, bindings))
                .collect(),
        ),
        Type::Tuple(fields) => Type::Tuple(
            fields
                .iter()
                .map(|field| substitute_type(field, bindings))
                .collect(),
        ),
        Type::Qualified(qualifier, inner) => Type::Qualified(
            qualifier.clone(),
            Box::new(substitute_type(inner, bindings)),
        ),
        Type::Fn(parameters, result, conventions, qualifiers) => Type::Fn(
            parameters
                .iter()
                .map(|parameter| substitute_type(parameter, bindings))
                .collect(),
            Box::new(substitute_type(result, bindings)),
            conventions.clone(),
            *qualifiers,
        ),
        Type::Dyn(name, arguments) => Type::Dyn(
            name.clone(),
            arguments
                .iter()
                .map(|argument| substitute_type(argument, bindings))
                .collect(),
        ),
        Type::RecordCompose { base, fields } => Type::RecordCompose {
            base: Box::new(substitute_type(base, bindings)),
            fields: fields
                .iter()
                .map(|(name, field)| (name.clone(), substitute_type(field, bindings)))
                .collect(),
        },
        Type::Slice(inner) => Type::Slice(Box::new(substitute_type(inner, bindings))),
    }
}

fn build_descriptor<'a>(
    kind: LayoutKind,
    mut lookup: impl FnMut(LayoutId) -> Option<&'a LayoutDescriptor>,
) -> Result<LayoutDescriptor, LayoutError> {
    match &kind {
        LayoutKind::Scalar(scalar) => {
            let operation = OperationShape::Scalar(*scalar);
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Fixed(scalar.size()),
                alignment: scalar.alignment(),
                fields: Vec::new(),
                variants: Vec::new(),
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: OperationShapes {
                    copy: operation.clone(),
                    duplicate: operation.clone(),
                    drop: OperationShape::None,
                    equality: operation.clone(),
                    render: operation.clone(),
                    serialization: operation,
                },
            })
        }
        LayoutKind::Tuple { fields } => {
            let (physical_fields, size, alignment) = aggregate_fields(fields, 0, &mut lookup)?;
            let operation = OperationShape::Fields(fields.clone());
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Fixed(size),
                alignment,
                fields: physical_fields,
                variants: Vec::new(),
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: OperationShapes {
                    copy: operation.clone(),
                    duplicate: operation.clone(),
                    drop: operation.clone(),
                    equality: operation.clone(),
                    render: operation.clone(),
                    serialization: operation,
                },
            })
        }
        LayoutKind::PackedRecord { fields } => {
            // These two exact records are private class seals minted only by
            // the named borrowed-byte and raw-index constructors above.
            // Representation-only RawByte remains rejected from every other
            // ordinary aggregate shape.
            let (physical_fields, size, alignment) =
                aggregate_fields(fields, 0, &mut lookup)?;
            let operation = OperationShape::Fields(fields.clone());
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Fixed(size),
                alignment,
                fields: physical_fields,
                variants: Vec::new(),
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: OperationShapes {
                    copy: operation.clone(),
                    duplicate: operation.clone(),
                    drop: operation.clone(),
                    equality: operation.clone(),
                    render: operation.clone(),
                    serialization: operation,
                },
            })
        }
        LayoutKind::PackedList { element, rc } => {
            let element_descriptor =
                lookup(*element).ok_or(LayoutError::UnknownLayout(*element))?;
            validate_source_inline_child(*element, element_descriptor)?;
            // A closure lane is a record slot, never an element: v1 has no
            // list of callables, and admitting one here would give the
            // element ABI a value type the registry cannot name.
            if element_descriptor.kind == LayoutKind::Scalar(ScalarKind::Callable) {
                return Err(LayoutError::DescriptorInvariant(
                    "a callable lane is a closure record slot, not a list element",
                ));
            }
            let element_size = match element_descriptor.size {
                LayoutSize::Fixed(size) => size,
                LayoutSize::Dynamic { .. } => {
                    return Err(LayoutError::DynamicInlineField(*element));
                }
            };
            let stride = align_up(element_size, element_descriptor.alignment)?;
            let data_offset = align_up(8, element_descriptor.alignment)?;
            let alignment = element_descriptor.alignment.max(4);
            let operation = OperationShape::PackedElements {
                element: *element,
                stride,
            };
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Dynamic {
                    base: data_offset,
                    stride,
                },
                alignment,
                fields: vec![
                    LayoutField {
                        offset: 0,
                        kind: FieldKind::Scalar(ScalarKind::U32),
                    },
                    LayoutField {
                        offset: 4,
                        kind: FieldKind::Scalar(ScalarKind::U32),
                    },
                    LayoutField {
                        offset: data_offset,
                        kind: FieldKind::Inline(*element),
                    },
                ],
                variants: Vec::new(),
                ownership: vec![OwnershipPosition::RootBuffer],
                header: HeaderLayout::PackedList {
                    rc: *rc,
                    length_offset: 0,
                    capacity_offset: 4,
                    data_offset,
                },
                anchor: RootAnchor::AllocationBase,
                operations: OperationShapes {
                    copy: operation.clone(),
                    duplicate: operation.clone(),
                    drop: operation.clone(),
                    equality: operation.clone(),
                    render: operation.clone(),
                    serialization: operation,
                },
            })
        }
        LayoutKind::ClosedSum { variants } => {
            let tag = tag_kind(variants.len())?;
            let mut payload_alignment = 1;
            let mut payload_size = 0;
            for variant in variants {
                let (_, size, alignment) = aggregate_fields(variant, 0, &mut lookup)?;
                payload_alignment = payload_alignment.max(alignment);
                payload_size = payload_size.max(size);
            }
            let payload_offset = align_up(tag.size(), payload_alignment)?;
            let alignment = tag.alignment().max(payload_alignment);
            let size = align_up(
                payload_offset
                    .checked_add(payload_size)
                    .ok_or(LayoutError::ArithmeticOverflow)?,
                alignment,
            )?;
            let mut variant_layouts = Vec::with_capacity(variants.len());
            for variant in variants {
                let (fields, size, alignment) =
                    aggregate_fields(variant, payload_offset, &mut lookup)?;
                variant_layouts.push(VariantLayout {
                    fields,
                    payload_size: size,
                    payload_alignment: alignment,
                });
            }
            let operation = OperationShape::Variants {
                tag,
                variants: variants.clone(),
            };
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Fixed(size),
                alignment,
                fields: vec![LayoutField {
                    offset: 0,
                    kind: FieldKind::Scalar(tag),
                }],
                variants: variant_layouts,
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: OperationShapes {
                    copy: operation.clone(),
                    duplicate: operation.clone(),
                    drop: operation.clone(),
                    equality: operation.clone(),
                    render: operation.clone(),
                    serialization: operation,
                },
            })
        }
        LayoutKind::RuntimeBuffer { element } => {
            let element_descriptor =
                lookup(*element).ok_or(LayoutError::UnknownLayout(*element))?;
            let LayoutKind::Scalar(element_scalar) = &element_descriptor.kind else {
                return Err(LayoutError::DescriptorInvariant(
                    "runtime buffer element must be an exact scalar layout",
                ));
            };
            if *element_scalar == ScalarKind::Callable {
                return Err(LayoutError::DescriptorInvariant(
                    "a callable lane is a closure record slot, not a buffer element",
                ));
            }
            let stride = element_scalar.size();
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Dynamic { base: 4, stride },
                // The legacy payload begins at +4 even for i64 and v128 lanes.
                // Four is therefore the strongest alignment the descriptor may
                // claim for every element in the runtime buffer.
                alignment: 4,
                fields: vec![
                    LayoutField {
                        offset: 0,
                        kind: FieldKind::Scalar(ScalarKind::U32),
                    },
                    LayoutField {
                        offset: 4,
                        kind: FieldKind::Inline(*element),
                    },
                ],
                variants: Vec::new(),
                // This primitive authenticates physical ranges only. A later
                // named runtime class must supply ownership and value-operation
                // semantics for Bytes, String, slot lists, or host buffers.
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: OperationShapes {
                    copy: OperationShape::None,
                    duplicate: OperationShape::None,
                    drop: OperationShape::None,
                    equality: OperationShape::None,
                    render: OperationShape::None,
                    serialization: OperationShape::None,
                },
            })
        }
        LayoutKind::ScratchBuffer { element } => {
            let element_descriptor =
                lookup(*element).ok_or(LayoutError::UnknownLayout(*element))?;
            let raw_byte = element_descriptor.kind == LayoutKind::Scalar(ScalarKind::RawByte)
                && element_descriptor.size == LayoutSize::Fixed(1);
            let borrowed_byte_seal = matches!(
                &element_descriptor.kind,
                LayoutKind::PackedRecord { fields }
                    if fields.len() == 1
                        && lookup(fields[0]).is_some_and(|field| {
                            field.kind == LayoutKind::Scalar(ScalarKind::RawByte)
                                && field.size == LayoutSize::Fixed(1)
                        })
            ) && element_descriptor.size == LayoutSize::Fixed(1);
            if !raw_byte && !borrowed_byte_seal {
                return Err(LayoutError::DescriptorInvariant(
                    "headerless byte carrier element must be RawByte or the sealed borrowed-byte class",
                ));
            }
            let operation = OperationShape::None;
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Dynamic { base: 0, stride: 1 },
                alignment: 1,
                fields: vec![LayoutField {
                    offset: 0,
                    kind: FieldKind::Inline(*element),
                }],
                variants: Vec::new(),
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: OperationShapes {
                    copy: operation.clone(),
                    duplicate: operation.clone(),
                    drop: operation.clone(),
                    equality: operation.clone(),
                    render: operation.clone(),
                    serialization: operation,
                },
            })
        }
        LayoutKind::RuntimePointerBuffer { pointee, semantics } => {
            lookup(*pointee).ok_or(LayoutError::UnknownLayout(*pointee))?;
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Dynamic { base: 4, stride: 8 },
                alignment: 4,
                fields: vec![
                    LayoutField {
                        offset: 0,
                        kind: FieldKind::Scalar(ScalarKind::U32),
                    },
                    LayoutField {
                        offset: 4,
                        kind: FieldKind::Pointer {
                            pointee: *pointee,
                            slot: *semantics,
                        },
                    },
                ],
                variants: Vec::new(),
                // Pointer metadata authenticates representation only. Even an
                // Owned slot needs separate live-object and ownership evidence.
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: OperationShapes {
                    copy: OperationShape::None,
                    duplicate: OperationShape::None,
                    drop: OperationShape::None,
                    equality: OperationShape::None,
                    render: OperationShape::None,
                    serialization: OperationShape::None,
                },
            })
        }
        LayoutKind::RuntimeDictionaryRow { key, value } => {
            validate_runtime_dictionary_slot(*key, lookup(*key))?;
            validate_runtime_dictionary_slot(*value, lookup(*value))?;
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Fixed(16),
                // Four. A row begins at the body's base of four and steps by
                // sixteen, so its address is 4-aligned; the eight its slots
                // suggest would misstate the ABI, and the body already says four.
                alignment: 4,
                fields: vec![
                    LayoutField { offset: 0, kind: FieldKind::Inline(*key) },
                    LayoutField { offset: 8, kind: FieldKind::Inline(*value) },
                ],
                variants: Vec::new(),
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: no_runtime_operations(),
            })
        }
        LayoutKind::RuntimeDictionaryBody { key, value, row } => {
            let _ = row;
            validate_runtime_dictionary_slot(*key, lookup(*key))?;
            validate_runtime_dictionary_slot(*value, lookup(*value))?;
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Dynamic { base: 4, stride: 16 },
                // Dense rows intentionally begin at exposed-handle + 4, so four
                // is the strongest alignment the physical ABI may claim.
                alignment: 4,
                fields: vec![
                    LayoutField {
                        offset: 0,
                        kind: FieldKind::Scalar(ScalarKind::U32),
                    },
                    LayoutField {
                        offset: 4,
                        kind: FieldKind::Inline(*key),
                    },
                    LayoutField {
                        offset: 12,
                        kind: FieldKind::Inline(*value),
                    },
                ],
                variants: Vec::new(),
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: no_runtime_operations(),
            })
        }
        LayoutKind::RuntimeDictionaryIndex { key, value } => {
            validate_runtime_dictionary_slot(*key, lookup(*key))?;
            validate_runtime_dictionary_slot(*value, lookup(*value))?;
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Dynamic { base: 20, stride: 25 },
                alignment: 4,
                fields: vec![LayoutField {
                    offset: 0,
                    kind: FieldKind::Scalar(ScalarKind::U32),
                }],
                variants: Vec::new(),
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: no_runtime_operations(),
            })
        }
        LayoutKind::RuntimeValueSlot { value } => {
            validate_runtime_value(*value, lookup(*value))?;
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Fixed(8),
                // Slot-backed aggregates place the first lane at byte four.
                alignment: 4,
                // Authenticate the logical scalar at the low bytes of the
                // widened lane; width alone never identifies the payload.
                fields: vec![LayoutField {
                    offset: 0,
                    kind: FieldKind::Inline(*value),
                }],
                variants: Vec::new(),
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: no_runtime_operations(),
            })
        }
        LayoutKind::RuntimePointerValueSlot { pointee, semantics } => {
            if *semantics == PointerSlot::Raw {
                return Err(LayoutError::DescriptorInvariant(
                    "ordinary aggregate pointer slots cannot erase ownership as Raw",
                ));
            }
            lookup(*pointee).ok_or(LayoutError::UnknownLayout(*pointee))?;
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Fixed(8),
                alignment: 4,
                fields: vec![LayoutField {
                    offset: 0,
                    kind: FieldKind::Pointer {
                        pointee: *pointee,
                        slot: *semantics,
                    },
                }],
                variants: Vec::new(),
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: no_runtime_operations(),
            })
        }
        LayoutKind::RuntimeClosedSum { variants } => {
            tag_kind(variants.len())?;
            let mut variant_layouts = Vec::with_capacity(variants.len());
            let mut maximum_payload = 0u32;
            for variant in variants {
                let fields = runtime_slot_fields(variant, 4, &mut lookup)?;
                let payload_size = u32::try_from(variant.len())
                    .map_err(|_| LayoutError::ArithmeticOverflow)?
                    .checked_mul(8)
                    .ok_or(LayoutError::ArithmeticOverflow)?;
                maximum_payload = maximum_payload.max(payload_size);
                variant_layouts.push(VariantLayout {
                    fields,
                    payload_size,
                    payload_alignment: 4,
                });
            }
            let size = 4u32
                .checked_add(maximum_payload)
                .ok_or(LayoutError::ArithmeticOverflow)?;
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Fixed(size),
                alignment: 4,
                fields: vec![LayoutField {
                    offset: 0,
                    kind: FieldKind::Scalar(ScalarKind::U32),
                }],
                variants: variant_layouts,
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: no_runtime_operations(),
            })
        }
        LayoutKind::ClosureEnvironment { captures } => {
            for capture in captures {
                validate_runtime_slot(*capture, lookup(*capture))?;
            }
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                // Captures live in a typed Wasm-GC struct, not linear memory.
                size: LayoutSize::Fixed(0),
                alignment: 1,
                fields: Vec::new(),
                variants: Vec::new(),
                ownership: Vec::new(),
                header: HeaderLayout::None,
                anchor: RootAnchor::AllocationBase,
                operations: no_runtime_operations(),
            })
        }
        LayoutKind::ExposedRoot {
            body,
            prefix,
            anchor_bias,
        } => {
            if prefix.is_empty() {
                return Err(LayoutError::DescriptorInvariant(
                    "exposed-root layout requires a non-empty prefix",
                ));
            }
            let body_descriptor = lookup(*body).ok_or(LayoutError::UnknownLayout(*body))?;
            if matches!(body_descriptor.kind, LayoutKind::ExposedRoot { .. }) {
                return Err(LayoutError::DescriptorInvariant(
                    "exposed-root layouts cannot be nested",
                ));
            }
            let mut offset = 0;
            let mut alignment = 1;
            let mut fields = Vec::with_capacity(prefix.len() + 1);
            for slot in prefix {
                let (field_kind, size, field_alignment) =
                    runtime_prefix_field(*slot, &mut lookup)?;
                offset = align_up(offset, field_alignment)?;
                fields.push(LayoutField {
                    offset,
                    kind: field_kind,
                });
                offset = offset
                    .checked_add(size)
                    .ok_or(LayoutError::ArithmeticOverflow)?;
                alignment = alignment.max(field_alignment);
            }
            let expected_anchor = align_up(offset, body_descriptor.alignment)?;
            if expected_anchor == 0 || *anchor_bias != expected_anchor {
                return Err(LayoutError::DescriptorInvariant(
                    "exposed-root anchor does not match its checked prefix",
                ));
            }
            fields.push(LayoutField {
                offset: *anchor_bias,
                kind: FieldKind::Inline(*body),
            });
            alignment = alignment.max(body_descriptor.alignment);
            let size = match body_descriptor.size {
                LayoutSize::Fixed(size) => LayoutSize::Fixed(
                    anchor_bias
                        .checked_add(size)
                        .ok_or(LayoutError::ArithmeticOverflow)?,
                ),
                LayoutSize::Dynamic { base, stride } => LayoutSize::Dynamic {
                    base: anchor_bias
                        .checked_add(base)
                        .ok_or(LayoutError::ArithmeticOverflow)?,
                    stride,
                },
            };
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size,
                alignment,
                fields,
                variants: Vec::new(),
                ownership: Vec::new(),
                // Exposed-root operation semantics are deliberately not
                // inferred from a structural prefix. A later named runtime
                // class must opt into copy/drop/equality behavior explicitly.
                header: HeaderLayout::None,
                anchor: RootAnchor::Exposed {
                    allocation_offset: *anchor_bias,
                },
                operations: OperationShapes {
                    copy: OperationShape::None,
                    duplicate: OperationShape::None,
                    drop: OperationShape::None,
                    equality: OperationShape::None,
                    render: OperationShape::None,
                    serialization: OperationShape::None,
                },
            })
        }
    }
}

fn validate_runtime_dictionary_slot(
    id: LayoutId,
    descriptor: Option<&LayoutDescriptor>,
) -> Result<(), LayoutError> {
    let descriptor = descriptor.ok_or(LayoutError::UnknownLayout(id))?;
    let exact_family = matches!(
        descriptor.kind,
        LayoutKind::Scalar(
            ScalarKind::BoolValueSlot
                | ScalarKind::Int
                | ScalarKind::Float
                | ScalarKind::Duration,
        ) | LayoutKind::RuntimePointerValueSlot {
            semantics: PointerSlot::Owned,
            ..
        }
    );
    if !exact_family
        || descriptor.anchor != RootAnchor::AllocationBase
        || descriptor.size != LayoutSize::Fixed(8)
        || !descriptor.ownership.is_empty()
    {
        return Err(LayoutError::DescriptorInvariant(
            "runtime dictionary slots must be exact eight-byte scalar or owned pointer carriers",
        ));
    }
    Ok(())
}

fn validate_runtime_value(
    id: LayoutId,
    descriptor: Option<&LayoutDescriptor>,
) -> Result<(), LayoutError> {
    let descriptor = descriptor.ok_or(LayoutError::UnknownLayout(id))?;
    let exact_source_scalar = matches!(
        descriptor.kind,
        LayoutKind::Scalar(
            ScalarKind::Bool
                | ScalarKind::Int
                | ScalarKind::Float
                | ScalarKind::Duration
                | ScalarKind::U32,
        )
    );
    if !exact_source_scalar
        || descriptor.anchor != RootAnchor::AllocationBase
        || !matches!(descriptor.size, LayoutSize::Fixed(_))
        || !descriptor.ownership.is_empty()
    {
        return Err(LayoutError::DescriptorInvariant(
            "runtime value slots require an exact fixed source scalar layout",
        ));
    }
    Ok(())
}

fn validate_runtime_slot(
    id: LayoutId,
    descriptor: Option<&LayoutDescriptor>,
) -> Result<(), LayoutError> {
    let descriptor = descriptor.ok_or(LayoutError::UnknownLayout(id))?;
    let exact_kind = matches!(
        descriptor.kind,
        LayoutKind::RuntimeValueSlot { .. }
            | LayoutKind::RuntimePointerValueSlot {
                semantics: PointerSlot::Owned | PointerSlot::Borrowed,
                ..
            }
    );
    if !exact_kind
        || descriptor.anchor != RootAnchor::AllocationBase
        || descriptor.size != LayoutSize::Fixed(8)
        || descriptor.alignment != 4
    {
        return Err(LayoutError::DescriptorInvariant(
            "runtime aggregate fields require exact canonical eight-byte value slots",
        ));
    }
    Ok(())
}

fn runtime_slot_fields<'a>(
    slots: &[LayoutId],
    base: u32,
    lookup: &mut impl FnMut(LayoutId) -> Option<&'a LayoutDescriptor>,
) -> Result<Vec<LayoutField>, LayoutError> {
    let mut fields = Vec::with_capacity(slots.len());
    for (index, slot) in slots.iter().copied().enumerate() {
        validate_runtime_slot(slot, lookup(slot))?;
        let offset = u32::try_from(index)
            .map_err(|_| LayoutError::ArithmeticOverflow)?
            .checked_mul(8)
            .and_then(|delta| base.checked_add(delta))
            .ok_or(LayoutError::ArithmeticOverflow)?;
        fields.push(LayoutField {
            offset,
            kind: FieldKind::Inline(slot),
        });
    }
    Ok(fields)
}

fn no_runtime_operations() -> OperationShapes {
    OperationShapes {
        copy: OperationShape::None,
        duplicate: OperationShape::None,
        drop: OperationShape::None,
        equality: OperationShape::None,
        render: OperationShape::None,
        serialization: OperationShape::None,
    }
}

fn derive_anchor_bias<'a>(
    prefix: &[RuntimePrefixSlot],
    body_alignment: u32,
    mut lookup: impl FnMut(LayoutId) -> Option<&'a LayoutDescriptor>,
) -> Result<u32, LayoutError> {
    if prefix.is_empty() {
        return Err(LayoutError::DescriptorInvariant(
            "exposed-root layout requires a non-empty prefix",
        ));
    }
    let mut offset = 0;
    for slot in prefix {
        let (_, size, alignment) = runtime_prefix_field(*slot, &mut lookup)?;
        offset = align_up(offset, alignment)?;
        offset = offset
            .checked_add(size)
            .ok_or(LayoutError::ArithmeticOverflow)?;
    }
    let anchor = align_up(offset, body_alignment)?;
    if anchor == 0 {
        Err(LayoutError::DescriptorInvariant(
            "exposed-root anchor must follow its prefix",
        ))
    } else {
        Ok(anchor)
    }
}

fn runtime_prefix_field<'a>(
    slot: RuntimePrefixSlot,
    lookup: &mut impl FnMut(LayoutId) -> Option<&'a LayoutDescriptor>,
) -> Result<(FieldKind, u32, u32), LayoutError> {
    match slot {
        RuntimePrefixSlot::Scalar(scalar) => {
            Ok((FieldKind::Scalar(scalar), scalar.size(), scalar.alignment()))
        }
        RuntimePrefixSlot::Pointer { pointee, semantics } => {
            lookup(pointee).ok_or(LayoutError::UnknownLayout(pointee))?;
            Ok((
                FieldKind::Pointer {
                    pointee,
                    slot: semantics,
                },
                4,
                4,
            ))
        }
    }
}

fn aggregate_fields<'a>(
    children: &[LayoutId],
    base: u32,
    lookup: &mut impl FnMut(LayoutId) -> Option<&'a LayoutDescriptor>,
) -> Result<(Vec<LayoutField>, u32, u32), LayoutError> {
    let mut offset = 0;
    let mut alignment = 1;
    let mut fields = Vec::with_capacity(children.len());
    for child in children {
        let descriptor = lookup(*child).ok_or(LayoutError::UnknownLayout(*child))?;
        validate_source_inline_child(*child, descriptor)?;
        let size = match descriptor.size {
            LayoutSize::Fixed(size) => size,
            LayoutSize::Dynamic { .. } => return Err(LayoutError::DynamicInlineField(*child)),
        };
        offset = align_up(offset, descriptor.alignment)?;
        let field_offset = base
            .checked_add(offset)
            .ok_or(LayoutError::ArithmeticOverflow)?;
        fields.push(LayoutField {
            offset: field_offset,
            kind: match descriptor.kind {
                LayoutKind::Scalar(scalar) => FieldKind::Scalar(scalar),
                _ => FieldKind::Inline(*child),
            },
        });
        offset = offset
            .checked_add(size)
            .ok_or(LayoutError::ArithmeticOverflow)?;
        alignment = alignment.max(descriptor.alignment);
    }
    Ok((fields, align_up(offset, alignment)?, alignment))
}

fn validate_source_inline_child(
    id: LayoutId,
    descriptor: &LayoutDescriptor,
) -> Result<(), LayoutError> {
    if descriptor.anchor != RootAnchor::AllocationBase {
        return Err(LayoutError::DescriptorInvariant(
            "an exposed-root layout cannot be embedded inline",
        ));
    }
    if matches!(
        descriptor.kind,
        LayoutKind::Scalar(
            ScalarKind::RawByte | ScalarKind::BoolValueSlot | ScalarKind::V128,
        )
    ) {
        return Err(LayoutError::DescriptorInvariant(
            "a representation-only scalar requires a named runtime descriptor",
        ));
    }
    if matches!(descriptor.size, LayoutSize::Dynamic { .. }) {
        return Err(LayoutError::DynamicInlineField(id));
    }
    Ok(())
}

fn align_up(value: u32, alignment: u32) -> Result<u32, LayoutError> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(LayoutError::DescriptorInvariant(
            "alignment must be a non-zero power of two",
        ));
    }
    value
        .checked_add(alignment - 1)
        .map(|sum| sum & !(alignment - 1))
        .ok_or(LayoutError::ArithmeticOverflow)
}

fn tag_kind(variants: usize) -> Result<ScalarKind, LayoutError> {
    let maximum = variants
        .checked_sub(1)
        .ok_or(LayoutError::DescriptorInvariant(
            "closed sum has no variants",
        ))?;
    if maximum <= u8::MAX as usize {
        Ok(ScalarKind::Tag8)
    } else if maximum <= u16::MAX as usize {
        Ok(ScalarKind::Tag16)
    } else if maximum <= u32::MAX as usize {
        Ok(ScalarKind::Tag32)
    } else {
        Err(LayoutError::ArithmeticOverflow)
    }
}

#[derive(Default)]
struct Encoder {
    output: Vec<u8>,
}

impl Encoder {
    fn byte(&mut self, value: u8) {
        self.output.push(value);
    }

    fn bytes(&mut self, value: &[u8]) {
        self.output.extend_from_slice(value);
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    fn id(&mut self, id: LayoutId) {
        self.bytes(id.as_bytes());
    }

    fn len(&mut self, value: usize) {
        debug_assert!(value <= MAX_DESCRIPTOR_ITEMS);
        self.u32(value as u32);
    }

    fn ids(&mut self, ids: &[LayoutId]) {
        self.len(ids.len());
        for id in ids {
            self.id(*id);
        }
    }

    fn scalar(&mut self, scalar: ScalarKind) {
        self.byte(match scalar {
            ScalarKind::Bool => 0,
            ScalarKind::Int => 1,
            ScalarKind::Float => 2,
            ScalarKind::Duration => 3,
            ScalarKind::U32 => 4,
            ScalarKind::Tag8 => 5,
            ScalarKind::Tag16 => 6,
            ScalarKind::Tag32 => 7,
            ScalarKind::RawByte => 8,
            ScalarKind::V128 => 9,
            ScalarKind::BoolValueSlot => 10,
            ScalarKind::Callable => 11,
        });
    }

    fn pointer_slot(&mut self, slot: PointerSlot) {
        self.byte(match slot {
            PointerSlot::Owned => 0,
            PointerSlot::Borrowed => 1,
            PointerSlot::Raw => 2,
        });
    }

    fn runtime_prefix_slot(&mut self, slot: RuntimePrefixSlot) {
        match slot {
            RuntimePrefixSlot::Scalar(scalar) => {
                self.byte(0);
                self.scalar(scalar);
            }
            RuntimePrefixSlot::Pointer { pointee, semantics } => {
                self.byte(1);
                self.id(pointee);
                self.pointer_slot(semantics);
            }
        }
    }

    fn field(&mut self, field: &LayoutField) {
        self.u32(field.offset);
        match field.kind {
            FieldKind::Scalar(scalar) => {
                self.byte(0);
                self.scalar(scalar);
            }
            FieldKind::Inline(id) => {
                self.byte(1);
                self.id(id);
            }
            FieldKind::Pointer { pointee, slot } => {
                self.byte(2);
                self.id(pointee);
                self.pointer_slot(slot);
            }
        }
    }

    fn fields(&mut self, fields: &[LayoutField]) {
        self.len(fields.len());
        for field in fields {
            self.field(field);
        }
    }

    fn kind(&mut self, kind: &LayoutKind) {
        match kind {
            LayoutKind::Scalar(scalar) => {
                self.byte(0);
                self.scalar(*scalar);
            }
            LayoutKind::Tuple { fields } => {
                self.byte(1);
                self.ids(fields);
            }
            LayoutKind::PackedRecord { fields } => {
                self.byte(2);
                self.ids(fields);
            }
            LayoutKind::PackedList { element, rc } => {
                self.byte(3);
                self.id(*element);
                self.byte(match rc {
                    RcHeader::Required => 0,
                    RcHeader::Elided => 1,
                });
            }
            LayoutKind::ClosedSum { variants } => {
                self.byte(4);
                self.len(variants.len());
                for variant in variants {
                    self.ids(variant);
                }
            }
            LayoutKind::RuntimeBuffer { element } => {
                self.byte(5);
                self.id(*element);
            }
            LayoutKind::ExposedRoot {
                body,
                prefix,
                anchor_bias,
            } => {
                self.byte(6);
                self.id(*body);
                self.len(prefix.len());
                for slot in prefix {
                    self.runtime_prefix_slot(*slot);
                }
                self.u32(*anchor_bias);
            }
            LayoutKind::RuntimePointerBuffer { pointee, semantics } => {
                self.byte(7);
                self.id(*pointee);
                self.pointer_slot(*semantics);
            }
            LayoutKind::RuntimeDictionaryBody { key, value, row } => {
                self.byte(8);
                self.id(*key);
                self.id(*value);
                self.id(*row);
            }
            LayoutKind::RuntimeDictionaryRow { key, value } => {
                self.byte(15);
                self.id(*key);
                self.id(*value);
            }
            LayoutKind::RuntimeDictionaryIndex { key, value } => {
                self.byte(9);
                self.id(*key);
                self.id(*value);
            }
            LayoutKind::ScratchBuffer { element } => {
                self.byte(10);
                self.id(*element);
            }
            LayoutKind::RuntimeValueSlot { value } => {
                self.byte(11);
                self.id(*value);
            }
            LayoutKind::RuntimePointerValueSlot { pointee, semantics } => {
                self.byte(12);
                self.id(*pointee);
                self.pointer_slot(*semantics);
            }
            LayoutKind::RuntimeClosedSum { variants } => {
                self.byte(13);
                self.len(variants.len());
                for variant in variants {
                    self.ids(variant);
                }
            }
            LayoutKind::ClosureEnvironment { captures } => {
                self.byte(14);
                self.ids(captures);
            }
        }
    }

    fn operation(&mut self, operation: &OperationShape) {
        match operation {
            OperationShape::None => self.byte(0),
            OperationShape::Scalar(scalar) => {
                self.byte(1);
                self.scalar(*scalar);
            }
            OperationShape::Fields(fields) => {
                self.byte(2);
                self.ids(fields);
            }
            OperationShape::Variants { tag, variants } => {
                self.byte(3);
                self.scalar(*tag);
                self.len(variants.len());
                for variant in variants {
                    self.ids(variant);
                }
            }
            OperationShape::PackedElements { element, stride } => {
                self.byte(4);
                self.id(*element);
                self.u32(*stride);
            }
        }
    }

    fn descriptor(&mut self, descriptor: &LayoutDescriptor) {
        self.kind(&descriptor.kind);
        match descriptor.size {
            LayoutSize::Fixed(size) => {
                self.byte(0);
                self.u32(size);
            }
            LayoutSize::Dynamic { base, stride } => {
                self.byte(1);
                self.u32(base);
                self.u32(stride);
            }
        }
        self.u32(descriptor.alignment);
        self.fields(&descriptor.fields);
        self.len(descriptor.variants.len());
        for variant in &descriptor.variants {
            self.fields(&variant.fields);
            self.u32(variant.payload_size);
            self.u32(variant.payload_alignment);
        }
        self.len(descriptor.ownership.len());
        for position in &descriptor.ownership {
            match position {
                OwnershipPosition::RootBuffer => self.byte(0),
                OwnershipPosition::Field(path) => {
                    self.byte(1);
                    self.len(path.len());
                    for index in path {
                        self.u32(*index);
                    }
                }
            }
        }
        match descriptor.header {
            HeaderLayout::None => self.byte(0),
            HeaderLayout::PackedList {
                rc,
                length_offset,
                capacity_offset,
                data_offset,
            } => {
                self.byte(1);
                self.byte(match rc {
                    RcHeader::Required => 0,
                    RcHeader::Elided => 1,
                });
                self.u32(length_offset);
                self.u32(capacity_offset);
                self.u32(data_offset);
            }
        }
        for operation in [
            &descriptor.operations.copy,
            &descriptor.operations.duplicate,
            &descriptor.operations.drop,
            &descriptor.operations.equality,
            &descriptor.operations.render,
            &descriptor.operations.serialization,
        ] {
            self.operation(operation);
        }
    }
}

struct Decoder<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    fn byte(&mut self) -> Result<u8, LayoutError> {
        let byte = self
            .input
            .get(self.offset)
            .copied()
            .ok_or(LayoutError::Decode {
                offset: self.offset,
                reason: "unexpected end of descriptor",
            })?;
        self.offset += 1;
        Ok(byte)
    }

    fn bytes(&mut self, count: usize) -> Result<&'a [u8], LayoutError> {
        let end = self.offset.checked_add(count).ok_or(LayoutError::Decode {
            offset: self.offset,
            reason: "descriptor offset overflow",
        })?;
        let bytes = self
            .input
            .get(self.offset..end)
            .ok_or(LayoutError::Decode {
                offset: self.offset,
                reason: "unexpected end of descriptor",
            })?;
        self.offset = end;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32, LayoutError> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn len(&mut self) -> Result<usize, LayoutError> {
        let length = self.u32()? as usize;
        if length > MAX_DESCRIPTOR_ITEMS {
            return Err(LayoutError::Decode {
                offset: self.offset - 4,
                reason: "descriptor item count exceeds limit",
            });
        }
        Ok(length)
    }

    fn id(&mut self) -> Result<LayoutId, LayoutError> {
        let mut id = [0; 32];
        id.copy_from_slice(self.bytes(32)?);
        Ok(LayoutId(id))
    }

    fn ids(&mut self) -> Result<Vec<LayoutId>, LayoutError> {
        let count = self.len()?;
        let mut ids = Vec::with_capacity(count.min(self.input.len() / 32));
        for _ in 0..count {
            ids.push(self.id()?);
        }
        Ok(ids)
    }

    fn scalar(&mut self) -> Result<ScalarKind, LayoutError> {
        let offset = self.offset;
        match self.byte()? {
            0 => Ok(ScalarKind::Bool),
            1 => Ok(ScalarKind::Int),
            2 => Ok(ScalarKind::Float),
            3 => Ok(ScalarKind::Duration),
            4 => Ok(ScalarKind::U32),
            5 => Ok(ScalarKind::Tag8),
            6 => Ok(ScalarKind::Tag16),
            7 => Ok(ScalarKind::Tag32),
            8 => Ok(ScalarKind::RawByte),
            9 => Ok(ScalarKind::V128),
            10 => Ok(ScalarKind::BoolValueSlot),
            11 => Ok(ScalarKind::Callable),
            _ => Err(LayoutError::Decode {
                offset,
                reason: "unknown scalar kind",
            }),
        }
    }

    fn pointer_slot(&mut self) -> Result<PointerSlot, LayoutError> {
        let offset = self.offset;
        match self.byte()? {
            0 => Ok(PointerSlot::Owned),
            1 => Ok(PointerSlot::Borrowed),
            2 => Ok(PointerSlot::Raw),
            _ => Err(LayoutError::Decode {
                offset,
                reason: "unknown pointer slot semantics",
            }),
        }
    }

    fn runtime_prefix_slot(&mut self) -> Result<RuntimePrefixSlot, LayoutError> {
        let offset = self.offset;
        match self.byte()? {
            0 => Ok(RuntimePrefixSlot::Scalar(self.scalar()?)),
            1 => Ok(RuntimePrefixSlot::Pointer {
                pointee: self.id()?,
                semantics: self.pointer_slot()?,
            }),
            _ => Err(LayoutError::Decode {
                offset,
                reason: "unknown runtime prefix slot",
            }),
        }
    }

    fn rc(&mut self) -> Result<RcHeader, LayoutError> {
        let offset = self.offset;
        match self.byte()? {
            0 => Ok(RcHeader::Required),
            1 => Ok(RcHeader::Elided),
            _ => Err(LayoutError::Decode {
                offset,
                reason: "unknown RC header kind",
            }),
        }
    }

    fn field(&mut self) -> Result<LayoutField, LayoutError> {
        let offset = self.u32()?;
        let tag_offset = self.offset;
        let kind = match self.byte()? {
            0 => FieldKind::Scalar(self.scalar()?),
            1 => FieldKind::Inline(self.id()?),
            2 => FieldKind::Pointer {
                pointee: self.id()?,
                slot: self.pointer_slot()?,
            },
            _ => {
                return Err(LayoutError::Decode {
                    offset: tag_offset,
                    reason: "unknown field kind",
                });
            }
        };
        Ok(LayoutField { offset, kind })
    }

    fn fields(&mut self) -> Result<Vec<LayoutField>, LayoutError> {
        let count = self.len()?;
        let mut fields = Vec::with_capacity(count.min(self.input.len()));
        for _ in 0..count {
            fields.push(self.field()?);
        }
        Ok(fields)
    }

    fn kind(&mut self) -> Result<LayoutKind, LayoutError> {
        let offset = self.offset;
        match self.byte()? {
            0 => Ok(LayoutKind::Scalar(self.scalar()?)),
            1 => Ok(LayoutKind::Tuple {
                fields: self.ids()?,
            }),
            2 => Ok(LayoutKind::PackedRecord {
                fields: self.ids()?,
            }),
            3 => Ok(LayoutKind::PackedList {
                element: self.id()?,
                rc: self.rc()?,
            }),
            4 => {
                let count = self.len()?;
                let mut variants = Vec::with_capacity(count.min(self.input.len()));
                for _ in 0..count {
                    variants.push(self.ids()?);
                }
                Ok(LayoutKind::ClosedSum { variants })
            }
            5 => Ok(LayoutKind::RuntimeBuffer {
                element: self.id()?,
            }),
            6 => {
                let body = self.id()?;
                let count = self.len()?;
                let mut prefix = Vec::with_capacity(count.min(self.input.len()));
                for _ in 0..count {
                    prefix.push(self.runtime_prefix_slot()?);
                }
                Ok(LayoutKind::ExposedRoot {
                    body,
                    prefix,
                    anchor_bias: self.u32()?,
                })
            }
            7 => Ok(LayoutKind::RuntimePointerBuffer {
                pointee: self.id()?,
                semantics: self.pointer_slot()?,
            }),
            8 => Ok(LayoutKind::RuntimeDictionaryBody {
                key: self.id()?,
                value: self.id()?,
                row: self.id()?,
            }),
            15 => Ok(LayoutKind::RuntimeDictionaryRow {
                key: self.id()?,
                value: self.id()?,
            }),
            9 => Ok(LayoutKind::RuntimeDictionaryIndex {
                key: self.id()?,
                value: self.id()?,
            }),
            10 => Ok(LayoutKind::ScratchBuffer {
                element: self.id()?,
            }),
            11 => Ok(LayoutKind::RuntimeValueSlot {
                value: self.id()?,
            }),
            12 => Ok(LayoutKind::RuntimePointerValueSlot {
                pointee: self.id()?,
                semantics: self.pointer_slot()?,
            }),
            13 => {
                let count = self.len()?;
                let mut variants = Vec::with_capacity(count.min(self.input.len()));
                for _ in 0..count {
                    variants.push(self.ids()?);
                }
                Ok(LayoutKind::RuntimeClosedSum { variants })
            }
            14 => Ok(LayoutKind::ClosureEnvironment {
                captures: self.ids()?,
            }),
            _ => Err(LayoutError::Decode {
                offset,
                reason: "unknown layout kind",
            }),
        }
    }

    fn operation(&mut self) -> Result<OperationShape, LayoutError> {
        let offset = self.offset;
        match self.byte()? {
            0 => Ok(OperationShape::None),
            1 => Ok(OperationShape::Scalar(self.scalar()?)),
            2 => Ok(OperationShape::Fields(self.ids()?)),
            3 => {
                let tag = self.scalar()?;
                let count = self.len()?;
                let mut variants = Vec::with_capacity(count.min(self.input.len()));
                for _ in 0..count {
                    variants.push(self.ids()?);
                }
                Ok(OperationShape::Variants { tag, variants })
            }
            4 => Ok(OperationShape::PackedElements {
                element: self.id()?,
                stride: self.u32()?,
            }),
            _ => Err(LayoutError::Decode {
                offset,
                reason: "unknown operation shape",
            }),
        }
    }

    fn descriptor(&mut self, schema_version: u32) -> Result<LayoutDescriptor, LayoutError> {
        let kind = self.kind()?;
        let size_offset = self.offset;
        let size = match self.byte()? {
            0 => LayoutSize::Fixed(self.u32()?),
            1 => LayoutSize::Dynamic {
                base: self.u32()?,
                stride: self.u32()?,
            },
            _ => {
                return Err(LayoutError::Decode {
                    offset: size_offset,
                    reason: "unknown layout size kind",
                });
            }
        };
        let alignment = self.u32()?;
        let fields = self.fields()?;
        let variant_count = self.len()?;
        let mut variants = Vec::with_capacity(variant_count.min(self.input.len()));
        for _ in 0..variant_count {
            variants.push(VariantLayout {
                fields: self.fields()?,
                payload_size: self.u32()?,
                payload_alignment: self.u32()?,
            });
        }
        let ownership_count = self.len()?;
        let mut ownership = Vec::with_capacity(ownership_count.min(self.input.len()));
        for _ in 0..ownership_count {
            let offset = self.offset;
            ownership.push(match self.byte()? {
                0 => OwnershipPosition::RootBuffer,
                1 => {
                    let count = self.len()?;
                    let mut path = Vec::with_capacity(count.min(self.input.len() / 4));
                    for _ in 0..count {
                        path.push(self.u32()?);
                    }
                    OwnershipPosition::Field(path)
                }
                _ => {
                    return Err(LayoutError::Decode {
                        offset,
                        reason: "unknown ownership position",
                    });
                }
            });
        }
        let header_offset = self.offset;
        let header = match self.byte()? {
            0 => HeaderLayout::None,
            1 => HeaderLayout::PackedList {
                rc: self.rc()?,
                length_offset: self.u32()?,
                capacity_offset: self.u32()?,
                data_offset: self.u32()?,
            },
            _ => {
                return Err(LayoutError::Decode {
                    offset: header_offset,
                    reason: "unknown header layout",
                });
            }
        };
        let operations = OperationShapes {
            copy: self.operation()?,
            duplicate: self.operation()?,
            drop: self.operation()?,
            equality: self.operation()?,
            render: self.operation()?,
            serialization: self.operation()?,
        };
        let anchor = match &kind {
            LayoutKind::ExposedRoot { anchor_bias, .. } => RootAnchor::Exposed {
                allocation_offset: *anchor_bias,
            },
            _ => RootAnchor::AllocationBase,
        };
        Ok(LayoutDescriptor {
            schema_version,
            kind,
            size,
            alignment,
            fields,
            variants,
            ownership,
            header,
            anchor,
            operations,
        })
    }
}

fn decode_descriptor(bytes: &[u8]) -> Result<LayoutDescriptor, LayoutError> {
    let mut decoder = Decoder::new(bytes);
    let magic = decoder.bytes(LAYOUT_MAGIC.len())?;
    if magic != LAYOUT_MAGIC {
        return Err(LayoutError::Decode {
            offset: 0,
            reason: "invalid layout magic",
        });
    }
    let schema_version = decoder.u32()?;
    if schema_version != LAYOUT_SCHEMA_VERSION {
        return Err(LayoutError::UnsupportedSchema {
            found: schema_version,
        });
    }
    let descriptor = decoder.descriptor(schema_version)?;
    if decoder.offset != bytes.len() {
        return Err(LayoutError::TrailingBytes {
            offset: decoder.offset,
        });
    }
    Ok(descriptor)
}

fn sha256(input: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0u32; 64];
        for (index, bytes) in chunk.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes(bytes.try_into().expect("four-byte SHA word"));
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let upper_e = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let first = h
                .wrapping_add(upper_e)
                .wrapping_add(choose)
                .wrapping_add(ROUND[index])
                .wrapping_add(words[index]);
            let upper_a = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let second = upper_a.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(first);
            d = c;
            c = b;
            b = a;
            a = first.wrapping_add(second);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut digest = [0u8; 32];
    for (word, bytes) in state.iter().zip(digest.chunks_exact_mut(4)) {
        bytes.copy_from_slice(&word.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {

    // RFC-0162: ordinary lowering must refuse every runtime layout. These two
    // predicates are the single choke point the codegen field-kind, field-read
    // and field-store paths all consult, so pinning them pins the refusal.
    //
    // This is the property the slice claims and the one a plain match on
    // `FieldKind::Pointer` does NOT give: a runtime value slot carries its
    // payload as an ordinary `FieldKind::Inline` child, so without this check a
    // consumer would treat it as an anonymous byte range and emit a load,
    // store, or `memory.copy` of the wrong shape.
    #[test]
    fn representation_only_scalars_are_refused_by_ordinary_lowering() {
        for ordinary in [
            ScalarKind::Bool,
            ScalarKind::Int,
            ScalarKind::Float,
            ScalarKind::Duration,
            ScalarKind::U32,
            ScalarKind::Tag8,
            ScalarKind::Tag16,
            ScalarKind::Tag32,
            ScalarKind::Callable,
        ] {
            assert!(
                ordinary.is_ordinary_lowering_target(),
                "{ordinary:?} is an ordinary scalar and must keep its direct load",
            );
        }
        for representation_only in
            [ScalarKind::RawByte, ScalarKind::BoolValueSlot, ScalarKind::V128]
        {
            assert!(
                !representation_only.is_ordinary_lowering_target(),
                "{representation_only:?} must be reached only through the checked place builder",
            );
        }
    }

    /// A callable lane is a closure record's slot 0 and nothing else: it
    /// interns as a record field and refuses as a list or buffer element,
    /// where the element ABI has no way to name the signature it must carry.
    #[test]
    fn callable_lane_is_a_record_slot_and_never_an_element() {
        let mut interner = LayoutInterner::default();
        let callable = interner.intern_scalar(ScalarKind::Callable).expect("callable lane");
        let capture = interner.intern_scalar(ScalarKind::Int).expect("int lane");
        let record = interner
            .intern_packed_record(vec![callable, capture])
            .expect("a callable lane is a valid record slot");
        let descriptor = interner.get(record).expect("record descriptor");
        assert_eq!(descriptor.fields()[0].kind(), FieldKind::Scalar(ScalarKind::Callable));
        assert_eq!(descriptor.fields()[0].offset(), 0);
        assert_eq!(descriptor.size(), LayoutSize::Fixed(16));
        assert!(matches!(
            interner.intern_packed_list(callable, RcHeader::Required),
            Err(LayoutError::DescriptorInvariant(_))
        ));
        assert!(matches!(
            interner.intern_runtime_buffer(callable),
            Err(LayoutError::DescriptorInvariant(_))
        ));
    }

    #[test]
    fn runtime_layouts_are_refused_as_ordinary_inline_fields() {
        let mut interner = LayoutInterner::default();
        let int = interner.intern_scalar(ScalarKind::Int).expect("scalar");

        // An ordinary scalar stays materializable inline.
        assert!(
            interner.get(int).expect("descriptor").kind().is_ordinary_inline_target(),
            "ordinary layouts keep inline lowering",
        );

        // A runtime value slot does not, even though its payload is an
        // ordinary fixed-size scalar reached as FieldKind::Inline.
        let slot = interner.intern_runtime_value_slot(int).expect("runtime value slot");
        let descriptor = interner.get(slot).expect("descriptor");
        assert!(
            !descriptor.kind().is_ordinary_inline_target(),
            "a runtime value slot must never be lowered as an inline byte range",
        );
        assert!(
            matches!(descriptor.fields()[0].kind(), FieldKind::Inline(child) if child == int),
            "the payload really is an ordinary Inline field -- which is why the \
             kind check, not the field kind, is what refuses it",
        );
    }
    use super::*;



    #[test]
    fn dictionary_pointer_carrier_requires_exact_owned_semantics() {
        let mut interner = LayoutInterner::new();
        let raw = interner.intern_scalar(ScalarKind::RawByte).unwrap();
        let bytes = interner.intern_runtime_buffer(raw).unwrap();
        let owned = interner
            .intern_runtime_pointer_value_slot(bytes, PointerSlot::Owned)
            .unwrap();
        let borrowed = interner
            .intern_runtime_pointer_value_slot(bytes, PointerSlot::Borrowed)
            .unwrap();
        assert!(interner
            .intern_runtime_dictionary_body(owned, owned)
            .is_ok());
        assert_eq!(
            interner.intern_runtime_dictionary_body(borrowed, owned),
            Err(LayoutError::DescriptorInvariant(
                "runtime dictionary slots must be exact eight-byte scalar or owned pointer carriers"
            )),
        );
    }

    #[test]
    fn sha256_implementation_matches_the_standard_vector() {
        let digest = LayoutId::from_bytes(sha256(b"abc"));
        assert_eq!(
            digest.to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn release_validation_rejects_forged_scalar_alignment() {
        let mut descriptor = build_descriptor(LayoutKind::Scalar(ScalarKind::Int), |_| None)
            .expect("scalar descriptor");
        descriptor.alignment = 3;
        let interner = LayoutInterner::new();
        assert_eq!(
            interner.validate_descriptor(&descriptor),
            Err(LayoutError::DescriptorInvariant(
                "alignment must be a non-zero power of two"
            ))
        );
    }

    #[test]
    fn closed_sum_live_extent_is_selected_from_arbitrary_variant_fields() {
        let mut interner = LayoutInterner::new();
        let int = interner.intern_scalar(ScalarKind::Int).unwrap();
        let slot = interner.intern_runtime_value_slot(int).unwrap();
        let sum = interner
            .intern_runtime_closed_sum(vec![vec![], vec![slot], vec![slot, slot]])
            .unwrap();

        assert_eq!(interner.get(sum).unwrap().size(), LayoutSize::Fixed(20));
        assert_eq!(interner.variant_live_extent(sum, 0), Ok(4));
        assert_eq!(interner.variant_live_extent(sum, 1), Ok(12));
        assert_eq!(interner.variant_live_extent(sum, 2), Ok(20));
        assert_eq!(
            interner.variant_live_extent(sum, 3),
            Err(LayoutError::DescriptorInvariant(
                "closed-sum variant index is out of range"
            ))
        );
        assert_eq!(
            interner.variant_live_extent(slot, 0),
            Err(LayoutError::DescriptorInvariant(
                "variant extent requires a closed-sum layout"
            ))
        );
    }

    #[test]
    fn closed_sum_metadata_selects_canonical_scalar_and_nested_fields() {
        let mut interner = LayoutInterner::new();
        let boolean = interner.intern_scalar(ScalarKind::Bool).unwrap();
        let int = interner.intern_scalar(ScalarKind::Int).unwrap();
        let nested = interner
            .intern_kind(LayoutKind::Tuple {
                fields: vec![boolean, int],
            })
            .unwrap();
        let sum = interner
            .intern_kind(LayoutKind::ClosedSum {
                variants: vec![vec![], vec![boolean], vec![int, nested]],
            })
            .unwrap();

        let tag = interner.closed_sum_tag_metadata(sum).unwrap();
        assert_eq!(tag.layout(), sum);
        assert_eq!(tag.offset(), 0);
        assert_eq!(tag.width(), 1);
        assert_eq!(tag.kind(), FieldKind::Scalar(ScalarKind::Tag8));

        let scalar = interner
            .closed_sum_variant_field_metadata(sum, 1, 0)
            .unwrap();
        assert_eq!(scalar.layout(), sum);
        assert_eq!(scalar.variant(), 1);
        assert_eq!(scalar.field(), 0);
        assert_eq!(scalar.child_layout(), boolean);
        assert_eq!(scalar.offset(), 8);
        assert_eq!(scalar.width(), 1);
        assert_eq!(scalar.kind(), FieldKind::Scalar(ScalarKind::Bool));

        let aggregate = interner
            .closed_sum_variant_field_metadata(sum, 2, 1)
            .unwrap();
        assert_eq!(aggregate.child_layout(), nested);
        assert_eq!(aggregate.offset(), 16);
        assert_eq!(aggregate.width(), 16);
        assert_eq!(aggregate.kind(), FieldKind::Inline(nested));
        assert_eq!(interner.variant_live_extent(sum, 0), Ok(8));
        assert_eq!(interner.variant_live_extent(sum, 1), Ok(16));
        assert_eq!(interner.variant_live_extent(sum, 2), Ok(32));
    }

    #[test]
    fn runtime_sum_metadata_preserves_value_and_pointer_carrier_identity() {
        let mut interner = LayoutInterner::new();
        let byte = interner.intern_scalar(ScalarKind::RawByte).unwrap();
        let pointee = interner.intern_runtime_buffer(byte).unwrap();
        let int = interner.intern_scalar(ScalarKind::Int).unwrap();
        let value_carrier = interner.intern_runtime_value_slot(int).unwrap();
        let pointer_carrier = interner
            .intern_runtime_pointer_value_slot(pointee, PointerSlot::Owned)
            .unwrap();
        let sum = interner
            .intern_runtime_closed_sum(vec![
                vec![],
                vec![value_carrier],
                vec![pointer_carrier, value_carrier],
            ])
            .unwrap();

        assert_eq!(
            interner.closed_sum_tag_metadata(sum).unwrap().kind(),
            FieldKind::Scalar(ScalarKind::U32)
        );
        assert_eq!(interner.closed_sum_tag_metadata(sum).unwrap().width(), 4);

        let pointer = interner
            .closed_sum_variant_field_metadata(sum, 2, 0)
            .unwrap();
        assert_eq!(pointer.child_layout(), pointer_carrier);
        assert_eq!(pointer.offset(), 4);
        assert_eq!(pointer.width(), 8);
        assert_eq!(pointer.kind(), FieldKind::Inline(pointer_carrier));

        let value = interner
            .closed_sum_variant_field_metadata(sum, 2, 1)
            .unwrap();
        assert_eq!(value.child_layout(), value_carrier);
        assert_eq!(value.offset(), 12);
        assert_eq!(value.width(), 8);
        assert_eq!(value.kind(), FieldKind::Inline(value_carrier));

        let canonical = interner.get(sum).unwrap().canonical_bytes();
        let decoded = interner.decode_canonical(&canonical).unwrap();
        assert_eq!(decoded.layout_id(), sum);
        assert_eq!(&decoded, interner.get(sum).unwrap());
    }

    #[test]
    fn closed_sum_metadata_rejects_wrong_ordinals_layouts_and_malformed_graphs() {
        let mut interner = LayoutInterner::new();
        let int = interner.intern_scalar(ScalarKind::Int).unwrap();
        let carrier = interner.intern_runtime_value_slot(int).unwrap();
        let sum = interner
            .intern_runtime_closed_sum(vec![vec![], vec![carrier], vec![carrier, carrier]])
            .unwrap();

        assert_eq!(
            interner.closed_sum_tag_metadata(carrier),
            Err(LayoutError::DescriptorInvariant(
                "sum metadata requires a closed-sum layout"
            ))
        );
        assert_eq!(
            interner.closed_sum_variant_field_metadata(sum, 3, 0),
            Err(LayoutError::DescriptorInvariant(
                "closed-sum variant index is out of range"
            ))
        );
        assert_eq!(
            interner.closed_sum_variant_field_metadata(sum, 0, 0),
            Err(LayoutError::DescriptorInvariant(
                "closed-sum field index is out of range"
            ))
        );
        assert_eq!(
            interner.closed_sum_variant_field_metadata(sum, 1, 1),
            Err(LayoutError::DescriptorInvariant(
                "closed-sum field index is out of range"
            ))
        );
        assert_eq!(
            interner.closed_sum_variant_field_metadata(carrier, 0, 0),
            Err(LayoutError::DescriptorInvariant(
                "sum metadata requires a closed-sum layout"
            ))
        );

        interner
            .descriptors
            .get_mut(&sum)
            .unwrap()
            .variants[2]
            .fields[0]
            .offset = 5;
        assert_eq!(
            interner.closed_sum_variant_field_metadata(sum, 2, 0),
            Err(LayoutError::DescriptorInvariant(
                "size, alignment, fields, header, ownership, or operation shape disagrees with kind"
            ))
        );
        assert_eq!(
            interner.closed_sum_tag_metadata(sum),
            Err(LayoutError::DescriptorInvariant(
                "size, alignment, fields, header, ownership, or operation shape disagrees with kind"
            ))
        );
    }

    #[test]
    fn release_validation_rejects_forged_size_header_and_unknown_child() {
        let interner = LayoutInterner::new();
        let mut descriptor = build_descriptor(LayoutKind::Scalar(ScalarKind::Int), |_| None)
            .expect("scalar descriptor");
        descriptor.size = LayoutSize::Fixed(4);
        assert!(matches!(
            interner.validate_descriptor(&descriptor),
            Err(LayoutError::DescriptorInvariant(_))
        ));

        let mut descriptor = build_descriptor(LayoutKind::Scalar(ScalarKind::Int), |_| None)
            .expect("scalar descriptor");
        descriptor.header = HeaderLayout::PackedList {
            rc: RcHeader::Required,
            length_offset: 0,
            capacity_offset: 4,
            data_offset: 8,
        };
        assert!(matches!(
            interner.validate_descriptor(&descriptor),
            Err(LayoutError::DescriptorInvariant(_))
        ));

        let missing = LayoutId::from_bytes([9; 32]);
        assert_eq!(
            build_descriptor(
                LayoutKind::Tuple {
                    fields: vec![missing]
                },
                |_| None
            ),
            Err(LayoutError::UnknownLayout(missing))
        );
    }

    #[test]
    fn release_validation_rejects_malformed_runtime_prefixes_and_anchors() {
        let mut interner = LayoutInterner::new();
        let byte = interner.intern_scalar(ScalarKind::RawByte).unwrap();
        let body = interner.intern_runtime_buffer(byte).unwrap();
        let exposed = interner
            .intern_exposed_root(
                body,
                vec![RuntimePrefixSlot::Pointer {
                    pointee: body,
                    semantics: PointerSlot::Borrowed,
                }],
            )
            .unwrap();

        let mut forged_anchor = interner.get(exposed).unwrap().clone();
        forged_anchor.anchor = RootAnchor::AllocationBase;
        assert!(matches!(
            interner.validate_descriptor(&forged_anchor),
            Err(LayoutError::DescriptorInvariant(_))
        ));

        let mut overlapping = interner.get(exposed).unwrap().clone();
        overlapping.fields[1].offset = 0;
        assert!(matches!(
            interner.validate_descriptor(&overlapping),
            Err(LayoutError::DescriptorInvariant(_))
        ));

        assert_eq!(
            interner.intern_exposed_root(body, Vec::new()),
            Err(LayoutError::DescriptorInvariant(
                "exposed-root layout requires a non-empty prefix"
            )),
        );
        assert_eq!(
            interner.intern_exposed_root(
                exposed,
                vec![RuntimePrefixSlot::Scalar(ScalarKind::U32)],
            ),
            Err(LayoutError::DescriptorInvariant(
                "exposed-root layouts cannot be nested"
            )),
        );

        assert_eq!(
            build_descriptor(
                LayoutKind::ExposedRoot {
                    body,
                    prefix: vec![RuntimePrefixSlot::Scalar(ScalarKind::RawByte)],
                    anchor_bias: 2,
                },
                |id| interner.get(id),
            ),
            Err(LayoutError::DescriptorInvariant(
                "exposed-root anchor does not match its checked prefix"
            )),
        );
    }

    #[test]
    fn release_validation_rejects_forged_runtime_pointer_buffer_shape() {
        let mut interner = LayoutInterner::new();
        let pointee = interner.intern_scalar(ScalarKind::Int).unwrap();
        let buffer = interner
            .intern_runtime_pointer_buffer(pointee, PointerSlot::Borrowed)
            .unwrap();
        let descriptor = interner.get(buffer).unwrap().clone();

        let mut wrong_stride = descriptor.clone();
        wrong_stride.size = LayoutSize::Dynamic { base: 4, stride: 4 };
        assert!(matches!(
            interner.validate_descriptor(&wrong_stride),
            Err(LayoutError::DescriptorInvariant(_))
        ));
        assert!(matches!(
            interner.decode_canonical(&wrong_stride.canonical_bytes()),
            Err(LayoutError::DescriptorInvariant(_))
        ));

        let mut wrong_width = descriptor.clone();
        wrong_width.fields[1].kind = FieldKind::Scalar(ScalarKind::Int);
        assert!(matches!(
            interner.validate_descriptor(&wrong_width),
            Err(LayoutError::DescriptorInvariant(_))
        ));

        let mut wrong_root = descriptor.clone();
        wrong_root.anchor = RootAnchor::Exposed { allocation_offset: 4 };
        assert!(matches!(
            interner.validate_descriptor(&wrong_root),
            Err(LayoutError::DescriptorInvariant(_))
        ));

        let mut wrong_offset = descriptor.clone();
        wrong_offset.fields[1].offset = 8;
        assert!(matches!(
            interner.validate_descriptor(&wrong_offset),
            Err(LayoutError::DescriptorInvariant(_))
        ));

        let canonical = descriptor.canonical_bytes();
        assert_eq!(interner.decode_canonical(&canonical).unwrap(), descriptor);
        assert_eq!(descriptor.layout_id(), buffer);
    }

    #[test]
    fn runtime_descriptors_reject_dangling_pointers_and_arithmetic_overflow() {
        let body_descriptor =
            build_descriptor(LayoutKind::Scalar(ScalarKind::RawByte), |_| None).unwrap();
        let body = body_descriptor.layout_id();
        let missing = LayoutId::from_bytes([0x5a; 32]);
        assert_eq!(
            build_descriptor(
                LayoutKind::ExposedRoot {
                    body,
                    prefix: vec![RuntimePrefixSlot::Pointer {
                        pointee: missing,
                        semantics: PointerSlot::Raw,
                    }],
                    anchor_bias: 4,
                },
                |id| (id == body).then_some(&body_descriptor),
            ),
            Err(LayoutError::UnknownLayout(missing)),
        );

        let mut huge_body = body_descriptor.clone();
        huge_body.size = LayoutSize::Fixed(u32::MAX);
        assert_eq!(
            build_descriptor(
                LayoutKind::ExposedRoot {
                    body,
                    prefix: vec![RuntimePrefixSlot::Scalar(ScalarKind::RawByte)],
                    anchor_bias: 1,
                },
                |id| (id == body).then_some(&huge_body),
            ),
            Err(LayoutError::ArithmeticOverflow),
        );
    }

    #[test]
    fn runtime_only_and_exposed_layouts_cannot_launder_through_source_aggregates() {
        let mut interner = LayoutInterner::new();
        let byte = interner.intern_scalar(ScalarKind::RawByte).unwrap();
        let bool_value_slot = interner
            .intern_scalar(ScalarKind::BoolValueSlot)
            .unwrap();
        assert_eq!(
            build_descriptor(
                LayoutKind::Tuple { fields: vec![byte] },
                |id| interner.get(id),
            ),
            Err(LayoutError::DescriptorInvariant(
                "a representation-only scalar requires a named runtime descriptor"
            )),
        );
        assert_eq!(
            build_descriptor(
                LayoutKind::PackedList {
                    element: byte,
                    rc: RcHeader::Required,
                },
                |id| interner.get(id),
            ),
            Err(LayoutError::DescriptorInvariant(
                "a representation-only scalar requires a named runtime descriptor"
            )),
        );
        assert_eq!(
            build_descriptor(
                LayoutKind::Tuple {
                    fields: vec![bool_value_slot],
                },
                |id| interner.get(id),
            ),
            Err(LayoutError::DescriptorInvariant(
                "a representation-only scalar requires a named runtime descriptor"
            )),
        );

        let exposed = interner
            .intern_exposed_root(
                byte,
                vec![RuntimePrefixSlot::Pointer {
                    pointee: byte,
                    semantics: PointerSlot::Raw,
                }],
            )
            .unwrap();
        assert_eq!(
            build_descriptor(
                LayoutKind::Tuple {
                    fields: vec![exposed],
                },
                |id| interner.get(id),
            ),
            Err(LayoutError::DescriptorInvariant(
                "an exposed-root layout cannot be embedded inline"
            )),
        );

        let forged = LayoutDescriptor {
            schema_version: LAYOUT_SCHEMA_VERSION,
            kind: LayoutKind::Tuple {
                fields: vec![exposed],
            },
            size: LayoutSize::Fixed(8),
            alignment: 4,
            fields: vec![LayoutField {
                offset: 0,
                kind: FieldKind::Inline(exposed),
            }],
            variants: Vec::new(),
            ownership: Vec::new(),
            header: HeaderLayout::None,
            anchor: RootAnchor::AllocationBase,
            operations: OperationShapes {
                copy: OperationShape::Fields(vec![exposed]),
                duplicate: OperationShape::Fields(vec![exposed]),
                drop: OperationShape::Fields(vec![exposed]),
                equality: OperationShape::Fields(vec![exposed]),
                render: OperationShape::Fields(vec![exposed]),
                serialization: OperationShape::Fields(vec![exposed]),
            },
        };
        assert_eq!(
            interner.decode_canonical(&forged.canonical_bytes()),
            Err(LayoutError::DescriptorInvariant(
                "an exposed-root layout cannot be embedded inline"
            )),
        );
    }
}
