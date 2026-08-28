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
};
use crate::safewir::AccessMode;
use crate::wir::WirExpr;

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
    /// One executable count operand, for a dynamically sized descriptor. The
    /// header bytes, the stride, and the range arithmetic all stay
    /// descriptor-derived: this supplies the count and nothing else.
    Counted(WirExpr),
}

impl SafeExactExtent {
    pub const fn fixed(bytes: u64) -> Self {
        Self { source: ExactExtentSource::Fixed(bytes) }
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
    let element = match descriptor.kind() {
        LayoutKind::PackedList { element, .. } | LayoutKind::RuntimeBuffer { element } => *element,
        _ => return Err(SafePlaceBuildError::UnsupportedDynamicLayout(layout)),
    };
    let LayoutSize::Dynamic { base, stride } = descriptor.size() else {
        return Err(SafePlaceBuildError::DynamicDescriptorMismatch(layout));
    };
    // A pointer slot anywhere in the object means a loaded pointer's
    // provenance would ride on a byte range this builder cannot authenticate.
    if descriptor
        .fields()
        .iter()
        .any(|field| matches!(field.kind(), FieldKind::Pointer { .. }))
    {
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
        Ok(self.finish(
            SafeProjectionExpr::root(),
            SafeSizeExpr::Constant(0),
            self.full_length.clone(),
            encoded,
        ))
    }

    /// Claim one descriptor-defined packed-list header component.
    /// Component zero is length and component one is capacity.
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
        Ok(self.finish(
            SafeProjectionExpr::from_steps([SafeProjectionStep::Header { component }]),
            SafeSizeExpr::Constant(u64::from(offset)),
            SafeSizeExpr::Constant(4),
            encoded,
        ))
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
        Ok(self.finish(
            SafeProjectionExpr::from_steps([SafeProjectionStep::Field { index }]),
            SafeSizeExpr::Constant(u64::from(field.offset())),
            SafeSizeExpr::Constant(u64::from(length)),
            encoded,
        ))
    }

    /// Claim one packed-list element. The index remains an executable scalar
    /// source for the final def-use adapter; no definition identity is invented.
    pub fn element(
        &self,
        index: WirExpr,
        encoded: SafeEncodedAddress,
    ) -> Result<Box<SafePlaceExpr>, SafePlaceBuildError> {
        validate_claim_operand(&index, "element index")?;
        let (base, stride) = self.packed_elements("element")?;
        self.validate_indexed_address(&encoded, &index, base, stride, "element")?;
        let start = indexed_start(&index, base, stride)?;
        Ok(self.finish(
            SafeProjectionExpr::from_steps([SafeProjectionStep::Element {
                index: Box::new(index),
            }]),
            start,
            SafeSizeExpr::Constant(u64::from(stride)),
            encoded,
        ))
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
        Ok(self.finish(
            SafeProjectionExpr::from_steps([SafeProjectionStep::Slice {
                start: Box::new(start),
                length: Box::new(length),
            }]),
            range_start,
            range_length,
            encoded,
        ))
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
        if matches!(descriptor.kind(), LayoutKind::RuntimeBuffer { .. }) {
            return Err(SafePlaceBuildError::RuntimeBufferProjection(self.object.layout));
        }
        if !matches!(descriptor.kind(), LayoutKind::PackedList { .. }) {
            return Err(SafePlaceBuildError::ProjectionRequiresPackedList { projection });
        }
        let LayoutSize::Dynamic { base, stride } = descriptor.size() else {
            return Err(SafePlaceBuildError::ProjectionRequiresPackedList { projection });
        };
        Ok((base, stride))
    }

    fn validate_fixed_address(
        &self,
        encoded: &SafeEncodedAddress,
        expected: u32,
        projection: &'static str,
    ) -> Result<(), SafePlaceBuildError> {
        if !same_address_leaf(&encoded.base, &self.object.root) {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        }
        if encoded.offset != expected {
            return Err(SafePlaceBuildError::EncodedOffsetMismatch {
                projection,
                expected,
                actual: encoded.offset,
            });
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
        if !same_address_leaf(&encoded.base, &self.object.root) {
            return Err(SafePlaceBuildError::EncodedBaseMismatch { projection });
        }
        let _ = (base, stride);
        Err(SafePlaceBuildError::DynamicEncodedAddressUnsupported { projection })
    }

    fn finish(
        &self,
        projection: SafeProjectionExpr,
        start: SafeSizeExpr,
        length: SafeSizeExpr,
        encoded: SafeEncodedAddress,
    ) -> Box<SafePlaceExpr> {
        Box::new(SafePlaceExpr {
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
        })
    }
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
    PackedListFieldProjection(LayoutId),
    ProjectionRequiresPackedList { projection: &'static str },
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
            Self::RuntimeBufferProjection(layout) => write!(
                formatter,
                "runtime buffer {layout} requires a dedicated checked buffer projection"
            ),
            Self::PackedListFieldProjection(layout) => write!(
                formatter,
                "packed-list layout {layout} requires header or element projection, not a raw field ordinal"
            ),
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

        let split_base = WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I32,
            lhs: Box::new(root()),
            rhs: Box::new(WirExpr::ConstI32(8)),
        };
        assert!(matches!(
            builder.fixed_field(1, SafeEncodedAddress::new(split_base, 0)),
            Err(SafePlaceBuildError::EncodedBaseMismatch { .. })
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
        assert!(matches!(
            builder.element(
                WirExpr::GetLocal("index".into()),
                SafeEncodedAddress::new(indexed, 8),
            ),
            Err(SafePlaceBuildError::EncodedBaseMismatch { .. })
        ));
        assert!(matches!(
            builder.element(
                WirExpr::GetLocal("index".into()),
                SafeEncodedAddress::new(root(), 8),
            ),
            Err(SafePlaceBuildError::DynamicEncodedAddressUnsupported { .. })
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
    fn runtime_buffers_require_length_and_reject_legacy_field_projection() {
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
        assert!(matches!(
            builder.element(WirExpr::ConstI32(0), address(4)),
            Err(SafePlaceBuildError::RuntimeBufferProjection(id)) if id == buffer
        ));
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
