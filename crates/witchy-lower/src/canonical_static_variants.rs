//! Payload-free sum variants as static data.
//!
//! `Leaf`, `None` and every other nullary variant of a reference-counted sum
//! carry one fact, the tag, and no payload. Allocating a fresh object for each
//! occurrence costs an allocation, a retain per store and a release per drop
//! for a value nothing can distinguish from any other. The lowering instead
//! installs one immutable segment per (layout, tag) and lets every occurrence
//! reference it, exactly as string literals already do. Static objects sit
//! below the heap base, so the outlined retain and release helpers skip them
//! and the release placer tracks no token for them.

use super::*;

/// Static segments interned while a module's functions lower. Segment IDs
/// continue after the module's string payloads so both families share one
/// `StaticDataId` space. A segment is keyed by its layout as well as its
/// bytes: the checker binds each segment to one (layout, extent) use, so two
/// sums whose nullary variants encode identically still get separate
/// segments.
pub(super) struct StaticVariantSegments {
    next: u32,
    by_layout_and_bytes: BTreeMap<(witchy_wir::layout::LayoutId, Vec<u8>), witchy_wir::canonical_safewir::StaticDataId>,
}

impl StaticVariantSegments {
    pub(super) fn starting_after(string_payloads: usize) -> Result<Self, CanonicalLoweringError> {
        Ok(Self {
            next: u32::try_from(string_payloads).map_err(|_| CanonicalLoweringError::IdOverflow)?,
            by_layout_and_bytes: BTreeMap::new(),
        })
    }

    fn intern(
        &mut self,
        layout: witchy_wir::layout::LayoutId,
        bytes: Vec<u8>,
    ) -> Result<witchy_wir::canonical_safewir::StaticDataId, CanonicalLoweringError> {
        if let Some(id) = self.by_layout_and_bytes.get(&(layout, bytes.clone())) {
            return Ok(*id);
        }
        let id = witchy_wir::canonical_safewir::StaticDataId::new(self.next);
        self.next = self.next.checked_add(1).ok_or(CanonicalLoweringError::IdOverflow)?;
        self.by_layout_and_bytes.insert((layout, bytes), id);
        Ok(id)
    }

    /// Every interned segment, in ID order.
    pub(super) fn into_segments(self) -> Vec<witchy_wir::canonical_safewir::StaticDataSegment> {
        let mut segments = self
            .by_layout_and_bytes
            .into_iter()
            .map(|((_, bytes), id)| witchy_wir::canonical_safewir::StaticDataSegment::new(id, bytes))
            .collect::<Vec<_>>();
        segments.sort_by_key(|segment| segment.id());
        segments
    }
}

impl FunctionLowerer<'_> {
    /// Lower a payload-free variant of a tagged, reference-counted, non-affine
    /// sum to a reference to its shared static object. The layout is the one
    /// `lower_heap_record_with_slots` would allocate for the same source, so
    /// loads, tag reads and pattern matches see the same bytes a fresh object
    /// would carry: the tag in slot 0 and zero in every other slot.
    pub(super) fn lower_static_nullary_variant(
        &mut self,
        source_type: &Type,
        shape: &ScalarAggregateShape,
        index: usize,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let key = (source_type.clone(), true);
        let cached = self.aggregate_layouts_by_source.borrow().get(&key).copied();
        let layout = if let Some(layout) = cached {
            layout
        } else {
            let layout = aggregate_layout_for_source(
                source_type, &shape.fields, true, self.layout_bundle,
                self.layouts, &self.closures.layouts, self.definitions,
            )?;
            self.aggregate_layouts_by_source.borrow_mut().insert(key, layout);
            layout
        };
        let missing = || CanonicalLoweringError::MissingLayout(Type::Named(shape.name.to_string(), Vec::new()));
        let physical = self.layouts.resolve_nominal(layout).map_err(|_| missing())?;
        let descriptor = self.layouts.get(physical).ok_or_else(missing)?;
        let LayoutSize::Fixed(size) = descriptor.size() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("record layout size"));
        };
        let tag = descriptor.fields().first().ok_or_else(missing)?;
        let tag_kind = match tag.kind() {
            FieldKind::Scalar(kind) => kind,
            FieldKind::Inline(child) => {
                let child = self.layouts.resolve_nominal(child).map_err(|_| missing())?;
                match self.layouts.get(child).map(|child| child.kind()) {
                    Some(LayoutKind::Scalar(kind)) => *kind,
                    _ => return Err(CanonicalLoweringError::UnsupportedExpression("sum tag slot layout")),
                }
            }
            FieldKind::Pointer { .. } => {
                return Err(CanonicalLoweringError::UnsupportedExpression("sum tag slot layout"));
            }
        };
        let mut bytes = vec![0u8; size as usize];
        let start = tag.offset() as usize;
        let width = tag_kind.size() as usize;
        let encoded = u64::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?.to_le_bytes();
        if width > encoded.len() || encoded[width..].iter().any(|byte| *byte != 0) {
            return Err(CanonicalLoweringError::IdOverflow);
        }
        bytes
            .get_mut(start..start + width)
            .ok_or_else(missing)?
            .copy_from_slice(&encoded[..width]);
        let data = self.static_variants.borrow_mut().intern(layout, bytes)?;
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::StaticData {
            destination: object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::FixedBytes(u64::from(size)),
            data,
        });
        Ok(object)
    }
}
