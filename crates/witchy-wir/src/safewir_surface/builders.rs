//! Descriptor-backed construction of executable SafeWIR place claims.
//!
//! The builder is intentionally narrower than the claim structs in the parent
//! module. Those structs are an untrusted transport surface; this module is the
//! production migration surface. It derives every semantic projection and byte
//! range from a canonical descriptor and accepts only encoded address shapes it
//! can compare with that derivation.

use std::fmt;

use crate::layout::{
    FieldKind, HeaderLayout, LayoutId, LayoutInterner, LayoutKind, LayoutSize, RootAnchor,
    SectionSpec,
};
use crate::safewir::AccessMode;
use crate::wir::{BinOp, WirExpr};

use super::{
    SafeExtentExpr, SafeObjectExpr, SafePlaceExpr, SafeProjectionExpr, SafeProjectionStep,
    SafeProvenanceExpr, SafeRangeExpr, SafeSizeExpr,
};

/// Exact object extent supplied at the migration site.
///
/// The representation is sealed and fixed-layout-only. Dynamic descriptors
/// remain unsupported until the final adapter can bind an actual executable
/// count definition from the final body. There is deliberately no dynamic or
/// `Unknown` form.
///
/// ```compile_fail
/// use witchy_wir::safewir_surface::SafeExactExtent;
/// let _ = SafeExactExtent::counted_from_header();
/// ```
///
/// ```compile_fail
/// use witchy_wir::safewir_surface::SafeExactExtent;
/// let _ = SafeExactExtent::counted_contract_parameter;
/// ```
///
/// ```compile_fail
/// use witchy_wir::safewir_surface::SafeExactExtent;
/// let _ = SafeExactExtent { bytes: 8 };
/// ```
#[derive(Debug, Clone)]
pub struct SafeExactExtent {
    source: ExactExtentSource,
}

#[derive(Debug, Clone)]
enum ExactExtentSource {
    /// A compile-time byte count, for a fixed-size descriptor.
    Fixed(u64),
    /// The descriptor's own header region, for a dynamically sized object whose
    /// count is not yet known.
    HeaderOnly,
    /// One executable count operand, for a dynamically sized descriptor. The
    /// header bytes, the stride, and the range arithmetic all stay
    /// descriptor-derived: this supplies the count and nothing else.
    Counted(WirExpr),
}

impl SafeExactExtent {
    pub const fn fixed(bytes: u64) -> Self {
        Self { source: ExactExtentSource::Fixed(bytes) }
    }

    /// Claim only the descriptor's header region of a dynamically sized object.
    ///
    /// Reading a buffer's length word is the operation that ESTABLISHES its
    /// count, so a counted extent is circular there. This claims strictly less:
    /// the `base` bytes the descriptor places before the payload, whose
    /// presence follows from the root being a well-formed allocation of this
    /// layout -- which claiming the layout at all already assumes.
    pub const fn header_only() -> Self {
        Self { source: ExactExtentSource::HeaderOnly }
    }

    /// Retain one exact executable count identity for final def-use binding.
    ///
    /// This is what lets a runtime-sized object be described at all. A bytes
    /// buffer is allocated as `rc_alloc(2 * len + 20)`, so its extent is not a
    /// constant and `fixed` cannot express it; without this the 399 raw sites
    /// in `wir_helpers` have no migration target.
    ///
    /// Construction grants no authority. The count is checked against the
    /// claim-operand grammar here, and bound to an exact reaching definition by
    /// the final validator.
    pub fn counted(count: WirExpr) -> Result<Self, SafePlaceBuildError> {
        validate_claim_operand(&count, "counted object length")?;
        Ok(Self { source: ExactExtentSource::Counted(count) })
    }
}

/// The original executable address operands of one raw memory operation.
///
/// Construction grants no authority. A [`SafePlaceBuilder`] accepts this value
/// only when its base expression and Wasm immediate offset exactly denote the
/// descriptor-derived projection under the builder's checked address grammar.

#[derive(Debug, Clone, Copy)]
struct DynamicShape {
    base: u32,
    stride: u32,
}

/// How deep the pointer search walks before failing closed.
///
/// Descriptor graphs are acyclic and shallow; a bound means a malformed or
/// adversarially deep graph costs a rejection rather than a stack.
const POINTER_SEARCH_DEPTH: usize = 8;

/// Whether a pointer slot is reachable by value from this descriptor.
///
/// Fails CLOSED: an unknown child or an exhausted budget answers "yes", because
/// the question is whether to hand out scalar-buffer authority and an incomplete
/// answer is not a yes.
fn contains_pointer_slot(
    layouts: &LayoutInterner,
    descriptor: &crate::layout::LayoutDescriptor,
    depth: usize,
) -> bool {
    if depth == 0 {
        return true;
    }
    descriptor.fields().iter().any(|field| match field.kind() {
        FieldKind::Pointer { .. } => true,
        FieldKind::Inline(child) => match layouts.get(child) {
            Some(child) => contains_pointer_slot(layouts, child, depth - 1),
            None => true,
        },
        FieldKind::Scalar(_) => false,
    })
}

/// Derive the checked dynamic shape of a runtime-sized descriptor.
///
/// Deliberately an allowlist. A dynamically sized layout only gets a counted
/// extent if this function recognises its exact shape; anything else -- in
/// particular a pointer-bearing family -- is rejected rather than inheriting
/// scalar buffer authority.
fn dynamic_shape(
    layouts: &LayoutInterner,
    descriptor: &crate::layout::LayoutDescriptor,
    layout: LayoutId,
) -> Result<DynamicShape, SafePlaceBuildError> {
    // A SECTION-MAJOR layout has no single element, so the packed invariant
    // below (stride == one element's size) does not apply to it. The equivalent
    // statement is an algebraic identity between the section table and the
    // descriptor's own size, checked in `validate_section_table`.
    if descriptor.kind().sections().is_some() {
        let LayoutSize::Dynamic { base, stride } = descriptor.size() else {
            return Err(SafePlaceBuildError::DynamicDescriptorMismatch(layout));
        };
        if contains_pointer_slot(layouts, descriptor, POINTER_SEARCH_DEPTH) {
            return Err(SafePlaceBuildError::UnsupportedDynamicLayout(layout));
        }
        validate_section_table(descriptor, layout)?;
        return Ok(DynamicShape { base, stride });
    }
    let element = match descriptor.kind() {
        LayoutKind::PackedList { element, .. } | LayoutKind::RuntimeBuffer { element } => *element,
        // A dense dictionary body is a packed run of equal-stride rows. It is
        // admitted only alongside the TRANSITIVE pointer check below: it is the
        // one allowlisted kind that can legitimately hold a pointer out of
        // top-level view, because `validate_runtime_dictionary_slot` accepts an
        // owned pointer slot as a key or value and the body inlines those.
        LayoutKind::RuntimeDictionaryBody { row, .. } => *row,
        _ => return Err(SafePlaceBuildError::UnsupportedDynamicLayout(layout)),
    };
    let LayoutSize::Dynamic { base, stride } = descriptor.size() else {
        return Err(SafePlaceBuildError::DynamicDescriptorMismatch(layout));
    };
    // A pointer slot anywhere in the object means a loaded pointer's provenance
    // would ride on a byte range this builder cannot authenticate.
    //
    // "Anywhere" is transitive, and has to be. For a buffer or a packed list the
    // top level alone would do -- a runtime buffer's element must be a scalar,
    // and the layout model refuses to inline an owning reference into an
    // aggregate (`ReferenceNotInline`), so a pointer cannot get deeper. A
    // DICTIONARY body is the exception: its slot validator accepts an owned
    // pointer slot as key or value, and the body holds those as `Inline` fields.
    // Reading only the top level would hand a string-keyed dictionary
    // scalar-buffer authority over owned pointers.
    if contains_pointer_slot(layouts, descriptor, POINTER_SEARCH_DEPTH) {
        return Err(SafePlaceBuildError::UnsupportedDynamicLayout(layout));
    }
    let element_descriptor = layouts
        .get(element)
        .ok_or(SafePlaceBuildError::UnknownLayout(element))?;
    let LayoutSize::Fixed(element_size) = element_descriptor.size() else {
        return Err(SafePlaceBuildError::DynamicInlineField(element));
    };
    // The stride must be the element's own size, or an index would not land on
    // an element boundary.
    if element_size != stride {
        return Err(SafePlaceBuildError::DynamicDescriptorMismatch(layout));
    }
    Ok(DynamicShape { base, stride })
}

/// The section table must describe exactly the object the descriptor describes.
///
/// The last section ends where the object ends, for EVERY element count:
///
///     last.constant_base + last.count_multiple * c + last.element_width * c
///         == base + stride * c        for all c
///
/// which holds precisely when `last.constant_base == base` and
/// `last.count_multiple + last.element_width == stride`. That identity is what
/// makes the table a derivation of the descriptor rather than a second opinion
/// about the same bytes -- if someone edits one without the other, every claim on
/// this layout stops building.
fn validate_section_table(
    descriptor: &crate::layout::LayoutDescriptor,
    layout: LayoutId,
) -> Result<(), SafePlaceBuildError> {
    let sections = descriptor
        .kind()
        .sections()
        .ok_or(SafePlaceBuildError::NotSectionMajor(layout))?;
    let LayoutSize::Dynamic { base, stride } = descriptor.size() else {
        return Err(SafePlaceBuildError::DynamicDescriptorMismatch(layout));
    };
    let last = sections
        .last()
        .ok_or(SafePlaceBuildError::DynamicDescriptorMismatch(layout))?;
    let span = last
        .count_multiple
        .checked_add(last.element_width)
        .ok_or(SafePlaceBuildError::ArithmeticOverflow)?;
    if last.constant_base != base || span != stride {
        return Err(SafePlaceBuildError::DynamicDescriptorMismatch(layout));
    }
    Ok(())
}

/// `base + count * stride`, as a checked size expression.
///
/// A constant count is folded and narrowed here so the common case carries no
/// symbolic arithmetic. A dynamic count stays symbolic for the final validator
/// to bind and prove non-wrapping.
fn counted_length(
    count: &WirExpr,
    base: u32,
    stride: u32,
) -> Result<SafeSizeExpr, SafePlaceBuildError> {
    if let Some(count) = constant_u32(count)? {
        let length = u64::from(count)
            .checked_mul(u64::from(stride))
            .and_then(|payload| u64::from(base).checked_add(payload))
            .ok_or(SafePlaceBuildError::ArithmeticOverflow)?;
        let length = u32::try_from(length)
            .map_err(|_| SafePlaceBuildError::NarrowingOverflow { value: length })?;
        return Ok(SafeSizeExpr::Constant(u64::from(length)));
    }
    Ok(checked_narrow(SafeSizeExpr::Add(
        Box::new(SafeSizeExpr::Constant(u64::from(base))),
        Box::new(SafeSizeExpr::Multiply(
            Box::new(SafeSizeExpr::Value(Box::new(count.clone()))),
            Box::new(SafeSizeExpr::Constant(u64::from(stride))),
        )),
    )))
}


/// A bound on how much address arithmetic will be decomposed before giving up,
/// so a pathological expression cannot make this walk unboundedly.
const ADDRESS_GRAMMAR_TERM_LIMIT: usize = 64;

/// A packed projection scales one term; a section-major projection scales two,
/// the section base and the slot index. Nothing in this grammar needs a third,
/// and a bound keeps a malformed address a rejection rather than a search.
const MAX_SCALED_ADDRESS_TERMS: usize = 2;

/// One address expression decomposed into `root + index * stride + constant`.
///
/// This exists so the dynamic-index check can be a reconciliation against the
/// descriptor rather than a catalogue of expression shapes. Helper code spells
/// the same address several ways -- operands commuted, additions nested either
/// way, constants split -- and matching each spelling would be a shape zoo of
/// exactly the kind this codebase forbids. Flattening first means one rule.
#[derive(Default)]
struct AffineAddress<'expr> {
    /// Non-constant terms carrying no explicit scale. There are at most two:
    /// the object root, and -- when the stride is one -- the index, which
    /// helper code spells as a bare `buf + i` rather than `buf + i * 1`.
    leaves: Vec<&'expr WirExpr>,
    /// Scaled terms. A packed projection has at most one (`index * stride`); a
    /// SECTION-MAJOR projection has two, because the section's own base is a
    /// multiple of the element count (`count * count_multiple + index * width`).
    scaled: Vec<(&'expr WirExpr, u64)>,
    constant: u64,
}

impl<'expr> AffineAddress<'expr> {
    fn of(expression: &'expr WirExpr) -> Option<Self> {
        let mut address = Self::default();
        let mut terms = 0usize;
        address.accumulate(expression, &mut terms).ok()?;
        Some(address)
    }

    fn accumulate(
        &mut self,
        expression: &'expr WirExpr,
        terms: &mut usize,
    ) -> Result<(), SafePlaceBuildError> {
        *terms += 1;
        if *terms > ADDRESS_GRAMMAR_TERM_LIMIT {
            return Err(SafePlaceBuildError::ArithmeticOverflow);
        }
        match expression {
            // Addition is flattened and treated as commutative, so `a + b` and
            // `b + a` decompose identically.
            WirExpr::Binary { op: BinOp::Add, lhs, rhs, .. } => {
                self.accumulate(lhs, terms)?;
                self.accumulate(rhs, terms)
            }
            WirExpr::ConstI32(value) => {
                let value = u64::try_from(*value)
                    .map_err(|_| SafePlaceBuildError::ArithmeticOverflow)?;
                self.constant = self
                    .constant
                    .checked_add(value)
                    .ok_or(SafePlaceBuildError::ArithmeticOverflow)?;
                Ok(())
            }
            // A scaled term, in either operand order.
            WirExpr::Binary { op: BinOp::Mul, lhs, rhs, .. } => {
                let (operand, scale) = match (constant_u32(lhs)?, constant_u32(rhs)?) {
                    (None, Some(scale)) => (lhs.as_ref(), scale),
                    (Some(scale), None) => (rhs.as_ref(), scale),
                    // Two constants would be a folded constant, and two
                    // non-constants is not an affine address.
                    _ => return Err(SafePlaceBuildError::ArithmeticOverflow),
                };
                if self.scaled.len() == MAX_SCALED_ADDRESS_TERMS {
                    return Err(SafePlaceBuildError::ArithmeticOverflow);
                }
                self.scaled.push((operand, u64::from(scale)));
                Ok(())
            }
            // Any other term is an unscaled leaf. Which one is the root and
            // which is a stride-one index cannot be decided here; the caller
            // knows the claimed index and settles it.
            other => {
                if self.leaves.len() == 2 {
                    return Err(SafePlaceBuildError::ArithmeticOverflow);
                }
                self.leaves.push(other);
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct SafeEncodedAddress {
    base: WirExpr,
    offset: u32,
}

impl SafeEncodedAddress {
    pub fn new(base: WirExpr, offset: u32) -> Self {
        Self { base, offset }
    }

    pub fn base(&self) -> &WirExpr {
        &self.base
    }

    pub const fn offset(&self) -> u32 {
        self.offset
    }
}

/// A descriptor-backed, exact-extent object claim ready to project into places.
///
/// Ordinary local and string-data roots conservatively receive `Unknown`
/// provenance. This builder cannot mint `Fresh`, `StaticData`, or accept
/// passthrough provenance; dedicated final-body analysis must establish those
/// states at their structural operation sites. The final adapter remains
/// responsible for binding claims to the actual def-use graph.
pub struct SafePlaceBuilder<'layouts> {
    layouts: &'layouts LayoutInterner,
    object: SafeObjectExpr,
    full_length: SafeSizeExpr,
    access: AccessMode,
}

impl<'layouts> SafePlaceBuilder<'layouts> {
    pub fn new(
        layouts: &'layouts LayoutInterner,
        layout: LayoutId,
        exact_extent: SafeExactExtent,
        object_root: WirExpr,
        access: AccessMode,
    ) -> Result<Self, SafePlaceBuildError> {
        let descriptor = layouts
            .get(layout)
            .ok_or(SafePlaceBuildError::UnknownLayout(layout))?;
        let canonical = descriptor.layout_id();
        if canonical != layout {
            return Err(SafePlaceBuildError::NonCanonicalLayout {
                claimed: layout,
                canonical,
            });
        }
        if descriptor.anchor() != RootAnchor::AllocationBase {
            if let Some((index, FieldKind::Pointer { pointee, slot })) = descriptor
                .fields()
                .iter()
                .enumerate()
                .find(|(_, field)| matches!(field.kind(), FieldKind::Pointer { .. }))
                .map(|(index, field)| (index, field.kind()))
            {
                return Err(SafePlaceBuildError::PointerSlotProvenance {
                    field: u32::try_from(index).unwrap_or(u32::MAX),
                    pointee,
                    slot,
                });
            }
            return Err(SafePlaceBuildError::ExposedRoot {
                layout,
                allocation_offset: descriptor.anchor().allocation_offset(),
            });
        }
        let provenance = provenance_for_ordinary_root(&object_root)?;

        let (extent, full_length) = match (descriptor.size(), exact_extent.source) {
            (LayoutSize::Fixed(expected), ExactExtentSource::Fixed(actual)) => {
                let actual = u32::try_from(actual)
                    .map_err(|_| SafePlaceBuildError::NarrowingOverflow { value: actual })?;
                if actual != expected {
                    return Err(SafePlaceBuildError::FixedExtentMismatch {
                        expected,
                        actual,
                    });
                }
                let size = SafeSizeExpr::Constant(u64::from(expected));
                (SafeExtentExpr::Exact(size.clone()), size)
            }
            // A fixed descriptor already knows its own size; accepting a count
            // for it would let a caller claim a length the layout contradicts.
            (LayoutSize::Fixed(_), ExactExtentSource::Counted(_)) => {
                return Err(SafePlaceBuildError::UnexpectedCheckedLength(layout));
            }
            (LayoutSize::Dynamic { .. }, ExactExtentSource::Fixed(_)) => {
                return Err(SafePlaceBuildError::MissingCheckedLength(layout));
            }
            (LayoutSize::Dynamic { base, .. }, ExactExtentSource::HeaderOnly) => {
                let size = SafeSizeExpr::Constant(u64::from(base));
                (SafeExtentExpr::Exact(size.clone()), size)
            }
            (LayoutSize::Fixed(_), ExactExtentSource::HeaderOnly) => {
                return Err(SafePlaceBuildError::UnexpectedCheckedLength(layout));
            }
            (LayoutSize::Dynamic { base, stride }, ExactExtentSource::Counted(count)) => {
                // The count must not be read through the object it measures:
                // that would make the extent depend on the storage it bounds.
                if same_address_leaf(&count, &object_root) {
                    return Err(SafePlaceBuildError::CountAliasesObjectRoot);
                }
                let shape = dynamic_shape(layouts, descriptor, layout)?;
                if shape.base != base || shape.stride != stride {
                    return Err(SafePlaceBuildError::DynamicDescriptorMismatch(layout));
                }
                let full_length = counted_length(&count, base, stride)?;
                (
                    SafeExtentExpr::Counted {
                        header: SafeSizeExpr::Constant(u64::from(base)),
                        count: Box::new(count),
                        stride: SafeSizeExpr::Constant(u64::from(stride)),
                    },
                    full_length,
                )
            }
        };

        Ok(Self {
            layouts,
            object: SafeObjectExpr {
                root: Box::new(object_root),
                layout,
                extent,
                provenance,
            },
            full_length,
            access,
        })
    }

    /// Claim the complete object. This is the bulk copy/fill migration shape;
    /// scalar loads and stores should select a header, field, or element.
    pub fn root(
        &self,
        encoded: SafeEncodedAddress,
    ) -> Result<Box<SafePlaceExpr>, SafePlaceBuildError> {
        self.validate_fixed_address(&encoded, 0, "root")?;
        self.finish(
            SafeProjectionExpr::root(),
            SafeSizeExpr::Constant(0),
            self.full_length.clone(),
            encoded,
        )
    }

    /// Claim one descriptor-defined packed-list header component.
    /// Component zero is length and component one is capacity.
    /// Claim a runtime buffer's length word.
    ///
    /// A runtime buffer carries its length as an ordinary descriptor field at
    /// offset zero rather than in a `HeaderLayout`, so `header()` does not
    /// apply to it and `fixed_field()` refuses it outright. This is the one
    /// sanctioned way to reach that word.
    ///
    /// It emits the existing `ProjectionStep::Field { index: 0 }`. That is
    /// deliberate: the validator's `Field` arm already derives exactly the
    /// right offset, length, and scalar kind for it with no layout-kind guard,
    /// so a new projection step would add arms across eight files for no new
    /// expressive power, and extending `HeaderLayout` would change
    /// `canonical_bytes` and re-mint every runtime buffer's `LayoutId`.
    ///
    /// The offset and width come from the descriptor's own field table.
    /// Nothing here hardcodes zero, four, or a stride.
    pub fn runtime_buffer_length(
        &self,
        encoded: SafeEncodedAddress,
    ) -> Result<Box<SafePlaceExpr>, SafePlaceBuildError> {
        let descriptor = self.descriptor(self.object.layout)?;
        if !matches!(descriptor.kind(), LayoutKind::RuntimeBuffer { .. }) {
            return Err(SafePlaceBuildError::ProjectionRequiresRuntimeBuffer {
                projection: "runtime buffer length",
            });
        }
        let field = descriptor
            .fields()
            .first()
            .ok_or(SafePlaceBuildError::FieldOutOfBounds { index: 0 })?;
        let FieldKind::Scalar(kind) = field.kind() else {
            return Err(SafePlaceBuildError::ProjectionRequiresRuntimeBuffer {
                projection: "runtime buffer length",
            });
        };
        self.validate_fixed_address(&encoded, field.offset(), "runtime buffer length")?;
        self.finish(
            SafeProjectionExpr::from_steps([SafeProjectionStep::Field { index: 0 }]),
            SafeSizeExpr::Constant(u64::from(field.offset())),
            SafeSizeExpr::Constant(u64::from(kind.size())),
            encoded,
        )
    }

    pub fn header(
        &self,
        component: u32,
        encoded: SafeEncodedAddress,
    ) -> Result<Box<SafePlaceExpr>, SafePlaceBuildError> {
        let descriptor = self.descriptor(self.object.layout)?;
        let HeaderLayout::PackedList {
            length_offset,
            capacity_offset,
            ..
        } = descriptor.header()
        else {
            return Err(SafePlaceBuildError::HeaderNotDefined(self.object.layout));
        };
        let offset = match component {
            0 => length_offset,
            1 => capacity_offset,
            _ => return Err(SafePlaceBuildError::UnknownHeaderComponent(component)),
        };
        self.validate_fixed_address(&encoded, offset, "header")?;
        self.finish(
            SafeProjectionExpr::from_steps([SafeProjectionStep::Header { component }]),
            SafeSizeExpr::Constant(u64::from(offset)),
            SafeSizeExpr::Constant(4),
            encoded,
        )
    }

    /// Claim one fixed descriptor field. Pointer slots are rejected because a
    /// field offset cannot authenticate the loaded pointer's provenance.
    pub fn fixed_field(
        &self,
        index: u32,
        encoded: SafeEncodedAddress,
    ) -> Result<Box<SafePlaceExpr>, SafePlaceBuildError> {
        let descriptor = self.descriptor(self.object.layout)?;
        if matches!(descriptor.kind(), LayoutKind::RuntimeBuffer { .. }) {
            return Err(SafePlaceBuildError::RuntimeBufferProjection(self.object.layout));
        }
        if matches!(descriptor.kind(), LayoutKind::PackedList { .. }) {
            return Err(SafePlaceBuildError::PackedListFieldProjection(
                self.object.layout,
            ));
        }
        let field = descriptor
            .fields()
            .get(index as usize)
            .ok_or(SafePlaceBuildError::FieldOutOfBounds { index })?;
        let length = match field.kind() {
            FieldKind::Scalar(kind) => kind.size(),
            FieldKind::Inline(child) => match self.descriptor(child)?.size() {
                LayoutSize::Fixed(size) => size,
                LayoutSize::Dynamic { .. } => {
                    return Err(SafePlaceBuildError::DynamicInlineField(child));
                }
            },
            FieldKind::Pointer { pointee, slot } => {
                return Err(SafePlaceBuildError::PointerSlotProvenance {
                    field: index,
                    pointee,
                    slot,
                });
            }
        };
        self.validate_fixed_address(&encoded, field.offset(), "fixed field")?;
        self.finish(
            SafeProjectionExpr::from_steps([SafeProjectionStep::Field { index }]),
            SafeSizeExpr::Constant(u64::from(field.offset())),
            SafeSizeExpr::Constant(u64::from(length)),
            encoded,
        )
    }

    /// Claim one packed-list element. The index remains an executable scalar
    /// source for the final def-use adapter; no definition identity is invented.
    /// A checked place for slot `index` of section `section`.
    ///
    /// A SECTION-MAJOR layout stores all of one field and then all of the next,
    /// so a section's base is `constant_base + count_multiple * count` -- a
    /// function of the element count rather than a descriptor constant. No
    /// `Element`/`Field` chain can name a slot in one, which is why this exists.
    ///
    /// Every number is the descriptor's: the caller names only WHICH section and
    /// which slot. The count is read from the object's own extent, not from an
    /// argument, so a caller cannot claim one count and address another.
    pub fn section_element(
        &self,
        section: u32,
        index: WirExpr,
        encoded: SafeEncodedAddress,
    ) -> Result<Box<SafePlaceExpr>, SafePlaceBuildError> {
        validate_claim_operand(&index, "section element index")?;
        let descriptor = self.descriptor(self.object.layout)?;
        let sections = descriptor
            .kind()
            .sections()
            .ok_or(SafePlaceBuildError::NotSectionMajor(self.object.layout))?;
        let spec = *sections
            .get(section as usize)
            .ok_or(SafePlaceBuildError::SectionOutOfBounds { section })?;
        let SafeExtentExpr::Counted { count, .. } = &self.object.extent else {
            return Err(SafePlaceBuildError::SectionRequiresCountedExtent(
                self.object.layout,
            ));
        };
        let count = count.as_ref().clone();
        self.validate_section_address(&encoded, &count, &index, spec, "section element")?;
        let start = section_start(&count, &index, spec)?;
        self.finish(
            SafeProjectionExpr::from_steps([SafeProjectionStep::Section {
                section,
                index: Box::new(index),
            }]),
            start,
            SafeSizeExpr::Constant(u64::from(spec.element_width)),
            encoded,
        )
    }

    pub fn element(
        &self,
        index: WirExpr,
        encoded: SafeEncodedAddress,
    ) -> Result<Box<SafePlaceExpr>, SafePlaceBuildError> {
        validate_claim_operand(&index, "element index")?;
        let (base, stride) = self.packed_elements("element")?;
        self.validate_indexed_address(&encoded, &index, base, stride, "element")?;
        let start = indexed_start(&index, base, stride)?;
        self.finish(
            SafeProjectionExpr::from_steps([SafeProjectionStep::Element {
                index: Box::new(index),
            }]),
            start,
            SafeSizeExpr::Constant(u64::from(stride)),
            encoded,
        )
    }

    /// Claim one fixed field WITHIN one packed element -- `[Element, Field]`.
    ///
    /// The only two-step projection the builder emits, and it exists for the
    /// dictionary row: row `i`'s key and value are fields at fixed offsets inside
    /// a sixteen-byte row, and neither `element` (which claims the whole row) nor
    /// `fixed_field` (which has no index) can name one. The validator already
    /// walks multi-step chains -- `derive_projection` sets `layout` to the element
    /// and then reads that descriptor's field table -- so this adds no validator
    /// surface.
    ///
    /// Every offset is derived: base and stride from the object's descriptor, the
    /// field's offset and width from the ELEMENT's descriptor.
    pub fn element_field(
        &self,
        index: WirExpr,
        field: u32,
        encoded: SafeEncodedAddress,
    ) -> Result<Box<SafePlaceExpr>, SafePlaceBuildError> {
        validate_claim_operand(&index, "element index")?;
        let (base, stride) = self.packed_elements("element field")?;
        let element = self
            .descriptor(self.object.layout)?
            .kind()
            .packed_element_layout()
            .ok_or(SafePlaceBuildError::ProjectionRequiresPackedList {
                projection: "element field",
            })?;
        let entry = *self
            .descriptor(element)?
            .fields()
            .get(field as usize)
            .ok_or(SafePlaceBuildError::FieldOutOfBounds { index: field })?;
        let width = match entry.kind() {
            FieldKind::Scalar(kind) => kind.size(),
            FieldKind::Inline(child) => match self.descriptor(child)?.size() {
                LayoutSize::Fixed(size) => size,
                LayoutSize::Dynamic { .. } => {
                    return Err(SafePlaceBuildError::DynamicInlineField(child));
                }
            },
            FieldKind::Pointer { pointee, slot } => {
                return Err(SafePlaceBuildError::PointerSlotProvenance {
                    field,
                    pointee,
                    slot,
                });
            }
        };
        let within = entry.offset();
        self.validate_indexed_address(
            &encoded,
            &index,
            base.checked_add(within)
                .ok_or(SafePlaceBuildError::ArithmeticOverflow)?,
            stride,
            "element field",
        )?;
        let start = checked_narrow(SafeSizeExpr::Add(
            Box::new(indexed_start(&index, base, stride)?),
            Box::new(SafeSizeExpr::Constant(u64::from(within))),
        ));
        self.finish(
            SafeProjectionExpr::from_steps([
                SafeProjectionStep::Element { index: Box::new(index) },
                SafeProjectionStep::Field { index: field },
            ]),
            start,
            SafeSizeExpr::Constant(u64::from(width)),
            encoded,
        )
    }

    /// Claim a contiguous packed-list slice. Start and length are independently
    /// retained so the final adapter can bind and prove both exact definitions.
    pub fn slice(
        &self,
        start: WirExpr,
        length: WirExpr,
        encoded: SafeEncodedAddress,
    ) -> Result<Box<SafePlaceExpr>, SafePlaceBuildError> {
        validate_claim_operand(&start, "slice start")?;
        validate_claim_operand(&length, "slice length")?;
        let (base, stride) = self.packed_elements("slice")?;
        self.validate_indexed_address(&encoded, &start, base, stride, "slice")?;
        let range_start = indexed_start(&start, base, stride)?;
        let range_length = scaled_length(&length, stride)?;
        self.finish(
            SafeProjectionExpr::from_steps([SafeProjectionStep::Slice {
                start: Box::new(start),
                length: Box::new(length),
            }]),
            range_start,
            range_length,
            encoded,
        )
    }

    fn descriptor(
        &self,
        layout: LayoutId,
    ) -> Result<&'layouts crate::layout::LayoutDescriptor, SafePlaceBuildError> {
        self.layouts
            .get(layout)
            .ok_or(SafePlaceBuildError::UnknownLayout(layout))
    }

    fn packed_elements(
        &self,
        projection: &'static str,
    ) -> Result<(u32, u32), SafePlaceBuildError> {
        let descriptor = self.descriptor(self.object.layout)?;
        // A runtime buffer is the same physical shape as a packed list -- a
        // length word then stride-sized elements -- so it indexes identically.
        // The element comes from `packed_element_layout` so this guard and the
        // validator's Element/Slice arms cannot drift apart.
        if descriptor.kind().packed_element_layout().is_none() {
            return Err(SafePlaceBuildError::ProjectionRequiresPackedList { projection });
        }
        let LayoutSize::Dynamic { base, stride } = descriptor.size() else {
            return Err(SafePlaceBuildError::ProjectionRequiresPackedList { projection });
        };
        Ok((base, stride))
    }

    /// Reconcile a constant projection's address against the descriptor.
    ///
    /// The descriptor-derived byte offset may ride in the wasm immediate, in the
    /// base expression, or be split between them; only the SUM is the address.
    /// The dynamic-index path below already answers it that way, and a constant
    /// projection is the same question with no index term, so it answers it the
    /// same way rather than insisting the base be the bare root leaf.
    ///
    /// This is not a convenience. A raw `memory.copy` has no immediate operand
    /// at all, so `erase_node` rejects any bulk-copy place carrying a nonzero
    /// one (`wir_encode.rs`, "a bulk-copy place retained a nonzero immediate
    /// offset"). A bulk copy of a `[u32 length][payload]` buffer must therefore
    /// spell its payload address as `root + 4` with immediate zero -- which the
    /// leaf-only rule refused, leaving constant-start bulk copies unbuildable.
    fn validate_fixed_address(
        &self,
        encoded: &SafeEncodedAddress,
        expected: u32,
        projection: &'static str,
    ) -> Result<(), SafePlaceBuildError> {
        // The common spelling: the bare root, with the whole offset immediate.
        if same_address_leaf(&encoded.base, &self.object.root) {
            if encoded.offset != expected {
                return Err(SafePlaceBuildError::EncodedOffsetMismatch {
                    projection,
                    expected,
                    actual: encoded.offset,
                });
            }
            return Ok(());
        }
        // Otherwise decompose, exactly as the dynamic path does, and require the
        // root plus a constant. A constant projection has no index, so a scaled
        // term or a second bare leaf means this address denotes something else.
        let Some(address) = AffineAddress::of(&encoded.base) else {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        };
        let ([root], []) = (address.leaves.as_slice(), address.scaled.as_slice()) else {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        };
        if !same_address_leaf(root, &self.object.root) {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        }
        let claimed = address
            .constant
            .checked_add(u64::from(encoded.offset))
            .ok_or(SafePlaceBuildError::ArithmeticOverflow)?;
        if claimed != u64::from(expected) {
            let actual = u32::try_from(claimed)
                .map_err(|_| SafePlaceBuildError::NarrowingOverflow { value: claimed })?;
            return Err(SafePlaceBuildError::EncodedOffsetMismatch {
                projection,
                expected,
                actual,
            });
        }
        Ok(())
    }

    /// Reconcile a section address against the descriptor's section spec.
    ///
    /// The address must decompose to exactly
    /// `root + constant_base + count * count_multiple + index * element_width`,
    /// where a multiple or width of one may ride as a bare leaf (helper code
    /// spells `idx + 4 + h`, not `idx + 4 + h * 1`) and a multiple of zero
    /// contributes no term at all. Anything left over means the address denotes
    /// something other than the claimed slot.
    fn validate_section_address(
        &self,
        encoded: &SafeEncodedAddress,
        count: &WirExpr,
        index: &WirExpr,
        spec: SectionSpec,
        projection: &'static str,
    ) -> Result<(), SafePlaceBuildError> {
        let Some(address) = AffineAddress::of(&encoded.base) else {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        };
        let constant = address
            .constant
            .checked_add(u64::from(encoded.offset))
            .ok_or(SafePlaceBuildError::ArithmeticOverflow)?;
        if constant != u64::from(spec.constant_base) {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        }
        let mut leaves: Vec<&WirExpr> = address.leaves.clone();
        let mut scaled: Vec<(&WirExpr, u64)> = address.scaled.clone();
        // Account for each descriptor-required term. A term whose scale is one is
        // accepted either explicitly scaled or as a bare leaf; a scale of zero
        // requires no term.
        for (operand, scale) in [
            (count, u64::from(spec.count_multiple)),
            (index, u64::from(spec.element_width)),
        ] {
            if scale == 0 {
                continue;
            }
            if let Some(position) = scaled
                .iter()
                .position(|(term, term_scale)| *term_scale == scale && same_address_leaf(term, operand))
            {
                scaled.remove(position);
                continue;
            }
            if scale == 1 {
                if let Some(position) =
                    leaves.iter().position(|leaf| same_address_leaf(leaf, operand))
                {
                    leaves.remove(position);
                    continue;
                }
            }
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        }
        // Exactly the root may remain.
        if !scaled.is_empty() {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        }
        let [root] = leaves.as_slice() else {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        };
        if !same_address_leaf(root, &self.object.root) {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        }
        Ok(())
    }

    fn validate_indexed_address(
        &self,
        encoded: &SafeEncodedAddress,
        index: &WirExpr,
        base: u32,
        stride: u32,
        projection: &'static str,
    ) -> Result<(), SafePlaceBuildError> {
        if let Some(index) = constant_u32(index)? {
            let expected = u64::from(base)
                .checked_add(
                    u64::from(index)
                        .checked_mul(u64::from(stride))
                        .ok_or(SafePlaceBuildError::ArithmeticOverflow)?,
                )
                .ok_or(SafePlaceBuildError::ArithmeticOverflow)?;
            let expected = u32::try_from(expected)
                .map_err(|_| SafePlaceBuildError::NarrowingOverflow { value: expected })?;
            return self.validate_fixed_address(encoded, expected, projection);
        }
        if !is_dynamic_operand(index) || same_address_leaf(&self.object.root, index) {
            return Err(SafePlaceBuildError::UnsupportedClaimOperand {
                role: "dynamic projection index",
            });
        }
        // For a dynamic index the wasm address cannot be `root + immediate`, so
        // asking whether `encoded.base` IS the root leaf is the wrong question.
        // Decompose it instead and reconcile every term against the descriptor.
        let Some(address) = AffineAddress::of(&encoded.base) else {
            // Outside the affine grammar: the encoded base does not denote the
            // claimed projection, which is the same failure the leaf-root case
            // reports.
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        };
        // Resolve which unscaled leaf is the root. With an explicit `* stride`
        // there is one leaf and it must be the root. With stride one the index
        // rides as a second bare leaf, and the claimed index picks it out.
        let (root, implicit_index) = match (address.leaves.as_slice(), address.scaled.as_slice()) {
            ([only], [_]) => (*only, None),
            ([first, second], []) => {
                if same_address_leaf(first, index) {
                    (*second, Some(*first))
                } else if same_address_leaf(second, index) {
                    (*first, Some(*second))
                } else {
                    return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
                }
            }
            _ => return Err(SafePlaceBuildError::EncodedBaseMismatch { projection }),
        };
        if !same_address_leaf(root, &self.object.root) {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        }
        // A bare index term denotes a stride of exactly one; anything else must
        // carry its scale explicitly.
        let Some((claimed_index, claimed_stride)) = address
            .scaled
            .first()
            .copied()
            .or_else(|| implicit_index.map(|leaf| (leaf, 1)))
        else {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        };
        // The scaled operand must be the very index being claimed, not merely
        // some index-shaped expression.
        if !same_address_leaf(claimed_index, index) {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        }
        // Stride and base come from the descriptor. A caller cannot supply
        // either, so an address that walks by the wrong step is refused.
        if claimed_stride != u64::from(stride) {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        }
        let claimed_base = address
            .constant
            .checked_add(u64::from(encoded.offset))
            .ok_or(SafePlaceBuildError::ArithmeticOverflow)?;
        if claimed_base != u64::from(base) {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        }
        Ok(())
    }

    /// Build the place, refusing a constant range that leaves the object's
    /// constant extent.
    ///
    /// `validate_range` proves `start + length <= extent` in the final
    /// validator, but that proof does not run during the RFC-0162 migration
    /// window: the bridge erases without validating. Settling the constant
    /// cases here means the builder never hands back a place that only a later
    /// validator would have caught. A symbolic extent or range is left to the
    /// validator, which is the only thing that can discharge it.
    fn finish(
        &self,
        projection: SafeProjectionExpr,
        start: SafeSizeExpr,
        length: SafeSizeExpr,
        encoded: SafeEncodedAddress,
    ) -> Result<Box<SafePlaceExpr>, SafePlaceBuildError> {
        if let (
            SafeExtentExpr::Exact(SafeSizeExpr::Constant(extent)),
            SafeSizeExpr::Constant(claimed_start),
            SafeSizeExpr::Constant(claimed_length),
        ) = (&self.object.extent, &start, &length)
        {
            let end = claimed_start
                .checked_add(*claimed_length)
                .ok_or(SafePlaceBuildError::ArithmeticOverflow)?;
            if end > *extent {
                return Err(SafePlaceBuildError::RangeOutsideExtent {
                    end,
                    extent: *extent,
                });
            }
        }
        Ok(Box::new(SafePlaceExpr {
            object: self.object.clone(),
            projection,
            range: SafeRangeExpr {
                object_root: self.object.root.clone(),
                start,
                length,
            },
            access: self.access,
            encoded_base: Box::new(encoded.base),
            encoded_offset: encoded.offset,
        }))
    }
}

/// The byte start of section slot `index`, as a symbolic size.
fn section_start(
    count: &WirExpr,
    index: &WirExpr,
    spec: SectionSpec,
) -> Result<SafeSizeExpr, SafePlaceBuildError> {
    let mut start = SafeSizeExpr::Constant(u64::from(spec.constant_base));
    if spec.count_multiple != 0 {
        start = SafeSizeExpr::Add(
            Box::new(start),
            Box::new(SafeSizeExpr::Multiply(
                Box::new(SafeSizeExpr::Value(Box::new(count.clone()))),
                Box::new(SafeSizeExpr::Constant(u64::from(spec.count_multiple))),
            )),
        );
    }
    start = SafeSizeExpr::Add(
        Box::new(start),
        Box::new(SafeSizeExpr::Multiply(
            Box::new(SafeSizeExpr::Value(Box::new(index.clone()))),
            Box::new(SafeSizeExpr::Constant(u64::from(spec.element_width))),
        )),
    );
    Ok(checked_narrow(start))
}

fn indexed_start(
    index: &WirExpr,
    base: u32,
    stride: u32,
) -> Result<SafeSizeExpr, SafePlaceBuildError> {
    if let Some(index) = constant_u32(index)? {
        let start = u64::from(base)
            .checked_add(
                u64::from(index)
                    .checked_mul(u64::from(stride))
                    .ok_or(SafePlaceBuildError::ArithmeticOverflow)?,
            )
            .ok_or(SafePlaceBuildError::ArithmeticOverflow)?;
        let start = u32::try_from(start)
            .map_err(|_| SafePlaceBuildError::NarrowingOverflow { value: start })?;
        return Ok(SafeSizeExpr::Constant(u64::from(start)));
    }
    Ok(checked_narrow(SafeSizeExpr::Add(
        Box::new(SafeSizeExpr::Constant(u64::from(base))),
        Box::new(SafeSizeExpr::Multiply(
            Box::new(SafeSizeExpr::Value(Box::new(index.clone()))),
            Box::new(SafeSizeExpr::Constant(u64::from(stride))),
        )),
    )))
}

fn scaled_length(
    length: &WirExpr,
    stride: u32,
) -> Result<SafeSizeExpr, SafePlaceBuildError> {
    if let Some(length) = constant_u32(length)? {
        let bytes = u64::from(length)
            .checked_mul(u64::from(stride))
            .ok_or(SafePlaceBuildError::ArithmeticOverflow)?;
        let bytes = u32::try_from(bytes)
            .map_err(|_| SafePlaceBuildError::NarrowingOverflow { value: bytes })?;
        return Ok(SafeSizeExpr::Constant(u64::from(bytes)));
    }
    Ok(checked_narrow(SafeSizeExpr::Multiply(
        Box::new(SafeSizeExpr::Value(Box::new(length.clone()))),
        Box::new(SafeSizeExpr::Constant(u64::from(stride))),
    )))
}

fn checked_narrow(value: SafeSizeExpr) -> SafeSizeExpr {
    SafeSizeExpr::NarrowU32(Box::new(value))
}

fn provenance_for_ordinary_root(
    object_root: &WirExpr,
) -> Result<SafeProvenanceExpr, SafePlaceBuildError> {
    match object_root {
        WirExpr::GetLocal(_) | WirExpr::StrPtr(_) => Ok(SafeProvenanceExpr::Unknown),
        WirExpr::GetGlobal(_) => Err(SafePlaceBuildError::GlobalObjectRoot),
        _ => Err(SafePlaceBuildError::UnsupportedObjectRoot),
    }
}

fn validate_claim_operand(
    expression: &WirExpr,
    role: &'static str,
) -> Result<(), SafePlaceBuildError> {
    match expression {
        WirExpr::GetLocal(_) => Ok(()),
        WirExpr::ConstI32(value) if *value >= 0 => Ok(()),
        WirExpr::ConstI64(value) if *value < 0 => Err(SafePlaceBuildError::NegativeClaimValue {
            role,
            value: *value,
        }),
        WirExpr::ConstI64(value) => {
            let value = u64::try_from(*value).map_err(|_| {
                SafePlaceBuildError::NegativeClaimValue {
                    role,
                    value: *value,
                }
            })?;
            u32::try_from(value)
                .map(|_| ())
                .map_err(|_| SafePlaceBuildError::NarrowingOverflow { value })
        }
        WirExpr::ConstI32(value) => Err(SafePlaceBuildError::NegativeClaimValue {
            role,
            value: i64::from(*value),
        }),
        _ => Err(SafePlaceBuildError::UnsupportedClaimOperand { role }),
    }
}

fn constant_u32(expression: &WirExpr) -> Result<Option<u32>, SafePlaceBuildError> {
    match expression {
        WirExpr::ConstI32(value) if *value >= 0 => Ok(Some(*value as u32)),
        WirExpr::ConstI32(value) => Err(SafePlaceBuildError::NegativeClaimValue {
            role: "projection operand",
            value: i64::from(*value),
        }),
        WirExpr::ConstI64(value) if *value < 0 => Err(SafePlaceBuildError::NegativeClaimValue {
            role: "projection operand",
            value: *value,
        }),
        WirExpr::ConstI64(value) => {
            let value = u64::try_from(*value).map_err(|_| {
                SafePlaceBuildError::NegativeClaimValue {
                    role: "projection operand",
                    value: *value,
                }
            })?;
            Ok(Some(u32::try_from(value).map_err(|_| {
                SafePlaceBuildError::NarrowingOverflow { value }
            })?))
        }
        _ => Ok(None),
    }
}

fn is_dynamic_operand(expression: &WirExpr) -> bool {
    matches!(expression, WirExpr::GetLocal(_))
}

fn same_address_leaf(left: &WirExpr, right: &WirExpr) -> bool {
    match (left, right) {
        (WirExpr::GetLocal(left), WirExpr::GetLocal(right)) => left == right,
        (WirExpr::StrPtr(left), WirExpr::StrPtr(right)) => left == right,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafePlaceBuildError {
    UnknownLayout(LayoutId),
    NonCanonicalLayout { claimed: LayoutId, canonical: LayoutId },
    ExposedRoot { layout: LayoutId, allocation_offset: u32 },
    UnsupportedObjectRoot,
    GlobalObjectRoot,
    MissingCheckedLength(LayoutId),
    /// A fixed-size descriptor was handed a counted extent.
    UnexpectedCheckedLength(LayoutId),
    /// The count operand reads through the object whose extent it defines.
    CountAliasesObjectRoot,
    /// The descriptor's dynamic base/stride disagree with its own element.
    DynamicDescriptorMismatch(LayoutId),
    /// A dynamically sized layout this builder has no extent grammar for.
    UnsupportedDynamicLayout(LayoutId),
    FixedExtentMismatch { expected: u32, actual: u32 },
    HeaderNotDefined(LayoutId),
    UnknownHeaderComponent(u32),
    FieldOutOfBounds { index: u32 },
    DynamicInlineField(LayoutId),
    PointerSlotProvenance {
        field: u32,
        pointee: LayoutId,
        slot: crate::layout::PointerSlot,
    },
    RuntimeBufferProjection(LayoutId),
    /// A section projection was attempted on a layout that is not section-major.
    NotSectionMajor(LayoutId),
    /// A section ordinal the descriptor's section table does not define.
    SectionOutOfBounds { section: u32 },
    /// A section's base is a multiple of the element count, so the count has to
    /// come from a counted extent; a header-only or fixed claim cannot supply it.
    SectionRequiresCountedExtent(LayoutId),
    PackedListFieldProjection(LayoutId),
    ProjectionRequiresPackedList { projection: &'static str },
    /// A runtime-buffer-only projection was attempted on another layout.
    ProjectionRequiresRuntimeBuffer { projection: &'static str },
    /// A projection's byte range leaves the object's claimed extent.
    RangeOutsideExtent { end: u64, extent: u64 },
    UnsupportedClaimOperand { role: &'static str },
    NegativeClaimValue { role: &'static str, value: i64 },
    EncodedBaseMismatch { projection: &'static str },
    EncodedOffsetMismatch {
        projection: &'static str,
        expected: u32,
        actual: u32,
    },
    DynamicEncodedAddressUnsupported { projection: &'static str },
    ArithmeticOverflow,
    NarrowingOverflow { value: u64 },
}

impl fmt::Display for SafePlaceBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownLayout(layout) => write!(formatter, "unknown canonical LayoutId {layout}"),
            Self::UnexpectedCheckedLength(layout) => write!(
                formatter,
                "layout {layout} has a fixed descriptor size and must not be claimed with a counted extent"
            ),
            Self::CountAliasesObjectRoot => formatter.write_str(
                "a counted extent must not read its count through the object it measures"
            ),
            Self::DynamicDescriptorMismatch(layout) => write!(
                formatter,
                "layout {layout} has a dynamic size that disagrees with its own element stride"
            ),
            Self::UnsupportedDynamicLayout(layout) => write!(
                formatter,
                "layout {layout} is dynamically sized but has no checked counted-extent grammar"
            ),
            Self::NonCanonicalLayout { claimed, canonical } => write!(
                formatter,
                "LayoutId {claimed} does not match canonical descriptor identity {canonical}"
            ),
            Self::ExposedRoot { layout, allocation_offset } => write!(
                formatter,
                "layout {layout} exposes a runtime root at allocation offset {allocation_offset}; an authenticated allocation-relative root is required"
            ),
            Self::UnsupportedObjectRoot => {
                formatter.write_str("object root must be one exact local or static-data address source")
            }
            Self::GlobalObjectRoot => {
                formatter.write_str("module globals cannot authenticate an object root")
            }
            Self::MissingCheckedLength(layout) => write!(
                formatter,
                "dynamic layout {layout} is unsupported until final-body count definitions are bound"
            ),
            Self::FixedExtentMismatch { expected, actual } => write!(
                formatter,
                "fixed object extent {actual} does not match descriptor extent {expected}"
            ),
            Self::HeaderNotDefined(layout) => {
                write!(formatter, "layout {layout} defines no semantic header")
            }
            Self::UnknownHeaderComponent(component) => {
                write!(formatter, "unknown descriptor header component {component}")
            }
            Self::FieldOutOfBounds { index } => {
                write!(formatter, "field index {index} is outside the descriptor")
            }
            Self::DynamicInlineField(layout) => write!(
                formatter,
                "inline field layout {layout} is dynamic and has no fixed field extent"
            ),
            Self::PointerSlotProvenance { field, pointee, slot } => write!(
                formatter,
                "field {field} is a {slot:?} pointer slot to {pointee}; descriptor position cannot authenticate loaded-pointer provenance"
            ),
            Self::NotSectionMajor(layout) => write!(
                formatter,
                "layout {layout} is not section-major, so it defines no sections to project"
            ),
            Self::SectionOutOfBounds { section } => write!(
                formatter,
                "section {section} is not defined by the descriptor's section table"
            ),
            Self::SectionRequiresCountedExtent(layout) => write!(
                formatter,
                "a section of {layout} is based on its element count, which requires a counted extent"
            ),
            Self::RuntimeBufferProjection(layout) => write!(
                formatter,
                "runtime buffer {layout} requires a dedicated checked buffer projection"
            ),
            Self::PackedListFieldProjection(layout) => write!(
                formatter,
                "packed-list layout {layout} requires header or element projection, not a raw field ordinal"
            ),
            Self::RangeOutsideExtent { end, extent } => write!(
                formatter,
                "projection reaches byte {end} of an object whose claimed extent is {extent}"
            ),
            Self::ProjectionRequiresRuntimeBuffer { projection } => {
                write!(formatter, "{projection} requires a runtime buffer descriptor")
            }
            Self::ProjectionRequiresPackedList { projection } => {
                write!(formatter, "{projection} projection requires a packed-list descriptor")
            }
            Self::UnsupportedClaimOperand { role } => write!(
                formatter,
                "{role} must be one exact scalar definition or nonnegative constant"
            ),
            Self::NegativeClaimValue { role, value } => {
                write!(formatter, "{role} is negative ({value})")
            }
            Self::EncodedBaseMismatch { projection } => write!(
                formatter,
                "encoded base does not exactly denote the descriptor-derived {projection} address"
            ),
            Self::EncodedOffsetMismatch { projection, expected, actual } => write!(
                formatter,
                "encoded {projection} offset {actual} does not match descriptor-derived offset {expected}"
            ),
            Self::DynamicEncodedAddressUnsupported { projection } => write!(
                formatter,
                "dynamic {projection} address is outside the adapter's leaf-root plus immediate-offset subset"
            ),
            Self::ArithmeticOverflow => {
                formatter.write_str("descriptor-derived address arithmetic overflowed")
            }
            Self::NarrowingOverflow { value } => write!(
                formatter,
                "mathematical address value {value} cannot be narrowed exactly to u32"
            ),
        }
    }
}

impl std::error::Error for SafePlaceBuildError {}

#[cfg(test)]
mod tests {
    use super::*;

    // A header-only extent exists so the length word can be READ -- the
    // operation that establishes the count, which makes a counted extent
    // circular there. It must authorise strictly less than a counted extent.
    #[test]
    fn a_header_only_extent_reaches_the_length_word_but_never_the_payload() {
        let mut interner = LayoutInterner::default();
        let byte = interner.intern_scalar(ScalarKind::RawByte).expect("scalar");
        let buffer = interner.intern_runtime_buffer(byte).expect("runtime buffer");
        let builder = SafePlaceBuilder::new(
            &interner,
            buffer,
            SafeExactExtent::header_only(),
            WirExpr::GetLocal("b".into()),
            AccessMode::Read,
        )
        .expect("the header region is claimable without knowing the count");

        assert!(
            builder
                .runtime_buffer_length(SafeEncodedAddress::new(WirExpr::GetLocal("b".into()), 0))
                .is_ok(),
            "the length word lies inside the header region",
        );

        // The payload does not. This is what `finish`'s range check enforces:
        // without it the builder handed back a place reaching byte 5 of a
        // 4-byte claim, which only the validator would have caught -- and the
        // migration bridge does not run the validator.
        let payload = builder.element(
            WirExpr::ConstI32(0),
            SafeEncodedAddress::new(WirExpr::GetLocal("b".into()), 4),
        );
        assert!(
            matches!(payload, Err(SafePlaceBuildError::RangeOutsideExtent { end: 5, extent: 4 })),
            "a header-only claim must not reach past the header: {payload:?}",
        );
    }

    #[test]
    fn a_fixed_layout_refuses_a_header_only_extent() {
        let mut interner = LayoutInterner::default();
        let int = interner.intern_scalar(ScalarKind::Int).expect("scalar");
        let refused = SafePlaceBuilder::new(
            &interner,
            int,
            SafeExactExtent::header_only(),
            WirExpr::GetLocal("b".into()),
            AccessMode::Read,
        );
        assert!(
            matches!(refused, Err(SafePlaceBuildError::UnexpectedCheckedLength(id)) if id == int),
            "a layout that knows its own size has no separate header region",
        );
    }

    // RFC-0162 S2: a dynamic index means the wasm address is
    // `root + base + index*stride`, so the old check -- "is encoded.base the
    // root leaf?" -- could never succeed and every dynamic-index site was
    // unreachable. The address is now decomposed and reconciled term by term
    // against the descriptor.
    fn dynamic_buffer(interner: &mut LayoutInterner) -> LayoutId {
        let byte = interner.intern_scalar(ScalarKind::RawByte).expect("scalar");
        interner.intern_runtime_buffer(byte).expect("runtime buffer")
    }

    fn scaled_address(base: u32, index: &str, stride: i32) -> WirExpr {
        WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I32,
            lhs: Box::new(WirExpr::GetLocal("res".into())),
            rhs: Box::new(WirExpr::Binary {
                op: BinOp::Add,
                kind: Kind::I32,
                lhs: Box::new(WirExpr::ConstI32(base as i32)),
                rhs: Box::new(WirExpr::Binary {
                    op: BinOp::Mul,
                    kind: Kind::I32,
                    lhs: Box::new(WirExpr::GetLocal(index.into())),
                    rhs: Box::new(WirExpr::ConstI32(stride)),
                }),
            }),
        }
    }

    // Stride-one payload access is spelled `buf + i`, with no `* 1`. That is
    // the single most common byte-access shape in the helper library, so the
    // decomposition must resolve which bare leaf is the root and which is the
    // index rather than refusing two unscaled terms.
    #[test]
    fn a_stride_one_index_needs_no_explicit_scale() {
        let mut interner = LayoutInterner::default();
        let buffer = dynamic_buffer(&mut interner);
        let builder = SafePlaceBuilder::new(
            &interner,
            buffer,
            SafeExactExtent::counted(WirExpr::GetLocal("n".into())).expect("counted"),
            WirExpr::GetLocal("s".into()),
            AccessMode::Read,
        )
        .expect("claimable");

        // `s + i` with immediate 4 -- exactly how the helpers write it.
        let bare = WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I32,
            lhs: Box::new(WirExpr::GetLocal("s".into())),
            rhs: Box::new(WirExpr::GetLocal("i".into())),
        };
        assert!(
            builder
                .element(
                    WirExpr::GetLocal("i".into()),
                    SafeEncodedAddress::new(bare, 4),
                )
                .is_ok(),
            "a bare index term denotes stride one",
        );

        // But an unrelated second leaf is still refused: the claimed index is
        // what disambiguates, not mere arity.
        let unrelated = WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I32,
            lhs: Box::new(WirExpr::GetLocal("s".into())),
            rhs: Box::new(WirExpr::GetLocal("k".into())),
        };
        assert!(
            builder
                .element(
                    WirExpr::GetLocal("i".into()),
                    SafeEncodedAddress::new(unrelated, 4),
                )
                .is_err(),
            "neither leaf matching the claimed index must not resolve",
        );
    }

    #[test]
    fn a_dynamic_index_address_is_reconciled_against_the_descriptor() {
        let mut interner = LayoutInterner::default();
        let buffer = dynamic_buffer(&mut interner);
        let builder = SafePlaceBuilder::new(
            &interner,
            buffer,
            SafeExactExtent::counted(WirExpr::GetLocal("n".into())).expect("counted"),
            WirExpr::GetLocal("res".into()),
            AccessMode::Read,
        )
        .expect("claimable");

        // base 4, stride 1 both come from the RuntimeBuffer descriptor.
        let place = builder
            .element(
                WirExpr::GetLocal("i".into()),
                SafeEncodedAddress::new(scaled_address(4, "i", 1), 0),
            )
            .expect("a dynamic index is claimable once the address reconciles");
        assert_eq!(place.encoded_offset, 0);
    }

    #[test]
    fn a_dynamic_address_that_walks_by_the_wrong_stride_is_refused() {
        let mut interner = LayoutInterner::default();
        let buffer = dynamic_buffer(&mut interner);
        let builder = SafePlaceBuilder::new(
            &interner,
            buffer,
            SafeExactExtent::counted(WirExpr::GetLocal("n".into())).expect("counted"),
            WirExpr::GetLocal("res".into()),
            AccessMode::Read,
        )
        .expect("claimable");
        // stride 8 where the descriptor says 1: this address does not land on
        // an element boundary of THIS layout.
        let refused = builder.element(
            WirExpr::GetLocal("i".into()),
            SafeEncodedAddress::new(scaled_address(4, "i", 8), 0),
        );
        assert!(refused.is_err(), "the descriptor's stride is not negotiable");

        // And an address that scales a DIFFERENT operand than the claimed index.
        let wrong_operand = builder.element(
            WirExpr::GetLocal("i".into()),
            SafeEncodedAddress::new(scaled_address(4, "j", 1), 0),
        );
        assert!(
            wrong_operand.is_err(),
            "the scaled operand must be the very index being claimed",
        );
    }

    #[test]
    fn address_decomposition_is_commutative_and_folds_split_constants() {
        // `(2 + res) + (i * 1) + 2` is the same address as `res + 4 + i*1`.
        let commuted = WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I32,
            lhs: Box::new(WirExpr::Binary {
                op: BinOp::Add,
                kind: Kind::I32,
                lhs: Box::new(WirExpr::ConstI32(2)),
                rhs: Box::new(WirExpr::GetLocal("res".into())),
            }),
            rhs: Box::new(WirExpr::Binary {
                op: BinOp::Add,
                kind: Kind::I32,
                lhs: Box::new(WirExpr::Binary {
                    op: BinOp::Mul,
                    kind: Kind::I32,
                    lhs: Box::new(WirExpr::ConstI32(1)),
                    rhs: Box::new(WirExpr::GetLocal("i".into())),
                }),
                rhs: Box::new(WirExpr::ConstI32(2)),
            }),
        };
        let address = AffineAddress::of(&commuted).expect("decomposes");
        assert_eq!(address.constant, 4, "split constants fold");
        assert!(
            matches!(address.leaves.as_slice(), [WirExpr::GetLocal(name)] if name == "res"),
            "the one unscaled leaf is the root when the index carries its scale",
        );
        let [(operand, stride)] = address.scaled.as_slice() else {
            panic!("exactly one scaled term");
        };
        let (operand, stride) = (*operand, *stride);
        assert_eq!(stride, 1, "the scale is read from either operand order");
        assert!(matches!(operand, WirExpr::GetLocal(name) if name == "i"));
    }

    /// A section-major slot is claimable, and every constant is the descriptor's.
    ///
    /// The Swiss carrier is the shape: `[u32 slots][ctrl: slots + 16]
    /// [index: slots * 4][keys: slots * 8][values: slots * 8][order: slots * 4]`.
    /// A key slot therefore sits at `20 + slots * 5 + h * 8`, which is TWO runtime
    /// products -- the section's own base scales with the element count, and the
    /// slot scales with its width. That is why a section step exists at all: no
    /// `Element`/`Field` chain can name it.
    #[test]
    fn a_section_major_slot_reconciles_and_a_wrong_section_does_not() {
        let mut interner = LayoutInterner::new();
        let slot = interner.intern_scalar(crate::layout::ScalarKind::Int).unwrap();
        let carrier = interner.intern_runtime_dictionary_index(slot, slot).unwrap();
        let root = WirExpr::GetLocal("idx".into());
        let slots = WirExpr::GetLocal("slots".into());
        let h = WirExpr::GetLocal("h".into());
        let add = |lhs: WirExpr, rhs: WirExpr| WirExpr::Binary {
            op: BinOp::Add,
            kind: crate::wir::Kind::I32,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
        let mul = |lhs: WirExpr, scale: i32| WirExpr::Binary {
            op: BinOp::Mul,
            kind: crate::wir::Kind::I32,
            lhs: Box::new(lhs),
            rhs: Box::new(WirExpr::ConstI32(scale)),
        };
        let builder = || {
            SafePlaceBuilder::new(
                &interner,
                carrier,
                SafeExactExtent::counted(slots.clone()).unwrap(),
                root.clone(),
                AccessMode::Write,
            )
            .expect("the carrier is claimable")
        };
        // The key section, spelled exactly as `dict_index_put` spells it.
        let key_address = add(
            add(root.clone(), WirExpr::ConstI32(20)),
            add(mul(slots.clone(), 5), mul(h.clone(), 8)),
        );
        builder()
            .section_element(
                crate::layout::CARRIER_KEY,
                h.clone(),
                SafeEncodedAddress::new(key_address.clone(), 0),
            )
            .expect("a key slot address reconciles against the section table");
        // The same address claimed as the VALUE section must be refused: the value
        // section's base is `slots * 13`, not `slots * 5`.
        let wrong = builder().section_element(
            crate::layout::CARRIER_VALUE,
            h.clone(),
            SafeEncodedAddress::new(key_address, 0),
        );
        assert!(
            matches!(wrong, Err(SafePlaceBuildError::EncodedBaseMismatch { .. })),
            "a key address claimed as a value slot must be refused, got {wrong:?}",
        );
        // A control byte rides as a bare leaf, because its width is one.
        builder()
            .section_element(
                crate::layout::CARRIER_CONTROL,
                h.clone(),
                SafeEncodedAddress::new(add(add(root.clone(), WirExpr::ConstI32(4)), h.clone()), 0),
            )
            .expect("a stride-one section slot needs no explicit multiply");
        // An ordinal the table does not define is a refusal, not a silent zero.
        assert!(matches!(
            builder().section_element(99, h, SafeEncodedAddress::new(root, 0)),
            Err(SafePlaceBuildError::SectionOutOfBounds { section: 99 }),
        ));
    }

    // RFC-0162 S1: a runtime buffer keeps its length in an ordinary descriptor
    // field, not a HeaderLayout, so `header()` does not apply and
    // `fixed_field()` refuses it. This is the sanctioned accessor, and it emits
    // the existing Field{0} step rather than a new projection kind.
    #[test]
    fn a_runtime_buffer_length_word_is_claimable_and_descriptor_derived() {
        let mut interner = LayoutInterner::default();
        let byte = interner.intern_scalar(ScalarKind::RawByte).expect("scalar");
        let buffer = interner.intern_runtime_buffer(byte).expect("runtime buffer");
        let builder = SafePlaceBuilder::new(
            &interner,
            buffer,
            SafeExactExtent::counted(WirExpr::GetLocal("n".into())).expect("counted"),
            WirExpr::GetLocal("res".into()),
            AccessMode::Write,
        )
        .expect("claimable");

        let place = builder
            .runtime_buffer_length(SafeEncodedAddress::new(WirExpr::GetLocal("res".into()), 0))
            .expect("the length word is claimable");
        assert_eq!(place.encoded_offset, 0, "offset comes from the field table");
        assert!(
            matches!(place.projection.steps(), [SafeProjectionStep::Field { index: 0 }]),
            "emits the existing Field step: {:?}",
            place.projection.steps(),
        );

        // The dedicated accessor is the only route: the generic one still refuses.
        let refused = builder
            .fixed_field(0, SafeEncodedAddress::new(WirExpr::GetLocal("res".into()), 0));
        assert!(
            matches!(refused, Err(SafePlaceBuildError::RuntimeBufferProjection(_))),
            "fixed_field must keep refusing a runtime buffer",
        );

        // And a claimed address that disagrees with the descriptor is refused.
        let wrong = builder
            .runtime_buffer_length(SafeEncodedAddress::new(WirExpr::GetLocal("res".into()), 4));
        assert!(wrong.is_err(), "an offset the descriptor does not justify is refused");
    }

    /// A constant projection may carry the descriptor offset in the immediate,
    /// in the base, or split -- only the sum is the address.
    ///
    /// The base spelling is not optional sugar. `erase_node` refuses a
    /// bulk-copy place with a nonzero immediate, because raw `memory.copy` has
    /// no immediate operand, so `root + 4` with immediate zero is the ONLY way
    /// to express a constant-start copy of a `[u32 length][payload]` payload.
    #[test]
    fn a_constant_projection_accepts_the_descriptor_offset_in_base_or_immediate() {
        let mut interner = LayoutInterner::default();
        let byte = interner.intern_scalar(ScalarKind::RawByte).expect("scalar");
        let buffer = interner.intern_runtime_buffer(byte).expect("runtime buffer");
        let root = || WirExpr::GetLocal("res".into());
        let builder = SafePlaceBuilder::new(
            &interner,
            buffer,
            SafeExactExtent::counted(WirExpr::GetLocal("n".into())).expect("counted"),
            root(),
            AccessMode::Read,
        )
        .expect("claimable");
        let payload_base = || {
            WirExpr::Binary {
                op: BinOp::Add,
                kind: Kind::I32,
                lhs: Box::new(root()),
                rhs: Box::new(WirExpr::ConstI32(4)),
            }
        };

        // Payload element zero sits at descriptor offset 4. Both spellings of
        // that one address are accepted, and they agree on the derived range.
        let immediate = builder
            .element(WirExpr::ConstI32(0), SafeEncodedAddress::new(root(), 4))
            .expect("offset in the immediate is claimable");
        let in_base = builder
            .element(WirExpr::ConstI32(0), SafeEncodedAddress::new(payload_base(), 0))
            .expect("offset folded into the base is claimable");
        assert_eq!(immediate.encoded_offset, 4);
        assert_eq!(
            in_base.encoded_offset, 0,
            "a bulk-copy place must be able to carry a zero immediate",
        );
        assert_eq!(
            format!("{:?}", immediate.range.start),
            format!("{:?}", in_base.range.start),
            "the same address must derive the same byte range either way",
        );

        // The sum is what is checked: splitting it across both still resolves.
        let split = builder.element(
            WirExpr::ConstI32(1),
            SafeEncodedAddress::new(payload_base(), 1),
        );
        assert!(split.is_ok(), "base 4 + immediate 1 denotes element one: {split:?}");

        // A sum the descriptor does not justify is still refused, and the report
        // names the total rather than whichever half the caller happened to use.
        let wrong = builder
            .element(WirExpr::ConstI32(0), SafeEncodedAddress::new(payload_base(), 4));
        assert!(
            matches!(
                wrong,
                Err(SafePlaceBuildError::EncodedOffsetMismatch { expected: 4, actual: 8, .. })
            ),
            "a wrong total must be refused with the total: {wrong:?}",
        );

        // A second bare leaf in a CONSTANT projection is an index the claim did
        // not make, so the base no longer denotes the claimed projection.
        let smuggled_index = builder.element(
            WirExpr::ConstI32(0),
            SafeEncodedAddress::new(
                WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I32,
                    lhs: Box::new(payload_base()),
                    rhs: Box::new(WirExpr::GetLocal("i".into())),
                },
                0,
            ),
        );
        assert!(
            matches!(smuggled_index, Err(SafePlaceBuildError::EncodedBaseMismatch { .. })),
            "an unclaimed index term must be refused: {smuggled_index:?}",
        );

        // And the root itself is still checked, not merely the arithmetic.
        let wrong_root = builder.element(
            WirExpr::ConstI32(0),
            SafeEncodedAddress::new(
                WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I32,
                    lhs: Box::new(WirExpr::GetLocal("other".into())),
                    rhs: Box::new(WirExpr::ConstI32(4)),
                },
                0,
            ),
        );
        assert!(
            matches!(wrong_root, Err(SafePlaceBuildError::EncodedBaseMismatch { .. })),
            "a different root must be refused: {wrong_root:?}",
        );
    }

    /// A dictionary row's key and value are nameable, every offset is derived,
    /// and a POINTER-bearing dictionary is still refused.
    ///
    /// The second half is the point. `validate_runtime_dictionary_slot` accepts an
    /// owned pointer slot as a key or value, and the body holds those as `Inline`
    /// fields -- so a top-level-only pointer check would grant a string-keyed
    /// dictionary scalar-buffer authority over owned pointers. This is the one
    /// allowlisted kind where that is reachable.
    #[test]
    fn dictionary_rows_are_claimable_and_pointer_keys_are_refused() {
        let mut layouts = LayoutInterner::new();
        let int = layouts.intern_scalar(ScalarKind::Int).expect("scalar");
        let body = layouts
            .intern_runtime_dictionary_body(int, int)
            .expect("an Int-keyed body interns");

        let descriptor = layouts.get(body).expect("interned");
        assert_eq!(descriptor.size(), LayoutSize::Dynamic { base: 4, stride: 16 });
        let row = descriptor
            .kind()
            .packed_element_layout()
            .expect("a dense body indexes by row");
        let row_descriptor = layouts.get(row).expect("the row interns with the body");
        assert_eq!(row_descriptor.size(), LayoutSize::Fixed(16));
        assert_eq!(row_descriptor.alignment(), 4, "a row address is 4-aligned");
        assert_eq!(row_descriptor.fields()[0].offset(), 0, "key at row+0");
        assert_eq!(row_descriptor.fields()[1].offset(), 8, "value at row+8");

        let root = || WirExpr::GetLocal("d".into());
        let builder = SafePlaceBuilder::new(
            &layouts,
            body,
            SafeExactExtent::counted(WirExpr::GetLocal("count".into())).expect("counted"),
            root(),
            AccessMode::Read,
        )
        .expect("a pointer-free dictionary body is claimable");

        let index = WirExpr::GetLocal("i".into());
        let scaled = WirExpr::Binary {
            op: BinOp::Mul,
            kind: Kind::I32,
            lhs: Box::new(index.clone()),
            rhs: Box::new(WirExpr::ConstI32(16)),
        };
        let address = || WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I32,
            lhs: Box::new(root()),
            rhs: Box::new(scaled.clone()),
        };

        // Row i's key sits at the body's base of four; its value eight beyond.
        let key = builder
            .element_field(index.clone(), 0, SafeEncodedAddress::new(address(), 4))
            .expect("row key is claimable");
        assert!(
            matches!(
                key.projection.steps(),
                [SafeProjectionStep::Element { .. }, SafeProjectionStep::Field { index: 0 }]
            ),
            "emits the two-step chain: {:?}",
            key.projection.steps(),
        );
        assert!(matches!(key.range.length, SafeSizeExpr::Constant(8)), "a slot is eight bytes");
        assert!(builder
            .element_field(index.clone(), 1, SafeEncodedAddress::new(address(), 12))
            .is_ok());

        // Immediates the descriptors do not justify, and ordinals off the row.
        assert!(builder
            .element_field(index.clone(), 1, SafeEncodedAddress::new(address(), 8))
            .is_err());
        assert!(matches!(
            builder.element_field(index, 2, SafeEncodedAddress::new(address(), 20)),
            Err(SafePlaceBuildError::FieldOutOfBounds { index: 2 }),
        ));

        // A string key is an OWNED POINTER slot, inlined into the row. It must be
        // refused a counted extent outright.
        let raw = layouts.intern_scalar(ScalarKind::RawByte).expect("scalar");
        let bytes = layouts.intern_runtime_buffer(raw).expect("byte buffer");
        let owned = layouts
            .intern_runtime_pointer_value_slot(bytes, PointerSlot::Owned)
            .expect("owned pointer slot");
        let pointer_body = layouts
            .intern_runtime_dictionary_body(owned, int)
            .expect("a string-keyed body interns");
        assert!(
            !layouts
                .get(pointer_body)
                .expect("interned")
                .fields()
                .iter()
                .any(|field| matches!(field.kind(), FieldKind::Pointer { .. })),
            "the body's own fields must be pointer-free, or this proves nothing",
        );
        let refused = SafePlaceBuilder::new(
            &layouts,
            pointer_body,
            SafeExactExtent::counted(WirExpr::GetLocal("count".into())).expect("counted"),
            root(),
            AccessMode::Read,
        )
        .err();
        assert!(
            matches!(&refused, Some(SafePlaceBuildError::UnsupportedDynamicLayout(id)) if *id == pointer_body),
            "a pointer-keyed dictionary must be refused, got {refused:?}",
        );
    }

    #[test]
    fn a_packed_only_layout_refuses_the_runtime_buffer_length_accessor() {
        let mut interner = LayoutInterner::default();
        let int = interner.intern_scalar(ScalarKind::Int).expect("scalar");
        let builder = SafePlaceBuilder::new(
            &interner,
            int,
            SafeExactExtent::fixed(8),
            WirExpr::GetLocal("res".into()),
            AccessMode::Read,
        )
        .expect("claimable");
        let refused = builder
            .runtime_buffer_length(SafeEncodedAddress::new(WirExpr::GetLocal("res".into()), 0));
        assert!(
            matches!(
                refused,
                Err(SafePlaceBuildError::ProjectionRequiresRuntimeBuffer { .. })
            ),
            "only a runtime buffer has a length word to claim",
        );
    }

    // RFC-0162 S0: a runtime-sized object could not be DESCRIBED before this.
    // `SafeExactExtent` offered only `fixed(bytes)`, so every dynamically sized
    // buffer -- which is what `wir_helpers` allocates, 399 of the 584 census
    // sites -- was rejected with `MissingCheckedLength` and had no migration
    // target at all.
    #[test]
    fn a_runtime_sized_buffer_can_be_claimed_with_a_counted_extent() {
        let mut interner = LayoutInterner::default();
        let byte = interner.intern_scalar(ScalarKind::RawByte).expect("scalar");
        let buffer = interner.intern_runtime_buffer(byte).expect("runtime buffer");

        // Before S0 this was `MissingCheckedLength`: a dynamic descriptor with
        // a fixed extent still is.
        let refused = SafePlaceBuilder::new(
            &interner,
            buffer,
            SafeExactExtent::fixed(8),
            WirExpr::GetLocal("res".into()),
            AccessMode::Write,
        );
        assert!(
            matches!(refused, Err(SafePlaceBuildError::MissingCheckedLength(id)) if id == buffer),
            "a dynamic descriptor still refuses a constant extent",
        );

        // With a counted extent it is accepted, and the extent is the
        // descriptor's own base and stride -- not anything the caller supplied.
        let builder = SafePlaceBuilder::new(
            &interner,
            buffer,
            SafeExactExtent::counted(WirExpr::GetLocal("n".into())).expect("counted extent"),
            WirExpr::GetLocal("res".into()),
            AccessMode::Write,
        )
        .expect("a runtime-sized buffer is claimable with a counted extent");
        match &builder.object.extent {
            SafeExtentExpr::Counted { header, count, stride } => {
                assert!(
                    matches!(header, SafeSizeExpr::Constant(4)),
                    "header is descriptor-derived, not caller-supplied: {header:?}",
                );
                assert!(
                    matches!(stride, SafeSizeExpr::Constant(1)),
                    "stride is descriptor-derived, not caller-supplied: {stride:?}",
                );
                assert!(
                    matches!(&**count, WirExpr::GetLocal(name) if name == "n"),
                    "the count is the operand the caller supplied: {count:?}",
                );
            }
            other => panic!("expected a counted extent, got {other:?}"),
        }
    }

    #[test]
    fn a_counted_extent_may_not_read_through_the_object_it_measures() {
        let mut interner = LayoutInterner::default();
        let byte = interner.intern_scalar(ScalarKind::RawByte).expect("scalar");
        let buffer = interner.intern_runtime_buffer(byte).expect("runtime buffer");
        let refused = SafePlaceBuilder::new(
            &interner,
            buffer,
            SafeExactExtent::counted(WirExpr::GetLocal("res".into())).expect("counted extent"),
            WirExpr::GetLocal("res".into()),
            AccessMode::Write,
        );
        assert!(
            matches!(refused, Err(SafePlaceBuildError::CountAliasesObjectRoot)),
            "the extent must not depend on the storage it bounds",
        );
    }

    #[test]
    fn a_fixed_descriptor_refuses_a_counted_extent() {
        let mut interner = LayoutInterner::default();
        let int = interner.intern_scalar(ScalarKind::Int).expect("scalar");
        let refused = SafePlaceBuilder::new(
            &interner,
            int,
            SafeExactExtent::counted(WirExpr::GetLocal("n".into())).expect("counted extent"),
            WirExpr::GetLocal("res".into()),
            AccessMode::Read,
        );
        assert!(
            matches!(refused, Err(SafePlaceBuildError::UnexpectedCheckedLength(id)) if id == int),
            "a layout that knows its own size must not accept a claimed length",
        );
    }
    use crate::layout::{
        ClosedTypeResolver, PointerSlot, RcHeader, ReferenceKind, ResolvedNamed,
        RuntimePrefixSlot, ScalarKind,
    };
    use crate::wir::{BinOp, Kind};
    use witchy_syntax::ast::Type;

    struct Resolver;

    impl ClosedTypeResolver for Resolver {
        fn resolve_named<'a>(
            &'a self,
            name: &str,
            _arguments: &[Type],
        ) -> Option<ResolvedNamed<'a>> {
            Some(match name {
                "Int" => ResolvedNamed::Scalar(ScalarKind::Int),
                "List" => ResolvedNamed::PackedList { rc: RcHeader::Required },
                "String" => ResolvedNamed::Reference(ReferenceKind::Owning),
                _ => return None,
            })
        }
    }

    fn root() -> WirExpr {
        WirExpr::GetLocal("object".into())
    }

    fn address(offset: u32) -> SafeEncodedAddress {
        SafeEncodedAddress::new(root(), offset)
    }

    fn builder<'a>(
        layouts: &'a LayoutInterner,
        layout: LayoutId,
        extent: SafeExactExtent,
    ) -> Result<SafePlaceBuilder<'a>, SafePlaceBuildError> {
        SafePlaceBuilder::new(
            layouts,
            layout,
            extent,
            root(),
            AccessMode::Read,
        )
    }

    /// Exercise projection/address rejection independently of the production
    /// constructor, which deliberately rejects every dynamic descriptor.
    fn dynamic_projection_fixture<'a>(
        layouts: &'a LayoutInterner,
        layout: LayoutId,
    ) -> SafePlaceBuilder<'a> {
        SafePlaceBuilder {
            layouts,
            object: SafeObjectExpr {
                root: Box::new(root()),
                layout,
                extent: SafeExtentExpr::Unknown,
                provenance: SafeProvenanceExpr::Unknown,
            },
            full_length: SafeSizeExpr::Constant(0),
            access: AccessMode::Read,
        }
    }

    #[test]
    fn fixed_root_and_field_claims_are_descriptor_derived() {
        let mut layouts = LayoutInterner::new();
        let tuple = layouts
            .intern_type(
                &Type::Tuple(vec![
                    Type::Named("Int".into(), Vec::new()),
                    Type::Named("Int".into(), Vec::new()),
                ]),
                &Resolver,
            )
            .unwrap();
        let builder = builder(&layouts, tuple, SafeExactExtent::fixed(16)).unwrap();

        let root_claim = builder.root(address(0)).unwrap();
        assert!(root_claim.projection.steps().is_empty());
        assert!(matches!(&root_claim.object.extent, SafeExtentExpr::Exact(_)));
        assert!(matches!(&root_claim.object.provenance, SafeProvenanceExpr::Unknown));
        assert_eq!(root_claim.access, AccessMode::Read);
        assert!(matches!(&root_claim.range.length, SafeSizeExpr::Constant(16)));

        let second = builder.fixed_field(1, address(8)).unwrap();
        assert!(matches!(
            second.projection.steps(),
            [SafeProjectionStep::Field { index: 1 }]
        ));
        assert!(matches!(second.range.start, SafeSizeExpr::Constant(8)));
        assert!(matches!(second.range.length, SafeSizeExpr::Constant(8)));
        assert!(matches!(second.encoded_base.as_ref(), WirExpr::GetLocal(name) if name == "object"));
        assert_eq!(second.encoded_offset, 8);

        // Folding the descriptor offset into the base denotes the same address,
        // so it is accepted and derives the same range. This assertion used to
        // require rejection; it was changed deliberately, because raw
        // `memory.copy` carries no immediate operand and `erase_node` therefore
        // refuses any bulk-copy place with a nonzero one -- which made a
        // constant-start bulk copy unbuildable in either spelling.
        //
        // What still guards the original concern is the SUM check plus the
        // root and index checks below: an address the descriptor does not
        // justify is refused, and a constant projection may not smuggle in an
        // index term. See
        // `a_constant_projection_accepts_the_descriptor_offset_in_base_or_immediate`.
        let split_base = WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I32,
            lhs: Box::new(root()),
            rhs: Box::new(WirExpr::ConstI32(8)),
        };
        let folded = builder
            .fixed_field(1, SafeEncodedAddress::new(split_base, 0))
            .expect("the folded spelling denotes the same field address");
        assert!(matches!(folded.range.start, SafeSizeExpr::Constant(8)));
        assert!(matches!(folded.range.length, SafeSizeExpr::Constant(8)));
        assert_eq!(folded.encoded_offset, 0);

        // A wrong total is still refused in the folded spelling.
        let wrong_total = WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I32,
            lhs: Box::new(root()),
            rhs: Box::new(WirExpr::ConstI32(12)),
        };
        assert!(matches!(
            builder.fixed_field(1, SafeEncodedAddress::new(wrong_total, 0)),
            Err(SafePlaceBuildError::EncodedOffsetMismatch { expected: 8, actual: 12, .. })
        ));
    }

    #[test]
    fn packed_projection_helpers_keep_exact_encoded_shapes() {
        let mut layouts = LayoutInterner::new();
        let list = layouts
            .intern_type(
                &Type::Named(
                    "List".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                &Resolver,
            )
            .unwrap();
        let builder = dynamic_projection_fixture(&layouts, list);

        assert!(builder.header(0, address(0)).is_ok());
        assert!(builder.header(1, address(4)).is_ok());
        assert!(builder.element(WirExpr::ConstI32(2), address(24)).is_ok());
        assert!(builder
            .slice(WirExpr::ConstI32(1), WirExpr::ConstI32(2), address(16))
            .is_ok());

        let indexed = WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I32,
            lhs: Box::new(root()),
            rhs: Box::new(WirExpr::Binary {
                op: BinOp::Mul,
                kind: Kind::I32,
                lhs: Box::new(WirExpr::GetLocal("index".into())),
                rhs: Box::new(WirExpr::ConstI32(8)),
            }),
        };
        // `root + index*8` with an immediate 8 IS the correct address for
        // element `index` of a stride-8 list. It was refused only because no
        // dynamic address could be proved at all; S2 reconciles it against the
        // descriptor and accepts it.
        assert!(
            builder
                .element(
                    WirExpr::GetLocal("index".into()),
                    SafeEncodedAddress::new(indexed, 8),
                )
                .is_ok(),
            "a correctly-formed dynamic element address is now provable",
        );
        // A bare root carries no index term, so it cannot denote a dynamic
        // element address whatever the immediate says.
        assert!(matches!(
            builder.element(
                WirExpr::GetLocal("index".into()),
                SafeEncodedAddress::new(root(), 8),
            ),
            Err(SafePlaceBuildError::EncodedBaseMismatch { .. })
        ));
    }

    #[test]
    fn unknown_mismatched_and_unbounded_objects_fail_before_claim_construction() {
        let layouts = LayoutInterner::new();
        let unknown = LayoutId::from_bytes([0x55; 32]);
        assert!(matches!(
            builder(&layouts, unknown, SafeExactExtent::fixed(8)),
            Err(SafePlaceBuildError::UnknownLayout(id)) if id == unknown
        ));

        let mut layouts = LayoutInterner::new();
        let int = layouts
            .intern_type(&Type::Named("Int".into(), Vec::new()), &Resolver)
            .unwrap();
        assert!(matches!(
            builder(&layouts, int, SafeExactExtent::fixed(4)),
            Err(SafePlaceBuildError::FixedExtentMismatch { expected: 8, actual: 4 })
        ));
        assert!(matches!(
            builder(&layouts, int, SafeExactExtent::fixed(u64::MAX)),
            Err(SafePlaceBuildError::NarrowingOverflow { .. })
        ));
    }

    #[test]
    fn runtime_buffers_require_a_checked_length_and_refuse_fixed_field() {
        let mut layouts = LayoutInterner::new();
        let byte = layouts.intern_scalar(ScalarKind::RawByte).unwrap();
        let buffer = layouts.intern_runtime_buffer(byte).unwrap();
        assert!(matches!(
            builder(&layouts, buffer, SafeExactExtent::fixed(4)),
            Err(SafePlaceBuildError::MissingCheckedLength(id)) if id == buffer
        ));

        let builder = dynamic_projection_fixture(&layouts, buffer);
        assert!(matches!(
            builder.fixed_field(1, address(4)),
            Err(SafePlaceBuildError::RuntimeBufferProjection(id)) if id == buffer
        ));
        // A runtime buffer is now indexable. It is the same physical shape as a
        // packed list -- length word then stride-sized elements -- and both go
        // through `packed_element_layout`, so the builder and the validator's
        // Element/Slice arms cannot disagree about which layouts admit an index.
        // Before S1 this was `RuntimeBufferProjection`, which left the payload
        // of every helper buffer unreachable.
        let element = builder
            .element(WirExpr::ConstI32(0), address(4))
            .expect("a runtime buffer payload is indexable");
        assert_eq!(
            element.encoded_offset, 4,
            "the payload begins at the descriptor's own base",
        );
    }

    #[test]
    fn exposed_roots_and_pointer_slots_cannot_launder_object_provenance() {
        let mut layouts = LayoutInterner::new();
        let byte = layouts.intern_scalar(ScalarKind::RawByte).unwrap();
        let scalar_prefix = layouts
            .intern_exposed_root(byte, vec![RuntimePrefixSlot::Scalar(ScalarKind::U32)])
            .unwrap();
        assert!(matches!(
            builder(&layouts, scalar_prefix, SafeExactExtent::fixed(5)),
            Err(SafePlaceBuildError::ExposedRoot { layout, .. }) if layout == scalar_prefix
        ));

        let pointer_prefix = layouts
            .intern_exposed_root(
                byte,
                vec![RuntimePrefixSlot::Pointer {
                    pointee: byte,
                    semantics: PointerSlot::Borrowed,
                }],
            )
            .unwrap();
        assert!(matches!(
            builder(&layouts, pointer_prefix, SafeExactExtent::fixed(5)),
            Err(SafePlaceBuildError::PointerSlotProvenance {
                pointee,
                slot: PointerSlot::Borrowed,
                ..
            }) if pointee == byte
        ));
    }

    #[test]
    fn encoded_offset_narrowing_and_mismatch_fail_closed() {
        let mut layouts = LayoutInterner::new();
        let list = layouts
            .intern_type(
                &Type::Named(
                    "List".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                &Resolver,
            )
            .unwrap();
        let builder = dynamic_projection_fixture(&layouts, list);

        assert!(matches!(
            builder.header(1, address(0)),
            Err(SafePlaceBuildError::EncodedOffsetMismatch {
                expected: 4,
                actual: 0,
                ..
            })
        ));
        assert!(matches!(
            builder.element(WirExpr::ConstI32(i32::MAX), address(0)),
            Err(SafePlaceBuildError::NarrowingOverflow { .. })
        ));
        assert!(matches!(
            builder.element(WirExpr::ConstI32(-1), address(0)),
            Err(SafePlaceBuildError::NegativeClaimValue { .. })
        ));
        assert!(matches!(
            builder.root(SafeEncodedAddress::new(
                WirExpr::GetLocal("not_object".into()),
                0,
            )),
            Err(SafePlaceBuildError::EncodedBaseMismatch { .. })
        ));
    }

    #[test]
    fn affine_address_spoofs_are_outside_the_leaf_root_adapter_subset() {
        let mut layouts = LayoutInterner::new();
        let list = layouts
            .intern_type(
                &Type::Named(
                    "List".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                &Resolver,
            )
            .unwrap();
        let builder = dynamic_projection_fixture(&layouts, list);
        let index = WirExpr::GetLocal("index".into());

        let add = |lhs, rhs, kind| WirExpr::Binary {
            op: BinOp::Add,
            kind,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
        let multiply = |value, stride| WirExpr::Binary {
            op: BinOp::Mul,
            kind: Kind::I32,
            lhs: Box::new(value),
            rhs: Box::new(WirExpr::ConstI32(stride)),
        };

        let duplicate = add(
            root(),
            add(index.clone(), index.clone(), Kind::I32),
            Kind::I32,
        );
        assert!(matches!(
            builder.element(index.clone(), SafeEncodedAddress::new(duplicate, 8)),
            Err(SafePlaceBuildError::EncodedBaseMismatch { .. })
        ));

        let wrong_index = add(
            root(),
            multiply(WirExpr::GetLocal("other_index".into()), 8),
            Kind::I32,
        );
        assert!(matches!(
            builder.element(index.clone(), SafeEncodedAddress::new(wrong_index, 8)),
            Err(SafePlaceBuildError::EncodedBaseMismatch { .. })
        ));

        let wrong_stride = add(root(), multiply(index.clone(), 4), Kind::I32);
        assert!(matches!(
            builder.element(index.clone(), SafeEncodedAddress::new(wrong_stride, 8)),
            Err(SafePlaceBuildError::EncodedBaseMismatch { .. })
        ));

        let i64_add = add(root(), WirExpr::ConstI64(8), Kind::I64);
        assert!(matches!(
            builder.element(index, SafeEncodedAddress::new(i64_add, 0)),
            Err(SafePlaceBuildError::EncodedBaseMismatch { .. })
        ));
    }

    #[test]
    fn ordinary_roots_cannot_forge_provenance() {
        let mut layouts = LayoutInterner::new();
        let int = layouts
            .intern_type(&Type::Named("Int".into(), Vec::new()), &Resolver)
            .unwrap();

        assert!(matches!(
            SafePlaceBuilder::new(
                &layouts,
                int,
                SafeExactExtent::fixed(8),
                WirExpr::GetGlobal("heap".into()),
                AccessMode::Read,
            ),
            Err(SafePlaceBuildError::GlobalObjectRoot)
        ));

        let static_root = WirExpr::StrPtr(64);
        let static_builder = SafePlaceBuilder::new(
            &layouts,
            int,
            SafeExactExtent::fixed(8),
            static_root.clone(),
            AccessMode::Read,
        )
        .unwrap();
        let claim = static_builder
            .root(SafeEncodedAddress::new(static_root.clone(), 0))
            .unwrap();
        assert!(matches!(
            &claim.object.provenance,
            SafeProvenanceExpr::Unknown
        ));
        assert!(matches!(
            static_builder.root(address(0)),
            Err(SafePlaceBuildError::EncodedBaseMismatch { .. })
        ));
    }

    #[test]
    fn dynamic_descriptors_have_no_production_builder_path() {
        let mut layouts = LayoutInterner::new();
        let byte = layouts.intern_scalar(ScalarKind::RawByte).unwrap();
        let buffer = layouts.intern_runtime_buffer(byte).unwrap();
        let list = layouts
            .intern_type(
                &Type::Named(
                    "List".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                &Resolver,
            )
            .unwrap();
        for (layout, bytes) in [(buffer, 0), (buffer, u64::MAX), (list, 0), (list, 64)] {
            assert!(matches!(
                builder(&layouts, layout, SafeExactExtent::fixed(bytes)),
                Err(SafePlaceBuildError::MissingCheckedLength(id)) if id == layout
            ));
        }
    }

    #[test]
    fn packed_list_field_ordinals_remain_rejected() {
        let mut layouts = LayoutInterner::new();
        let list = layouts
            .intern_type(
                &Type::Named(
                    "List".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                &Resolver,
            )
            .unwrap();
        let list_builder = dynamic_projection_fixture(&layouts, list);
        assert!(matches!(
            list_builder.fixed_field(0, address(0)),
            Err(SafePlaceBuildError::PackedListFieldProjection(id)) if id == list
        ));
    }
}
