use std::ops::{Deref, DerefMut};

use crate::layout::{
    FieldKind, HeaderLayout, LayoutDescriptor, LayoutInterner, LayoutKind, LayoutSize, PointerSlot,
    RcHeader, ScalarKind,
};

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use super::sorted::{SortedMap, SortedSet};
use super::{InitialCheckError, value_type};

use crate::canonical_safewir::{
    AccessMode, AllocationClass, ArithmeticOperator, BasicBlock, CallArgument,
    CallableContract, ComparisonOperator, Extent, Function, FunctionSignature, GuardKind,
    FunctionSignatureId, HeaderField, LoadTransfer, Operation, OverflowBehavior, Ownership, ParameterContract,
    ParameterOwnership, Place, Projection, ResultContract, ReturnedExtent, StoreTransfer, ValueId,
    ValueType,
};

mod join;
mod calls;
mod ranges;
mod operations;
mod ownership;
mod relations;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObjectState {
    /// The allocation or entry parameter from which this reference originates.
    /// SSA edge arguments rename values, never object identity.
    identity: ValueId,
    /// The logical owner epoch of this allocation.  It is checker-derived,
    /// never an annotation supplied by a transform.  Bounds evidence must
    /// name this exact epoch before it can justify an indexed access.
    owner_generation: u64,
    layout: crate::layout::LayoutId,
    extent: Extent,
    ownership: Ownership,
    allocation_class: Option<AllocationClass>,
    /// A `Shared` ABI boundary may carry either an RC object or immutable
    /// static data. This is deliberately distinct from an absent/unknown
    /// allocation class, which carries no authority.
    shared_immutable: bool,
    /// A managed ABI entry parameter denotes a complete logical value even
    /// though its dynamic extent is read at runtime rather than represented by
    /// local slot facts.
    initialized_owned_slots: bool,
}

/// Abstract owner epoch for a merge of paths with different concrete mutation
/// histories. It is a finite dataflow-top marker, not a concrete mutation
/// counter, and remains stable through later mutations and loop iterations.
const JOINED_OWNER_GENERATION: u64 = u64::MAX;

#[derive(Debug, Clone, Copy)]
struct CheckedRange {
    object: ValueId,
    start: ValueId,
    length: ValueId,
    layout: crate::layout::LayoutId,
    element: crate::layout::LayoutId,
    identity: ValueId,
    ownership: Ownership,
    allocation_class: Option<AllocationClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RangeIdentity {
    object: ValueId,
    start: ValueId,
    length: ValueId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RangeState {
    element: crate::layout::LayoutId,
    ownership: Ownership,
    allocation_class: Option<AllocationClass>,
    initialized: bool,
    zero_width: bool,
    zero_start: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Comparison {
    operator: ComparisonOperator,
    left: ValueId,
    right: ValueId,
}

/// A bound extracted from one particular live owner generation.  Scalar SSA
/// values can describe several owners (for example, a shared loop count), so
/// the relation is stored separately from the scalar value itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct OwnerGeneration {
    identity: ValueId,
    epoch: u64,
}

/// Private storage sharing, never a source of checker facts. Reads retain the
/// underlying collection's exact iteration/comparison behavior. Every mutable
/// access first detaches this collection, including mutations through entries,
/// iterators, and nested values. Other snapshots keep their original contents.
/// Snapshots stay within one synchronous memory-CFG check. Rc avoids atomic
/// refcounts here; moving snapshots between checker threads would require an
/// explicit change to this private representation, not an unsafe Send impl.
struct CowFacts<T>(Rc<T>);

impl<T> Clone for CowFacts<T> {
    fn clone(&self) -> Self { Self(Rc::clone(&self.0)) }
}

impl<T: std::fmt::Debug> std::fmt::Debug for CowFacts<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.as_ref().fmt(formatter)
    }
}

impl<T: PartialEq> PartialEq for CowFacts<T> {
    fn eq(&self, other: &Self) -> bool {
        // Equality remains equality of facts, not Rc identity or a cache hit.
        self.0.as_ref() == other.0.as_ref()
    }
}

impl<T: Eq> Eq for CowFacts<T> {}

impl<T> From<T> for CowFacts<T> {
    fn from(value: T) -> Self { Self(Rc::new(value)) }
}

impl<T: Default> Default for CowFacts<T> {
    fn default() -> Self { T::default().into() }
}

impl<T, Item> FromIterator<Item> for CowFacts<T>
where T: FromIterator<Item> {
    fn from_iter<I: IntoIterator<Item = Item>>(items: I) -> Self {
        T::from_iter(items).into()
    }
}

impl<T> Deref for CowFacts<T> {
    type Target = T;
    fn deref(&self) -> &T { self.0.as_ref() }
}

impl<T: Clone> DerefMut for CowFacts<T> {
    fn deref_mut(&mut self) -> &mut T { Rc::make_mut(&mut self.0) }
}

// Keep the receiver's two-phase borrow at the wrapper boundary: callers may
// read existing facts while computing the key/value before mutation starts.
// A DerefMut-only insert would acquire mutable access before those arguments.
impl<K: Ord + Clone, V: Clone> CowFacts<SortedMap<K, V>> {
    fn insert(&mut self, key: K, value: V) -> Option<V> {
        Rc::make_mut(&mut self.0).insert(key, value)
    }
}

impl<T: Ord + Clone> CowFacts<SortedSet<T>> {
    fn insert(&mut self, value: T) -> bool {
        Rc::make_mut(&mut self.0).insert(value)
    }
}

impl<'a, T> IntoIterator for &'a CowFacts<T>
where &'a T: IntoIterator {
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter { self.0.as_ref().into_iter() }
}

impl<'a, T: Clone> IntoIterator for &'a mut CowFacts<T>
where &'a mut T: IntoIterator {
    type Item = <&'a mut T as IntoIterator>::Item;
    type IntoIter = <&'a mut T as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter { Rc::make_mut(&mut self.0).into_iter() }
}

impl<T: Clone + IntoIterator> IntoIterator for CowFacts<T> {
    type Item = T::Item;
    type IntoIter = T::IntoIter;
    fn into_iter(self) -> Self::IntoIter { Rc::unwrap_or_clone(self.0).into_iter() }
}

/// All derived memory facts at one program point.  This is deliberately a
/// checker-owned snapshot: transforms cannot attach a claimed range, extent,
/// or ownership fact to an edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MemoryState {
    objects: CowFacts<SortedMap<ValueId, ObjectState>>,
    /// Physical identities that a Shared phi may select at runtime. The phi
    /// value is the SSA identity used by subsequent operations; this side
    /// relation prevents the checker from pretending distinct allocations
    /// became one allocation merely because control flow joined.
    shared_phi_origins: CowFacts<SortedMap<ValueId, SortedSet<ValueId>>>,
    stored_objects: CowFacts<SortedMap<(ValueId, SlotIdentity), ObjectState>>,
    /// Every present child of this nullable slot has an exclusive token.
    /// Absence satisfies the property vacuously. Unlike an exact stored child
    /// identity, this proof survives joins of independently built variants.
    nullable_unique_slots: CowFacts<SortedSet<(ValueId, SlotIdentity)>>,
    /// Positive proof that a required nullable field contains a child on
    /// every incoming path. Absence of an absent fact is not such a proof.
    nullable_present_slots: CowFacts<SortedSet<(ValueId, SlotIdentity)>>,
    /// Source-bound recursive ownership shapes, indexed by the current SSA
    /// root. Presence describes expected storage, never initializes a slot.
    affine_plans: CowFacts<SortedMap<ValueId, super::AffinePhysicalPlan>>,
    /// Checked move-only list roots whose initialized prefix owns exclusive
    /// child tokens. A physical list shape alone never creates this fact.
    affine_list_roots: CowFacts<SortedSet<ValueId>>,
    /// Explicitly initialized nullable fixed slots that contain absence. This
    /// is distinct from an uninitialized slot and from a live stored edge.
    nullable_absent_slots: CowFacts<SortedSet<(ValueId, SlotIdentity)>>,
    range_states: CowFacts<SortedMap<RangeIdentity, RangeState>>,
    /// Count/capacity SSA values authenticated by the current generation of
    /// the owner from which they were read or carried.
    bound_generations: CowFacts<SortedMap<ValueId, SortedSet<OwnerGeneration>>>,
    /// Bounds guards retain the owner generations of their operands.  A
    /// mutation, move, consume, unknown call, or disagreeing join cannot
    /// reuse the old guard for the next generation.
    guard_generations: CowFacts<SortedMap<(ComparisonOperator, ValueId, ValueId), SortedSet<OwnerGeneration>>>,
    /// A scalar loaded from a dynamic object's length header on this owner
    /// generation.  It is not a usable extent by itself: only the exact,
    /// allocation-checked bounds guard below may promote it to one.
    fresh_header_lengths: CowFacts<SortedMap<ValueId, SortedSet<OwnerGeneration>>>,
    /// This exact live range covered its allocation's entire current logical
    /// prefix on every predecessor. This relates storage to its current count,
    /// not two scalar SSA values, and grants no child ownership authority.
    full_current_prefixes: CowFacts<SortedSet<RangeIdentity>>,
    constants: CowFacts<SortedMap<ValueId, u64>>,
    exact_additions: CowFacts<SortedMap<ValueId, (ValueId, ValueId)>>,
    comparisons: CowFacts<SortedMap<ValueId, Comparison>>,
    guards: CowFacts<SortedSet<(ComparisonOperator, ValueId, ValueId)>>,
    header_values: CowFacts<SortedMap<(ValueId, HeaderField), ValueId>>,
    static_data_objects: CowFacts<SortedSet<ValueId>>,
    /// Scalar lanes a Store has initialized, by container value and slot.
    /// Fresh storage has no entries. A record or collection that arrived from
    /// outside is summarized complete by initialized_owned_slots.
    initialized_scalar_slots: CowFacts<SortedSet<(ValueId, SlotIdentity)>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SlotIdentity {
    Constant(u64),
    Dynamic(ValueId),
}

pub(super) struct MemoryChecker<'a> {
    function: &'a Function,
    signature: &'a FunctionSignature,
    layouts: &'a LayoutInterner,
    /// The module's closure-object registry: the signature slot 0 of each
    /// closure layout carries. It is the only source of a callable lane's
    /// value type, so a layout missing from it has no readable lane.
    closure_layouts: &'a BTreeMap<crate::layout::LayoutId, FunctionSignatureId>,
    affine_abi: Option<&'a super::AffineAbiProofs>,
    /// Which fields of a fixed layout carry owned pointers.
    ///
    /// This is a pure function of the layout graph, but consolidation asks it
    /// after every store, and re-deriving it allocated a fresh `Vec` each
    /// time — the checker's single largest source of allocator traffic
    /// (BUG-640: 30% of the allocation band, nearly all of it `Vec` growth,
    /// because the filtered collect has no size hint). Derived once per
    /// layout, a repeat ask is a refcount bump. Shared with the scratch
    /// checkers forked off this one, which ask about the same layouts.
    pointer_fields: PointerFieldCache,
    state: MemoryState,
}

type PointerFieldCache =
    Rc<RefCell<BTreeMap<crate::layout::LayoutId, Rc<[(u64, crate::layout::LayoutId)]>>>>;

impl<'a> MemoryChecker<'a> {
    pub(super) const fn layouts(&self) -> &'a LayoutInterner {
        self.layouts
    }

    pub(super) fn new(
        function: &'a Function,
        layouts: &'a LayoutInterner,
        signature: &'a FunctionSignature,
        closure_layouts: &'a BTreeMap<crate::layout::LayoutId, FunctionSignatureId>,
    ) -> Result<Self, InitialCheckError> {
        Self::new_with_affine_abi(function, layouts, signature, closure_layouts, None)
    }

    pub(super) fn new_with_affine_abi(
        function: &'a Function,
        layouts: &'a LayoutInterner,
        signature: &'a FunctionSignature,
        closure_layouts: &'a BTreeMap<crate::layout::LayoutId, FunctionSignatureId>,
        affine_abi: Option<&'a super::AffineAbiProofs>,
    ) -> Result<Self, InitialCheckError> {
        let mut checker = Self {
            function,
            signature,
            layouts,
            closure_layouts,
            affine_abi,
            pointer_fields: PointerFieldCache::default(),
            state: MemoryState {
                objects: SortedMap::new().into(),
                shared_phi_origins: SortedMap::new().into(),
                stored_objects: SortedMap::new().into(),
                nullable_unique_slots: SortedSet::new().into(),
                nullable_present_slots: SortedSet::new().into(),
                affine_plans: SortedMap::new().into(),
                affine_list_roots: SortedSet::new().into(),
                nullable_absent_slots: SortedSet::new().into(),
                range_states: SortedMap::new().into(),
                bound_generations: SortedMap::new().into(),
                guard_generations: SortedMap::new().into(),
                fresh_header_lengths: SortedMap::new().into(),
                full_current_prefixes: SortedSet::new().into(),
                constants: SortedMap::new().into(),
                exact_additions: SortedMap::new().into(),
                comparisons: SortedMap::new().into(),
                guards: SortedSet::new().into(),
                header_values: SortedMap::new().into(),
                static_data_objects: SortedSet::new().into(),
                initialized_scalar_slots: SortedSet::new().into(),
            },
        };
        checker.initialize_parameters(signature)?;
        Ok(checker)
    }

    pub(super) fn snapshot(&self) -> MemoryState {
        self.state.clone()
    }

    pub(super) fn restore(&mut self, state: MemoryState) {
        self.state = state;
    }

    pub(super) fn constant_condition(&self, condition: ValueId) -> Option<bool> {
        let comparison = self.state.comparisons.get(&condition)?;
        let left = *self.state.constants.get(&comparison.left)?;
        let right = *self.state.constants.get(&comparison.right)?;
        let (signed_left, signed_right) = match value_type(self.function, comparison.left).ok()? {
            ValueType::Size => (
                i64::from(u32::try_from(left).ok()? as i32),
                i64::from(u32::try_from(right).ok()? as i32),
            ),
            ValueType::Int => (left as i64, right as i64),
            _ => return None,
        };
        Some(match comparison.operator {
            ComparisonOperator::Equal => left == right,
            ComparisonOperator::NotEqual => left != right,
            ComparisonOperator::LessSigned => signed_left < signed_right,
            ComparisonOperator::LessUnsigned => left < right,
            ComparisonOperator::LessEqualSigned => signed_left <= signed_right,
            ComparisonOperator::LessEqualUnsigned => left <= right,
            ComparisonOperator::GreaterSigned => signed_left > signed_right,
            ComparisonOperator::GreaterUnsigned => left > right,
            ComparisonOperator::GreaterEqualSigned => signed_left >= signed_right,
            ComparisonOperator::GreaterEqualUnsigned => left >= right,
        })
    }

}

/// The direct-exclusive carrier always has two lanes: the ordinary alias and
/// the declaration-derived replacement for the exact `WriteBack` input.  A
/// standalone alias contract cannot acquire that authority.
fn has_distinct_write_back_lane(
    results: &[ResultContract],
    result_index: usize,
    parameter: u32,
    layout: crate::layout::LayoutId,
    parameters: &[ParameterContract],
) -> bool {
    let Ok(parameter) = usize::try_from(parameter) else { return false };
    let write_back_ordinal = parameters.iter().take(parameter + 1)
        .filter(|contract| contract.ownership == ParameterOwnership::WriteBack)
        .count()
        .checked_sub(1);
    let Some(write_back_ordinal) = write_back_ordinal else { return false };
    let write_back_count = parameters.iter()
        .filter(|contract| contract.ownership == ParameterOwnership::WriteBack)
        .count();
    let ordinary_results = results.len().checked_sub(write_back_count);
    let Some(hidden_lane) = ordinary_results.and_then(|ordinary| ordinary.checked_add(write_back_ordinal)) else {
        return false;
    };
    // The declaration result is the leading ordinary lane. Keeping the
    // dedicated carrier there prevents a manually assembled later `Ref`
    // result from impersonating source result authority.
    result_index == 0
        && result_index < ordinary_results.unwrap_or_default()
        && hidden_lane != result_index
        && matches!(parameters.get(parameter), Some(ParameterContract {
            value_type: ValueType::Ref(parameter_layout),
            ownership: ParameterOwnership::WriteBack,
        }) if match results.get(result_index) {
            Some(ResultContract::AliasesWriteBackParameter {
                parameter: claimed_parameter,
                projection,
                ..
            }) => usize::try_from(*claimed_parameter).ok() == Some(parameter)
                && (projection.is_some() || *parameter_layout == layout),
            Some(ResultContract::AliasesOneOfWriteBackParameters {
                parameters: candidates,
                layout: claimed_layout,
            }) => *claimed_layout == layout
                && candidates.len() >= 2
                && candidates.windows(2).all(|pair| pair[0] < pair[1])
                && candidates.binary_search(&(parameter as u32)).is_ok()
                && *parameter_layout == layout,
            _ => false,
        })
        && matches!(results.get(hidden_lane), Some(ResultContract::Shared { layout: actual })
            if parameters.get(parameter).is_some_and(|contract| contract.value_type == ValueType::Ref(*actual)))
}

/// Return the direct-exclusive carrier whose exact hidden lane is
/// `result_index`. This is intentionally reverse lookup over the dedicated
/// contract rather than a layout/qualifier heuristic: unrelated `Shared`
/// results keep their existing semantics.
fn write_back_alias_parameter_for_hidden_lane(
    results: &[ResultContract],
    result_index: usize,
    parameters: &[ParameterContract],
) -> Option<(u32, crate::layout::LayoutId)> {
    results.iter().enumerate().find_map(|(alias_index, result)| {
        let (candidates, layout) = match result {
            ResultContract::AliasesWriteBackParameter { parameter, layout, .. } => (vec![*parameter], *layout),
            ResultContract::AliasesOneOfWriteBackParameters { parameters, layout } => {
                (parameters.clone(), *layout)
            }
            _ => return None,
        };
        candidates.into_iter().find_map(|parameter| {
            let parameter_index = usize::try_from(parameter).ok()?;
            let ValueType::Ref(hidden_layout) = parameters.get(parameter_index)?.value_type else {
                return None;
            };
            let has_lane = has_distinct_write_back_lane(
                results,
                alias_index,
                parameter,
                layout,
                parameters,
            );
            (has_lane && write_back_hidden_lane(results, parameter, parameters) == Some(result_index))
                .then_some((parameter, hidden_layout))
        })
    }).or_else(|| terminal_write_back_parameter_for_hidden_lane(results, result_index, parameters))
}

/// The final ABI lanes return one replacement for each `WriteBack` parameter.
/// A direct exclusive procedure has no source-reference result carrier, so
/// identify that terminal lane only from its fixed ABI ordinal and exact root
/// layout.  Any declared alias carrier suppresses this fallback: ordinary
/// `Shared` results must never acquire write-back authority from shape alone.
fn terminal_write_back_parameter_for_hidden_lane(
    results: &[ResultContract],
    result_index: usize,
    parameters: &[ParameterContract],
) -> Option<(u32, crate::layout::LayoutId)> {
    if results.iter().any(|result| matches!(result,
        ResultContract::AliasesWriteBackParameter { .. }
            | ResultContract::AliasesOneOfWriteBackParameters { .. }
    )) {
        return None;
    }
    let candidates = parameters.iter().enumerate().filter_map(|(index, contract)| {
        let ValueType::Ref(layout) = contract.value_type else { return None };
        (contract.ownership == ParameterOwnership::WriteBack
            && write_back_hidden_lane(results, index as u32, parameters) == Some(result_index)
            && matches!(results.get(result_index), Some(ResultContract::Shared { layout: actual }) if *actual == layout))
            .then_some((index as u32, layout))
    }).collect::<Vec<_>>();
    (candidates.len() == 1).then(|| candidates[0])
}

/// The Bump root exception is deliberately narrower than the generic
/// write-back carrier: one projected ordinary result, then its one exact
/// root-layout hidden lane.  The initial authority check binds that carrier
/// to the source field; this local predicate only gates the physical relay.
fn authenticated_projected_bump_write_back_parameter(
    results: &[ResultContract],
    parameters: &[ParameterContract],
) -> Option<usize> {
    let [
        ResultContract::AliasesWriteBackParameter {
            parameter,
            layout,
            projection: Some(_),
        },
        ResultContract::Shared { layout: hidden_layout },
    ] = results else {
        return None;
    };
    let parameter_index = usize::try_from(*parameter).ok()?;
    let ParameterContract {
        value_type: ValueType::Ref(parameter_layout),
        ownership: ParameterOwnership::WriteBack,
    } = parameters.get(parameter_index)? else {
        return None;
    };
    (*hidden_layout == *parameter_layout
        && has_distinct_write_back_lane(results, 0, *parameter, *layout, parameters)
        && write_back_hidden_lane(results, *parameter, parameters) == Some(1))
        .then_some(parameter_index)
}

fn write_back_hidden_lane(
    results: &[ResultContract],
    parameter: u32,
    parameters: &[ParameterContract],
) -> Option<usize> {
    let parameter = usize::try_from(parameter).ok()?;
    let write_back_ordinal = parameters.iter().take(parameter + 1)
        .filter(|contract| contract.ownership == ParameterOwnership::WriteBack)
        .count()
        .checked_sub(1)?;
    let write_back_count = parameters.iter()
        .filter(|contract| contract.ownership == ParameterOwnership::WriteBack)
        .count();
    results.len().checked_sub(write_back_count)?.checked_add(write_back_ordinal)
}

fn drop_ref_layout_supported(
    layouts: &LayoutInterner,
    layout: crate::layout::LayoutId,
    visited: &mut SortedSet<crate::layout::LayoutId>,
) -> bool {
    let mut pending = vec![layout];
    while let Some(layout) = pending.pop() {
        let Ok(physical) = layouts.resolve_nominal(layout) else { return false };
        if !visited.insert(physical) {
            continue;
        }
        let Some(descriptor) = layouts.get(physical) else { return false };
        match descriptor.kind() {
            LayoutKind::Scalar(_) => {}
            // Recursion is in the layout graph, not the compiler's call stack.
            // Every physical node still checks all of its owned child edges.
            LayoutKind::PackedRecord { fields } | LayoutKind::Tuple { fields } => {
                for field in fields {
                    let Some(field) = layouts.get(*field) else { return false };
                    match field.kind() {
                        LayoutKind::Scalar(_) => {}
                        LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned | PointerSlot::OwnedNullable } => pending.push(*pointee),
                        _ => return false,
                    }
                }
            }
            LayoutKind::PackedList { element, .. } | LayoutKind::RuntimeBuffer { element } => {
                let Some(element) = layouts.get(*element) else { return false };
                match element.kind() {
                    LayoutKind::Scalar(_) => {}
                    LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } => pending.push(*pointee),
                    _ => return false,
                }
            }
            _ => return false,
        }
    }
    true
}

fn remap_value(renames: &SortedMap<ValueId, ValueId>, value: ValueId) -> ValueId {
    renames.get(&value).copied().unwrap_or(value)
}

fn remap_extent(renames: &SortedMap<ValueId, ValueId>, extent: &Extent) -> Extent {
    match extent {
        Extent::FixedBytes(bytes) => Extent::FixedBytes(*bytes),
        Extent::HeaderPrefix { minimum_bytes } => Extent::HeaderPrefix {
            minimum_bytes: *minimum_bytes,
        },
        Extent::Counted {
            header_bytes,
            count,
            capacity,
            stride_bytes,
        } => Extent::Counted {
            header_bytes: *header_bytes,
            count: remap_value(renames, *count),
            capacity: remap_value(renames, *capacity),
            stride_bytes: *stride_bytes,
        },
    }
}

/// The meet of two shared, managed states of one phi object that agree on
/// identity, layout, and extent: the allocation class survives only if both
/// sides name the same one, the object is immutable (both sides are managed
/// storage, so `is_managed_storage` stays true), and owned slots count as
/// initialized only if both sides say so. Different owner generations meet at
/// the stable joined epoch; predecessor-local bounds facts are intersected
/// separately and therefore cannot authenticate that joined epoch.
fn shared_phi_meet(left: &ObjectState, right: &ObjectState) -> Option<ObjectState> {
    // A shared object is never written, so its capacity bounds nothing; two
    // counted extents that differ only in the capacity id meet at the count.
    let extent = match (&left.extent, &right.extent) {
        (l, r) if l == r => l.clone(),
        (
            Extent::Counted { header_bytes, count, stride_bytes, .. },
            Extent::Counted { header_bytes: rh, count: rc, stride_bytes: rs, .. },
        ) if header_bytes == rh && count == rc && stride_bytes == rs => Extent::Counted {
            header_bytes: *header_bytes,
            count: *count,
            capacity: *count,
            stride_bytes: *stride_bytes,
        },
        _ => return None,
    };
    if left.identity != right.identity
        || left.layout != right.layout
        || left.ownership != Ownership::Shared
        || right.ownership != Ownership::Shared
        || !MemoryChecker::is_managed_storage(left)
        || !MemoryChecker::is_managed_storage(right)
    {
        return None;
    }
    Some(ObjectState {
        identity: left.identity,
        owner_generation: if left.owner_generation == right.owner_generation {
            left.owner_generation
        } else {
            JOINED_OWNER_GENERATION
        },
        layout: left.layout,
        extent,
        ownership: Ownership::Shared,
        allocation_class: (left.allocation_class == right.allocation_class).then_some(left.allocation_class).flatten(),
        shared_immutable: true,
        initialized_owned_slots: left.initialized_owned_slots && right.initialized_owned_slots,
    })
}

/// When two states of one object differ only in their dynamic extent facts
/// (one side still holds the header-prefix form the other has promoted, or
/// both hold counted forms whose count and capacity were loaded on different
/// paths), both sides as the header-prefix form with that header size. Each
/// side's count is a true fact about the same header on its own path; the
/// join forgets it and later code re-derives it from its own loads.
fn demoted_pair(left: &ObjectState, right: &ObjectState) -> Option<(ObjectState, ObjectState)> {
    let shape = |extent: &Extent| match extent {
        Extent::HeaderPrefix { minimum_bytes } => Some((*minimum_bytes, None)),
        Extent::Counted { header_bytes, stride_bytes, .. } => Some((*header_bytes, Some(*stride_bytes))),
        Extent::FixedBytes(_) => None,
    };
    let (left_bytes, left_stride) = shape(&left.extent)?;
    let (right_bytes, right_stride) = shape(&right.extent)?;
    if left_bytes != right_bytes
        || left.extent == right.extent
        || matches!((left_stride, right_stride), (Some(l), Some(r)) if l != r)
    {
        return None;
    }
    let prefix = Extent::HeaderPrefix { minimum_bytes: left_bytes };
    let mut demoted_left = left.clone();
    let mut demoted_right = right.clone();
    demoted_left.extent = prefix.clone();
    demoted_right.extent = prefix;
    Some((demoted_left, demoted_right))
}

/// Join the same live object after one predecessor invalidated its owner
/// generation. The result deliberately names an abstract joined generation:
/// predecessor-local header and guard facts must not become facts about the
/// conditional result.
fn join_path_mutated_owner(left: &ObjectState, right: &ObjectState) -> Option<ObjectState> {
    if !matches!(left.ownership, Ownership::Unique | Ownership::Shared)
        || !matches!(right.ownership, Ownership::Unique | Ownership::Shared)
        || left.owner_generation == right.owner_generation
    {
        return None;
    }
    let mut joined = left.clone();
    joined.owner_generation = JOINED_OWNER_GENERATION;
    let mut normalized_right = right.clone();
    normalized_right.owner_generation = joined.owner_generation;
    (joined == normalized_right).then_some(joined)
}

/// Return extent lanes only when both predecessor states carry precisely the
/// same SSA definitions.  Arithmetic equality or a matching runtime value is
/// insufficient: this is the narrow authorization for a fresh joined owner to
/// use an extent lane in a *new* bounds guard.
fn exact_carried_counted_extent(left: &ObjectState, right: &ObjectState) -> Option<(ValueId, ValueId)> {
    match (&left.extent, &right.extent) {
        (
            Extent::Counted { count: left_count, capacity: left_capacity, .. },
            Extent::Counted { count: right_count, capacity: right_capacity, .. },
        ) if left_count == right_count && left_capacity == right_capacity => Some((*left_count, *left_capacity)),
        _ => None,
    }
}

/// Keep the shared fact container whenever the meet did not remove anything.
///
/// The containers are copy-on-write, but `meet` used to hand every field a
/// freshly allocated container, so the sharing never survived a join: two
/// states were never the same allocation even when their facts were identical.
/// Measured on a cold async compile, `Arc::ptr_eq` between the two incoming
/// sides hit 0 times out of 4,372,863.
///
/// Both meets here are intersections, so the result is always a subset of
/// `left`. Equal length therefore proves the result IS `left`, which is an
/// O(1) test once the walk is done. Returning `left`'s container then keeps
/// the pointer shared into the next round, where the `ptr_eq` fast path can
/// skip the walk entirely -- the executor's blocks are revisited many times,
/// so that is the common case after the first couple of visits.
/// Does every entry of `left` survive intersection with `right`?
///
/// The obvious spelling is `left.iter().all(|(k, v)| right.get(k) == Some(v))`,
/// which is one tree descent per entry: `n` random walks with the pointer
/// chasing and cache misses that implies. Both maps already iterate in key
/// order, so a merge walks each side once, sequentially, and stops at the
/// first key `right` is missing. Measured on a cold async compile, a third of
/// these meets carry 65-256 entries and another fifth carry 17-64, so the
/// difference is the difference between `n log m` scattered lookups and
/// `n + m` sequential steps.
/// Join two tables of owner-generation proofs.
///
/// Each entry says "this scalar is an authenticated observation of its owner
/// at these generations". A join keeps what BOTH predecessors proved, which is
/// the intersection of the two generation sets — not, as the generic meet
/// does, only entries whose sets are exactly equal. Requiring equality is
/// sound but discards the generation the predecessors agreed on whenever one
/// of them happens to have proved another alongside it.
fn meet_generation_facts<K: Ord + Copy>(
    left: &CowFacts<SortedMap<K, SortedSet<OwnerGeneration>>>,
    right: &CowFacts<SortedMap<K, SortedSet<OwnerGeneration>>>,
) -> CowFacts<SortedMap<K, SortedSet<OwnerGeneration>>> {
    // Preserve the exact predecessor container when no fact is removed, as
    // ordinary map meets do. Re-authentication writes detach through CowFacts.
    if all_entries_survive(left, right) {
        return left.clone();
    }
    left.iter()
        .filter_map(|(key, ours)| {
            let theirs = right.get(key)?;
            let shared: SortedSet<OwnerGeneration> = ours.intersection(theirs).copied().collect();
            (!shared.is_empty()).then(|| (*key, shared))
        })
        .collect()
}

/// Carry a *fresh header observation* about a concrete owner epoch into the
/// joined epoch its owner is about to have.
///
/// [`JOINED_OWNER_GENERATION`] is the dataflow top for an identity: the merge
/// of paths with different concrete mutation histories. An entry in
/// `fresh_header_lengths` says "this SSA value is an exact Length header load
/// of that owner". A predecessor that recorded the load at epoch 1 and one
/// that recorded the same load at the top are making the same claim about the
/// same value — on whichever path control arrived, that load was the current
/// length — so the merged observation is about the merged epoch. Left alone,
/// the two sets share no member and the intersection throws the claim away.
///
/// That is how a list built in an inner loop and pushed into an outer
/// accumulator lost its length: both predecessors named the same SSA value as
/// the list's length lane, one at epoch 1 and one at the top, so the counted
/// copy was left with a bound it could not authenticate (RFC-0167 foundation
/// debt, corpus `018`).
///
/// This applies to `fresh_header_lengths` and to nothing else. `bound_generations`
/// and `guard_generations` hold *derived* proofs — "this guard established
/// `start < count`" — which a predecessor may have established BEFORE a
/// mutation that the other predecessor performed. There the concrete epoch is
/// the whole point, and rebasing both sides into the top would revive a guard
/// the mutation retired; `bounds_guards_name_the_current_owner_generation`
/// pins that refusal.
fn rebase_joined_generations<K: Ord + Copy>(
    facts: &CowFacts<SortedMap<K, SortedSet<OwnerGeneration>>>,
    joined: &SortedSet<ValueId>,
) -> CowFacts<SortedMap<K, SortedSet<OwnerGeneration>>> {
    let affected = |set: &SortedSet<OwnerGeneration>| {
        set.iter().any(|fact| {
            fact.epoch != JOINED_OWNER_GENERATION && joined.contains(&fact.identity)
        })
    };
    if !facts.values().any(affected) {
        return facts.clone();
    }
    facts
        .iter()
        .map(|(key, set)| {
            let rebased = set
                .iter()
                .map(|fact| {
                    if joined.contains(&fact.identity) {
                        OwnerGeneration { identity: fact.identity, epoch: JOINED_OWNER_GENERATION }
                    } else {
                        *fact
                    }
                })
                .collect();
            (*key, rebased)
        })
        .collect()
}

fn all_entries_survive<K: Ord, V: PartialEq>(
    left: &SortedMap<K, V>,
    right: &SortedMap<K, V>,
) -> bool {
    // `right` may hold keys `left` does not; those simply are not in the
    // intersection of `left` with it, so they are skipped rather than refused.
    let mut candidates = right.iter().peekable();
    for (key, value) in left {
        loop {
            match candidates.peek() {
                None => return false,
                Some((candidate, _)) if *candidate < key => { candidates.next(); }
                Some((candidate, found)) if *candidate == key => {
                    if **found != *value {
                        return false;
                    }
                    candidates.next();
                    break;
                }
                // The next key `right` holds is already past this one, so
                // `right` does not hold it at all.
                Some(_) => return false,
            }
        }
    }
    true
}

fn all_elements_survive<T: Ord>(left: &SortedSet<T>, right: &SortedSet<T>) -> bool {
    let mut candidates = right.iter().peekable();
    for element in left {
        loop {
            match candidates.peek() {
                None => return false,
                Some(candidate) if *candidate < element => { candidates.next(); }
                Some(candidate) if *candidate == element => { candidates.next(); break; }
                Some(_) => return false,
            }
        }
    }
    true
}

fn meet_map<K: Ord + Copy, V: Clone + PartialEq>(
    left: &CowFacts<SortedMap<K, V>>,
    right: &CowFacts<SortedMap<K, V>>,
) -> CowFacts<SortedMap<K, V>> {
    // Decide first, allocate second. An intersection can only REMOVE entries
    // from `left`, so if every one of them survives the result is `left`
    // itself -- and that is the overwhelmingly common case (4,109,563 of
    // 4,372,863 meets on a cold async compile).
    if all_entries_survive(left, right) {
        return left.clone();
    }
    intersect_map(left, right).into()
}

fn meet_set<T: Ord + Copy>(
    left: &CowFacts<SortedSet<T>>,
    right: &CowFacts<SortedSet<T>>,
) -> CowFacts<SortedSet<T>> {
    if all_elements_survive(left, right) {
        return left.clone();
    }
    left.intersection(right).copied().collect()
}

fn intersect_map<K: Ord + Copy, V: Clone + PartialEq>(
    left: &SortedMap<K, V>,
    right: &SortedMap<K, V>,
) -> SortedMap<K, V> {
    left
        .iter()
        .filter_map(|(key, value)| (right.get(key) == Some(value)).then(|| (*key, value.clone())))
        .collect()
}

fn meet_shared_phi_origins(
    left: &MemoryState,
    right: &MemoryState,
) -> SortedMap<ValueId, SortedSet<ValueId>> {
    left.shared_phi_origins
        .iter()
        .filter_map(|(value, left_origins)| {
            let right_origins = right.shared_phi_origins.get(value)?;
            let mut origins = left_origins.clone();
            origins.extend(right_origins);
            Some((*value, origins))
        })
        .collect()
}

/// Meet initialized aggregate prefixes across CFG edges. An empty prefix has
/// no elements and therefore places no constraint on their allocation class;
/// every other ownership and initialization fact must agree exactly.
fn meet_range_states(
    existing: &MemoryState,
    incoming: &MemoryState,
) -> SortedMap<RangeIdentity, RangeState> {
    existing
        .range_states
        .iter()
        .filter_map(|(identity, current)| {
            let mut candidates = incoming.range_states.iter().filter(|(candidate, next)| {
                range_states_match(existing, identity, current, incoming, candidate, next)
            });
            let (_next_identity, next) = candidates.next()?;
            if candidates.next().is_some() {
                return None;
            }
            if current.zero_width && next.zero_width {
                return Some((
                    *identity,
                    RangeState {
                        element: current.element,
                        ownership: Ownership::Unique,
                        allocation_class: None,
                        initialized: true,
                        zero_width: true,
                        zero_start: current.zero_start && next.zero_start,
                    },
                ));
            }
            if current == next {
                return Some((*identity, *current));
            }
            if current.element != next.element
                || current.initialized != next.initialized
                || !current.initialized
                || current.zero_start != next.zero_start
            {
                return None;
            }
            let ownership = match (current.ownership, next.ownership) {
                (Ownership::Unique, Ownership::Unique) => Ownership::Unique,
                (Ownership::Unique | Ownership::Shared, Ownership::Shared)
                | (Ownership::Shared, Ownership::Unique) => Ownership::Shared,
                _ => return None,
            };
            let allocation_class =
                (current.allocation_class == next.allocation_class)
                    .then_some(current.allocation_class)
                    .flatten();
            Some((
                *identity,
                RangeState { ownership, allocation_class, ..*current },
            ))
        })
        .collect()
}

/// Match range coordinates without introducing any new scalar equivalence.
fn range_states_match(
    existing: &MemoryState,
    identity: &RangeIdentity,
    current: &RangeState,
    incoming: &MemoryState,
    candidate: &RangeIdentity,
    next: &RangeState,
) -> bool {
    candidate.object == identity.object && next.element == current.element
        // A zero-start fact survives losing the SSA name of a loop-local zero.
        && if current.zero_width || next.zero_width {
            current.zero_width && next.zero_width
                && current.zero_start == next.zero_start
                && same_scalar_across(existing, identity.length, incoming, candidate.length)
        } else {
            ((current.zero_start && next.zero_start)
                || same_scalar_across(existing, identity.start, incoming, candidate.start))
                && same_scalar_across(existing, identity.length, incoming, candidate.length)
        }
}

fn range_is_full_current_prefix(
    facts: &MemoryState,
    identity: &RangeIdentity,
    range: &RangeState,
) -> bool {
    if !range.initialized || !range.zero_start
        || !matches!(range.ownership, Ownership::Unique | Ownership::Shared)
    {
        return false;
    }
    let Some(root) = facts.objects.get(&identity.object) else { return false; };
    let header_lengths = MemoryChecker::header_length_pairs(facts);
    // MoveValue can leave the range under a consumed SSA root. Only a live
    // token for that very allocation can retain its proof; overlapping Shared
    // phi origin sets are deliberately not storage identity.
    facts.objects.values().any(|object| {
        object.identity == root.identity && object.layout == root.layout
            && matches!(object.ownership, Ownership::Unique | Ownership::Shared)
            && match object.extent {
                Extent::Counted { count, .. } => {
                    MemoryChecker::same_scalar_with(facts, &header_lengths, identity.length, count)
                        && (!range.zero_width || facts.constants.get(&count) == Some(&0))
                }
                Extent::HeaderPrefix { .. } => facts.full_current_prefixes.contains(identity),
                _ => false,
            }
    })
}

fn meet_full_current_prefixes(
    existing: &MemoryState,
    incoming: &MemoryState,
    ranges: &SortedMap<RangeIdentity, RangeState>,
) -> SortedSet<RangeIdentity> {
    ranges.keys().filter_map(|identity| {
        let current = existing.range_states.get(identity)?;
        if !range_is_full_current_prefix(existing, identity, current) {
            return None;
        }
        let mut candidates = incoming.range_states.iter().filter(|(candidate, next)|
            range_states_match(existing, identity, current, incoming, candidate, next));
        let (candidate, next) = candidates.next()?;
        if candidates.next().is_some()
            || !range_is_full_current_prefix(incoming, candidate, next)
        {
            return None;
        }
        Some(*identity)
    }).collect()
}

fn same_scalar_across(
    left: &MemoryState,
    left_value: ValueId,
    right: &MemoryState,
    right_value: ValueId,
) -> bool {
    left_value == right_value
        || left
            .constants
            .get(&left_value)
            .zip(right.constants.get(&right_value))
            .is_some_and(|(left, right)| left == right)
}

fn operation_name(operation: &Operation) -> &'static str {
    match operation {
        Operation::CopyBytes { .. } => "CopyBytes",
        Operation::MoveRange { .. } => "MoveRange",
        Operation::CloneRange { .. } => "CloneRange",
        Operation::CloneRef { .. } => "CloneRef",
        Operation::Retain { .. } => "Retain",
        Operation::Release { .. } => "Release",
        Operation::DropRef { .. } => "DropRef",
        _ => "memory operation",
    }
}

fn returned_extent_matches(actual: &Extent, expected: &ReturnedExtent) -> bool {
    match (actual, expected) {
        (Extent::FixedBytes(actual), ReturnedExtent::FixedBytes(expected)) => actual == expected,
        (
            Extent::Counted { header_bytes: actual_header, stride_bytes: actual_stride, .. },
            ReturnedExtent::Counted { header_bytes: expected_header, stride_bytes: expected_stride },
        ) => actual_header == expected_header && actual_stride == expected_stride,
        _ => false,
    }
}

#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
pub mod self_checks {
    use super::*;

    pub fn recursive_drop_layout_graph_requires_bound_supported_children() {
        let mut layouts = LayoutInterner::new();
        let nominal = layouts.intern_opaque_nominal([23; 32]).unwrap();
        let child = layouts.intern_runtime_pointer_value_slot(nominal, PointerSlot::OwnedNullable).unwrap();
        let physical = layouts.intern_packed_record(vec![child]).unwrap();
        assert!(!drop_ref_layout_supported(&layouts, nominal, &mut SortedSet::new()));
        layouts.bind_nominal(nominal, physical).unwrap();
        assert!(drop_ref_layout_supported(&layouts, nominal, &mut SortedSet::new()));
        let missing = layouts.intern_opaque_nominal([24; 32]).unwrap();
        let missing_child = layouts.intern_runtime_pointer_value_slot(missing, PointerSlot::Owned).unwrap();
        let with_missing = layouts.intern_packed_record(vec![missing_child, child]).unwrap();
        assert!(!drop_ref_layout_supported(&layouts, with_missing, &mut SortedSet::new()), "a closed recursive edge must not hide another unbound child");
        let nullable_list = layouts.intern_packed_list(child, crate::layout::RcHeader::Required).unwrap();
        assert!(!drop_ref_layout_supported(&layouts, nullable_list, &mut SortedSet::new()), "nullable collection slots remain unsupported");
    }

    const COUNT: ValueId = ValueId::new(0);
    const CAPACITY: ValueId = ValueId::new(1);
    const START: ValueId = ValueId::new(2);
    const LENGTH: ValueId = ValueId::new(3);
    const LONG_LENGTH: ValueId = ValueId::new(4);
    const SOURCE: ValueId = ValueId::new(5);
    const DESTINATION: ValueId = ValueId::new(6);
    const END: ValueId = ValueId::new(7);
    const CHILD: ValueId = ValueId::new(8);
    const LOADED: ValueId = ValueId::new(9);
    const CONDITION: ValueId = ValueId::new(10);
    const ALIAS_START: ValueId = ValueId::new(11);
    const ALIAS_LENGTH: ValueId = ValueId::new(12);
    const ZERO: ValueId = ValueId::new(13);

    fn with_header_promotion_checker(
        family: u8,
        test: impl FnOnce(&mut MemoryChecker<'_>, u32, u32),
    ) {
        let mut layouts = LayoutInterner::new();
        let element = match family {
            0 => layouts.intern_scalar(ScalarKind::RawByte).unwrap(),
            1 => layouts.intern_scalar(ScalarKind::U32).unwrap(),
            2 => {
                let child = layouts.intern_scalar(ScalarKind::Int).unwrap();
                layouts.intern_runtime_pointer_value_slot(child, PointerSlot::Owned).unwrap()
            }
            3 => layouts.intern_scalar(ScalarKind::Unit).unwrap(),
            _ => unreachable!(),
        };
        let layout = if family < 2 {
            layouts.intern_runtime_buffer(element).unwrap()
        } else {
            layouts.intern_packed_list(element, RcHeader::Required).unwrap()
        };
        let LayoutSize::Dynamic { base, stride } = layouts.get(layout).unwrap().size() else {
            panic!("dynamic promotion fixture");
        };
        let function = Function {
            id: crate::canonical_safewir::FunctionId::new(0),
            signature: FunctionSignatureId::new(0),
            entry: crate::canonical_safewir::BlockId::new(0),
            value_types: [
                (SOURCE, ValueType::Ref(layout)),
                (LOADED, ValueType::Size),
                (LONG_LENGTH, ValueType::Size),
                (CONDITION, ValueType::Bool),
            ]
            .into_iter()
            .collect(),
            blocks: vec![BasicBlock {
                id: crate::canonical_safewir::BlockId::new(0),
                parameters: Vec::new(), operations: Vec::new(),
                terminator: crate::canonical_safewir::Terminator::Return { values: Vec::new() },
            }],
        };
        let signature = FunctionSignature {
            id: function.signature, parameters: Vec::new(), results: Vec::new(),
            contract: CallableContract { parameters: Vec::new(), results: Vec::new(), parameter_effects: BTreeMap::new() },
        };
        let closure_layouts = BTreeMap::new();
        let mut checker = MemoryChecker::new(&function, &layouts, &signature, &closure_layouts).unwrap();
        // White-box edge facts model a complete managed ABI result whose
        // header loads are authenticated but whose extent is still lazy.
        checker.state.objects.insert(SOURCE, ObjectState {
            identity: SOURCE, owner_generation: 0, layout, extent: Extent::HeaderPrefix { minimum_bytes: base },
            ownership: Ownership::Shared, allocation_class: None,
            shared_immutable: true, initialized_owned_slots: true,
        });
        checker.state.constants.extend([(COUNT, 1), (CAPACITY, 2), (ZERO, 0)]);
        checker.state.header_values.extend([
            ((SOURCE, HeaderField::Length), COUNT), ((SOURCE, HeaderField::Capacity), CAPACITY),
        ]);
        test(&mut checker, base, stride);
    }

    pub fn reauthenticate_raw_header_extent_requires_current_fresh_proofs_and_rebinds_conservatively() {
        with_header_promotion_checker(0, |checker, base, stride| {
            let maximum = (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride);
            checker.check(&Operation::Load {
                destination: LOADED,
                place: Place::projected(SOURCE, Projection::Header(HeaderField::Length), ValueType::Size, AccessMode::Read),
                transfer: LoadTransfer::CopyScalar,
            }).expect("fresh raw length load");
            checker.check(&Operation::ConstSize { destination: LONG_LENGTH, value: maximum })
                .expect("allocation maximum");
            checker.check(&Operation::Compare {
                destination: CONDITION, operator: ComparisonOperator::LessEqualUnsigned,
                left: LOADED, right: LONG_LENGTH,
            }).expect("allocation comparison");
            checker.check(&Operation::Guard { condition: CONDITION, kind: GuardKind::Bounds })
                .expect("allocation guard");
            checker.check(&Operation::Compare {
                destination: CONDITION, operator: ComparisonOperator::LessEqualUnsigned,
                left: LOADED, right: LOADED,
            }).expect("exact comparison");
            checker.check(&Operation::Guard { condition: CONDITION, kind: GuardKind::Bounds })
                .expect("exact guard");
            let established = checker.snapshot();
            let reauth = Operation::ReauthenticateRawHeaderExtent { object: SOURCE, header: LOADED };
            checker.check(&reauth).expect("fresh guarded raw header reauthenticates");
            assert!(matches!(checker.state.objects[&SOURCE].extent,
                Extent::Counted { count, capacity, .. } if count == LOADED && capacity == LOADED));
            // A second rebind starts from Counted and remains conservative.
            checker.check(&reauth).expect("already-counted raw header may rebind to its fresh length");

            for mutate in [
                |state: &mut MemoryState| { state.header_values.remove(&(SOURCE, HeaderField::Length)); },
                |state: &mut MemoryState| { state.fresh_header_lengths.remove(&LOADED); },
                |state: &mut MemoryState| { state.guards.remove(&(ComparisonOperator::LessEqualUnsigned, LOADED, LONG_LENGTH)); },
                |state: &mut MemoryState| { state.guards.remove(&(ComparisonOperator::LessEqualUnsigned, LOADED, LOADED)); },
            ] {
                checker.restore(established.clone());
                mutate(&mut checker.state);
                assert!(checker.check(&reauth).is_err(), "missing reauthentication precondition must reject");
            }
            checker.restore(established);
            checker.invalidate_owner_generation(SOURCE).expect("invalidate generation");
            assert!(checker.check(&reauth).is_err(), "stale fresh header must reject");
        });
    }

    pub fn scalar_relation_shortcuts_match_materialized_search() {
        with_header_promotion_checker(0, |checker, base, stride| {
            let unknown = ValueId::new(110);
            let sum = ValueId::new(111);
            let equal_constant = ValueId::new(112);
            let unrelated = ValueId::new(113);
            checker.state.objects.get_mut(&SOURCE).unwrap().extent = Extent::Counted {
                header_bytes: base, count: COUNT, capacity: CAPACITY, stride_bytes: stride,
            };
            checker.state.header_values.insert((SOURCE, HeaderField::Length), LOADED);
            checker.state.constants.insert(equal_constant, 1);
            checker.state.guards.insert((ComparisonOperator::LessEqualUnsigned, unknown, LOADED));
            checker.state.exact_additions.insert(sum, (unknown, COUNT));
            let facts = &checker.state;
            let headers = MemoryChecker::header_length_pairs(facts);
            let steps = UpperSteps::new(facts, |_| true);
            let values = [COUNT, CAPACITY, ZERO, LOADED, unknown, sum, equal_constant, unrelated];
            // Compare to the unchanged materialized evaluators, including
            // reflexive/constants, header equality, guard/addition reachability,
            // and missing relations. No duplicated proof algorithm in the test.
            for left in values {
                for right in values {
                    assert_eq!(MemoryChecker::same_scalar_in(facts, left, right),
                        MemoryChecker::same_scalar_with(facts, &headers, left, right), "equality {left:?} {right:?}");
                    assert_eq!(MemoryChecker::less_equal_in(facts, left, right),
                        MemoryChecker::less_equal_in_with(facts, &steps, left, right), "order {left:?} {right:?}");
                }
            }
            assert!(MemoryChecker::less_equal_in(facts, unknown, COUNT));
            assert!(!MemoryChecker::less_equal_in(facts, unrelated, CAPACITY));
        });
    }

    pub fn cow_fact_mutations_preserve_snapshots_and_return_values() {
        let original: CowFacts<SortedMap<u32, SortedSet<u32>>> =
            SortedMap::from([(1, SortedSet::from([10])), (2, SortedSet::from([20]))]).into();
        let frozen = (*original).clone();
        let mut inserted = original.clone();
        assert_eq!(inserted.insert(1, SortedSet::from([11])), Some(SortedSet::from([10])));
        assert_eq!(inserted.insert(3, SortedSet::from([30])), None);
        assert_eq!(*original, frozen);

        let mut nested = original.clone();
        nested.entry(1).or_default().insert(12);
        nested.get_mut(&2).unwrap().insert(22);
        assert_eq!(nested.get(&1), Some(&SortedSet::from([10, 12])));
        assert_eq!(nested.get(&2), Some(&SortedSet::from([20, 22])));
        assert_eq!(*original, frozen);

        let mut iterated = original.clone();
        for (_, values) in &mut iterated { values.insert(99); }
        assert_eq!(iterated.get(&1), Some(&SortedSet::from([10, 99])));
        assert_eq!(iterated.get(&2), Some(&SortedSet::from([20, 99])));
        assert_eq!(*original, frozen);

        let mut consumed: SortedMap<_, _> = original.clone().into_iter().collect();
        consumed.entry(1).or_default().insert(100);
        assert_eq!(*original, frozen);
        let unique: CowFacts<SortedMap<_, _>> = consumed.clone().into();
        assert_eq!(unique.into_iter().collect::<SortedMap<_, _>>(), consumed);
        let independent: CowFacts<SortedMap<_, _>> = frozen.clone().into();
        assert_eq!(original, independent);
        assert_ne!(original, inserted);

        let set: CowFacts<SortedSet<u32>> = SortedSet::from([1, 2]).into();
        let mut changed = set.clone();
        assert!(!changed.insert(1));
        assert!(changed.insert(3));
        assert!(changed.remove(&2));
        assert_eq!(*set, SortedSet::from([1, 2]));
        assert_eq!(*changed, SortedSet::from([1, 3]));
        assert_eq!(set.clone().into_iter().collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(changed.into_iter().collect::<Vec<_>>(), vec![1, 3]);
    }

    pub fn generation_meets_preserve_intersections_epochs_and_snapshot_isolation() {
        type GenerationFacts = CowFacts<SortedMap<ValueId, SortedSet<OwnerGeneration>>>;
        let first = OwnerGeneration { identity: SOURCE, epoch: 1 };
        let later = OwnerGeneration { identity: SOURCE, epoch: 2 };
        let other = OwnerGeneration { identity: DESTINATION, epoch: 1 };
        let joined = OwnerGeneration { identity: SOURCE, epoch: JOINED_OWNER_GENERATION };
        let left: GenerationFacts = SortedMap::from([
            (COUNT, SortedSet::from([first, later])),
            (CAPACITY, SortedSet::from([other])),
            (ZERO, SortedSet::from([first])),
        ]).into();
        let frozen = (*left).clone();
        let right: GenerationFacts = SortedMap::from([
            (COUNT, SortedSet::from([later, other])),
            (ZERO, SortedSet::from([first])),
            (LENGTH, SortedSet::from([other])),
        ]).into();
        let met = meet_generation_facts(&left, &right);
        assert_eq!(*met, SortedMap::from([
            (COUNT, SortedSet::from([later])), (ZERO, SortedSet::from([first])),
        ]));
        // An unchanged intersection may share its storage, but later nested
        // writes must not alter any predecessor snapshot.
        let mut superset = left.clone();
        superset.insert(LENGTH, SortedSet::from([other]));
        let mut unchanged = meet_generation_facts(&left, &superset);
        assert_eq!(*unchanged, frozen);
        unchanged.entry(COUNT).or_default().insert(other);
        assert_eq!(*left, frozen);
        assert_eq!(superset.get(&COUNT), frozen.get(&COUNT));

        let identities = SortedSet::from([SOURCE]);
        let rebased = rebase_joined_generations(&left, &identities);
        assert_eq!(*rebased, SortedMap::from([
            (COUNT, SortedSet::from([joined])),
            (CAPACITY, SortedSet::from([other])),
            (ZERO, SortedSet::from([joined])),
        ]));
        assert_eq!(*left, frozen);
        assert_eq!(*rebase_joined_generations(&left, &SortedSet::new()), frozen);
        // Ordinary bounds/guards cannot combine a concrete epoch with top.
        let stale = meet_generation_facts(&left, &rebased);
        assert_eq!(*stale, SortedMap::from([(CAPACITY, SortedSet::from([other]))]));
        // Only the existing fresh-header rule rebases BOTH predecessors.
        let fresh = meet_generation_facts(&rebased,
            &rebase_joined_generations(&right, &identities));
        assert_eq!(*fresh, SortedMap::from([
            (COUNT, SortedSet::from([joined])), (ZERO, SortedSet::from([joined])),
        ]));
    }

    fn with_checker(
        pointer_element: bool,
        test: impl FnOnce(&mut MemoryChecker<'_>, crate::layout::LayoutId),
    ) {
        with_checker_capacity(pointer_element, 1, test);
    }

    fn with_checker_capacity(
        pointer_element: bool,
        capacity: u64,
        test: impl FnOnce(&mut MemoryChecker<'_>, crate::layout::LayoutId),
    ) {
        let mut layouts = LayoutInterner::new();
        let element = layouts
            .intern_scalar(ScalarKind::U32)
            .expect("scalar layout");
        let list = if pointer_element {
            let pointee = layouts
                .intern_scalar(ScalarKind::Int)
                .expect("pointee layout");
            let pointer = layouts
                .intern_runtime_pointer_value_slot(pointee, PointerSlot::Owned)
                .expect("pointer slot layout");
            layouts
                .intern_packed_list(pointer, RcHeader::Required)
                .expect("pointer list layout")
        } else {
            layouts
                .intern_packed_list(element, RcHeader::Required)
                .expect("scalar list layout")
        };
        let function = Function {
            id: crate::canonical_safewir::FunctionId::new(0),
            signature: crate::canonical_safewir::FunctionSignatureId::new(0),
            entry: crate::canonical_safewir::BlockId::new(0),
            value_types: [
                (COUNT, ValueType::Size),
                (CAPACITY, ValueType::Size),
                (START, ValueType::Size),
                (LENGTH, ValueType::Size),
                (LONG_LENGTH, ValueType::Size),
                (END, ValueType::Size),
                (SOURCE, ValueType::Ref(list)),
                (DESTINATION, ValueType::Ref(list)),
                (CHILD, ValueType::Ref(list)),
                (LOADED, ValueType::Size),
                (CONDITION, ValueType::Bool),
            ]
            .into_iter()
            .collect(),
            blocks: vec![BasicBlock {
                id: crate::canonical_safewir::BlockId::new(0),
                parameters: Vec::new(),
                operations: Vec::new(),
                terminator: crate::canonical_safewir::Terminator::Return { values: Vec::new() },
            }],
        };
        let signature = FunctionSignature {
            id: function.signature,
            parameters: Vec::new(),
            results: Vec::new(),
            contract: CallableContract {
                parameters: Vec::new(),
                results: Vec::new(),
                parameter_effects: BTreeMap::new(),
            },
        };
        let closure_layouts = BTreeMap::new();
        let mut checker = MemoryChecker::new(&function, &layouts, &signature, &closure_layouts)
            .expect("checker construction");
        for (destination, value) in [(COUNT, 1), (CAPACITY, capacity), (START, 0), (LENGTH, 1), (LONG_LENGTH, 2)] {
            checker
                .check(&Operation::ConstSize { destination, value })
                .expect("constant is accepted");
        }
        let LayoutSize::Dynamic { base, stride } = layouts.get(list).expect("list descriptor").size() else {
            panic!("range test requires a dynamic layout");
        };
        for object in [SOURCE, DESTINATION] {
            checker
                .check(&Operation::Alloc {
                    destination: object,
                    layout: list,
                    extent: Extent::Counted {
                        header_bytes: base,
                        count: COUNT,
                        capacity: CAPACITY,
                        stride_bytes: stride,
                    },
                    class: AllocationClass::ReferenceCounted,
                })
                .expect("allocation is accepted");
            if !pointer_element {
                checker.state.initialized_scalar_slots
                    .insert((object, SlotIdentity::Constant(0)));
            }
        }
        test(&mut checker, list);
    }

    fn range_place(object: ValueId, list: crate::layout::LayoutId, length: ValueId, access: AccessMode) -> Place {
        Place::projected(
            object,
            Projection::Slice { start: START, length },
            ValueType::Ref(list),
            access,
        )
    }

    pub fn bounds_guards_name_the_current_owner_generation() {
        let guarded_read = |checker: &mut MemoryChecker<'_>, object, list| {
            checker.check(&Operation::Compare {
                destination: CONDITION,
                operator: ComparisonOperator::LessUnsigned,
                left: START,
                right: COUNT,
            }).expect("checked comparison");
            checker.check(&Operation::Guard { condition: CONDITION, kind: GuardKind::Bounds })
                .expect("checked bounds guard");
            checker.check_place(&Place::projected(
                object,
                Projection::Element { index: START },
                ValueType::Size,
                AccessMode::Read,
            )).map(|_| ()).expect("guard is bound to this live owner");
            let _ = list;
        };

        with_checker(false, |checker, _list| {
            checker.check_place(&Place::projected(
                SOURCE,
                Projection::Element { index: START },
                ValueType::Size,
                AccessMode::Read,
            )).expect("literal index and literal count are self-authenticating arithmetic facts");
        });

        with_checker_capacity(false, 2, |checker, list| {
            checker.state.constants.remove(&START);
            guarded_read(checker, SOURCE, list);
            checker.check(&Operation::Compare {
                destination: CONDITION,
                operator: ComparisonOperator::LessUnsigned,
                left: START,
                right: CAPACITY,
            }).expect("checked write comparison");
            checker.check(&Operation::Guard { condition: CONDITION, kind: GuardKind::Bounds })
                .expect("checked write bounds guard");
            checker.check(&Operation::Store {
                place: Place::projected(
                    SOURCE,
                    Projection::Element { index: START },
                    ValueType::Size,
                    AccessMode::Write,
                ),
                value: COUNT,
                transfer: StoreTransfer::CopyScalar,
            }).expect("a scalar element store keeps the collection extent");
            checker.check_place(&Place::projected(
                SOURCE, Projection::Element { index: START }, ValueType::Size, AccessMode::Read,
            )).expect("an element store does not stale a current bounds guard");
            assert_eq!(checker.current_generation(SOURCE).unwrap().epoch, 1,
                "the store still advances ownership state");
            assert!(checker.state.guard_generations
                .get(&(ComparisonOperator::LessUnsigned, START, COUNT))
                .is_some_and(|facts| facts.contains(&checker.current_generation(SOURCE).unwrap())),
                "only the current allocation's extent guard is rebased");
            checker.state.initialized_scalar_slots.insert((SOURCE, SlotIdentity::Constant(1)));
            checker.check(&Operation::UpdateCount { object: SOURCE, count: LONG_LENGTH })
                .expect("initialized scalar prefix may grow");
            assert!(matches!(
                checker.check_place(&Place::projected(
                    SOURCE, Projection::Element { index: START }, ValueType::Size, AccessMode::Read,
                )),
                Err(InitialCheckError::MissingBoundsGuard { .. })
            ), "a pre-mutation guard cannot prove the new owner generation");
            checker.check(&Operation::Compare {
                destination: CONDITION,
                operator: ComparisonOperator::LessUnsigned,
                left: START,
                right: LONG_LENGTH,
            }).unwrap();
            checker.check(&Operation::Guard { condition: CONDITION, kind: GuardKind::Bounds }).unwrap();
            checker.check_place(&Place::projected(
                SOURCE, Projection::Element { index: START }, ValueType::Size, AccessMode::Read,
            )).expect("fresh guard admits the current generation");
        });

        with_checker_capacity(false, 2, |checker, list| {
            checker.state.constants.remove(&START);
            guarded_read(checker, SOURCE, list);
            checker.state.objects.remove(&DESTINATION);
            checker.check(&Operation::MoveValue { destination: DESTINATION, source: SOURCE })
                .expect("sole owner moves");
            checker.check_place(&Place::projected(
                DESTINATION, Projection::Element { index: START }, ValueType::Size, AccessMode::Read,
            )).expect("a sole move preserves the allocation's current extent proof");
            assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Consumed,
                "moving the root still consumes the source token");
        });

        with_checker_capacity(false, 2, |checker, list| {
            checker.state.constants.remove(&START);
            guarded_read(checker, SOURCE, list);
            checker.clone_ref(DESTINATION, SOURCE).expect("shared alias");
            let callee = FunctionSignature {
                id: checker.signature.id,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: CallableContract {
                    parameters: vec![ParameterContract {
                        value_type: ValueType::Ref(list), ownership: ParameterOwnership::Shared,
                    }],
                    results: Vec::new(),
                    parameter_effects: BTreeMap::new(),
                },
            };
            checker.check_call(
                &callee,
                &[CallArgument::Move(SOURCE)],
                &callee.contract.parameters,
                &[],
                &[],
            ).expect("checked call consumes one shared token");
            assert!(matches!(
                checker.check_place(&Place::projected(
                    DESTINATION, Projection::Element { index: START }, ValueType::Size, AccessMode::Read,
                )),
                Err(InitialCheckError::MissingBoundsGuard { .. })
            ));
        });

        with_checker_capacity(false, 2, |checker, list| {
            checker.state.constants.remove(&START);
            checker.check(&Operation::Compare {
                destination: CONDITION,
                operator: ComparisonOperator::LessUnsigned,
                left: START,
                right: COUNT,
            }).expect("dynamic predecessor comparison");
            checker.check(&Operation::Guard { condition: CONDITION, kind: GuardKind::Bounds })
                .expect("dynamic predecessor bounds guard");
            let stable = checker.snapshot();
            checker.invalidate_owner_generation(SOURCE).expect("test mutation");
            let changed = checker.snapshot();
            let mut target = unique_phi_target(ValueId::new(99), list);
            target.parameters.clear();
            let joined = checker.meet(&target, &stable, &changed)
                .expect("a same-owner conditional mutation joins with fresh bounds evidence");
            assert_eq!(joined.objects[&SOURCE].identity, SOURCE);
            assert_eq!(joined.objects[&SOURCE].ownership, Ownership::Unique);
            assert_eq!(joined.objects[&SOURCE].owner_generation, JOINED_OWNER_GENERATION,
                "the join uses the stable abstract epoch rather than a predecessor epoch");
            checker.restore(joined);
            assert!(matches!(
                checker.check_place(&Place::projected(
                    SOURCE, Projection::Element { index: START }, ValueType::Size, AccessMode::Read,
                )),
                Err(InitialCheckError::MissingBoundsGuard { .. })
            ), "a predecessor guard cannot prove the joined owner generation");
            checker.check(&Operation::Compare {
                destination: CONDITION,
                operator: ComparisonOperator::LessUnsigned,
                left: START,
                right: CAPACITY,
            }).expect("checked comparison");
            checker.check(&Operation::Guard { condition: CONDITION, kind: GuardKind::Bounds })
                .expect("fresh bounds guard");
            checker.check_place(&Place::projected(
                SOURCE, Projection::Element { index: START }, ValueType::Size, AccessMode::Write,
            )).expect("a fresh guard may authenticate an exactly carried capacity lane");
        });
    }

    fn unique_phi_target(value: ValueId, layout: crate::layout::LayoutId) -> BasicBlock {
        BasicBlock {
            id: crate::canonical_safewir::BlockId::new(1),
            parameters: vec![crate::canonical_safewir::BlockParameter {
                value,
                value_type: ValueType::Ref(layout),
                ownership: Some(Ownership::Unique),
            }],
            operations: Vec::new(),
            terminator: crate::canonical_safewir::Terminator::Return { values: Vec::new() },
        }
    }

    pub fn same_identity_ownership_meet_rejects_identity_consumption_and_alias_changes() {
        for case in 0..4 {
            with_checker(false, |checker, list| {
                let mut unique = checker.snapshot();
                let mut shared = unique.clone();
                shared.objects.get_mut(&SOURCE).unwrap().ownership = Ownership::Shared;
                match case {
                    0 => shared.objects.get_mut(&SOURCE).unwrap().identity = DESTINATION,
                    1 => shared.objects.get_mut(&SOURCE).unwrap().ownership = Ownership::Consumed,
                    2 => {
                        let alias = shared.objects[&SOURCE].clone();
                        unique.stored_objects.insert((DESTINATION, SlotIdentity::Constant(0)), alias.clone());
                        shared.stored_objects.insert((DESTINATION, SlotIdentity::Constant(0)), alias);
                    }
                    3 => unique.objects.get_mut(&SOURCE).unwrap().allocation_class = Some(AllocationClass::Bump),
                    _ => unreachable!(),
                }
                let mut target = unique_phi_target(ValueId::new(99), list);
                target.parameters.clear();
                assert!(matches!(checker.meet(&target, &unique, &shared), Err(InitialCheckError::IncompatibleMemoryJoin { object: SOURCE, .. })));
            });
        }
    }

    pub fn owned_prefix_summary_is_invalidated_by_move() {
        with_checker(true, |checker, list| {
            checker.state.objects.get_mut(&SOURCE).unwrap().initialized_owned_slots = true;
            checker.state.range_states.retain(|identity, _| identity.object != SOURCE);
            checker.consolidate_pointer_prefix(SOURCE, COUNT).expect("authenticated current prefix");
            checker.update_count(DESTINATION, START).expect("empty destination");
            let LayoutKind::PackedList { element, .. } = checker.layouts.get(list).unwrap().kind() else { unreachable!() };
            let LayoutKind::RuntimePointerValueSlot { pointee, .. } = checker.layouts.get(*element).unwrap().kind() else { unreachable!() };
            let source = range_place(SOURCE, *pointee, COUNT, AccessMode::Read);
            let destination = range_place(DESTINATION, *pointee, COUNT, AccessMode::Write);
            checker.move_range(&destination, &source, COUNT).expect("move initialized prefix");
            assert!(checker.require_complete_value_initialized(SOURCE, "moved source").is_err());
            checker.update_count(DESTINATION, COUNT).expect("moved destination is initialized");
            checker.require_complete_value_initialized(DESTINATION, "moved destination").expect("complete destination");
        });
    }


    pub fn move_value_rejects_shared_source() {
        with_checker(false, |checker, _list| {
            checker.state.objects.remove(&DESTINATION);
            checker.state.objects.get_mut(&SOURCE).expect("source").ownership = Ownership::Shared;

            assert!(matches!(
                checker.move_value(DESTINATION, SOURCE),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    pub fn clone_ref_tracks_two_affine_shared_tokens() {
        with_checker(false, |checker, _list| {
            checker.state.objects.remove(&DESTINATION);
            checker.clone_ref(DESTINATION, SOURCE).expect("clone reference");

            let source = &checker.state.objects[&SOURCE];
            let destination = &checker.state.objects[&DESTINATION];
            assert_eq!(source.identity, destination.identity);
            assert_eq!(source.ownership, Ownership::Shared);
            assert_eq!(destination.ownership, Ownership::Shared);

            checker.drop_ref(SOURCE).expect("drop source token");
            checker.drop_ref(DESTINATION).expect("drop destination token");
            assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Consumed);
            assert_eq!(checker.state.objects[&DESTINATION].ownership, Ownership::Consumed);
        });
    }



    pub fn constant_prefix_publication_rejects_gaps_and_missing_children() {
        for (next, store_child) in [(3, true), (2, false)] {
            with_pointer_slot_checker(|checker, list, pointee| {
                checker.state.constants.insert(CAPACITY, 3);
                checker.state.constants.insert(ALIAS_LENGTH, next);
                checker.check(&Operation::Compare {
                    destination: CONDITION, operator: ComparisonOperator::LessUnsigned,
                    left: COUNT, right: CAPACITY,
                }).expect("next child fits spare capacity");
                checker.check(&Operation::Guard { condition: CONDITION, kind: GuardKind::Bounds })
                    .expect("guard the next child store");
                let element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
                checker.state.range_states.clear();
                checker.state.range_states.insert(
                    RangeIdentity { object: SOURCE, start: START, length: COUNT },
                    RangeState { element, ownership: Ownership::Unique,
                        allocation_class: Some(AllocationClass::ReferenceCounted),
                        initialized: true, zero_width: false, zero_start: true },
                );
                if store_child {
                    checker.check(&Operation::Store {
                        place: Place::projected(SOURCE, Projection::Element { index: COUNT },
                            ValueType::Ref(pointee), AccessMode::Write),
                        value: CHILD, transfer: StoreTransfer::MoveUnique,
                    }).expect("store only the next child");
                }
                assert!(checker.check(&Operation::UpdateCount {
                    object: SOURCE, count: ALIAS_LENGTH,
                }).is_err(), "every newly published child must be initialized");
            });
        }
    }

    pub fn shared_static_storage_clones_and_drops_without_mutation_authority() {
        with_checker(false, |checker, _list| {
            checker.state.objects.remove(&DESTINATION);
            let source = checker.state.objects.get_mut(&SOURCE).expect("source");
            source.ownership = Ownership::Shared;
            source.allocation_class = None;
            source.shared_immutable = false;
            checker.state.static_data_objects.insert(SOURCE);

            checker.clone_ref(DESTINATION, SOURCE).expect("static clone is a no-op token split");
            assert!(checker.state.static_data_objects.contains(&DESTINATION));
            assert!(matches!(
                checker.reclaim(SOURCE),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
            assert!(matches!(
                checker.update_count(SOURCE, LENGTH),
                Err(InitialCheckError::WritableStaticData { object: SOURCE, .. })
            ));
            checker.drop_ref(SOURCE).expect("static drop is a no-op token consume");
            checker.drop_ref(DESTINATION).expect("static clone drop is a no-op token consume");
        });
    }

    pub fn release_rejects_shared_value_without_refcount_authority() {
        with_checker(false, |checker, _list| {
            let source = checker.state.objects.get_mut(&SOURCE).expect("source");
            source.ownership = Ownership::Shared;
            source.allocation_class = None;

            assert!(matches!(
                checker.release(SOURCE),
                Err(InitialCheckError::InvalidAllocationClass { object: SOURCE, .. })
            ));
        });
    }

    pub fn return_contract_accepts_only_sole_unique_fresh_object() {
        with_checker(false, |checker, list| {
            let LayoutSize::Dynamic { base, stride } = checker.descriptor(list).expect("list layout").size() else { panic!("dynamic list") };
            let contract = ResultContract::Fresh { layout: list, class: AllocationClass::ReferenceCounted, extent: ReturnedExtent::Counted { header_bytes: base, stride_bytes: stride } };
            assert!(checker.check_return(&[SOURCE], std::slice::from_ref(&contract)).is_ok());

            let alias = checker.state.objects[&SOURCE].clone();
            checker.state.objects.insert(DESTINATION, alias);
            assert!(matches!(
                checker.check_return(&[SOURCE], &[contract]),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    pub fn return_contract_rejects_fresh_allocation_class_mismatch() {
        with_checker(false, |checker, list| {
            assert!(matches!(
                checker.check_return(
                    &[SOURCE],
                    &[ResultContract::Fresh {
                        layout: list,
                        class: AllocationClass::Bump,
                        extent: ReturnedExtent::Counted { header_bytes: 4, stride_bytes: 4 },
                    }],
                ),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    pub fn affine_boundary_rejects_duplicate_owned_return_lanes() {
        with_checker(false, |checker, list| {
            checker.state.objects.remove(&DESTINATION);
            let LayoutSize::Dynamic { base, stride } = checker.descriptor(list).unwrap().size() else {
                panic!("dynamic list");
            };
            let fresh = ResultContract::Fresh {
                layout: list,
                class: AllocationClass::ReferenceCounted,
                extent: ReturnedExtent::Counted { header_bytes: base, stride_bytes: stride },
            };
            assert!(matches!(
                checker.check_return(&[SOURCE, SOURCE], &[fresh.clone(), fresh]),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
            checker.clone_ref(DESTINATION, SOURCE).unwrap();
            let shared = ResultContract::Shared { layout: list };
            assert!(matches!(
                checker.check_return(&[SOURCE, SOURCE], &[shared.clone(), shared.clone()]),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
            checker.check_return(&[SOURCE, DESTINATION], &[shared.clone(), shared]).unwrap();
        });
    }

    pub fn affine_boundary_tail_call_requires_no_untransferred_tokens() {
        with_checker(false, |checker, _list| {
            use crate::canonical_safewir::{TailSiteId, Terminator};
            let function = checker.function;
            let signature = FunctionSignature {
                id: function.signature,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: CallableContract {
                    parameters: Vec::new(), results: Vec::new(), parameter_effects: BTreeMap::new(),
                },
            };
            let terminator = Terminator::TailCall {
                site: TailSiteId::from_digest([0; 32]),
                callee: function.id,
                arguments: Vec::new(),
            };
            let signatures = BTreeMap::from([(signature.id, &signature)]);
            let functions: super::super::FunctionIndex = [(function.id, function)].into_iter().collect();
            assert!(matches!(
                super::super::check_terminator(
                    function, function.entry, &terminator, &signature, &signatures, &functions,
                    &BTreeMap::new(), checker,
                ),
                Err(InitialCheckError::UnconsumedReference { .. })
            ));
            checker.drop_ref(SOURCE).unwrap();
            checker.drop_ref(DESTINATION).unwrap();
            super::super::check_terminator(
                function, function.entry, &terminator, &signature, &signatures, &functions,
                &BTreeMap::new(), checker,
            ).unwrap();
        });
    }



    fn with_nullable_checker(test: impl FnOnce(&mut MemoryChecker<'_>, crate::layout::LayoutId)) {
        with_fixed_pointer_checker(PointerSlot::OwnedNullable, test);
    }

    fn with_fixed_pointer_checker(semantics: PointerSlot, test: impl FnOnce(&mut MemoryChecker<'_>, crate::layout::LayoutId)) {
        let mut layouts = LayoutInterner::new();
        let child = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let slot = layouts.intern_runtime_pointer_value_slot(child, semantics).unwrap();
        let record = layouts.intern_packed_record(vec![slot]).unwrap();
        let function = Function {
            id: crate::canonical_safewir::FunctionId::new(0),
            signature: crate::canonical_safewir::FunctionSignatureId::new(0),
            entry: crate::canonical_safewir::BlockId::new(0),
            value_types: BTreeMap::from([
                (SOURCE, ValueType::Ref(record)), (DESTINATION, ValueType::Ref(record)),
                (CHILD, ValueType::Ref(child)), (LOADED, ValueType::Ref(child)),
                (ValueId::new(11), ValueType::Ref(child)),
            ]),
            blocks: vec![BasicBlock {
                id: crate::canonical_safewir::BlockId::new(0), parameters: vec![], operations: vec![],
                terminator: crate::canonical_safewir::Terminator::Return { values: vec![] },
            }],
        };
        let signature = FunctionSignature {
            id: function.signature, parameters: vec![], results: vec![],
            contract: CallableContract { parameters: vec![], results: vec![], parameter_effects: BTreeMap::new() },
        };
        let closures = BTreeMap::new();
        let mut checker = MemoryChecker::new(&function, &layouts, &signature, &closures).unwrap();
        for (destination, layout) in [(SOURCE, record), (CHILD, child)] {
            let LayoutSize::Fixed(bytes) = layouts.get(layout).unwrap().size() else { panic!("fixed fixture"); };
            checker.check(&Operation::Alloc {
                destination, layout, extent: Extent::FixedBytes(u64::from(bytes)),
                class: AllocationClass::ReferenceCounted,
            }).unwrap();
        }
        test(&mut checker, child);
    }



    fn nullable_field(object: ValueId, child: crate::layout::LayoutId, access: AccessMode) -> Place {
        Place::projected(object, Projection::Field { index: 0 }, ValueType::Ref(child), access)
    }

    pub fn nullable_owned_checker_requires_initialization_and_checked_presence() {
        with_nullable_checker(|checker, child| {
            let read = nullable_field(SOURCE, child, AccessMode::Read);
            assert!(checker.check(&Operation::Load { destination: LOADED, place: read.clone(), transfer: LoadTransfer::ClonePresent }).is_err());
            assert!(checker.check(&Operation::DropRef { object: SOURCE }).is_err());
            let init = Operation::InitNullableOwned { place: nullable_field(SOURCE, child, AccessMode::Write) };
            assert_eq!(checker.check(&init), Ok(true));
            assert!(checker.check(&init).is_err());
            for transfer in [LoadTransfer::CopyScalar, LoadTransfer::CloneShared, LoadTransfer::MoveUnique] {
                assert!(checker.check(&Operation::Load { destination: LOADED, place: read.clone(), transfer }).is_err());
            }
            // A known absent load is still a well-defined trapping operation.
            assert_eq!(checker.check(&Operation::Load { destination: LOADED, place: read, transfer: LoadTransfer::ClonePresent }), Ok(true));
            assert_eq!(checker.state.objects[&LOADED].ownership, Ownership::Shared);
        });
        with_nullable_checker(|checker, child| {
            let read = nullable_field(SOURCE, child, AccessMode::Read);
            assert!(checker.check(&Operation::Load { destination: LOADED, place: read.clone(), transfer: LoadTransfer::TakeNullableUnique }).is_err());
            checker.check(&Operation::Store {
                place: nullable_field(SOURCE, child, AccessMode::Write),
                value: CHILD,
                transfer: StoreTransfer::MoveUnique,
            }).unwrap();
            checker.check(&Operation::Load { destination: LOADED, place: read, transfer: LoadTransfer::TakeNullableUnique }).unwrap();
            assert_eq!(checker.state.objects[&LOADED].ownership, Ownership::Unique);
            assert!(checker.state.nullable_absent_slots.contains(&(SOURCE, SlotIdentity::Constant(0))));
        });
        with_nullable_checker(|checker, child| {
            checker.check(&Operation::Store {
                place: nullable_field(SOURCE, child, AccessMode::Write),
                value: CHILD,
                transfer: StoreTransfer::MoveUnique,
            }).unwrap();
            let read = nullable_field(SOURCE, child, AccessMode::Read);
            checker.check(&Operation::Load { destination: ValueId::new(11), place: read.clone(), transfer: LoadTransfer::ClonePresent }).unwrap();
            assert!(checker.check(&Operation::Load { destination: LOADED, place: read, transfer: LoadTransfer::TakeNullableUnique }).is_err());
        });
        with_nullable_checker(|checker, child| {
            checker.check(&Operation::Store {
                place: nullable_field(SOURCE, child, AccessMode::Write),
                value: CHILD,
                transfer: StoreTransfer::MoveUnique,
            }).unwrap();
            checker.check(&Operation::MoveValue { destination: DESTINATION, source: SOURCE }).unwrap();
            let read = nullable_field(DESTINATION, child, AccessMode::Read);
            checker.check(&Operation::Load { destination: LOADED, place: read.clone(), transfer: LoadTransfer::TakeNullableUnique }).unwrap();
            assert!(checker.check(&Operation::Load { destination: ValueId::new(11), place: read, transfer: LoadTransfer::TakeNullableUnique }).is_err());
        });
    }

    fn with_pointer_slot_checker(
        test: impl FnOnce(
            &mut MemoryChecker<'_>,
            crate::layout::LayoutId,
            crate::layout::LayoutId,
        ),
    ) {
        with_nominal_pointer_slot_checker( |checker, list, pointee, _nominal| {
            test(checker, list, pointee);
        });
    }

    fn with_nominal_pointer_slot_checker(
        test: impl FnOnce(
            &mut MemoryChecker<'_>,
            crate::layout::LayoutId,
            crate::layout::LayoutId,
            crate::layout::LayoutId,
        ),
    ) {
        with_nominal_pointer_slot_checker_values(1, 0, 1, test);
    }

    fn with_nominal_pointer_slot_checker_values(
        count: u64,
        start: u64,
        length: u64,
        test: impl FnOnce(
            &mut MemoryChecker<'_>,
            crate::layout::LayoutId,
            crate::layout::LayoutId,
            crate::layout::LayoutId,
        ),
    ) {
        let mut layouts = LayoutInterner::new();
        let pointee = layouts.intern_scalar(ScalarKind::Int).expect("pointee layout");
        let pointer = layouts
            .intern_runtime_pointer_value_slot(pointee, PointerSlot::Owned)
            .expect("owned pointer slot");
        let nominal = layouts
            .intern_opaque_nominal([0x4e; 32])
            .expect("nominal element layout");
        layouts.bind_nominal(nominal, pointer).expect("bind nominal element");
        let list = layouts
            .intern_packed_list(pointer, RcHeader::Required)
            .expect("pointer list layout");
        let function = Function {
            id: crate::canonical_safewir::FunctionId::new(0),
            signature: crate::canonical_safewir::FunctionSignatureId::new(0),
            entry: crate::canonical_safewir::BlockId::new(0),
            value_types: [
                (COUNT, ValueType::Size),
                (CAPACITY, ValueType::Size),
                (START, ValueType::Size),
                (LENGTH, ValueType::Size),
                (CHILD, ValueType::Ref(pointee)),
                (LOADED, ValueType::Ref(pointee)),
                (SOURCE, ValueType::Ref(list)),
                (DESTINATION, ValueType::Ref(list)),
                (ALIAS_START, ValueType::Size),
                (ALIAS_LENGTH, ValueType::Size),
                (ZERO, ValueType::Size),
                (CONDITION, ValueType::Bool),
            ]
            .into_iter()
            .collect(),
            blocks: vec![BasicBlock {
                id: crate::canonical_safewir::BlockId::new(0),
                parameters: Vec::new(),
                operations: Vec::new(),
                terminator: crate::canonical_safewir::Terminator::Return { values: Vec::new() },
            }],
        };
        let signature = FunctionSignature {
            id: function.signature,
            parameters: Vec::new(),
            results: Vec::new(),
            contract: CallableContract {
                parameters: Vec::new(),
                results: Vec::new(),
                parameter_effects: BTreeMap::new(),
            },
        };
        let closure_layouts = BTreeMap::new();
        let mut checker = MemoryChecker::new(&function, &layouts, &signature, &closure_layouts)
            .expect("checker construction");
        for (destination, value) in [
            (COUNT, count),
            (CAPACITY, count),
            (START, start),
            (LENGTH, length),
            (ALIAS_START, 0),
            (ALIAS_LENGTH, 1),
            (ZERO, 0),
        ] {
            checker
                .check(&Operation::ConstSize { destination, value })
                .expect("constant is accepted");
        }
        checker
            .check(&Operation::Compare {
                destination: CONDITION,
                operator: ComparisonOperator::LessUnsigned,
                left: START,
                right: COUNT,
            })
            .expect("comparison is accepted");
        checker
            .check(&Operation::Guard {
                condition: CONDITION,
                kind: GuardKind::Bounds,
            })
            .expect("bounds guard is accepted");
        checker
            .state
            .guards
            .insert((ComparisonOperator::LessUnsigned, START, CAPACITY));
        let LayoutSize::Dynamic { base, stride } = layouts.get(list).unwrap().size() else {
            panic!("pointer list must be dynamic");
        };
        checker
            .check(&Operation::Alloc {
                destination: SOURCE,
                layout: list,
                extent: Extent::Counted {
                    header_bytes: base,
                    count: COUNT,
                    capacity: CAPACITY,
                    stride_bytes: stride,
                },
                class: AllocationClass::ReferenceCounted,
            })
            .expect("list allocation is accepted");
        let LayoutSize::Fixed(bytes) = layouts.get(pointee).unwrap().size() else {
            panic!("pointee must be fixed");
        };
        checker
            .check(&Operation::Alloc {
                destination: CHILD,
                layout: pointee,
                extent: Extent::FixedBytes(u64::from(bytes)),
                class: AllocationClass::ReferenceCounted,
            })
            .expect("child allocation is accepted");
        checker
            .check(&Operation::Alloc {
                destination: DESTINATION,
                layout: list,
                extent: Extent::Counted {
                    header_bytes: base,
                    count: COUNT,
                    capacity: CAPACITY,
                    stride_bytes: stride,
                },
                class: AllocationClass::ReferenceCounted,
            })
            .expect("destination allocation is accepted");
        test(&mut checker, list, pointee, nominal);
    }

    pub fn scalar_move_range_is_bounds_checked_and_admitted() {
        with_checker(false, |checker, list| {
            let element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            let value_type = checker.value_type_for_layout(element).unwrap();
            let source = Place::projected(
                SOURCE,
                Projection::Element { index: START },
                value_type,
                AccessMode::Read,
            );
            checker
                .check(&Operation::ConstSize { destination: LOADED, value: 23 })
                .expect("scalar fixture value is accepted");
            checker
                .check(&Operation::Compare {
                    destination: CONDITION,
                    operator: ComparisonOperator::LessUnsigned,
                    left: START,
                    right: COUNT,
                })
                .expect("scalar fixture comparison is accepted");
            checker
                .check(&Operation::Guard {
                    condition: CONDITION,
                    kind: GuardKind::Bounds,
                })
                .expect("scalar fixture guard is accepted");
            checker
                .check(&Operation::Store {
                    place: Place { access: AccessMode::Write, ..source.clone() },
                    value: LOADED,
                    transfer: StoreTransfer::CopyScalar,
                })
                .expect("scalar source initialization is accepted");
            assert_eq!(
                checker.check(&Operation::MoveRange {
                    destination: range_place(DESTINATION, list, LENGTH, AccessMode::Write),
                    source: range_place(SOURCE, list, LENGTH, AccessMode::Read),
                    count: LENGTH,
                }),
                Ok(true)
            );
            let destination = Place::projected(
                DESTINATION,
                Projection::Element { index: START },
                value_type,
                AccessMode::Read,
            );
            assert!(!checker.checked_scalar_slot_initialized(&source).unwrap());
            assert!(checker.checked_scalar_slot_initialized(&destination).unwrap());
            assert!(matches!(
                checker.check(&Operation::Load {
                    destination: LOADED,
                    place: source,
                    transfer: LoadTransfer::CopyScalar,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "Load(uninitialized scalar)",
                    ..
                })
            ));
        });
    }

    pub fn scalar_clone_range_is_ownership_safe_and_admitted() {
        with_checker(false, |checker, list| {
            let element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            let value_type = checker.value_type_for_layout(element).unwrap();
            checker
                .check(&Operation::ConstSize { destination: LOADED, value: 23 })
                .expect("scalar fixture value is accepted");
            checker
                .check(&Operation::Compare {
                    destination: CONDITION,
                    operator: ComparisonOperator::LessUnsigned,
                    left: START,
                    right: COUNT,
                })
                .expect("scalar fixture comparison is accepted");
            checker
                .check(&Operation::Guard {
                    condition: CONDITION,
                    kind: GuardKind::Bounds,
                })
                .expect("scalar fixture guard is accepted");
            checker
                .check(&Operation::Store {
                    place: Place::projected(
                        SOURCE,
                        Projection::Element { index: START },
                        value_type,
                        AccessMode::Write,
                    ),
                    value: LOADED,
                    transfer: StoreTransfer::CopyScalar,
                })
                .expect("scalar source initialization is accepted");
            assert_eq!(
                checker.check(&Operation::CloneRange {
                    destination: range_place(DESTINATION, list, LENGTH, AccessMode::Write),
                    source: range_place(SOURCE, list, LENGTH, AccessMode::Read),
                    count: LENGTH,
                }),
                Ok(true)
            );
        });
    }


    pub fn range_transfer_rejects_a_slice_past_logical_length() {
        with_checker(false, |checker, list| {
            assert!(matches!(
                checker.check(&Operation::CloneRange {
                    destination: range_place(DESTINATION, list, LONG_LENGTH, AccessMode::Write),
                    source: range_place(SOURCE, list, LONG_LENGTH, AccessMode::Read),
                    count: LONG_LENGTH,
                }),
                Err(InitialCheckError::MissingBoundsGuard { .. })
            ));
        });
    }

    pub fn pointer_bearing_ranges_fail_closed_without_element_transfer_state() {
        with_checker(true, |checker, list| {
            let result = checker.check(&Operation::CloneRange {
                destination: range_place(DESTINATION, list, LENGTH, AccessMode::Write),
                source: range_place(SOURCE, list, LENGTH, AccessMode::Read),
                count: LENGTH,
            });
            // A pointer-bearing clone needs an initialized source prefix
            // whose elements it can retain; without one it fails closed.
            assert!(matches!(
                result,
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "CloneRange(uninitialized-source)",
                    ..
                })
            ));
        });
    }


    pub fn complete_managed_abi_subrange_admits_bounded_clone() {
        with_nominal_pointer_slot_checker_values(3, 1, 1, |checker, list, _pointee, _nominal| {
            let element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            // Model an authenticated complete ABI source and an empty
            // destination range to isolate bounded subrange admission here.
            // The source-level Wasm witness checks actual retain behavior.
            let source = checker.state.objects.get_mut(&SOURCE).unwrap();
            source.ownership = Ownership::Shared;
            source.allocation_class = None;
            source.shared_immutable = true;
            source.initialized_owned_slots = true;
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: ZERO, length: ZERO },
                RangeState {
                    element,
                    ownership: Ownership::Unique,
                    allocation_class: None,
                    initialized: true,
                    zero_width: true,
                    zero_start: true,
                },
            );
            let destination = Place::projected(
                DESTINATION,
                Projection::Slice { start: ZERO, length: LENGTH },
                ValueType::Ref(list),
                AccessMode::Write,
            );
            let source = Place::projected(
                SOURCE,
                Projection::Slice { start: START, length: LENGTH },
                ValueType::Ref(list),
                AccessMode::Read,
            );
            assert_eq!(checker.check(&Operation::CloneRange { destination, source, count: LENGTH }), Ok(true));
        });
    }


    pub fn dynamic_pointer_clone_range_shares_initialized_elements() {
        with_pointer_slot_checker(|checker, list, pointee| {
            let pointer_element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: START, length: ZERO },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: None, initialized: true, zero_width: true, zero_start: true },
            );
            checker.state.constants.remove(&COUNT);
            checker.state.guards.insert((ComparisonOperator::LessEqualUnsigned, COUNT, CAPACITY));
            checker.state.guard_generations.insert(
                (ComparisonOperator::LessEqualUnsigned, COUNT, CAPACITY),
                [checker.current_generation(SOURCE).unwrap(), checker.current_generation(DESTINATION).unwrap()]
                    .into_iter().collect(),
            );
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: COUNT },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            checker.check(&Operation::Compare {
                destination: CONDITION,
                operator: ComparisonOperator::LessUnsigned,
                left: START,
                right: COUNT,
            }).expect("dynamic source/destination bounds comparison");
            checker.check(&Operation::Guard { condition: CONDITION, kind: GuardKind::Bounds })
                .expect("dynamic source/destination bounds guard");
            assert_eq!(
                checker.check(&Operation::CloneRange {
                    destination: pointer_range_place(DESTINATION, list, COUNT, AccessMode::Write),
                    source: pointer_range_place(SOURCE, list, COUNT, AccessMode::Read),
                    count: COUNT,
                }),
                Ok(true)
            );
            let _ = pointee;
            // Both prefixes own their elements jointly from here on.
            assert_eq!(
                checker.state.range_states[&RangeIdentity { object: DESTINATION, start: START, length: COUNT }],
                RangeState { element: pointer_element, ownership: Ownership::Shared, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true }
            );
            assert_eq!(
                checker.state.range_states[&RangeIdentity { object: SOURCE, start: START, length: COUNT }],
                RangeState { element: pointer_element, ownership: Ownership::Shared, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true }
            );
            checker.check_place(&Place::projected(
                DESTINATION,
                Projection::Element { index: START },
                ValueType::Ref(pointee),
                AccessMode::Read,
            )).expect("clone preserves the destination's extent authentication");
            assert!(checker.check(&Operation::Load {
                destination: LOADED,
                place: Place::projected(
                    SOURCE,
                    Projection::Element { index: START },
                    ValueType::Ref(pointee),
                    AccessMode::Read,
                ),
                transfer: LoadTransfer::MoveUnique,
            }).is_err(), "clone must not leave a stale unique source-element fact");
        });
    }

    pub fn pointer_clone_range_rejects_partially_initialized_source() {
        with_pointer_slot_checker(|checker, list, _| {
            let element = checker
                .descriptor(list)
                .unwrap()
                .kind()
                .packed_element_layout()
                .unwrap();
            let Extent::Counted { header_bytes, stride_bytes, .. } =
                checker.state.objects[&SOURCE].extent.clone()
            else {
                panic!("pointer list extent is counted")
            };
            checker.state.constants.insert(COUNT, 2);
            checker.state.objects.get_mut(&SOURCE).unwrap().extent = Extent::Counted {
                header_bytes,
                count: COUNT,
                capacity: COUNT,
                stride_bytes,
            };
            checker.state.objects.get_mut(&DESTINATION).unwrap().extent = Extent::Counted {
                header_bytes,
                count: ZERO,
                capacity: COUNT,
                stride_bytes,
            };
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: LENGTH },
                RangeState {
                    element,
                    ownership: Ownership::Unique,
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    initialized: true,
                    zero_width: false,
                    zero_start: true,
                },
            );
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: START, length: ZERO },
                RangeState {
                    element,
                    ownership: Ownership::Unique,
                    allocation_class: None,
                    initialized: true,
                    zero_width: true,
                    zero_start: true,
                },
            );
            assert!(matches!(
                checker.check(&Operation::CloneRange {
                    destination: pointer_range_place(DESTINATION, list, COUNT, AccessMode::Write),
                    source: pointer_range_place(SOURCE, list, COUNT, AccessMode::Read),
                    count: COUNT,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "CloneRange(uninitialized-source)",
                    ..
                })
            ));
        });
    }

    fn pointer_range_place(object: ValueId, pointee: crate::layout::LayoutId, count: ValueId, access: AccessMode) -> Place {
        Place::projected(
            object,
            Projection::Slice { start: START, length: count },
            ValueType::Ref(pointee),
            access,
        )
    }

    pub fn dynamic_pointer_move_range_transfers_initialized_ownership_once() {
        with_pointer_slot_checker(|checker, list, pointee| {
            let pointer_element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: START, length: ZERO },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: None, initialized: true, zero_width: true, zero_start: true },
            );
            checker.state.constants.remove(&COUNT);
            checker.state.guards.insert((ComparisonOperator::LessEqualUnsigned, COUNT, CAPACITY));
            checker.state.guard_generations.insert(
                (ComparisonOperator::LessEqualUnsigned, COUNT, CAPACITY),
                [checker.current_generation(SOURCE).unwrap(), checker.current_generation(DESTINATION).unwrap()]
                    .into_iter().collect(),
            );
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: COUNT },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            checker.check(&Operation::Compare {
                destination: CONDITION,
                operator: ComparisonOperator::LessUnsigned,
                left: START,
                right: COUNT,
            }).expect("dynamic source/destination bounds comparison");
            checker.check(&Operation::Guard { condition: CONDITION, kind: GuardKind::Bounds })
                .expect("dynamic source/destination bounds guard");
            assert_eq!(
                checker.check(&Operation::MoveRange {
                    destination: pointer_range_place(DESTINATION, pointee, COUNT, AccessMode::Write),
                    source: pointer_range_place(SOURCE, pointee, COUNT, AccessMode::Read),
                    count: COUNT,
                }),
                Ok(true)
            );
            assert_eq!(
                checker.state.range_states[&RangeIdentity { object: DESTINATION, start: START, length: COUNT }],
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true }
            );
            assert_eq!(
                checker.state.range_states[&RangeIdentity { object: SOURCE, start: START, length: COUNT }],
                RangeState { element: pointer_element, ownership: Ownership::Consumed, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: false, zero_width: false, zero_start: true }
            );
            checker.check_place(&Place::projected(
                DESTINATION,
                Projection::Element { index: START },
                ValueType::Ref(pointee),
                AccessMode::Read,
            )).expect("move preserves the destination's extent authentication");
            assert!(matches!(
                checker.check(&Operation::Load {
                    destination: LOADED,
                    place: Place::projected(
                        SOURCE,
                        Projection::Element { index: START },
                        ValueType::Ref(pointee),
                        AccessMode::Read,
                    ),
                    transfer: LoadTransfer::CloneShared,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "Load(uninitialized reference)",
                    ..
                })
            ), "move must not leave a stale initialized source-element fact");
        });
    }

    pub fn pointer_bearing_reclaim_requires_recursive_drop() {
        with_pointer_slot_checker(|checker, list, _pointee| {
            let element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: COUNT },
                RangeState { element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            assert!(matches!(
                checker.check(&Operation::Reclaim { object: SOURCE }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "Reclaim(pointer-bearing layout requires DropRef)",
                    ..
                })
            ));
        });
    }

    pub fn dynamic_pointer_prefix_cannot_hide_an_unaccounted_child_slot() {
        with_pointer_slot_checker(|checker, list, _pointee| {
            let element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.constants.remove(&COUNT);
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: COUNT },
                RangeState { element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            let child = checker.live_object(CHILD).unwrap().clone();
            checker.state.stored_objects.insert(
                (SOURCE, SlotIdentity::Dynamic(ALIAS_LENGTH)),
                child,
            );
            assert!(matches!(
                checker.check(&Operation::DropRef { object: SOURCE }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "DropRef(uninitialized owned slot)",
                    ..
                })
            ));
        });
    }

    pub fn pointer_move_range_rejects_source_reuse_after_consumption() {
        with_pointer_slot_checker(|checker, list, pointee| {
            let pointer_element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: COUNT },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: START, length: ZERO },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: None, initialized: true, zero_width: true, zero_start: true },
            );
            let operation = Operation::MoveRange {
                destination: pointer_range_place(DESTINATION, pointee, COUNT, AccessMode::Write),
                source: pointer_range_place(SOURCE, pointee, COUNT, AccessMode::Read),
                count: COUNT,
            };
            assert_eq!(checker.check(&operation), Ok(true));
            assert!(matches!(
                checker.check(&operation),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "MoveRange(uninitialized-source)",
                    ..
                })
            ));
        });
    }


    pub fn store_cannot_overwrite_a_stored_reference() {
        // B2 "replace": a slot that already holds a reference is never
        // overwritten in place; the old token would leak. Replacement is
        // a load (or drop) followed by a store into a fresh object.
        with_pointer_slot_checker(|checker, _list, pointee| {
            let place = Place::projected(
                SOURCE,
                Projection::Element { index: START },
                ValueType::Ref(pointee),
                AccessMode::Write,
            );
            assert_eq!(
                checker.check(&Operation::Store { place: place.clone(), value: CHILD, transfer: StoreTransfer::MoveUnique }),
                Ok(true)
            );
            // A shared clone of the stored element is a live reference the
            // store could otherwise accept; the slot is taken.
            assert_eq!(
                checker.check(&Operation::Load {
                    destination: LOADED,
                    place: Place::projected(SOURCE, Projection::Element { index: START }, ValueType::Ref(pointee), AccessMode::Read),
                    transfer: LoadTransfer::CloneShared,
                }),
                Ok(true)
            );
            assert!(matches!(
                checker.check(&Operation::Store { place, value: LOADED, transfer: StoreTransfer::CloneShared }),
                Err(InitialCheckError::UnsupportedMemoryOperation { operation: "Store(overwrite reference)", .. })
            ));
        });
    }

    pub fn retain_is_refused_in_v1() {
        // B1/B7: `Retain` is not a v1 rule; a second owner comes only from
        // `CloneRef`, which the backend maps. The checker refuses it before
        // the backend could see it.
        with_pointer_slot_checker(|checker, _list, _pointee| {
            assert!(matches!(
                checker.check(&Operation::Retain { object: CHILD }),
                Err(InitialCheckError::UnsupportedMemoryOperation { operation: "Retain", .. })
            ));
        });
    }

    pub fn byte_copy_requires_a_dominating_dynamic_end_guard() {
        let mut layouts = LayoutInterner::new();
        let byte = layouts
            .intern_scalar(ScalarKind::RawByte)
            .expect("raw-byte layout");
        let buffer = layouts.intern_runtime_buffer(byte).expect("byte buffer layout");
        let value_types = [
            (COUNT, ValueType::Size),
            (CAPACITY, ValueType::Size),
            (START, ValueType::Size),
            (LENGTH, ValueType::Size),
            (END, ValueType::Size),
            (SOURCE, ValueType::Ref(buffer)),
            (DESTINATION, ValueType::Ref(buffer)),
        ]
        .into_iter()
        .collect();
        let function = Function {
            id: crate::canonical_safewir::FunctionId::new(0),
            signature: crate::canonical_safewir::FunctionSignatureId::new(0),
            entry: crate::canonical_safewir::BlockId::new(0),
            value_types,
            blocks: vec![BasicBlock {
                id: crate::canonical_safewir::BlockId::new(0),
                parameters: Vec::new(),
                operations: Vec::new(),
                terminator: crate::canonical_safewir::Terminator::Return { values: Vec::new() },
            }],
        };
        let signature = FunctionSignature {
            id: function.signature,
            parameters: Vec::new(),
            results: Vec::new(),
            contract: CallableContract {
                parameters: Vec::new(),
                results: Vec::new(),
                parameter_effects: BTreeMap::new(),
            },
        };
        let closure_layouts = BTreeMap::new();
        let mut checker = MemoryChecker::new(&function, &layouts, &signature, &closure_layouts)
            .expect("checker construction");
        for object in [SOURCE, DESTINATION] {
            checker.state.objects.insert(
                object,
                ObjectState {
                    identity: object,
                    owner_generation: 0,
                    layout: buffer,
                    extent: Extent::Counted {
                        header_bytes: 4,
                        count: COUNT,
                        capacity: CAPACITY,
                        stride_bytes: 1,
                    },
                    ownership: Ownership::Unique,
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    shared_immutable: false,
                    initialized_owned_slots: false,
                },
            );
            checker.bind_extent_generation(object).unwrap();
        }
        checker.state.exact_additions.insert(END, (START, LENGTH));
        let operation = Operation::CopyBytes {
            destination: Place::projected(
                DESTINATION,
                Projection::Slice { start: START, length: LENGTH },
                ValueType::Ref(buffer),
                AccessMode::Write,
            ),
            source: Place::projected(
                SOURCE,
                Projection::Slice { start: START, length: LENGTH },
                ValueType::Ref(buffer),
                AccessMode::Read,
            ),
            length: LENGTH,
        };

        let before_refusal = checker.snapshot();
        assert!(matches!(
            checker.check(&operation),
            Err(InitialCheckError::MissingBoundsGuard { index: START, count: CAPACITY, .. })
        ));
        checker.restore(before_refusal);
        checker
            .state
            .guards
            .insert((ComparisonOperator::LessEqualUnsigned, END, CAPACITY));
        checker.state.guard_generations.insert(
            (ComparisonOperator::LessEqualUnsigned, END, CAPACITY),
            SortedSet::from([checker.current_generation(DESTINATION).unwrap()]),
        );
        let destination_only = checker.snapshot();
        assert!(matches!(
            checker.check(&operation),
            Err(InitialCheckError::MissingBoundsGuard { index: START, count: COUNT, .. })
        ));
        checker.restore(destination_only);
        checker
            .state
            .guards
            .insert((ComparisonOperator::LessEqualUnsigned, END, COUNT));
        checker.state.guard_generations.insert(
            (ComparisonOperator::LessEqualUnsigned, END, COUNT),
            SortedSet::from([checker.current_generation(SOURCE).unwrap()]),
        );
        checker.state.range_states.insert(
            RangeIdentity { object: SOURCE, start: START, length: LENGTH },
            RangeState {
                element: byte,
                ownership: Ownership::Unique,
                allocation_class: Some(AllocationClass::ReferenceCounted),
                initialized: true,
                zero_width: false,
                zero_start: false,
            },
        );
        assert_eq!(checker.check(&operation), Ok(true));
    }
}

/// The upward steps of a `value <= bound` search over one memory state: an
/// unsigned guard `a < b` / `a <= b` steps from any scalar the same as `a` to
/// `b`, and an exact addition `s = a + b` steps from any scalar the same as
/// `a` or `b` to `s`.
///
/// "The same scalar" is `MemoryChecker::same_scalar_with`: the identical
/// value, an equal constant, or a (length header load, count) pair. The first
/// two are exactly equality of `ScalarKey`; a header pair names two exact
/// values, so its partner's steps are looked up by exact value, never by
/// key. The search used to rescan every guard and addition, answering
/// `same_scalar_with` for each, at every visited value -- quadratic in the
/// facts of a large straight-line function, and 33s to check one seven-line
/// program full of string comparisons.
struct UpperSteps {
    header_lengths: SortedSet<(ValueId, ValueId)>,
    by_key: foldhash::HashMap<ScalarKey, Vec<ValueId>>,
    by_value: foldhash::HashMap<ValueId, Vec<ValueId>>,
    header_partners: foldhash::HashMap<ValueId, Vec<ValueId>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ScalarKey {
    Constant(u64),
    Value(ValueId),
}

impl UpperSteps {
    fn new(
        facts: &MemoryState,
        mut guard_counts: impl FnMut(&(ComparisonOperator, ValueId, ValueId)) -> bool,
    ) -> Self {
        let key = |value: ValueId| facts.constants.get(&value)
            .map_or(ScalarKey::Value(value), |constant| ScalarKey::Constant(*constant));
        let mut by_key: foldhash::HashMap<ScalarKey, Vec<ValueId>> = Default::default();
        let mut by_value: foldhash::HashMap<ValueId, Vec<ValueId>> = Default::default();
        let mut step = |from: ValueId, to: ValueId| {
            by_key.entry(key(from)).or_default().push(to);
            by_value.entry(from).or_default().push(to);
        };
        for guard in facts.guards.iter() {
            let (operator, guarded_left, guarded_right) = *guard;
            if matches!(operator, ComparisonOperator::LessUnsigned | ComparisonOperator::LessEqualUnsigned)
                && guard_counts(guard)
            {
                step(guarded_left, guarded_right);
            }
        }
        for (sum, (addend_left, addend_right)) in facts.exact_additions.iter() {
            step(*addend_left, *sum);
            step(*addend_right, *sum);
        }
        let header_lengths = MemoryChecker::header_length_pairs(facts);
        let mut header_partners: foldhash::HashMap<ValueId, Vec<ValueId>> = Default::default();
        for (loaded, count) in header_lengths.iter() {
            header_partners.entry(*loaded).or_default().push(*count);
            header_partners.entry(*count).or_default().push(*loaded);
        }
        Self { header_lengths, by_key, by_value, header_partners }
    }

    /// Whether any value reachable from `start` by upward steps satisfies
    /// `done`. Exhaustive until the first success, so the order in which
    /// steps are explored cannot change the answer.
    fn reaches(&self, facts: &MemoryState, start: ValueId, mut done: impl FnMut(ValueId) -> bool) -> bool {
        let mut pending = vec![start];
        let mut visited = SortedSet::new();
        while let Some(value) = pending.pop() {
            if !visited.insert(value) {
                continue;
            }
            if done(value) {
                return true;
            }
            let key = facts.constants.get(&value)
                .map_or(ScalarKey::Value(value), |constant| ScalarKey::Constant(*constant));
            if let Some(next) = self.by_key.get(&key) {
                pending.extend_from_slice(next);
            }
            for partner in self.header_partners.get(&value).into_iter().flatten() {
                if let Some(next) = self.by_value.get(partner) {
                    pending.extend_from_slice(next);
                }
            }
        }
        false
    }
}
