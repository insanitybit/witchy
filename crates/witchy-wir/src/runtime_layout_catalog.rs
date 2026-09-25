//! Canonical runtime-layout classes for RFC-0162 helper migration.
//!
//! [`crate::layout::LayoutInterner`] provides checked physical primitives. This
//! module gives the small subset used by today's ordinary runtime helpers stable
//! semantic names and exact extent/projection recipes. It deliberately does not
//! infer a layout from a byte width. A caller must request one named class, and
//! cached or transported claims must authenticate the exact class, `LayoutId`,
//! root convention, header size, and stride.
//!
//! Current ordinary strings/byte buffers are `[length: u32][bytes...]` and
//! ordinary scalar lists are `[length: u32][8-byte slots...]`. List capacity is
//! not stored in the object: it is a compiler-side ownership/allocation fact.
//! `$rc_alloc` places `[refcount: u32][size_and_tag: u32]` before either body and
//! returns a handle eight bytes into the complete allocation. Both the body-only
//! and allocation-relative exposed-root descriptors are catalogued so a helper
//! cannot silently use one convention as the other.
//!
//! Pointer-bearing slots, dictionaries, closure environments, and the distinct
//! packed-boolean list representation remain deferred. Their representations
//! need value/provenance contracts that these scalar-only entries cannot supply;
//! requests for them fail closed.

use std::collections::BTreeMap;
use std::fmt;

use crate::layout::{
    FieldKind, HeaderLayout, LayoutDescriptor, LayoutError, LayoutId, LayoutInterner,
    LayoutKind, LayoutSize, PointerSlot, RC_SIZE_MASK, RootAnchor, RuntimePrefixSlot,
    ScalarKind,
};

const BODY_LENGTH_BYTES: u32 = 4;
const RC_PREFIX_BYTES: u32 = 8;
const VALUE_SLOT_BYTES: u32 = 8;

/// Exact representation-only scalar identities admitted by the runtime catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeScalar {
    RawByte,
    U32,
    Int,
    Float,
    Duration,
}

impl RuntimeScalar {
    const ALL: [Self; 5] = [
        Self::RawByte,
        Self::U32,
        Self::Int,
        Self::Float,
        Self::Duration,
    ];

    pub const fn scalar_kind(self) -> ScalarKind {
        match self {
            Self::RawByte => ScalarKind::RawByte,
            Self::U32 => ScalarKind::U32,
            Self::Int => ScalarKind::Int,
            Self::Float => ScalarKind::Float,
            Self::Duration => ScalarKind::Duration,
        }
    }
}

/// Scalar values whose current ordinary-list ABI is one exact eight-byte slot.
///
/// `Bool` is absent: its optimized packed-list lane has a different header and
/// stride. `Float` is absent because the generic helper carries its bits through
/// an erased `i64` slot rather than an `f64` memory operation; the current
/// primitive graph has no opaque 64-bit lane. Pointer-like values are absent
/// because a byte-compatible lane does not authenticate pointee layout,
/// provenance, generation, or ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScalarSlotKind {
    Int,
    Duration,
}

impl ScalarSlotKind {
    const ALL: [Self; 2] = [Self::Int, Self::Duration];

    pub const fn scalar(self) -> RuntimeScalar {
        match self {
            Self::Int => RuntimeScalar::Int,
            Self::Duration => RuntimeScalar::Duration,
        }
    }
}

/// Which root is described by a requested runtime layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeStorage {
    /// The runtime-visible `[length][payload]` handle is the descriptor root.
    /// This is a physical body view, not proof that the value is headerless;
    /// static-data or allocation provenance must justify its use separately.
    Body,
    /// The descriptor root is the complete allocation and the runtime handle is
    /// exposed after the checked hidden RC prefix.
    RcAllocation,
}

/// Every supported semantic class has one exact canonical descriptor identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeLayoutClass {
    FixedScalar(RuntimeScalar),
    BytePayload(RuntimeStorage),
    ScalarSlotList {
        slot: ScalarSlotKind,
        storage: RuntimeStorage,
    },
}

/// Untrusted requests presented to the catalog.
///
/// Unsupported families are represented explicitly so callers cannot silently
/// substitute a scalar-slot or byte-buffer descriptor for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLayoutRequest {
    FixedScalar(RuntimeScalar),
    BytePayload(RuntimeStorage),
    ScalarSlotList {
        slot: ScalarSlotKind,
        storage: RuntimeStorage,
    },
    PointerSlotList {
        pointee: LayoutId,
        semantics: PointerSlot,
    },
    Dictionary,
    ClosureEnvironment,
    PackedBoolList,
}

impl From<RuntimeLayoutClass> for RuntimeLayoutRequest {
    fn from(class: RuntimeLayoutClass) -> Self {
        match class {
            RuntimeLayoutClass::FixedScalar(scalar) => Self::FixedScalar(scalar),
            RuntimeLayoutClass::BytePayload(storage) => Self::BytePayload(storage),
            RuntimeLayoutClass::ScalarSlotList { slot, storage } => {
                Self::ScalarSlotList { slot, storage }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferredRuntimeFamily {
    PointerSlotList,
    Dictionary,
    ClosureEnvironment,
    PackedBoolList,
}

/// Runtime address convention carried alongside the canonical descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeRootConvention {
    AllocationBase,
    ExposedHandle { allocation_offset: u32 },
}

impl RuntimeRootConvention {
    pub const fn allocation_offset(self) -> u32 {
        match self {
            Self::AllocationBase => 0,
            Self::ExposedHandle { allocation_offset } => allocation_offset,
        }
    }
}

/// Exact physical shape authenticated with a runtime class and `LayoutId`.
///
/// This type is also the transport form for an untrusted cached claim. Use
/// [`RuntimeLayoutCatalog::authenticate_shape`] before relying on one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeLayoutShape {
    root: RuntimeRootConvention,
    hidden_prefix: u32,
    body_header: u32,
    stride: Option<u32>,
}

impl RuntimeLayoutShape {
    pub const fn claimed(
        root: RuntimeRootConvention,
        hidden_prefix: u32,
        body_header: u32,
        stride: Option<u32>,
    ) -> Self {
        Self { root, hidden_prefix, body_header, stride }
    }

    pub const fn root(self) -> RuntimeRootConvention {
        self.root
    }

    pub const fn hidden_prefix(self) -> u32 {
        self.hidden_prefix
    }

    pub const fn body_header(self) -> u32 {
        self.body_header
    }

    pub const fn stride(self) -> Option<u32> {
        self.stride
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCapacitySource {
    /// A body/static-root extent fact supplies capacity. It grants no authority
    /// to construct or size an RC allocation.
    AuthenticatedBodyExtent,
    /// The compiler-side allocation/ownership fact supplies capacity and must
    /// additionally fit the masked RC size-header bound.
    AuthenticatedAllocationFactWithSizeWordBound,
}

/// Descriptor-derived extent formula. Arithmetic is mathematical until the
/// checked `u32` result is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeExtentRecipe {
    kind: RuntimeExtentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeExtentKind {
    Fixed {
        bytes: u32,
    },
    BodyCounted {
        body_header: u32,
        stride: u32,
    },
    RcAllocationCounted {
        hidden_prefix: u32,
        body_header: u32,
        stride: u32,
        /// `$rc_alloc` stores the body request in the low bits selected by this
        /// mask; the remaining bits are debug type-tag state.
        size_mask: u32,
    },
}

/// Read-only description of a sealed extent recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeExtentForm {
    Fixed {
        bytes: u32,
    },
    BodyCounted {
        body_header: u32,
        stride: u32,
        capacity: RuntimeCapacitySource,
    },
    RcAllocationCounted {
        hidden_prefix: u32,
        body_header: u32,
        stride: u32,
        capacity: RuntimeCapacitySource,
        size_mask: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckedRuntimeExtent {
    descriptor_bytes: u32,
    live_descriptor_bytes: u32,
    body_extent_bytes: u32,
    allocator_request_bytes: Option<u32>,
}

impl CheckedRuntimeExtent {
    /// Complete bytes covered by the selected descriptor at allocation capacity.
    pub const fn descriptor_bytes(self) -> u32 {
        self.descriptor_bytes
    }

    /// Prefix/header plus the initialized logical-length payload.
    pub const fn live_descriptor_bytes(self) -> u32 {
        self.live_descriptor_bytes
    }

    /// Exact body extent `[length][capacity payload]`, excluding any hidden RC
    /// prefix. This is a range fact, not allocator authority.
    pub const fn body_extent_bytes(self) -> u32 {
        self.body_extent_bytes
    }

    /// Body bytes authorized as a `$rc_alloc` request. Body/static-root recipes
    /// return `None`; only an RC-allocation recipe can mint this value.
    pub const fn allocator_request_bytes(self) -> Option<u32> {
        self.allocator_request_bytes
    }
}

impl RuntimeExtentRecipe {
    const fn fixed(bytes: u32) -> Self {
        Self { kind: RuntimeExtentKind::Fixed { bytes } }
    }

    const fn body_counted(body_header: u32, stride: u32) -> Self {
        Self { kind: RuntimeExtentKind::BodyCounted { body_header, stride } }
    }

    const fn rc_allocation_counted(
        hidden_prefix: u32,
        body_header: u32,
        stride: u32,
        size_mask: u32,
    ) -> Self {
        Self {
            kind: RuntimeExtentKind::RcAllocationCounted {
                hidden_prefix,
                body_header,
                stride,
                size_mask,
            },
        }
    }

    pub const fn form(self) -> RuntimeExtentForm {
        match self.kind {
            RuntimeExtentKind::Fixed { bytes } => RuntimeExtentForm::Fixed { bytes },
            RuntimeExtentKind::BodyCounted { body_header, stride } => {
                RuntimeExtentForm::BodyCounted {
                    body_header,
                    stride,
                    capacity: RuntimeCapacitySource::AuthenticatedBodyExtent,
                }
            }
            RuntimeExtentKind::RcAllocationCounted {
                hidden_prefix,
                body_header,
                stride,
                size_mask,
            } => RuntimeExtentForm::RcAllocationCounted {
                hidden_prefix,
                body_header,
                stride,
                capacity:
                    RuntimeCapacitySource::AuthenticatedAllocationFactWithSizeWordBound,
                size_mask,
            },
        }
    }

    pub const fn fixed_bytes(self) -> Option<u32> {
        match self.kind {
            RuntimeExtentKind::Fixed { bytes } => Some(bytes),
            RuntimeExtentKind::BodyCounted { .. }
            | RuntimeExtentKind::RcAllocationCounted { .. } => None,
        }
    }

    pub fn checked_counts(
        self,
        length: u32,
        capacity: u32,
    ) -> Result<CheckedRuntimeExtent, RuntimeLayoutCatalogError> {
        match self.kind {
            RuntimeExtentKind::BodyCounted { .. } => {
                self.checked_counts_inner(length, capacity)
            }
            RuntimeExtentKind::RcAllocationCounted { .. } => {
                Err(RuntimeLayoutCatalogError::RcAllocationRequiresContext)
            }
            RuntimeExtentKind::Fixed { .. } => {
                Err(RuntimeLayoutCatalogError::CountsForFixedLayout)
            }
        }
    }

    /// Size a newly-created RC allocation. Existing objects must instead use
    /// [`Self::checked_counts_in_allocation`] so their compiler-side capacity
    /// claim is bounded by the actual masked size word.
    pub fn checked_new_allocation(
        self,
        length: u32,
        capacity: u32,
    ) -> Result<CheckedRuntimeExtent, RuntimeLayoutCatalogError> {
        match self.kind {
            RuntimeExtentKind::RcAllocationCounted { .. } => {
                self.checked_counts_inner(length, capacity)
            }
            RuntimeExtentKind::BodyCounted { .. } | RuntimeExtentKind::Fixed { .. } => {
                Err(RuntimeLayoutCatalogError::NewAllocationForNonRcLayout)
            }
        }
    }

    fn checked_counts_inner(
        self,
        length: u32,
        capacity: u32,
    ) -> Result<CheckedRuntimeExtent, RuntimeLayoutCatalogError> {
        let (hidden_prefix, body_header, stride, request_limit, grants_allocator) = match self.kind {
            RuntimeExtentKind::Fixed { .. } => {
                return Err(RuntimeLayoutCatalogError::CountsForFixedLayout);
            }
            RuntimeExtentKind::BodyCounted { body_header, stride } => {
                (0, body_header, stride, None, false)
            }
            RuntimeExtentKind::RcAllocationCounted {
                hidden_prefix,
                body_header,
                stride,
                size_mask,
            } => (hidden_prefix, body_header, stride, Some(size_mask), true),
        };
        if stride == 0 {
            return Err(RuntimeLayoutCatalogError::InvalidStride(stride));
        }
        if length > capacity {
            return Err(RuntimeLayoutCatalogError::LengthExceedsCapacity { length, capacity });
        }
        let payload_capacity = capacity
            .checked_mul(stride)
            .ok_or(RuntimeLayoutCatalogError::ArithmeticOverflow)?;
        let payload_length = length
            .checked_mul(stride)
            .ok_or(RuntimeLayoutCatalogError::ArithmeticOverflow)?;
        let allocator_request_bytes = body_header
            .checked_add(payload_capacity)
            .ok_or(RuntimeLayoutCatalogError::ArithmeticOverflow)?;
        if let Some(limit) = request_limit
            && allocator_request_bytes > limit
        {
            return Err(RuntimeLayoutCatalogError::AllocatorRequestExceedsLimit {
                request: allocator_request_bytes,
                limit,
            });
        }
        let descriptor_bytes = hidden_prefix
            .checked_add(allocator_request_bytes)
            .ok_or(RuntimeLayoutCatalogError::ArithmeticOverflow)?;
        let live_descriptor_bytes = hidden_prefix
            .checked_add(body_header)
            .and_then(|base| base.checked_add(payload_length))
            .ok_or(RuntimeLayoutCatalogError::ArithmeticOverflow)?;
        Ok(CheckedRuntimeExtent {
            descriptor_bytes,
            live_descriptor_bytes,
            body_extent_bytes: allocator_request_bytes,
            allocator_request_bytes: grants_allocator.then_some(allocator_request_bytes),
        })
    }

    /// Recover the exact capacity represented by an authenticated body extent.
    /// Header and stride disagreement rejects instead of rounding to a nearby
    /// element count. This does not grant allocator authority.
    pub fn checked_capacity_from_body_extent(
        self,
        request: u32,
    ) -> Result<u32, RuntimeLayoutCatalogError> {
        let (body_header, stride, request_limit) = match self.kind {
            RuntimeExtentKind::Fixed { .. } => {
                return Err(RuntimeLayoutCatalogError::CountsForFixedLayout);
            }
            RuntimeExtentKind::BodyCounted { body_header, stride } => {
                (body_header, stride, None)
            }
            RuntimeExtentKind::RcAllocationCounted { body_header, stride, size_mask, .. } => {
                (body_header, stride, Some(size_mask))
            }
        };
        if stride == 0 {
            return Err(RuntimeLayoutCatalogError::InvalidStride(stride));
        }
        if let Some(limit) = request_limit
            && request > limit
        {
            return Err(RuntimeLayoutCatalogError::AllocatorRequestExceedsLimit {
                request,
                limit,
            });
        }
        let payload = request
            .checked_sub(body_header)
            .ok_or(RuntimeLayoutCatalogError::AllocatorRequestTooSmall {
                request,
                header: body_header,
            })?;
        if payload % stride != 0 {
            return Err(RuntimeLayoutCatalogError::AllocatorRequestMisaligned {
                request,
                header: body_header,
                stride,
            });
        }
        Ok(payload / stride)
    }

    /// Derive a conservative capacity upper bound from the actual RC
    /// `size_and_tag` word. Tail slack and debug redzones make this a bound, not
    /// proof that every byte is a usable element slot.
    pub fn capacity_bound_from_size_word(
        self,
        size_word: u32,
    ) -> Result<u32, RuntimeLayoutCatalogError> {
        let RuntimeExtentKind::RcAllocationCounted {
            body_header,
            stride,
            size_mask,
            ..
        } = self.kind
        else {
            return Err(RuntimeLayoutCatalogError::SizeWordForNonRcLayout);
        };
        if stride == 0 {
            return Err(RuntimeLayoutCatalogError::InvalidStride(stride));
        }
        let request = size_word & size_mask;
        let payload = request
            .checked_sub(body_header)
            .ok_or(RuntimeLayoutCatalogError::AllocatorRequestTooSmall {
                request,
                header: body_header,
            })?;
        Ok(payload / stride)
    }

    /// Validate the compiler-side length/capacity claim against both the
    /// descriptor recipe and the current allocation's masked size-header bound.
    pub fn checked_counts_in_allocation(
        self,
        length: u32,
        capacity: u32,
        size_word: u32,
    ) -> Result<CheckedRuntimeExtent, RuntimeLayoutCatalogError> {
        if !matches!(self.kind, RuntimeExtentKind::RcAllocationCounted { .. }) {
            return Err(RuntimeLayoutCatalogError::SizeWordForNonRcLayout);
        }
        let extent = self.checked_counts_inner(length, capacity)?;
        let allocated_capacity = self.capacity_bound_from_size_word(size_word)?;
        if capacity > allocated_capacity {
            return Err(RuntimeLayoutCatalogError::CapacityExceedsAllocation {
                capacity,
                allocated_capacity,
            });
        }
        Ok(extent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProjectionKind {
    ScalarValue,
    RcRefCount,
    RcAllocationSizeAndTag,
    Length,
    Element,
}

/// Allocation-relative projection metadata for a catalogued runtime class.
///
/// Hidden-prefix projections have no `handle_offset`: they may be addressed
/// only after the exposed handle has been authenticated back to its allocation
/// root. Repeated element offsets use the descriptor-owned stride. Offset
/// calculation checks arithmetic only; `SafePlaceBuilder` must separately bind
/// the index to the exact length source and prove or retain its bounds check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeProjection {
    kind: RuntimeProjectionKind,
    layout: LayoutId,
    allocation_offset: u32,
    handle_offset: Option<u32>,
    width: u32,
    stride: Option<u32>,
}

impl RuntimeProjection {
    const fn fixed(
        kind: RuntimeProjectionKind,
        layout: LayoutId,
        allocation_offset: u32,
        handle_offset: Option<u32>,
        width: u32,
    ) -> Self {
        Self { kind, layout, allocation_offset, handle_offset, width, stride: None }
    }

    const fn repeated(
        kind: RuntimeProjectionKind,
        layout: LayoutId,
        allocation_offset: u32,
        handle_offset: Option<u32>,
        width: u32,
        stride: u32,
    ) -> Self {
        Self { kind, layout, allocation_offset, handle_offset, width, stride: Some(stride) }
    }

    pub const fn kind(self) -> RuntimeProjectionKind {
        self.kind
    }

    pub const fn layout(self) -> LayoutId {
        self.layout
    }

    pub const fn allocation_offset(self) -> u32 {
        self.allocation_offset
    }

    pub const fn handle_offset(self) -> Option<u32> {
        self.handle_offset
    }

    pub const fn width(self) -> u32 {
        self.width
    }

    pub const fn stride(self) -> Option<u32> {
        self.stride
    }

    /// Bit mask required when reading the combined RC allocation-size/type-tag
    /// word. Other projections carry their complete scalar value.
    pub const fn value_mask(self) -> Option<u32> {
        match self.kind {
            RuntimeProjectionKind::RcAllocationSizeAndTag => Some(RC_SIZE_MASK as u32),
            _ => None,
        }
    }

    pub fn indexed_allocation_offset(
        self,
        index: u32,
    ) -> Result<u32, RuntimeLayoutCatalogError> {
        let Some(stride) = self.stride else {
            return Err(RuntimeLayoutCatalogError::ProjectionIsNotRepeated(self.kind));
        };
        index
            .checked_mul(stride)
            .and_then(|delta| self.allocation_offset.checked_add(delta))
            .ok_or(RuntimeLayoutCatalogError::ArithmeticOverflow)
    }

    pub fn indexed_handle_offset(
        self,
        index: u32,
    ) -> Result<u32, RuntimeLayoutCatalogError> {
        let Some(base) = self.handle_offset else {
            return Err(RuntimeLayoutCatalogError::HiddenProjection(self.kind));
        };
        let Some(stride) = self.stride else {
            return Err(RuntimeLayoutCatalogError::ProjectionIsNotRepeated(self.kind));
        };
        index
            .checked_mul(stride)
            .and_then(|delta| base.checked_add(delta))
            .ok_or(RuntimeLayoutCatalogError::ArithmeticOverflow)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLayoutEntry {
    class: RuntimeLayoutClass,
    layout: LayoutId,
    shape: RuntimeLayoutShape,
    extent: RuntimeExtentRecipe,
    projections: Vec<RuntimeProjection>,
}

impl RuntimeLayoutEntry {
    pub const fn class(&self) -> RuntimeLayoutClass {
        self.class
    }

    pub const fn layout(&self) -> LayoutId {
        self.layout
    }

    pub const fn shape(&self) -> RuntimeLayoutShape {
        self.shape
    }

    pub const fn extent(&self) -> RuntimeExtentRecipe {
        self.extent
    }

    pub fn projections(&self) -> &[RuntimeProjection] {
        &self.projections
    }

    pub fn projection(&self, kind: RuntimeProjectionKind) -> Option<RuntimeProjection> {
        self.projections.iter().copied().find(|projection| projection.kind == kind)
    }
}

/// A closed deterministic catalog. Every returned `LayoutId`, including every
/// projection child, resolves in this catalog's interner.
#[derive(Debug)]
pub struct RuntimeLayoutCatalog {
    layouts: LayoutInterner,
    entries: BTreeMap<RuntimeLayoutClass, RuntimeLayoutEntry>,
    classes_by_id: BTreeMap<LayoutId, RuntimeLayoutClass>,
}

impl RuntimeLayoutCatalog {
    pub fn new() -> Result<Self, RuntimeLayoutCatalogError> {
        let mut layouts = LayoutInterner::new();
        let mut scalar_ids = BTreeMap::new();
        for scalar in RuntimeScalar::ALL {
            let id = layouts.intern_scalar(scalar.scalar_kind())?;
            scalar_ids.insert(scalar, id);
        }

        let mut catalog = Self {
            layouts,
            entries: BTreeMap::new(),
            classes_by_id: BTreeMap::new(),
        };

        for scalar in RuntimeScalar::ALL {
            let id = scalar_ids[&scalar];
            catalog.register(RuntimeLayoutEntry {
                class: RuntimeLayoutClass::FixedScalar(scalar),
                layout: id,
                shape: RuntimeLayoutShape::claimed(
                    RuntimeRootConvention::AllocationBase,
                    0,
                    0,
                    None,
                ),
                extent: RuntimeExtentRecipe::fixed(scalar.scalar_kind().size()),
                projections: vec![RuntimeProjection::fixed(
                    RuntimeProjectionKind::ScalarValue,
                    id,
                    0,
                    Some(0),
                    scalar.scalar_kind().size(),
                )],
            })?;
        }

        let byte_element = scalar_ids[&RuntimeScalar::RawByte];
        let byte_body = catalog.layouts.intern_runtime_buffer(byte_element)?;
        catalog.register(dynamic_body_entry(
            RuntimeLayoutClass::BytePayload(RuntimeStorage::Body),
            byte_body,
            byte_element,
            1,
            scalar_ids[&RuntimeScalar::U32],
        ))?;

        for slot in ScalarSlotKind::ALL {
            let element = scalar_ids[&slot.scalar()];
            let body = catalog.layouts.intern_runtime_buffer(element)?;
            catalog.register(dynamic_body_entry(
                RuntimeLayoutClass::ScalarSlotList {
                    slot,
                    storage: RuntimeStorage::Body,
                },
                body,
                element,
                VALUE_SLOT_BYTES,
                scalar_ids[&RuntimeScalar::U32],
            ))?;
        }

        let byte_allocation = catalog.intern_rc_allocation(byte_body)?;
        catalog.register(dynamic_rc_entry(
            RuntimeLayoutClass::BytePayload(RuntimeStorage::RcAllocation),
            byte_allocation,
            byte_element,
            1,
            scalar_ids[&RuntimeScalar::U32],
        ))?;

        for slot in ScalarSlotKind::ALL {
            let body_class = RuntimeLayoutClass::ScalarSlotList {
                slot,
                storage: RuntimeStorage::Body,
            };
            let body = catalog.entries[&body_class].layout;
            let allocation = catalog.intern_rc_allocation(body)?;
            catalog.register(dynamic_rc_entry(
                RuntimeLayoutClass::ScalarSlotList {
                    slot,
                    storage: RuntimeStorage::RcAllocation,
                },
                allocation,
                scalar_ids[&slot.scalar()],
                VALUE_SLOT_BYTES,
                scalar_ids[&RuntimeScalar::U32],
            ))?;
        }

        catalog.validate_all()?;
        Ok(catalog)
    }

    fn intern_rc_allocation(&mut self, body: LayoutId) -> Result<LayoutId, LayoutError> {
        self.layouts.intern_exposed_root(
            body,
            vec![
                RuntimePrefixSlot::Scalar(ScalarKind::U32),
                RuntimePrefixSlot::Scalar(ScalarKind::U32),
            ],
        )
    }

    fn register(
        &mut self,
        entry: RuntimeLayoutEntry,
    ) -> Result<(), RuntimeLayoutCatalogError> {
        if self.entries.contains_key(&entry.class) {
            return Err(RuntimeLayoutCatalogError::DuplicateClass(entry.class));
        }
        if let Some(previous) = self.classes_by_id.insert(entry.layout, entry.class) {
            return Err(RuntimeLayoutCatalogError::DuplicateLayoutId {
                layout: entry.layout,
                first: previous,
                second: entry.class,
            });
        }
        self.entries.insert(entry.class, entry);
        Ok(())
    }

    pub fn interner(&self) -> &LayoutInterner {
        &self.layouts
    }

    /// Install the complete descriptor graph into an existing compiler
    /// interner. Dependencies are imported explicitly in topological order;
    /// iteration order of the content-addressed map is never treated as an
    /// authority signal.
    pub fn install_into(
        &self,
        target: &mut LayoutInterner,
    ) -> Result<(), RuntimeLayoutCatalogError> {
        for scalar in RuntimeScalar::ALL {
            self.import_class_into(
                RuntimeLayoutClass::FixedScalar(scalar),
                target,
            )?;
        }
        self.import_class_into(
            RuntimeLayoutClass::BytePayload(RuntimeStorage::Body),
            target,
        )?;
        for slot in ScalarSlotKind::ALL {
            self.import_class_into(
                RuntimeLayoutClass::ScalarSlotList {
                    slot,
                    storage: RuntimeStorage::Body,
                },
                target,
            )?;
        }
        self.import_class_into(
            RuntimeLayoutClass::BytePayload(RuntimeStorage::RcAllocation),
            target,
        )?;
        for slot in ScalarSlotKind::ALL {
            self.import_class_into(
                RuntimeLayoutClass::ScalarSlotList {
                    slot,
                    storage: RuntimeStorage::RcAllocation,
                },
                target,
            )?;
        }
        Ok(())
    }

    fn import_class_into(
        &self,
        class: RuntimeLayoutClass,
        target: &mut LayoutInterner,
    ) -> Result<(), RuntimeLayoutCatalogError> {
        let entry = self
            .entries
            .get(&class)
            .ok_or(RuntimeLayoutCatalogError::MissingClass(class))?;
        let descriptor = self
            .layouts
            .get(entry.layout)
            .ok_or(RuntimeLayoutCatalogError::UnknownLayout(entry.layout))?;
        target.import_canonical(entry.layout, &descriptor.canonical_bytes())?;
        Ok(())
    }

    pub fn iter(
        &self,
    ) -> impl Iterator<Item = (RuntimeLayoutClass, &RuntimeLayoutEntry)> {
        self.entries.iter().map(|(class, entry)| (*class, entry))
    }

    pub fn entry(
        &self,
        request: RuntimeLayoutRequest,
    ) -> Result<&RuntimeLayoutEntry, RuntimeLayoutCatalogError> {
        let class = supported_class(request)?;
        self.entries
            .get(&class)
            .ok_or(RuntimeLayoutCatalogError::MissingClass(class))
    }

    pub fn descriptor(
        &self,
        request: RuntimeLayoutRequest,
    ) -> Result<&LayoutDescriptor, RuntimeLayoutCatalogError> {
        let entry = self.entry(request)?;
        self.layouts
            .get(entry.layout)
            .ok_or(RuntimeLayoutCatalogError::UnknownLayout(entry.layout))
    }

    pub fn resolve(
        &self,
        layout: LayoutId,
    ) -> Result<&RuntimeLayoutEntry, RuntimeLayoutCatalogError> {
        if self.layouts.get(layout).is_none() {
            return Err(RuntimeLayoutCatalogError::UnknownLayout(layout));
        }
        let class = self
            .classes_by_id
            .get(&layout)
            .copied()
            .ok_or(RuntimeLayoutCatalogError::UncataloguedLayout(layout))?;
        self.entries
            .get(&class)
            .ok_or(RuntimeLayoutCatalogError::MissingClass(class))
    }

    /// Authenticate an untrusted class/id pair. A canonical descriptor for a
    /// different class is still rejected; physical similarity grants nothing.
    pub fn authenticate(
        &self,
        request: RuntimeLayoutRequest,
        claimed: LayoutId,
    ) -> Result<&RuntimeLayoutEntry, RuntimeLayoutCatalogError> {
        let expected = self.entry(request)?;
        if self.layouts.get(claimed).is_none() {
            return Err(RuntimeLayoutCatalogError::UnknownLayout(claimed));
        }
        if expected.layout != claimed {
            return Err(RuntimeLayoutCatalogError::ClassMismatch {
                expected: expected.class,
                actual: self.classes_by_id.get(&claimed).copied(),
                claimed,
            });
        }
        self.validate_entry(expected)?;
        Ok(expected)
    }

    /// Authenticate cached shape metadata as well as the class and `LayoutId`.
    pub fn authenticate_shape(
        &self,
        request: RuntimeLayoutRequest,
        claimed: LayoutId,
        shape: RuntimeLayoutShape,
    ) -> Result<&RuntimeLayoutEntry, RuntimeLayoutCatalogError> {
        let entry = self.authenticate(request, claimed)?;
        if entry.shape != shape {
            return Err(RuntimeLayoutCatalogError::ShapeMismatch {
                class: entry.class,
                expected: entry.shape,
                actual: shape,
            });
        }
        Ok(entry)
    }

    fn validate_all(&self) -> Result<(), RuntimeLayoutCatalogError> {
        for entry in self.entries.values() {
            self.validate_entry(entry)?;
        }
        Ok(())
    }

    fn validate_entry(
        &self,
        entry: &RuntimeLayoutEntry,
    ) -> Result<(), RuntimeLayoutCatalogError> {
        let descriptor = self
            .layouts
            .get(entry.layout)
            .ok_or(RuntimeLayoutCatalogError::UnknownLayout(entry.layout))?;
        if descriptor.layout_id() != entry.layout {
            return Err(invalid(entry.class, "descriptor digest disagrees with catalog id"));
        }

        match entry.class {
            RuntimeLayoutClass::FixedScalar(scalar) => {
                if descriptor.kind() != &LayoutKind::Scalar(scalar.scalar_kind())
                    || descriptor.size() != LayoutSize::Fixed(scalar.scalar_kind().size())
                    || descriptor.anchor() != RootAnchor::AllocationBase
                    || entry.shape
                        != RuntimeLayoutShape::claimed(
                            RuntimeRootConvention::AllocationBase,
                            0,
                            0,
                            None,
                        )
                    || entry.extent != RuntimeExtentRecipe::fixed(scalar.scalar_kind().size())
                    || entry.projections
                        != vec![RuntimeProjection::fixed(
                            RuntimeProjectionKind::ScalarValue,
                            entry.layout,
                            0,
                            Some(0),
                            scalar.scalar_kind().size(),
                        )]
                {
                    return Err(invalid(entry.class, "fixed scalar shape drift"));
                }
            }
            RuntimeLayoutClass::BytePayload(storage) => {
                let element = self.entry(RuntimeLayoutRequest::FixedScalar(
                    RuntimeScalar::RawByte,
                ))?.layout;
                self.validate_dynamic(entry, descriptor, storage, element, 1)?;
            }
            RuntimeLayoutClass::ScalarSlotList { slot, storage } => {
                let element = self.entry(RuntimeLayoutRequest::FixedScalar(slot.scalar()))?.layout;
                self.validate_dynamic(
                    entry,
                    descriptor,
                    storage,
                    element,
                    VALUE_SLOT_BYTES,
                )?;
            }
        }

        for projection in &entry.projections {
            let child = self.layouts.get(projection.layout).ok_or(
                RuntimeLayoutCatalogError::UnknownProjectionLayout {
                    class: entry.class,
                    layout: projection.layout,
                },
            )?;
            let LayoutSize::Fixed(width) = child.size() else {
                return Err(invalid(entry.class, "projection child is dynamic"));
            };
            if width != projection.width {
                return Err(invalid(entry.class, "projection width disagrees with child layout"));
            }
            if projection.stride.is_some_and(|stride| width > stride) {
                return Err(invalid(entry.class, "projection width exceeds stride"));
            }
        }
        Ok(())
    }

    fn validate_dynamic(
        &self,
        entry: &RuntimeLayoutEntry,
        descriptor: &LayoutDescriptor,
        storage: RuntimeStorage,
        element: LayoutId,
        stride: u32,
    ) -> Result<(), RuntimeLayoutCatalogError> {
        let body_class = match entry.class {
            RuntimeLayoutClass::BytePayload(_) => {
                RuntimeLayoutClass::BytePayload(RuntimeStorage::Body)
            }
            RuntimeLayoutClass::ScalarSlotList { slot, .. } => {
                RuntimeLayoutClass::ScalarSlotList {
                    slot,
                    storage: RuntimeStorage::Body,
                }
            }
            RuntimeLayoutClass::FixedScalar(_) => {
                return Err(invalid(entry.class, "fixed scalar reached dynamic validation"));
            }
        };
        let (hidden_prefix, root, expected_kind) = match storage {
            RuntimeStorage::Body => (
                0,
                RuntimeRootConvention::AllocationBase,
                LayoutKind::RuntimeBuffer { element },
            ),
            RuntimeStorage::RcAllocation => {
                let body = self
                    .entries
                    .get(&body_class)
                    .ok_or(RuntimeLayoutCatalogError::MissingClass(body_class))?
                    .layout;
                (
                    RC_PREFIX_BYTES,
                    RuntimeRootConvention::ExposedHandle {
                        allocation_offset: RC_PREFIX_BYTES,
                    },
                    LayoutKind::ExposedRoot {
                        body,
                        prefix: vec![
                            RuntimePrefixSlot::Scalar(ScalarKind::U32),
                            RuntimePrefixSlot::Scalar(ScalarKind::U32),
                        ],
                        anchor_bias: RC_PREFIX_BYTES,
                    },
                )
            }
        };
        let expected_size = LayoutSize::Dynamic {
            base: hidden_prefix + BODY_LENGTH_BYTES,
            stride,
        };
        let expected_anchor = match root {
            RuntimeRootConvention::AllocationBase => RootAnchor::AllocationBase,
            RuntimeRootConvention::ExposedHandle { allocation_offset } => {
                RootAnchor::Exposed { allocation_offset }
            }
        };
        let expected_extent = match storage {
            RuntimeStorage::Body => {
                RuntimeExtentRecipe::body_counted(BODY_LENGTH_BYTES, stride)
            }
            RuntimeStorage::RcAllocation => RuntimeExtentRecipe::rc_allocation_counted(
                hidden_prefix,
                BODY_LENGTH_BYTES,
                stride,
                RC_SIZE_MASK as u32,
            ),
        };
        if descriptor.kind() != &expected_kind
            || descriptor.size() != expected_size
            || descriptor.anchor() != expected_anchor
            || descriptor.header() != HeaderLayout::None
            || entry.shape
                != RuntimeLayoutShape::claimed(
                    root,
                    hidden_prefix,
                    BODY_LENGTH_BYTES,
                    Some(stride),
                )
            || entry.extent != expected_extent
        {
            return Err(invalid(entry.class, "dynamic runtime shape drift"));
        }

        let body_descriptor = match storage {
            RuntimeStorage::Body => descriptor,
            RuntimeStorage::RcAllocation => {
                let LayoutKind::ExposedRoot { body, .. } = descriptor.kind() else {
                    return Err(invalid(entry.class, "RC descriptor is not exposed-root"));
                };
                self.layouts
                    .get(*body)
                    .ok_or(RuntimeLayoutCatalogError::UnknownLayout(*body))?
            }
        };
        if body_descriptor.kind() != &(LayoutKind::RuntimeBuffer { element })
            || body_descriptor.size()
                != (LayoutSize::Dynamic { base: BODY_LENGTH_BYTES, stride })
            || body_descriptor.fields().len() != 2
            || body_descriptor.fields()[0].offset() != 0
            || body_descriptor.fields()[0].kind() != FieldKind::Scalar(ScalarKind::U32)
            || body_descriptor.fields()[1].offset() != BODY_LENGTH_BYTES
            || body_descriptor.fields()[1].kind() != FieldKind::Inline(element)
        {
            return Err(invalid(entry.class, "runtime body header or stride drift"));
        }
        let u32_layout = self
            .entry(RuntimeLayoutRequest::FixedScalar(RuntimeScalar::U32))?
            .layout;
        let expected_projections = match storage {
            RuntimeStorage::Body => body_projections(element, stride, u32_layout),
            RuntimeStorage::RcAllocation => rc_projections(element, stride, u32_layout),
        };
        if entry.projections != expected_projections {
            return Err(invalid(entry.class, "runtime projection metadata drift"));
        }
        Ok(())
    }
}

fn dynamic_body_entry(
    class: RuntimeLayoutClass,
    layout: LayoutId,
    element: LayoutId,
    stride: u32,
    u32_layout: LayoutId,
) -> RuntimeLayoutEntry {
    RuntimeLayoutEntry {
        class,
        layout,
        shape: RuntimeLayoutShape::claimed(
            RuntimeRootConvention::AllocationBase,
            0,
            BODY_LENGTH_BYTES,
            Some(stride),
        ),
        extent: RuntimeExtentRecipe::body_counted(BODY_LENGTH_BYTES, stride),
        projections: body_projections(element, stride, u32_layout),
    }
}

fn dynamic_rc_entry(
    class: RuntimeLayoutClass,
    layout: LayoutId,
    element: LayoutId,
    stride: u32,
    u32_layout: LayoutId,
) -> RuntimeLayoutEntry {
    RuntimeLayoutEntry {
        class,
        layout,
        shape: RuntimeLayoutShape::claimed(
            RuntimeRootConvention::ExposedHandle {
                allocation_offset: RC_PREFIX_BYTES,
            },
            RC_PREFIX_BYTES,
            BODY_LENGTH_BYTES,
            Some(stride),
        ),
        extent: RuntimeExtentRecipe::rc_allocation_counted(
            RC_PREFIX_BYTES,
            BODY_LENGTH_BYTES,
            stride,
            RC_SIZE_MASK as u32,
        ),
        projections: rc_projections(element, stride, u32_layout),
    }
}

fn body_projections(
    element: LayoutId,
    stride: u32,
    u32_layout: LayoutId,
) -> Vec<RuntimeProjection> {
    vec![
        RuntimeProjection::fixed(
            RuntimeProjectionKind::Length,
            u32_layout,
            0,
            Some(0),
            BODY_LENGTH_BYTES,
        ),
        RuntimeProjection::repeated(
            RuntimeProjectionKind::Element,
            element,
            BODY_LENGTH_BYTES,
            Some(BODY_LENGTH_BYTES),
            stride,
            stride,
        ),
    ]
}

fn rc_projections(
    element: LayoutId,
    stride: u32,
    u32_layout: LayoutId,
) -> Vec<RuntimeProjection> {
    vec![
        RuntimeProjection::fixed(
            RuntimeProjectionKind::RcRefCount,
            u32_layout,
            0,
            None,
            4,
        ),
        RuntimeProjection::fixed(
            RuntimeProjectionKind::RcAllocationSizeAndTag,
            u32_layout,
            4,
            None,
            4,
        ),
        RuntimeProjection::fixed(
            RuntimeProjectionKind::Length,
            u32_layout,
            RC_PREFIX_BYTES,
            Some(0),
            BODY_LENGTH_BYTES,
        ),
        RuntimeProjection::repeated(
            RuntimeProjectionKind::Element,
            element,
            RC_PREFIX_BYTES + BODY_LENGTH_BYTES,
            Some(BODY_LENGTH_BYTES),
            stride,
            stride,
        ),
    ]
}

fn supported_class(
    request: RuntimeLayoutRequest,
) -> Result<RuntimeLayoutClass, RuntimeLayoutCatalogError> {
    match request {
        RuntimeLayoutRequest::FixedScalar(scalar) => {
            Ok(RuntimeLayoutClass::FixedScalar(scalar))
        }
        RuntimeLayoutRequest::BytePayload(storage) => {
            Ok(RuntimeLayoutClass::BytePayload(storage))
        }
        RuntimeLayoutRequest::ScalarSlotList { slot, storage } => {
            Ok(RuntimeLayoutClass::ScalarSlotList { slot, storage })
        }
        RuntimeLayoutRequest::PointerSlotList { .. } => Err(
            RuntimeLayoutCatalogError::UnsupportedFamily(
                DeferredRuntimeFamily::PointerSlotList,
            ),
        ),
        RuntimeLayoutRequest::Dictionary => Err(RuntimeLayoutCatalogError::UnsupportedFamily(
            DeferredRuntimeFamily::Dictionary,
        )),
        RuntimeLayoutRequest::ClosureEnvironment => Err(
            RuntimeLayoutCatalogError::UnsupportedFamily(
                DeferredRuntimeFamily::ClosureEnvironment,
            ),
        ),
        RuntimeLayoutRequest::PackedBoolList => Err(
            RuntimeLayoutCatalogError::UnsupportedFamily(
                DeferredRuntimeFamily::PackedBoolList,
            ),
        ),
    }
}

fn invalid(
    class: RuntimeLayoutClass,
    reason: &'static str,
) -> RuntimeLayoutCatalogError {
    RuntimeLayoutCatalogError::InvalidDescriptor { class, reason }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RuntimeLayoutCatalogError {
    Layout(LayoutError),
    UnsupportedFamily(DeferredRuntimeFamily),
    UnknownLayout(LayoutId),
    UncataloguedLayout(LayoutId),
    MissingClass(RuntimeLayoutClass),
    DuplicateClass(RuntimeLayoutClass),
    DuplicateLayoutId {
        layout: LayoutId,
        first: RuntimeLayoutClass,
        second: RuntimeLayoutClass,
    },
    ClassMismatch {
        expected: RuntimeLayoutClass,
        actual: Option<RuntimeLayoutClass>,
        claimed: LayoutId,
    },
    ShapeMismatch {
        class: RuntimeLayoutClass,
        expected: RuntimeLayoutShape,
        actual: RuntimeLayoutShape,
    },
    UnknownProjectionLayout {
        class: RuntimeLayoutClass,
        layout: LayoutId,
    },
    InvalidDescriptor {
        class: RuntimeLayoutClass,
        reason: &'static str,
    },
    LengthExceedsCapacity {
        length: u32,
        capacity: u32,
    },
    AllocatorRequestExceedsLimit {
        request: u32,
        limit: u32,
    },
    AllocatorRequestTooSmall {
        request: u32,
        header: u32,
    },
    AllocatorRequestMisaligned {
        request: u32,
        header: u32,
        stride: u32,
    },
    CapacityExceedsAllocation {
        capacity: u32,
        allocated_capacity: u32,
    },
    SizeWordForNonRcLayout,
    RcAllocationRequiresContext,
    NewAllocationForNonRcLayout,
    CountsForFixedLayout,
    ProjectionIsNotRepeated(RuntimeProjectionKind),
    HiddenProjection(RuntimeProjectionKind),
    InvalidStride(u32),
    ArithmeticOverflow,
}

impl From<LayoutError> for RuntimeLayoutCatalogError {
    fn from(error: LayoutError) -> Self {
        Self::Layout(error)
    }
}

impl fmt::Display for RuntimeLayoutCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Layout(error) => write!(formatter, "runtime layout interning failed: {error}"),
            Self::UnsupportedFamily(family) => {
                write!(formatter, "runtime layout family {family:?} is not catalogued")
            }
            Self::UnknownLayout(layout) => write!(formatter, "unknown runtime LayoutId `{layout}`"),
            Self::UncataloguedLayout(layout) => {
                write!(formatter, "LayoutId `{layout}` has no runtime semantic class")
            }
            Self::MissingClass(class) => write!(formatter, "missing runtime layout class {class:?}"),
            Self::DuplicateClass(class) => {
                write!(formatter, "duplicate runtime layout class {class:?}")
            }
            Self::DuplicateLayoutId { layout, first, second } => write!(
                formatter,
                "runtime classes {first:?} and {second:?} share LayoutId `{layout}`"
            ),
            Self::ClassMismatch { expected, actual, claimed } => write!(
                formatter,
                "runtime LayoutId `{claimed}` is {actual:?}, not required class {expected:?}"
            ),
            Self::ShapeMismatch { class, expected, actual } => write!(
                formatter,
                "runtime class {class:?} shape {actual:?} does not match {expected:?}"
            ),
            Self::UnknownProjectionLayout { class, layout } => write!(
                formatter,
                "runtime class {class:?} projects unknown LayoutId `{layout}`"
            ),
            Self::InvalidDescriptor { class, reason } => {
                write!(formatter, "runtime class {class:?} is invalid: {reason}")
            }
            Self::LengthExceedsCapacity { length, capacity } => write!(
                formatter,
                "runtime length {length} exceeds authenticated capacity {capacity}"
            ),
            Self::AllocatorRequestExceedsLimit { request, limit } => write!(
                formatter,
                "runtime allocator request {request} exceeds header limit {limit}"
            ),
            Self::AllocatorRequestTooSmall { request, header } => write!(
                formatter,
                "runtime allocator request {request} is smaller than header {header}"
            ),
            Self::AllocatorRequestMisaligned { request, header, stride } => write!(
                formatter,
                "runtime allocator request {request} is not {header} plus a multiple of stride {stride}"
            ),
            Self::CapacityExceedsAllocation { capacity, allocated_capacity } => write!(
                formatter,
                "runtime capacity {capacity} exceeds allocation bound {allocated_capacity}"
            ),
            Self::SizeWordForNonRcLayout => {
                formatter.write_str("RC size word supplied for a non-RC runtime layout")
            }
            Self::RcAllocationRequiresContext => formatter.write_str(
                "RC extent requires either new-allocation or existing-size-word context"
            ),
            Self::NewAllocationForNonRcLayout => {
                formatter.write_str("non-RC runtime layout cannot mint an allocator request")
            }
            Self::CountsForFixedLayout => {
                formatter.write_str("dynamic counts supplied for a fixed runtime layout")
            }
            Self::ProjectionIsNotRepeated(kind) => {
                write!(formatter, "runtime projection {kind:?} is not repeated")
            }
            Self::HiddenProjection(kind) => write!(
                formatter,
                "hidden runtime projection {kind:?} has no exposed-handle offset"
            ),
            Self::InvalidStride(stride) => {
                write!(formatter, "invalid zero runtime stride {stride}")
            }
            Self::ArithmeticOverflow => formatter.write_str("runtime layout arithmetic overflow"),
        }
    }
}

impl std::error::Error for RuntimeLayoutCatalogError {}
