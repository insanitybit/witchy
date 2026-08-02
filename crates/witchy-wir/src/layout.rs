//! Shared compiled-backend layout facts.
//!
//! These constants are part of the ABI between WIR helper generation, lowering,
//! and the wasmtime host runtime. Keep one home here so checked-heap and
//! sanitizer instrumentation cannot silently drift across crates.

use std::collections::BTreeMap;
use std::fmt;

use witchy_syntax::ast::TypeDef;

/// First guest-data byte. The compiled backend leaves the low address range
/// reserved for null/sentinel values and starts static data here.
pub const DATA_BASE: u32 = 8;

/// Bytes in the tag/length header that starts every slot-backed aggregate:
/// records, tuples, lists, and enum payload blocks.
pub(crate) const SLOT_HEADER_SIZE: i32 = 4;

/// Bytes in one universal value slot. Scalars are stored as i64/f64-width
/// values, and pointers/bools are widened into the same slot at aggregate
/// boundaries.
pub(crate) const VALUE_SLOT_SIZE: i32 = 8;

/// (RFC-0023) Trailing redzone size, in bytes, reserved after each checked
/// allocation. The guest allocator reserves exactly this many bytes and the
/// host poisons/sweeps exactly this many bytes at `[end, end + HEAP_REDZONE)`.
pub const HEAP_REDZONE: usize = 8;

/// The alloc-size header word (`ptr-4`, written by `$rc_alloc`) holds the
/// allocated size in its low 24 bits; the high 8 bits are reserved for the
/// debug type tag.
pub(crate) const RC_SIZE_MASK: i32 = 0x00FF_FFFF;

/// Total byte size of a slot-backed aggregate with `slots` payload fields.
pub(crate) const fn slot_record_size(slots: usize) -> i32 {
    SLOT_HEADER_SIZE + VALUE_SLOT_SIZE * slots as i32
}

/// Byte offset of payload slot `index` inside a slot-backed aggregate.
pub(crate) const fn slot_offset(index: usize) -> i32 {
    SLOT_HEADER_SIZE + VALUE_SLOT_SIZE * index as i32
}

const FNV1A_OFFSET: u32 = 2_166_136_261;
const FNV1A_PRIME: u32 = 16_777_619;

/// (RFC-0037 §3) A stable, stateless 8-bit type id for the type-confusion
/// sanitizer. The same type name always maps to the same non-zero tag; 0 means
/// "untagged". Collisions only miss a confusion, never false-trap.
pub fn type_tag_of(name: &str) -> u8 {
    let mut h: u32 = FNV1A_OFFSET;
    for byte in name.bytes() {
        h ^= byte as u32;
        h = h.wrapping_mul(FNV1A_PRIME);
    }
    (h % 255) as u8 + 1
}

/// Canonical descriptor encoding version (RFC-0111).
///
/// This version is part of every [`LayoutId`]. Changing the meaning or canonical
/// encoding of any public descriptor field requires incrementing it.
pub const LAYOUT_SCHEMA_VERSION: u32 = 1;

/// A content-addressed physical layout identity.
///
/// The bytes are SHA-256 over the schema version and canonical descriptor. This
/// identity is safe to persist in compiler caches and compare at module/host
/// boundaries. It deliberately contains no logical type or field names: two
/// byte-compatible closed shapes share a physical identity, while logical type
/// identity remains the type checker's responsibility.
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

/// Fixed scalar forms admitted by the first RFC-0111 layout class.
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
}

impl ScalarKind {
    pub const fn size(self) -> u32 {
        match self {
            Self::Bool | Self::Tag8 => 1,
            Self::Tag16 => 2,
            Self::U32 | Self::Tag32 => 4,
            Self::Int | Self::Float | Self::Duration => 8,
        }
    }

    pub const fn alignment(self) -> u32 {
        self.size()
    }
}

/// Fixed or packed-list-sized storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutSize {
    Fixed(u32),
    Dynamic { base: u32, stride: u32 },
}

/// Physical kind of one field. Logical field names are intentionally absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Scalar(ScalarKind),
    Inline(LayoutId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutField {
    pub offset: u32,
    pub kind: FieldKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantLayout {
    /// Variant payload fields, with offsets relative to the complete sum value.
    pub fields: Vec<LayoutField>,
    pub payload_size: u32,
    pub payload_alignment: u32,
}

/// Whether an owning packed buffer retains the ordinary reference-count header.
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

/// Ownership-bearing positions in the physical value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipPosition {
    /// The value is the sole owning handle for its packed element buffer.
    RootBuffer,
    /// Reserved for later reference-bearing descriptor classes.
    Field(Vec<u32>),
}

/// Descriptor-driven traversal needed by a generated operation.
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

/// Every physical operation receives an explicit shape. Later consumers may
/// share generated helpers, but may not reconstruct these shapes from names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationShapes {
    pub copy: OperationShape,
    pub duplicate: OperationShape,
    pub drop: OperationShape,
    pub equality: OperationShape,
    pub render: OperationShape,
    pub serialization: OperationShape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutKind {
    Scalar(ScalarKind),
    Tuple,
    PackedRecord,
    PackedList {
        element: LayoutId,
        element_stride: u32,
    },
    ClosedSum {
        tag: ScalarKind,
        payload_offset: u32,
        variants: Vec<VariantLayout>,
    },
}

/// Canonical compiler-internal physical layout.
///
/// This descriptor is not part of `meta.Type` or public reflection. Reflection
/// continues to traverse logical [`witchy_syntax::ast::Type`] and [`TypeDef`]
/// values, so offsets, padding, headers, and ownership positions stay opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutDescriptor {
    pub schema_version: u32,
    pub kind: LayoutKind,
    pub size: LayoutSize,
    pub alignment: u32,
    pub fields: Vec<LayoutField>,
    pub ownership: Vec<OwnershipPosition>,
    pub header: HeaderLayout,
    pub operations: OperationShapes,
}

/// Reference categories that cannot enter the initial inline layout class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    Owning,
    BorrowedView,
    ExternRef,
    GcReference,
    Capability,
}

/// Representation classification retained in a rejection diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageClass {
    OwningReference,
    BorrowedView,
    ExternRef,
    GcReference,
    CapabilityReference,
}

impl ReferenceKind {
    pub const fn storage_class(self) -> StorageClass {
        match self {
            Self::Owning => StorageClass::OwningReference,
            Self::BorrowedView => StorageClass::BorrowedView,
            Self::ExternRef => StorageClass::ExternRef,
            Self::GcReference => StorageClass::GcReference,
            Self::Capability => StorageClass::CapabilityReference,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    UnknownLayout(LayoutId),
    DynamicInlineField(LayoutId),
    ArithmeticOverflow,
    NotPackedRecord,
    CapabilityDefinition,
    InvalidClosedSum,
    FieldCount { expected: usize, actual: usize },
    VariantCount { expected: usize, actual: usize },
    ReferenceNotInline { kind: ReferenceKind, class: StorageClass },
    DigestCollision(LayoutId),
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownLayout(id) => write!(formatter, "unknown layout `{id}`"),
            Self::DynamicInlineField(id) => {
                write!(formatter, "dynamic layout `{id}` cannot be stored inline")
            }
            Self::ArithmeticOverflow => formatter.write_str("physical layout size overflow"),
            Self::NotPackedRecord => formatter.write_str(
                "nominal packed-record layout requires one variant on a `packed` definition",
            ),
            Self::CapabilityDefinition => {
                formatter.write_str("capability definitions require reference-safe storage")
            }
            Self::InvalidClosedSum => {
                formatter.write_str("closed-sum layout requires a non-empty nominal sum")
            }
            Self::FieldCount { expected, actual } => {
                write!(formatter, "layout field count mismatch: expected {expected}, got {actual}")
            }
            Self::VariantCount { expected, actual } => {
                write!(formatter, "layout variant count mismatch: expected {expected}, got {actual}")
            }
            Self::ReferenceNotInline { kind, class } => {
                write!(formatter, "{kind:?} is classified as {class:?} and cannot be stored inline")
            }
            Self::DigestCollision(id) => write!(formatter, "layout digest collision for `{id}`"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// Deterministic content-addressed descriptor store.
///
/// Callers resolve logical and generic types first, then intern child layouts
/// before parents. The API accepts typed scalar/reference enums and resolved
/// [`TypeDef`] structure, avoiding a second string-keyed type catalog in WIR.
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
        self.descriptors.iter().map(|(id, descriptor)| (*id, descriptor))
    }

    pub fn intern_scalar(&mut self, scalar: ScalarKind) -> Result<LayoutId, LayoutError> {
        let operation = OperationShape::Scalar(scalar);
        self.intern(LayoutDescriptor {
            schema_version: LAYOUT_SCHEMA_VERSION,
            kind: LayoutKind::Scalar(scalar),
            size: LayoutSize::Fixed(scalar.size()),
            alignment: scalar.alignment(),
            fields: Vec::new(),
            ownership: Vec::new(),
            header: HeaderLayout::None,
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

    pub fn intern_tuple(&mut self, children: &[LayoutId]) -> Result<LayoutId, LayoutError> {
        let (fields, size, alignment) = self.aggregate_fields(children, 0)?;
        self.intern_aggregate(
            LayoutKind::Tuple,
            children,
            fields,
            size,
            alignment,
        )
    }

    pub fn intern_packed_record(
        &mut self,
        definition: &TypeDef,
        children: &[LayoutId],
    ) -> Result<LayoutId, LayoutError> {
        if definition.is_capability {
            return Err(LayoutError::CapabilityDefinition);
        }
        if !definition.packed || definition.variants.len() != 1 {
            return Err(LayoutError::NotPackedRecord);
        }
        let expected = definition.variants[0].fields.len();
        if children.len() != expected {
            return Err(LayoutError::FieldCount { expected, actual: children.len() });
        }
        let (fields, size, alignment) = self.aggregate_fields(children, 0)?;
        self.intern_aggregate(
            LayoutKind::PackedRecord,
            children,
            fields,
            size,
            alignment,
        )
    }

    pub fn intern_packed_list(
        &mut self,
        element: LayoutId,
        rc: RcHeader,
    ) -> Result<LayoutId, LayoutError> {
        let descriptor = self.descriptor(element)?;
        let element_size = match descriptor.size {
            LayoutSize::Fixed(size) => size,
            LayoutSize::Dynamic { .. } => return Err(LayoutError::DynamicInlineField(element)),
        };
        let stride = align_up(element_size, descriptor.alignment)?;
        let data_offset = align_up(8, descriptor.alignment)?;
        let alignment = descriptor.alignment.max(4);
        let operation = OperationShape::PackedElements { element, stride };
        self.intern(LayoutDescriptor {
            schema_version: LAYOUT_SCHEMA_VERSION,
            kind: LayoutKind::PackedList { element, element_stride: stride },
            size: LayoutSize::Dynamic { base: data_offset, stride },
            alignment,
            fields: vec![
                LayoutField { offset: 0, kind: FieldKind::Scalar(ScalarKind::U32) },
                LayoutField { offset: 4, kind: FieldKind::Scalar(ScalarKind::U32) },
                LayoutField { offset: data_offset, kind: FieldKind::Inline(element) },
            ],
            ownership: vec![OwnershipPosition::RootBuffer],
            header: HeaderLayout::PackedList {
                rc,
                length_offset: 0,
                capacity_offset: 4,
                data_offset,
            },
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

    pub fn intern_closed_sum(
        &mut self,
        definition: &TypeDef,
        variants: &[Vec<LayoutId>],
    ) -> Result<LayoutId, LayoutError> {
        if definition.is_capability {
            return Err(LayoutError::CapabilityDefinition);
        }
        if definition.variants.is_empty() {
            return Err(LayoutError::InvalidClosedSum);
        }
        if variants.len() != definition.variants.len() {
            return Err(LayoutError::VariantCount {
                expected: definition.variants.len(),
                actual: variants.len(),
            });
        }
        for (source, layouts) in definition.variants.iter().zip(variants) {
            if layouts.len() != source.fields.len() {
                return Err(LayoutError::FieldCount {
                    expected: source.fields.len(),
                    actual: layouts.len(),
                });
            }
        }

        let tag = tag_kind(variants.len())?;
        let mut payload_alignment = 1;
        let mut payload_size = 0;
        for variant in variants {
            let (_, size, alignment) = self.aggregate_fields(variant, 0)?;
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
            let (fields, variant_size, variant_alignment) =
                self.aggregate_fields(variant, payload_offset)?;
            variant_layouts.push(VariantLayout {
                fields,
                payload_size: variant_size,
                payload_alignment: variant_alignment,
            });
        }
        let operation = OperationShape::Variants { tag, variants: variants.to_vec() };
        self.intern(LayoutDescriptor {
            schema_version: LAYOUT_SCHEMA_VERSION,
            kind: LayoutKind::ClosedSum {
                tag,
                payload_offset,
                variants: variant_layouts,
            },
            size: LayoutSize::Fixed(size),
            alignment,
            fields: vec![LayoutField { offset: 0, kind: FieldKind::Scalar(tag) }],
            ownership: Vec::new(),
            header: HeaderLayout::None,
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

    pub fn reject_reference(&self, kind: ReferenceKind) -> LayoutError {
        LayoutError::ReferenceNotInline { kind, class: kind.storage_class() }
    }

    fn intern_aggregate(
        &mut self,
        kind: LayoutKind,
        children: &[LayoutId],
        fields: Vec<LayoutField>,
        size: u32,
        alignment: u32,
    ) -> Result<LayoutId, LayoutError> {
        let operation = OperationShape::Fields(children.to_vec());
        self.intern(LayoutDescriptor {
            schema_version: LAYOUT_SCHEMA_VERSION,
            kind,
            size: LayoutSize::Fixed(size),
            alignment,
            fields,
            ownership: Vec::new(),
            header: HeaderLayout::None,
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

    fn aggregate_fields(
        &self,
        children: &[LayoutId],
        base: u32,
    ) -> Result<(Vec<LayoutField>, u32, u32), LayoutError> {
        let mut offset = 0;
        let mut alignment = 1;
        let mut fields = Vec::with_capacity(children.len());
        for child in children {
            let descriptor = self.descriptor(*child)?;
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
                kind: descriptor.field_kind(*child),
            });
            offset = offset.checked_add(size).ok_or(LayoutError::ArithmeticOverflow)?;
            alignment = alignment.max(descriptor.alignment);
        }
        Ok((fields, align_up(offset, alignment)?, alignment))
    }

    fn descriptor(&self, id: LayoutId) -> Result<&LayoutDescriptor, LayoutError> {
        self.descriptors.get(&id).ok_or(LayoutError::UnknownLayout(id))
    }

    fn intern(&mut self, descriptor: LayoutDescriptor) -> Result<LayoutId, LayoutError> {
        debug_assert_eq!(descriptor.schema_version, LAYOUT_SCHEMA_VERSION);
        let id = descriptor_id(&descriptor);
        if let Some(existing) = self.descriptors.get(&id) {
            if existing != &descriptor {
                return Err(LayoutError::DigestCollision(id));
            }
            return Ok(id);
        }
        self.descriptors.insert(id, descriptor);
        Ok(id)
    }
}

impl LayoutDescriptor {
    /// Recompute this descriptor's versioned content identity for cache or
    /// boundary validation.
    pub fn layout_id(&self) -> LayoutId {
        descriptor_id(self)
    }

    fn field_kind(&self, id: LayoutId) -> FieldKind {
        match self.kind {
            LayoutKind::Scalar(scalar) => FieldKind::Scalar(scalar),
            _ => FieldKind::Inline(id),
        }
    }
}

fn align_up(value: u32, alignment: u32) -> Result<u32, LayoutError> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|sum| sum & !(alignment - 1))
        .ok_or(LayoutError::ArithmeticOverflow)
}

fn tag_kind(variants: usize) -> Result<ScalarKind, LayoutError> {
    let maximum = variants.checked_sub(1).ok_or(LayoutError::InvalidClosedSum)?;
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

fn descriptor_id(descriptor: &LayoutDescriptor) -> LayoutId {
    let mut encoder = StableEncoder::default();
    encoder.bytes(b"witchy-layout\0");
    encoder.u32(descriptor.schema_version);
    encoder.descriptor(descriptor);
    LayoutId(sha256(&encoder.output))
}

#[derive(Default)]
struct StableEncoder {
    output: Vec<u8>,
}

impl StableEncoder {
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

    fn ids(&mut self, ids: &[LayoutId]) {
        self.u32(ids.len() as u32);
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
        });
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
        }
    }

    fn fields(&mut self, fields: &[LayoutField]) {
        self.u32(fields.len() as u32);
        for field in fields {
            self.field(field);
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
                self.u32(variants.len() as u32);
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
        match &descriptor.kind {
            LayoutKind::Scalar(scalar) => {
                self.byte(0);
                self.scalar(*scalar);
            }
            LayoutKind::Tuple => self.byte(1),
            LayoutKind::PackedRecord => self.byte(2),
            LayoutKind::PackedList { element, element_stride } => {
                self.byte(3);
                self.id(*element);
                self.u32(*element_stride);
            }
            LayoutKind::ClosedSum { tag, payload_offset, variants } => {
                self.byte(4);
                self.scalar(*tag);
                self.u32(*payload_offset);
                self.u32(variants.len() as u32);
                for variant in variants {
                    self.fields(&variant.fields);
                    self.u32(variant.payload_size);
                    self.u32(variant.payload_alignment);
                }
            }
        }
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
        self.u32(descriptor.ownership.len() as u32);
        for position in &descriptor.ownership {
            match position {
                OwnershipPosition::RootBuffer => self.byte(0),
                OwnershipPosition::Field(path) => {
                    self.byte(1);
                    self.u32(path.len() as u32);
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

// Small dependency-free SHA-256 implementation. The layout crate is also built
// for the browser compiler; keeping this local avoids a native-only digest path.
fn sha256(input: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
        0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
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
    use super::*;

    #[test]
    fn type_tag_vectors_are_stable() {
        assert_eq!(type_tag_of("Point"), 181);
        assert_eq!(type_tag_of("packed:Point"), 118);
        assert_eq!(type_tag_of("main.Point"), 40);
        assert_eq!(type_tag_of("Option"), 77);
        assert_eq!(type_tag_of("Result"), 208);
        assert_eq!(type_tag_of(""), 2);
    }

    #[test]
    fn slot_layout_vectors_are_stable() {
        assert_eq!(DATA_BASE, 8);
        assert_eq!(SLOT_HEADER_SIZE, 4);
        assert_eq!(VALUE_SLOT_SIZE, 8);
        assert_eq!(slot_record_size(0), 4);
        assert_eq!(slot_record_size(3), 28);
        assert_eq!(slot_offset(0), 4);
        assert_eq!(slot_offset(3), 28);
    }

    #[test]
    fn sha256_implementation_matches_the_standard_vector() {
        let digest = LayoutId::from_bytes(sha256(b"abc"));
        assert_eq!(
            digest.to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
