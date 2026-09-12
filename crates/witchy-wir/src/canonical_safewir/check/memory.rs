use std::collections::{BTreeMap, BTreeSet};

use crate::layout::{
    FieldKind, HeaderLayout, LayoutDescriptor, LayoutInterner, LayoutKind, LayoutSize, PointerSlot,
    RcHeader, ScalarKind,
};

use super::{InitialCheckError, value_type};

#[cfg(test)]
#[path = "move_range_identity_tests.rs"]
mod move_range_identity_tests;

use crate::canonical_safewir::{
    AccessMode, AllocationClass, ArithmeticOperator, BasicBlock, CallArgument,
    CallableContract, ComparisonOperator, Extent, Function, FunctionSignature, GuardKind,
    FunctionSignatureId, HeaderField, LoadTransfer, Operation, OverflowBehavior, Ownership, ParameterContract,
    ParameterOwnership, Place, Projection, ResultContract, ReturnedExtent, StoreTransfer, ValueId,
    ValueType,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObjectState {
    /// The allocation or entry parameter from which this reference originates.
    /// SSA edge arguments rename values, never object identity.
    identity: ValueId,
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

/// All derived memory facts at one program point.  This is deliberately a
/// checker-owned snapshot: transforms cannot attach a claimed range, extent,
/// or ownership fact to an edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MemoryState {
    objects: BTreeMap<ValueId, ObjectState>,
    /// Physical identities that a Shared phi may select at runtime. The phi
    /// value is the SSA identity used by subsequent operations; this side
    /// relation prevents the checker from pretending distinct allocations
    /// became one allocation merely because control flow joined.
    shared_phi_origins: BTreeMap<ValueId, BTreeSet<ValueId>>,
    stored_objects: BTreeMap<(ValueId, SlotIdentity), ObjectState>,
    /// Explicitly initialized nullable fixed slots that contain absence. This
    /// is distinct from an uninitialized slot and from a live stored edge.
    nullable_absent_slots: BTreeSet<(ValueId, SlotIdentity)>,
    range_states: BTreeMap<RangeIdentity, RangeState>,
    constants: BTreeMap<ValueId, u64>,
    exact_additions: BTreeMap<ValueId, (ValueId, ValueId)>,
    comparisons: BTreeMap<ValueId, Comparison>,
    guards: BTreeSet<(ComparisonOperator, ValueId, ValueId)>,
    header_values: BTreeMap<(ValueId, HeaderField), ValueId>,
    static_data_objects: BTreeSet<ValueId>,
    /// The callable and Unit lanes a `Store` has initialized, by container value and
    /// slot. A fresh closure record's code slot is unreadable until then;
    /// a record that arrived from outside (a parameter, a loaded child) is
    /// summarized complete by `initialized_owned_slots` like its owned slots.
    initialized_scalar_slots: BTreeSet<(ValueId, SlotIdentity)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SlotIdentity {
    Constant(u64),
    Dynamic(ValueId),
}

pub(super) struct MemoryChecker<'a> {
    function: &'a Function,
    layouts: &'a LayoutInterner,
    /// The module's closure-object registry: the signature slot 0 of each
    /// closure layout carries. It is the only source of a callable lane's
    /// value type, so a layout missing from it has no readable lane.
    closure_layouts: &'a BTreeMap<crate::layout::LayoutId, FunctionSignatureId>,
    state: MemoryState,
}

impl<'a> MemoryChecker<'a> {
    pub(super) const fn layouts(&self) -> &'a LayoutInterner {
        self.layouts
    }

    pub(super) fn new(
        function: &'a Function,
        layouts: &'a LayoutInterner,
        signature: &FunctionSignature,
        closure_layouts: &'a BTreeMap<crate::layout::LayoutId, FunctionSignatureId>,
    ) -> Result<Self, InitialCheckError> {
        let mut checker = Self {
            function,
            layouts,
            closure_layouts,
            state: MemoryState {
                objects: BTreeMap::new(),
                shared_phi_origins: BTreeMap::new(),
                stored_objects: BTreeMap::new(),
                nullable_absent_slots: BTreeSet::new(),
                range_states: BTreeMap::new(),
                constants: BTreeMap::new(),
                exact_additions: BTreeMap::new(),
                comparisons: BTreeMap::new(),
                guards: BTreeSet::new(),
                header_values: BTreeMap::new(),
                static_data_objects: BTreeSet::new(),
                initialized_scalar_slots: BTreeSet::new(),
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

    /// Rename facts through an explicit SSA edge.  A block parameter is a new
    /// definition, so any count, capacity, header value, guard, or object state
    /// carried by an argument must be expressed in terms of that definition.
    pub(super) fn transfer_edge(
        &mut self,
        target: &BasicBlock,
        arguments: &[ValueId],
    ) -> Result<MemoryState, InitialCheckError> {
        // A shared phi needs the argument's counted extent, which promotion
        // derives lazily from the header loads already in this state. Derive
        // it now from the same facts; a promotion the facts do not justify is
        // left alone and the extent rule below reports it.
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            if parameter.ownership == Some(Ownership::Shared) && self.state.objects.contains_key(argument) {
                let _ = self.promote_header_extent(*argument);
                // Preserve completeness of this predecessor's current logical
                // value before phi renaming or extent demotion loses its count.
                // Missing slot evidence remains missing; capacity is not proof.
                if self.require_owned_slots_initialized(*argument, "shared phi(uninitialized owned slot)").is_ok() {
                    self.state.objects.get_mut(argument).expect("object exists").initialized_owned_slots = true;
                }
            }
        }
        let mut renames = BTreeMap::new();
        let mut unique_phi_rebases = BTreeMap::new();
        let mut shared_phi_rebases = BTreeMap::new();
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            if parameter.ownership == Some(Ownership::Unique) {
                if !matches!(parameter.value_type, ValueType::Ref(_)) {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: parameter.value,
                    });
                }
                let state = self.state.objects.get(argument).ok_or(
                    InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: *argument,
                    },
                )?;
                if state.ownership != Ownership::Unique
                    || !self.is_sole_live_token(*argument, state.identity)
                {
                    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                        eprintln!("canonical unique phi refused at {:?}: argument {argument:?} state {state:#?}", target.id);
                    }
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: *argument,
                    });
                }
                unique_phi_rebases.insert(*argument, parameter.value);
            }
            if parameter.ownership == Some(Ownership::Shared) && !matches!(parameter.value_type, ValueType::Gc(_)) {
                let ValueType::Ref(parameter_layout) = parameter.value_type else {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: parameter.value,
                    });
                };
                let state = self.state.objects.get(argument).ok_or(
                    InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: *argument,
                    },
                )?;
                // A unique reference-counted token may weaken to shared at a
                // shared phi, as a call weakens a unique argument into a
                // shared parameter: the object keeps its one token and loses
                // its write authority. Only a sole live token may weaken.
                let weakenable = state.ownership == Ownership::Unique
                    && state.allocation_class == Some(AllocationClass::ReferenceCounted)
                    && self.is_sole_live_token(*argument, state.identity);
                if !(state.ownership == Ownership::Shared || weakenable) || state.layout != parameter_layout {
                    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                        eprintln!("canonical shared phi refused at {:?}: argument {argument:?} state {state:#?}", target.id);
                    }
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: *argument,
                    });
                }
                shared_phi_rebases.insert(*argument, parameter.value);
            }
            if self.state.objects.contains_key(argument)
                && renames.insert(*argument, parameter.value).is_some_and(|previous| previous != parameter.value)
            {
                if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                    eprintln!("canonical edge refused at {:?}: object {argument:?} is passed in two lanes", target.id);
                }
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: *argument,
                });
            }
            renames.insert(*argument, parameter.value);
        }

        // A checked header read can name the same logical count with a fresh
        // SSA definition. Transport that equality, not an optimizer annotation.
        // Capacity header reads are deliberately excluded: a shared meet may
        // retain only a conservative capacity bound rather than physical capacity.
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            if parameter.ownership != Some(Ownership::Shared) {
                continue;
            }
            let Some(state) = self.state.objects.get(argument) else { continue };
            let Extent::Counted { count, capacity, .. } = state.extent else { continue };
            for bound in [count, capacity] {
                if renames.contains_key(&bound) {
                    continue;
                }
                let replacement = arguments.iter().zip(&target.parameters).find(|(source, target)| {
                    target.value_type == ValueType::Size
                        && (self.state.constants.get(&bound).zip(self.state.constants.get(source))
                            .is_some_and(|(left, right)| left == right)
                            || (bound == count
                                && self.state.header_values.get(&(*argument, HeaderField::Length)) == Some(*source)))
                });
                if let Some((_, replacement)) = replacement {
                    renames.insert(bound, replacement.value);
                }
            }
        }

        let mut objects = BTreeMap::new();
        for (value, state) in &self.state.objects {
            let original_value = *value;
            let mut state = state.clone();
            state.extent = remap_extent(&renames, &state.extent);
            let value = remap_value(&renames, original_value);
            if let Some(identity) = unique_phi_rebases.get(&original_value) {
                state.identity = *identity;
            }
            if let Some(identity) = shared_phi_rebases.get(&original_value) {
                state.identity = *identity;
                if state.ownership == Ownership::Unique {
                    state.ownership = Ownership::Shared;
                }
                match &state.extent {
                    Extent::Counted { count, capacity, .. }
                        if target.parameters.iter().any(|parameter| {
                            parameter.value == *count && parameter.value_type == ValueType::Size
                        }) && target.parameters.iter().any(|parameter| {
                            parameter.value == *capacity
                                && parameter.value_type == ValueType::Size
                        }) => {}
                    Extent::FixedBytes(_) => {}
                    extent => {
                        if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                            eprintln!("canonical shared phi refused at {:?}: object {value:?} extent {extent:?} is not carried by the parameters", target.id);
                        }
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: value,
                        });
                    }
                }
            }
            if let Some(previous) = objects.insert(value, state.clone()) {
                if previous != state {
                    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                        eprintln!("canonical edge refused at {:?}: {value:?} renamed from two objects: {previous:#?} vs {state:#?}", target.id);
                    }
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: value,
                    });
                }
            }
        }
        for parameter in &target.parameters {
            // Only a linear-memory reference carries object state across an
            // edge. Capabilities, GC references, and callables are opaque host
            // or table references with no extent or ownership to rename, so a
            // loop that threads `console` through its header is not a missing
            // object (BUG: capability-in-loop refusal, census B4).
            if matches!(parameter.value_type, ValueType::Ref(_)) && !objects.contains_key(&parameter.value) {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: parameter.value,
                });
            }
        }

        let mut shared_phi_origins = self
            .state
            .shared_phi_origins
            .iter()
            .map(|(value, origins)| (remap_value(&renames, *value), origins.clone()))
            .collect::<BTreeMap<_, _>>();
        for (argument, parameter) in shared_phi_rebases {
            let origins = self
                .state
                .shared_phi_origins
                .get(&argument)
                .cloned()
                .unwrap_or_else(|| BTreeSet::from([self.state.objects[&argument].identity]));
            shared_phi_origins
                .entry(parameter)
                .or_default()
                .extend(origins);
        }

        Ok(MemoryState {
            objects,
            shared_phi_origins,
            initialized_scalar_slots: self
                .state
                .initialized_scalar_slots
                .iter()
                .map(|(container, index)| (remap_value(&renames, *container), match index { SlotIdentity::Constant(n) => SlotIdentity::Constant(*n), SlotIdentity::Dynamic(value) => SlotIdentity::Dynamic(remap_value(&renames, *value)) }))
                .collect(),
            stored_objects: self
                .state
                .stored_objects
                .iter()
                .map(|((container, index), state)| {
                    (
                        (
                            remap_value(&renames, *container),
                            match index {
                                SlotIdentity::Constant(value) => SlotIdentity::Constant(*value),
                                SlotIdentity::Dynamic(value) => {
                                    SlotIdentity::Dynamic(remap_value(&renames, *value))
                                }
                            },
                        ),
                        state.clone(),
                    )
                })
                .collect(),
            nullable_absent_slots: self.state.nullable_absent_slots.iter().map(|(container, slot)| {
                (remap_value(&renames, *container), *slot)
            }).collect(),
            range_states: self
                .state
                .range_states
                .iter()
                .map(|(identity, state)| {
                    (
                        RangeIdentity {
                            object: remap_value(&renames, identity.object),
                            start: remap_value(&renames, identity.start),
                            length: remap_value(&renames, identity.length),
                        },
                        *state,
                    )
                })
                .collect(),
            constants: self
                .state
                .constants
                .iter()
                .map(|(value, constant)| (remap_value(&renames, *value), *constant))
                .collect(),
            exact_additions: self
                .state
                .exact_additions
                .iter()
                .map(|(result, (left, right))| {
                    (
                        remap_value(&renames, *result),
                        (remap_value(&renames, *left), remap_value(&renames, *right)),
                    )
                })
                .collect(),
            comparisons: self
                .state
                .comparisons
                .iter()
                .map(|(value, comparison)| {
                    (
                        remap_value(&renames, *value),
                        Comparison {
                            operator: comparison.operator,
                            left: remap_value(&renames, comparison.left),
                            right: remap_value(&renames, comparison.right),
                        },
                    )
                })
                .collect(),
            guards: self
                .state
                .guards
                .iter()
                .map(|(operator, left, right)| {
                    (*operator, remap_value(&renames, *left), remap_value(&renames, *right))
                })
                .collect(),
            header_values: self
                .state
                .header_values
                .iter()
                .map(|((object, field), value)| {
                    (
                        (remap_value(&renames, *object), *field),
                        remap_value(&renames, *value),
                    )
                })
                .collect(),
            static_data_objects: self
                .state
                .static_data_objects
                .iter()
                .map(|object| remap_value(&renames, *object))
                .collect(),
        })
    }

    /// A Unique phi may change object identity only when its edge argument is
    /// the sole remaining live representation of that identity.  Container
    /// slots count: rebasing a value while the same object remains stored
    /// elsewhere would leave a physical alias under the old identity.
    fn is_sole_live_token(&self, value: ValueId, identity: ValueId) -> bool {
        Self::is_sole_live_token_in(&self.state, value, identity)
    }

    fn is_sole_live_token_in(edge: &MemoryState, value: ValueId, identity: ValueId) -> bool {
        edge.objects.iter().all(|(candidate, state)| {
            state.identity != identity || state.ownership == Ownership::Consumed || *candidate == value
        }) && edge.stored_objects.values().all(|state| {
            state.identity != identity || state.ownership == Ownership::Consumed
        })
    }

    /// Meet facts from predecessor edges.  Scalar facts are usable only when
    /// every predecessor establishes the same fact.  An object state is never
    /// widened: different allocation identity, ownership, layout, or extent is
    /// an explicit unsafe join rather than an inferred alias relation.
    pub(super) fn meet(
        &self,
        block: &BasicBlock,
        existing: &MemoryState,
        incoming: &MemoryState,
    ) -> Result<MemoryState, InitialCheckError> {
        let mut objects = BTreeMap::new();
        // Analysis facts may be intersected, but owned references are release
        // obligations. An incoming edge must not erase a live affine token.
        for (state, other) in [(existing, incoming), (incoming, existing)] {
            for (value, object) in &state.objects {
                if object.ownership != Ownership::Consumed
                    && (object.allocation_class == Some(AllocationClass::ReferenceCounted)
                        || object.shared_immutable)
                    && !other.objects.contains_key(value)
                {
                    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                        eprintln!("canonical memory join at {:?}: live token {value:?} is missing on the other edge: {object:#?}", block.id);
                    }
                    return Err(InitialCheckError::IncompatibleMemoryJoin {
                        function: self.function.id,
                        block: block.id,
                        object: *value,
                    });
                }
            }
            for (slot, object) in &state.stored_objects {
                if other.stored_objects.get(slot) != Some(object) {
                    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                        eprintln!("canonical memory join at {:?}: stored slot {slot:?} differs: {object:#?} vs {:#?}", block.id, other.stored_objects.get(slot));
                    }
                    return Err(InitialCheckError::IncompatibleMemoryJoin {
                        function: self.function.id,
                        block: block.id,
                        object: slot.0,
                    });
                }
            }
        }
        for (value, current) in &existing.objects {
            let Some(next) = incoming.objects.get(value) else {
                continue;
            };
            // Extent promotion is lazy (it happens at the first element or
            // slice access), so a back edge may carry the counted extent the
            // entry edge has not derived yet from the same header loads.
            // Promote both sides before comparing; a promotion that fails on
            // one side leaves that side as it was, so nothing is widened.
            let promoted_current = self.promoted_object(existing, *value).unwrap_or_else(|| current.clone());
            let promoted_next = self.promoted_object(incoming, *value).unwrap_or_else(|| next.clone());
            // Preserve a common logical count before the generic header-only
            // fallback erases it. The shared meet drops capacity precision,
            // never identity or ownership obligations.
            if promoted_current != promoted_next
                && let Some(met) = shared_phi_meet(&promoted_current, &promoted_next)
            {
                objects.insert(*value, met);
                continue;
            }
            // A back edge may carry a counted extent whose count was loaded
            // inside the loop body, which the entry edge cannot know. The
            // header-prefix form is the weaker fact of the same object, so the
            // join keeps it and the body re-derives the count from its own
            // loads; nothing else about the object may differ.
            let (current, next) = match demoted_pair(&promoted_current, &promoted_next) {
                Some(pair) => {
                    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                        eprintln!(
                            "canonical memory join at {:?} demotes {value:?}: {:?} vs {:?}",
                            block.id, promoted_current.extent, promoted_next.extent
                        );
                    }
                    pair
                }
                None => (promoted_current, promoted_next),
            };
            let (current, next) = (&current, &next);
            // Predecessors are alternatives. A clone on only one path loses
            // uniqueness, but does not create another token at the join. This
            // rule never identifies different allocations or erases a release
            // obligation; it only drops write/sole-token authority.
            let unique_edge = match (current.ownership, next.ownership) {
                (Ownership::Unique, Ownership::Shared) => Some((existing, current)),
                (Ownership::Shared, Ownership::Unique) => Some((incoming, next)),
                _ => None,
            };
            if let Some((edge, unique)) = unique_edge
                && current.identity == next.identity
                && current.layout == next.layout
                && current.extent == next.extent
                && unique.allocation_class == Some(AllocationClass::ReferenceCounted)
                && Self::is_managed_storage(current)
                && Self::is_managed_storage(next)
                && Self::is_sole_live_token_in(edge, *value, unique.identity)
            {
                let mut met = current.clone();
                met.ownership = Ownership::Shared;
                met.allocation_class = (current.allocation_class == next.allocation_class)
                    .then_some(current.allocation_class).flatten();
                met.shared_immutable = true;
                met.initialized_owned_slots = current.initialized_owned_slots && next.initialized_owned_slots;
                objects.insert(*value, met);
                continue;
            }
            // A shared phi may join a parameter-derived shared reference with
            // a weakened fresh allocation: both are managed storage, so the
            // meet keeps only what both establish (unknown class, immutable,
            // owned slots initialized only if both say so).
            // A consumed token carries no obligation; residual class or
            // immutability facts about the storage it named do not matter.
            if current != next
                && current.ownership == Ownership::Consumed
                && next.ownership == Ownership::Consumed
                && current.identity == next.identity
                && current.layout == next.layout
            {
                objects.insert(*value, current.clone());
                continue;
            }
            // A shared managed object may reach a block through different
            // paths with different class or immutability facts (a phi block,
            // or any block after one across the fixpoint's rounds); the meet
            // keeps only what both establish.
            if current != next
                && let Some(met) = shared_phi_meet(current, next)
            {
                objects.insert(*value, met);
                continue;
            }
            if current != next {
                if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                    eprintln!(
                        "canonical memory join mismatch at {:?} for {value:?}:\n existing: {current:#?}\n incoming: {next:#?}",
                        block.id
                    );
                }
                return Err(InitialCheckError::IncompatibleMemoryJoin {
                    function: self.function.id,
                    block: block.id,
                    object: *value,
                });
            }
            objects.insert(*value, current.clone());
        }
        for parameter in &block.parameters {
            // Only a linear-memory reference carries object state across an
            // edge. Capabilities, GC references, and callables are opaque host
            // or table references with no extent or ownership to rename, so a
            // loop that threads `console` through its header is not a missing
            // object (BUG: capability-in-loop refusal, census B4).
            if matches!(parameter.value_type, ValueType::Ref(_)) && !objects.contains_key(&parameter.value) {
                return Err(InitialCheckError::IncompatibleMemoryJoin {
                    function: self.function.id,
                    block: block.id,
                    object: parameter.value,
                });
            }
        }

        Ok(MemoryState {
            objects,
            shared_phi_origins: meet_shared_phi_origins(existing, incoming),
            stored_objects: intersect_map(&existing.stored_objects, &incoming.stored_objects),
            nullable_absent_slots: existing.nullable_absent_slots.intersection(&incoming.nullable_absent_slots).copied().collect(),
            initialized_scalar_slots: existing.initialized_scalar_slots.intersection(&incoming.initialized_scalar_slots).copied().collect(),
            range_states: meet_range_states(existing, incoming),
            constants: intersect_map(&existing.constants, &incoming.constants),
            exact_additions: intersect_map(&existing.exact_additions, &incoming.exact_additions),
            comparisons: intersect_map(&existing.comparisons, &incoming.comparisons),
            guards: existing.guards.intersection(&incoming.guards).copied().collect(),
            header_values: intersect_map(&existing.header_values, &incoming.header_values),
            static_data_objects: existing
                .static_data_objects
                .intersection(&incoming.static_data_objects)
                .copied()
                .collect(),
        })
    }

    /// `object`'s state in `state` after header-extent promotion, when the
    /// facts in `state` justify it.
    // (see `demoted_pair` below for the inverse direction at a join)
    fn promoted_object(&self, state: &MemoryState, object: ValueId) -> Option<ObjectState> {
        if !matches!(state.objects.get(&object)?.extent, Extent::HeaderPrefix { .. }) {
            return None;
        }
        let mut scratch = MemoryChecker { function: self.function, layouts: self.layouts, closure_layouts: self.closure_layouts, state: state.clone() };
        scratch.promote_header_extent(object).ok()?;
        scratch.state.objects.get(&object).cloned()
    }

    fn initialize_parameters(
        &mut self,
        signature: &FunctionSignature,
    ) -> Result<(), InitialCheckError> {
        let entry = self
            .function
            .blocks
            .iter()
            .find(|block| block.id == self.function.entry)
            .ok_or(InitialCheckError::MissingBlock {
                function: self.function.id,
                block: self.function.entry,
            })?;
        for (index, parameter) in entry.parameters.iter().enumerate() {
            let ValueType::Ref(layout) = parameter.value_type else {
                continue;
            };
            let contract = signature
                .contract
                .parameters
                .get(index)
                .ok_or(InitialCheckError::UnsupportedMemorySignature {
                    signature: signature.id,
                })?;
            let ownership = match contract.ownership {
                ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack => Ownership::Unique,
                ParameterOwnership::Shared | ParameterOwnership::WriteBack => Ownership::Shared,
                ParameterOwnership::Value => {
                    continue;
                }
            };
            let extent = self.parameter_extent(layout)?;
            let (allocation_class, shared_immutable) = match contract.ownership {
                ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack => (Some(AllocationClass::ReferenceCounted), false),
                ParameterOwnership::Shared | ParameterOwnership::WriteBack => (None, true),
                ParameterOwnership::Value => unreachable!(),
            };
            let initialized_owned_slots = matches!(
                contract.ownership,
                ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack | ParameterOwnership::Shared | ParameterOwnership::WriteBack
            );
            self.state.objects.insert(
                parameter.value,
                ObjectState {
                    identity: parameter.value,
                    layout,
                    extent,
                    ownership,
                    allocation_class,
                    shared_immutable,
                    initialized_owned_slots,
                },
            );
        }
        Ok(())
    }

    fn parameter_extent(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<Extent, InitialCheckError> {
        match self.descriptor(layout)?.size() {
            LayoutSize::Fixed(bytes) => Ok(Extent::FixedBytes(u64::from(bytes))),
            LayoutSize::Dynamic { base, .. } => {
                Ok(Extent::HeaderPrefix { minimum_bytes: base })
            }
        }
    }

    pub(super) fn check_call(
        &mut self,
        arguments: &[CallArgument],
        parameters: &[ParameterContract],
        destinations: &[ValueId],
        results: &[ResultContract],
    ) -> Result<(), InitialCheckError> {
        for (argument, parameter) in arguments.iter().zip(parameters) {
            let value = match argument {
                CallArgument::Value(value) | CallArgument::Move(value) => *value,
            };
            let actual = value_type(self.function, value)?;
            if matches!(actual, ValueType::Gc(_)) {
                // The separate opaque-token CFG judgment consumes this lane.
                // It has no linear address, extent, or allocation class.
                if !matches!(argument, CallArgument::Move(_)) || parameter.ownership != ParameterOwnership::Shared {
                    return self.unsupported("Call(opaque ownership)");
                }
                continue;
            }
            if !actual.is_linear_memory_ref() {
                if !matches!(argument, CallArgument::Value(_)) {
                    return self.unsupported("Call(non-memory ownership)");
                }
                continue;
            }
            match argument {
                CallArgument::Value(_) => return self.unsupported("Call(Value memory)"),
                CallArgument::Move(value) => {
                    self.require_owned_slots_initialized(*value, "Call(uninitialized owned slot)")?;
                    self.consume_call_move(*value, parameter.ownership)?;
                }
            }
        }
        for (destination, result) in destinations.iter().zip(results) {
            match result {
                ResultContract::Value(value_type) if !value_type.is_linear_memory_ref() => {}
                ResultContract::CheckedValue { layout } => {
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(
                        *destination,
                        ObjectState {
                            identity: *destination,
                            layout: *layout,
                            extent,
                            ownership: Ownership::Shared,
                            allocation_class: None,
                            shared_immutable: false,
                            initialized_owned_slots: true,
                        },
                    );
                }
                ResultContract::Fresh { layout, class, .. } => {
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(
                        *destination,
                        ObjectState {
                            identity: *destination,
                            layout: *layout,
                            extent,
                            ownership: Ownership::Unique,
                            allocation_class: Some(*class),
                            shared_immutable: false,
                            initialized_owned_slots: true,
                        },
                    );
                }
                ResultContract::Unique { layout } => {
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(*destination, ObjectState {
                        identity: *destination, layout: *layout, extent,
                        ownership: Ownership::Unique,
                        allocation_class: Some(AllocationClass::ReferenceCounted),
                        shared_immutable: false, initialized_owned_slots: true,
                    });
                }
                ResultContract::Shared { layout } => {
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(
                        *destination,
                        ObjectState {
                            identity: *destination,
                            layout: *layout,
                            extent,
                            ownership: Ownership::Shared,
                            allocation_class: None,
                            shared_immutable: true,
                            // A checked Shared result is a complete logical
                            // value at the ABI boundary. This is distinct from
                            // locally allocated or Fresh storage, which needs
                            // slot/range evidence before it can escape.
                            initialized_owned_slots: true,
                        },
                    );
                }
                ResultContract::StaticData { layout } => {
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(
                        *destination,
                        ObjectState {
                            identity: *destination,
                            layout: *layout,
                            extent,
                            ownership: Ownership::Shared,
                            allocation_class: None,
                            shared_immutable: false,
                            initialized_owned_slots: false,
                        },
                    );
                    self.state.static_data_objects.insert(*destination);
                }
                _ => return self.unsupported("Call(memory result)"),
            }
        }
        Ok(())
    }

    pub(super) fn check_exit(&self, returned: &[ValueId]) -> Result<(), InitialCheckError> {
        for (object, state) in &self.state.objects {
            if state.ownership != Ownership::Consumed
                && (state.allocation_class == Some(AllocationClass::ReferenceCounted)
                    || state.shared_immutable)
                && !returned.contains(object)
            {
                return Err(InitialCheckError::UnconsumedReference {
                    function: self.function.id,
                    object: *object,
                });
            }
        }
        Ok(())
    }

    pub(super) fn check_return(
        &self,
        values: &[ValueId],
        results: &[ResultContract],
    ) -> Result<(), InitialCheckError> {
        let mut transferred = BTreeSet::new();
        for (value, result) in values.iter().zip(results) {
            // Each result lane transfers one token. Repeating an SSA reference
            // does not execute CloneRef, even when both lanes claim Shared.
            if matches!(value_type(self.function, *value)?, ValueType::Ref(_))
                && !matches!(result, ResultContract::StaticData { .. })
                && !transferred.insert(*value)
            {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: *value,
                });
            }
            match result {
                ResultContract::Value(value_type) if !value_type.is_linear_memory_ref() => {}
                ResultContract::CheckedValue { layout } => {
                    self.require_owned_slots_initialized(*value, "Return(uninitialized owned slot)")?;
                    let state = self.live_object(*value)?;
                    if state.layout != *layout
                        || !matches!(state.ownership, Ownership::Unique | Ownership::Shared)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                }
                ResultContract::Fresh { layout, class, extent } => {
                    self.require_owned_slots_initialized(*value, "Return(uninitialized owned slot)")?;
                    let state = self.live_object(*value)?;
                    if state.layout != *layout
                        || state.ownership != Ownership::Unique
                        || state.allocation_class != Some(*class)
                        || !returned_extent_matches(&state.extent, extent)
                        || !self.is_sole_live_token(*value, state.identity)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                }
                ResultContract::StaticData { layout } => {
                    let state = self.live_object(*value)?;
                    if state.layout != *layout
                        || state.ownership != Ownership::Shared
                        || !self.state.static_data_objects.contains(value)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                }
                ResultContract::Unique { layout } => {
                    self.require_owned_slots_initialized(*value, "Return(uninitialized unique write-back)")?;
                    let state = self.live_object(*value)?;
                    if state.layout != *layout || state.ownership != Ownership::Unique
                        || state.allocation_class != Some(AllocationClass::ReferenceCounted)
                        || !self.is_sole_live_token(*value, state.identity)
                    {
                        return Err(InitialCheckError::InvalidOwnership { function: self.function.id, object: *value });
                    }
                }
                ResultContract::Shared { layout } => {
                    self.require_owned_slots_initialized(*value, "Return(uninitialized owned slot)")?;
                    let state = self.live_object(*value)?;
                    // Shared owns either an RC token or immutable static data.
                    // The union never grants mutation or reclamation authority.
                    if state.layout != *layout
                        || !matches!(state.ownership, Ownership::Unique | Ownership::Shared)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                    if !self.is_shared_immutable_storage(*value, state) {
                        return Err(InitialCheckError::InvalidAllocationClass {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                }
                _ => return self.unsupported("Return(memory result contract)"),
            }
        }
        Ok(())
    }

    pub(super) fn check_host_buffer(
        &mut self,
        place: &Place,
        extent: &crate::canonical_safewir::HostBufferExtent,
        import: crate::canonical_safewir::ImportId,
        parameter: usize,
    ) -> Result<(), InitialCheckError> {
        let Projection::Slice { start, length } = place.projection() else {
            return Err(InitialCheckError::UnsupportedHostBuffer {
                function: self.function.id,
                import,
                parameter,
            });
        };
        self.promote_header_extent(place.object())?;
        let object = self.live_object(place.object())?.clone();
        self.require_ref_type(place.object(), object.layout)?;
        if place.value_type() != ValueType::Ref(object.layout) {
            return Err(InitialCheckError::TypeMismatch {
                function: self.function.id,
                value: place.object(),
                expected: ValueType::Ref(object.layout),
                actual: place.value_type(),
            });
        }
        if place.access() == AccessMode::Write {
            if object.ownership != Ownership::Unique
                || self.state.static_data_objects.contains(&place.object())
            {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: place.object(),
                });
            }
        }
        self.require_type(*start, ValueType::Size)?;
        self.require_type(*length, ValueType::Size)?;
        let Extent::Counted { count, capacity, stride_bytes, .. } = object.extent else {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: place.object(),
            });
        };
        if stride_bytes != 1 || !self.is_raw_byte_buffer(object.layout)? {
            return Err(InitialCheckError::UnsupportedHostBuffer {
                function: self.function.id,
                import,
                parameter,
            });
        }
        // A host read stays inside the logical length; a host write lands
        // inside the capacity, and the guest publishes the written length
        // afterwards under its own capacity guard.
        let logical_count = if place.access() == AccessMode::Write {
            self.state
                .header_values
                .get(&(place.object(), HeaderField::Capacity))
                .copied()
                .unwrap_or(capacity)
        } else {
            self.state
                .header_values
                .get(&(place.object(), HeaderField::Length))
                .copied()
                .unwrap_or(count)
        };
        let starts_at_zero = self.state.constants.get(start) == Some(&0);
        let whole_logical_body = starts_at_zero && *length == logical_count;
        let constant_range = self
            .state
            .constants
            .get(start)
            .zip(self.state.constants.get(length))
            .zip(self.state.constants.get(&logical_count))
            .is_some_and(|((start, length), count)| {
                start.checked_add(*length).is_some_and(|end| end <= *count)
            });
        if !whole_logical_body && !constant_range {
            return Err(InitialCheckError::MissingBoundsGuard {
                function: self.function.id,
                index: *start,
                count: logical_count,
            });
        }
        let constant_bytes = self
            .state
            .constants
            .get(length)
            .and_then(|length| length.checked_mul(u64::from(stride_bytes)));
        match extent {
            crate::canonical_safewir::HostBufferExtent::Slice => {}
            crate::canonical_safewir::HostBufferExtent::Exact(bytes)
                if constant_bytes == Some(*bytes) => {}
            crate::canonical_safewir::HostBufferExtent::AtLeast(bytes)
                if constant_bytes.is_some_and(|actual| actual >= *bytes) => {}
            crate::canonical_safewir::HostBufferExtent::Exact(_)
            | crate::canonical_safewir::HostBufferExtent::AtLeast(_)
            | crate::canonical_safewir::HostBufferExtent::Unknown => {
                return Err(InitialCheckError::HostBufferExtentMismatch {
                    function: self.function.id,
                    import,
                    parameter,
                });
            }
        }
        Ok(())
    }

    fn copy_bytes(
        &mut self,
        destination: &Place,
        source: &Place,
        length: ValueId,
    ) -> Result<(), InitialCheckError> {
        self.require_type(length, ValueType::Size)?;
        let destination_length = self.check_byte_slice(destination, AccessMode::Write)?;
        let source_length = self.check_byte_slice(source, AccessMode::Read)?;
        if destination_length != length || source_length != length {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: destination.object(),
            });
        }
        Ok(())
    }

    fn move_range(
        &mut self,
        destination: &Place,
        source: &Place,
        count: ValueId,
    ) -> Result<(), InitialCheckError> {
        self.require_type(count, ValueType::Size)?;
        let destination = self.check_typed_range(destination, AccessMode::Write, "MoveRange")?;
        let source = self.check_typed_range(source, AccessMode::Read, "MoveRange")?;
        self.check_range_pair(destination, source, count, "MoveRange")?;

        if self.value_type_for_layout(destination.element)?.is_memory_bearing() {
            self.require_source_range_complete(source)?;
            let moved = self.require_initialized_range(source, destination.element)?;
            self.require_destination_range_empty(destination)?;
            let source_object = source.object;
            let consumed = self.state.stored_objects.keys()
                .filter(|(object, _)| self.same_allocation(source_object, *object))
                .copied()
                .collect::<Vec<_>>();
            for key in consumed {
                self.state.stored_objects.remove(&key);
            }
            {
                // Range facts follow the allocation, not the token: an
                // alias's prefix fact must not survive the move of the
                // elements it names.
                let source_identity = self.state.objects.get(&source.object).map(|state| state.identity);
                let destination_identity = self.state.objects.get(&destination.object).map(|state| state.identity);
                let MemoryState { objects, range_states, .. } = &mut self.state;
                range_states.retain(|identity, _| {
                    let identity = objects.get(&identity.object).map(|state| state.identity);
                    identity != source_identity && identity != destination_identity
                });
                for object in objects.values_mut() {
                    if Some(object.identity) == source_identity || Some(object.identity) == destination_identity {
                        object.initialized_owned_slots = false;
                    }
                }
            }
            self.state.range_states.insert(
                RangeIdentity { object: destination.object, start: destination.start, length: destination.length },
                RangeState { element: destination.element, ownership: moved.ownership, allocation_class: moved.allocation_class, initialized: true, zero_width: self.is_zero(count), zero_start: self.is_zero(destination.start) },
            );
            self.state.range_states.insert(
                RangeIdentity { object: source.object, start: source.start, length: source.length },
                RangeState { element: source.element, ownership: Ownership::Consumed, allocation_class: moved.allocation_class, initialized: false, zero_width: self.is_zero(count), zero_start: self.is_zero(source.start) },
            );
        } else {
            self.require_scalar_range_element(destination.element, "MoveRange")?;
        }
        if destination.ownership != Ownership::Unique
            || source.ownership != Ownership::Unique
            || destination.allocation_class != Some(AllocationClass::ReferenceCounted)
            || source.allocation_class != Some(AllocationClass::ReferenceCounted)
            || self.state.static_data_objects.contains(&destination.object)
        {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: destination.object,
            });
        }
        Ok(())
    }

    fn require_source_range_complete(&self, range: CheckedRange) -> Result<(), InitialCheckError> {
        // MoveRange removes source facts across the allocation, not merely
        // the current SSA token. Its completeness obligation must use the
        // same scope, including facts retained under a consumed MoveValue
        // source and complete-value summaries with no materialized slots.
        let object = self.live_object(range.object)?;
        let summarized_partial = object.initialized_owned_slots
            && !(self.is_zero(range.start)
                && matches!(object.extent, Extent::Counted { count, .. }
                    if self.same_scalar(count, range.length)));
        if summarized_partial || self.state.stored_objects.iter().any(|((object, slot), _)| {
            self.same_allocation(range.object, *object) && !self.slot_in_range(*slot, range)
        }) || self.state.range_states.iter().any(|(identity, state)| {
            self.same_allocation(range.object, identity.object)
                && state.initialized
                && !self.is_zero(identity.length)
                && !(self.range_start_matches(identity, state, range.start)
                    && self.same_scalar(identity.length, range.length))
        }) {
            if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                let ranges = self.state.range_states.iter().filter(|(identity, _)| self.same_allocation(range.object, identity.object)).collect::<Vec<_>>();
                let stored = self.state.stored_objects.keys().filter(|(object, _)| self.same_allocation(range.object, *object)).collect::<Vec<_>>();
                eprintln!("MoveRange(source-partial) object={:?} start={:?} length={:?} ranges={ranges:#?} stored={stored:?}", range.object, range.start, range.length);
            }
            return self.unsupported("MoveRange(source-partial)");
        }
        Ok(())
    }

    fn slot_in_range(&self, slot: SlotIdentity, range: CheckedRange) -> bool {
        let SlotIdentity::Constant(index) = slot else {
            return false;
        };
        let Some(start) = self.state.constants.get(&range.start) else {
            return false;
        };
        let Some(length) = self.state.constants.get(&range.length) else {
            return false;
        };
        index >= *start && index - *start < *length
    }

    fn require_initialized_range(
        &self,
        range: CheckedRange,
        element: crate::layout::LayoutId,
    ) -> Result<RangeState, InitialCheckError> {
        if let Some(state) = self.state.range_states.iter().find_map(|(identity, state)| {
            (self.same_allocation(range.object, identity.object)
                && self.range_start_matches(identity, state, range.start)
                && self.same_scalar(identity.length, range.length)
                && state.element == element
                // A prefix some element of which has a live shared clone
                // still owns its slots: relocating them keeps every clone
                // valid, so a move accepts the shared mark and carries it.
                && matches!(state.ownership, Ownership::Unique | Ownership::Shared)
                && state.initialized
                && (!state.zero_width || self.is_zero(range.length)))
                .then_some(*state)
        }) {
            return Ok(state);
        }
        // A checked ABI parameter or result denotes a complete logical value.
        // Its child slots were initialized by the caller, so the full counted
        // prefix can be cloned or moved without reconstructing per-slot facts
        // in this function. Partial ranges still require local evidence.
        let object = self.live_object(range.object)?.clone();
        if object.initialized_owned_slots
            && self.is_zero(range.start)
            && matches!(object.extent, Extent::Counted { count, .. }
                if self.same_scalar(count, range.length))
            && self.descriptor(object.layout)?.kind().packed_element_layout() == Some(element)
        {
            return Ok(RangeState {
                element,
                // Container completeness proves the children exist, not that
                // they are unique or belong to one allocation class.
                ownership: Ownership::Shared,
                allocation_class: None,
                initialized: true,
                zero_width: self.is_zero(range.length),
                zero_start: true,
            });
        }
        if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
            let ranges = self.state.range_states.iter().filter(|(identity, _)| identity.object == range.object).collect::<Vec<_>>();
            let stored = self.state.stored_objects.keys().filter(|(object, _)| *object == range.object).collect::<Vec<_>>();
            eprintln!("MoveRange(uninitialized-source) object={:?} start={:?} length={:?} object_state={:#?} ranges={ranges:#?} stored={stored:?}", range.object, range.start, range.length, self.state.objects.get(&range.object));
        }
        let Some(start) = self.state.constants.get(&range.start).copied() else {
            return self.unsupported("MoveRange(uninitialized-source)");
        };
        let Some(length) = self.state.constants.get(&range.length).copied() else {
            return self.unsupported("MoveRange(uninitialized-source)");
        };
        let pointee = self.owned_pointer_slot_pointee(element)?;
        let end = start.checked_add(length).ok_or(InitialCheckError::UnsupportedMemoryOperation {
            function: self.function.id,
            operation: "MoveRange(uninitialized-source)",
        })?;
        let mut allocation_class = None;
        let mut ownership = Ownership::Unique;
        for index in start..end {
            // A slot fact may be keyed by an alias of this object (a clone
            // of the same allocation); resolve it through the identity.
            let Some(stored) = self.stored_slot_by_identity(range.object, SlotIdentity::Constant(index)) else {
                return self.unsupported("MoveRange(uninitialized-source)");
            };
            if !matches!(stored.ownership, Ownership::Unique | Ownership::Shared) || stored.layout != pointee {
                return self.unsupported("MoveRange(uninitialized-source)");
            }
            if stored.ownership == Ownership::Shared {
                ownership = Ownership::Shared;
            }
            if allocation_class.is_some() && allocation_class != stored.allocation_class {
                return self.unsupported("MoveRange(incompatible element storage)");
            }
            allocation_class = stored.allocation_class;
        }
        Ok(RangeState { element, ownership, allocation_class, initialized: true, zero_width: self.is_zero(range.length), zero_start: self.is_zero(range.start) })
    }

    /// Whether `object` names the same allocation as `other` (an alias such
    /// as a `CloneRef` of it, or the object itself).
    fn same_allocation(&self, object: ValueId, other: ValueId) -> bool {
        object == other
            || match (self.state.objects.get(&object), self.state.objects.get(&other)) {
                (Some(left), Some(right)) => left.identity == right.identity,
                _ => false,
            }
    }

    /// Whether a checked scalar lane of `object`'s allocation has been stored, under
    /// any alias.
    fn checked_scalar_slot_stored(&self, object: ValueId, slot: SlotIdentity) -> bool {
        self.state
            .initialized_scalar_slots
            .iter()
            .any(|(container, stored)| *stored == slot && self.same_allocation(object, *container))
    }

    /// Whether the checked scalar lane at `place` may be read: stored on every
    /// path into this block, or held by a record that arrived complete (a
    /// parameter, a loaded child).
    fn checked_scalar_slot_initialized(&self, place: &Place) -> Result<bool, InitialCheckError> {
        let (object, slot) = self.reference_slot(place)?;
        if self.live_object(object)?.initialized_owned_slots {
            return Ok(true);
        }
        Ok(self.checked_scalar_slot_stored(object, slot))
    }

    /// A stored slot fact of `object`'s allocation under any alias.
    fn stored_slot_by_identity(&self, object: ValueId, slot: SlotIdentity) -> Option<&ObjectState> {
        self.state
            .stored_objects
            .iter()
            .find(|((stored_object, stored_slot), _)| *stored_slot == slot && self.same_allocation(object, *stored_object))
            .map(|(_, stored)| stored)
    }

    fn nullable_slot_absent(&self, object: ValueId, slot: SlotIdentity) -> bool {
        self.state.nullable_absent_slots.iter().any(|(container, candidate)| {
            *candidate == slot && self.same_allocation(object, *container)
        })
    }

    /// Unit initialization is dataflow evidence, never implied by its width.
    fn unit_prefix_initialized(&self, object: ValueId, mut count: ValueId) -> bool {
        let Ok(state) = self.live_object(object) else { return false };
        if state.initialized_owned_slots && (matches!(state.extent, Extent::Counted { count: current, .. } if self.same_scalar(current, count))
            || matches!(state.extent, Extent::HeaderPrefix { .. }) && self.state.header_values.get(&(object, HeaderField::Length)).is_some_and(|current| self.same_scalar(*current, count))) {
            return true;
        }
        for _ in 0..=self.state.exact_additions.len() {
            if self.is_zero(count) { return true; }
            if self.state.range_states.iter().any(|(identity, range)| {
                self.same_allocation(object, identity.object) && range.initialized && range.zero_start
                    && self.same_scalar(identity.length, count)
                    && self.layouts.get(range.element).is_some_and(|d| d.kind() == &LayoutKind::Scalar(ScalarKind::Unit))
            }) { return true; }
            if let Some(length) = self.state.constants.get(&count) {
                let initialized = self.state.initialized_scalar_slots.iter().filter_map(|(container, slot)| match slot {
                    SlotIdentity::Constant(index) if index < length && self.same_allocation(object, *container) => Some(*index),
                    _ => None,
                }).collect::<BTreeSet<_>>();
                if initialized.len() as u64 == *length { return true; }
                let mut end = self.state.range_states.iter().filter_map(|(identity, range)| {
                    (self.same_allocation(object, identity.object) && range.initialized && range.zero_start
                        && self.layouts.get(range.element).is_some_and(|d| d.kind() == &LayoutKind::Scalar(ScalarKind::Unit)))
                        .then(|| self.state.constants.get(&identity.length).copied()).flatten()
                }).max().unwrap_or(0);
                for index in initialized { if index == end { end += 1; } }
                if end >= *length { return true; }

            }
            let Some(&(left, right)) = self.state.exact_additions.get(&count) else { return false };
            let prefix = if self.state.constants.get(&right) == Some(&1) { left }
                else if self.state.constants.get(&left) == Some(&1) { right } else { return false };
            let slot = self.state.constants.get(&prefix).map_or(SlotIdentity::Dynamic(prefix), |n| SlotIdentity::Constant(*n));
            if !self.checked_scalar_slot_stored(object, slot) { return false; }
            count = prefix;
        }
        false
    }

    /// Completeness describes the logical prefix, not arbitrary capacity-tail
    /// writes. Keep that evidence, but account for every separately held edge
    /// before accepting it. Facts follow allocation identity across SSA moves.
    fn require_owned_edges_within_prefix(
        &self,
        object: ValueId,
        state: &ObjectState,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
        let Some(element) = self.descriptor(state.layout)?.kind().packed_element_layout() else {
            return Ok(());
        };
        if !matches!(self.descriptor(element)?.kind(), LayoutKind::RuntimePointerValueSlot { .. }) {
            return Ok(());
        }
        let count = match state.extent {
            Extent::Counted { count, .. } => Some(count),
            Extent::HeaderPrefix { .. } => self.state.header_values.get(&(object, HeaderField::Length)).copied(),
            _ => None,
        };
        for ((container, slot), stored) in &self.state.stored_objects {
            if !self.same_allocation(object, *container) || stored.ownership == Ownership::Consumed {
                continue;
            }
            // Unknown count or slot bounds grant no containment proof. For a
            // constant-keyed slot, reuse an existing scalar/bounds fact rather
            // than inventing a new scalar or treating capacity as length.
            let inside = count.is_some_and(|count| match slot {
                SlotIdentity::Dynamic(index) => self.strict_less(*index, count),
                SlotIdentity::Constant(index) => {
                    self.state.constants.get(&count).is_some_and(|count| index < count)
                        || self.state.constants.iter().any(|(value, constant)|
                            constant == index && self.strict_less(*value, count))
                }
            });
            if !inside { return self.unsupported(operation); }
        }
        for (identity, range) in &self.state.range_states {
            if !self.same_allocation(object, identity.object)
                || !range.initialized || range.ownership == Ownership::Consumed
            {
                // A moved-out range retains its extent as history, not as
                // live child ownership. It must not prevent source cleanup.
                continue;
            }
            if range.zero_width || self.is_zero(identity.length) { continue; }
            // A checked zero-start prefix may have lost its original zero SSA
            // value at a join. Non-prefix ranges need the ordinary exact-end
            // judgment. Unknown bounds fail closed; no extent is guessed.
            let inside = count.is_some_and(|count| {
                if range.zero_start { self.less_equal(identity.length, count) }
                else { self.range_within(identity.start, identity.length, count) }
            });
            if !inside { return self.unsupported(operation); }
        }
        Ok(())
    }

    fn require_owned_slots_initialized(

        &self,
        object: ValueId,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
        if self.state.static_data_objects.contains(&object) {
            return Ok(());
        }
        let state = self.live_object(object)?;
        let allocation_identity = state.identity;
        self.require_owned_edges_within_prefix(object, state, operation)?;
        if state.initialized_owned_slots {
            return Ok(());
        }
        // A closure record leaves the function that built it (a call
        // argument, a return, a stored child, a drop) only with its code
        // slot stored.
        if self.closure_layouts.contains_key(&state.layout)
            && !self.checked_scalar_slot_stored(object, SlotIdentity::Constant(0))
        {
            return Err(InitialCheckError::InvalidClosureSlot {
                function: self.function.id,
                object,
            });
        }
        let descriptor = self.descriptor(state.layout)?;
        if !self.unit_fields_initialized(object, state.layout)? {
            return self.unsupported(operation);
        }
        let Some(element) = descriptor.kind().packed_element_layout() else {
            // A fixed layout with owned pointer fields is initialized once
            // every field holds a stored reference (the summary flag above,
            // or the per-slot facts before consolidation).
            let pointer_fields = self.fixed_pointer_fields(state.layout)?;
            if pointer_fields.iter().all(|(index, _)| {
                let slot = SlotIdentity::Constant(*index);
                self.stored_slot_by_identity(object, slot).is_some()
                    || self.nullable_slot_absent(object, slot)
            }) {
                return Ok(());
            }
            return self.unsupported(operation);
        };
        // Scalar storage has no child reference tokens to account for. In
        // particular RawByte and BoolValueSlot are storage layouts, not
        // first-class SafeWIR scalar values. Do not interpret them as values.
        if self.descriptor(element)?.kind() == &LayoutKind::Scalar(ScalarKind::Unit) {
            let Extent::Counted { count, .. } = state.extent else { return self.unsupported(operation) };
            return if self.unit_prefix_initialized(object, count) { Ok(()) } else { self.unsupported(operation) };
        }
        if matches!(self.descriptor(element)?.kind(), LayoutKind::Scalar(_)) {
            return Ok(());
        }
        let Extent::Counted { count, .. } = state.extent else {
            return self.unsupported(operation);
        };
        if self.is_zero(count) {
            return Ok(());
        }
        if self.state.range_states.iter().any(|(identity, range)| {
            // Prefix facts follow SSA tokens through CFG edges and aliases,
            // while initialization belongs to backing storage. Resolve the
            // fact's token before comparing; comparing either raw token ids or
            // a token id directly with ObjectIdentity loses one of those cases.
            self.state.objects.get(&identity.object)
                .is_some_and(|candidate| candidate.identity == allocation_identity)
                && self.same_scalar(identity.length, count)
                && range.element == element
                && range.initialized
                && range.zero_start
        }) && !self.state.stored_objects.keys().any(|(stored_object, _)| {
            self.state.objects.get(stored_object)
                .is_some_and(|candidate| candidate.identity == allocation_identity)
        }) {
            return Ok(());
        }
        let Some(count) = self.state.constants.get(&count).copied() else {
            return self.unsupported(operation);
        };
        let pointee = self.owned_pointer_slot_pointee(element)?;
        let mut initialized_slots = BTreeSet::new();
        for ((stored_object, slot), stored) in &self.state.stored_objects {
            if !self.state.objects.get(stored_object)
                .is_some_and(|candidate| candidate.identity == allocation_identity)
            {
                continue;
            }
            let SlotIdentity::Constant(index) = slot else {
                return self.unsupported(operation);
            };
            if *index >= count
                || stored.layout != pointee
                || stored.ownership == Ownership::Consumed
                || !Self::is_managed_storage(stored)
            {
                return self.unsupported(operation);
            }
            initialized_slots.insert(*index);
        }
        if u64::try_from(initialized_slots.len()).ok() == Some(count) {
            Ok(())
        } else {
            self.unsupported(operation)
        }
    }

    fn require_destination_range_empty(&self, range: CheckedRange) -> Result<(), InitialCheckError> {
        if !self.is_zero(range.start)
            || self.state.stored_objects.keys().any(|(object, _)| *object == range.object)
            || self.state.range_states.iter().any(|(identity, state)| {
                identity.object == range.object
                    && state.initialized
                    && !self.is_zero(identity.length)
            })
            || !self.state.range_states.iter().any(|(identity, state)| {
                identity.object == range.object
                    && state.initialized
                    && state.zero_start
                    && self.is_zero(identity.length)
            })
        {
            return self.unsupported("MoveRange(destination-initialized)");
        }
        Ok(())
    }

    fn clone_range(
        &mut self,
        destination: &Place,
        source: &Place,
        count: ValueId,
    ) -> Result<(), InitialCheckError> {
        self.require_type(count, ValueType::Size)?;
        let destination = self.check_typed_range(destination, AccessMode::Write, "CloneRange")?;
        let source = self.check_typed_range(source, AccessMode::Read, "CloneRange")?;
        self.check_range_pair(destination, source, count, "CloneRange")?;

        if destination.ownership != Ownership::Unique
            || self.state.static_data_objects.contains(&destination.object)
        {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: destination.object,
            });
        }
        if self.value_type_for_layout(destination.element)?.is_memory_bearing() {
            // Cloning a pointer-bearing prefix retains every element (the
            // backend traps on a zero or saturated count, so a partial clone
            // never survives). Both prefixes then share their elements.
            let cloned = self.require_initialized_range(source, destination.element)?;
            self.require_destination_range_empty(destination)?;
            // A zero-width prefix fact carries no class of its own. The
            // destination is a fresh reference-counted list; the source is
            // any managed object (a parameter's class is unknown but its
            // elements are managed, and the retain loop respects the static
            // boundary).
            let source_state = self.live_object(source.object)?.clone();
            if !Self::is_managed_storage(&source_state)
                || destination.allocation_class != Some(AllocationClass::ReferenceCounted)
            {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: source.object,
                });
            }
            let shared = self.state.stored_objects.keys()
                .filter(|(object, _)| self.same_allocation(source.object, *object))
                .copied()
                .collect::<Vec<_>>();
            for key in shared {
                self.state.stored_objects.get_mut(&key).expect("key exists").ownership = Ownership::Shared;
            }
            let shared_ranges = self.state.range_states.keys()
                .filter(|identity| self.same_allocation(source.object, identity.object))
                .copied()
                .collect::<Vec<_>>();
            for identity in shared_ranges {
                self.state.range_states.get_mut(&identity).expect("range exists").ownership = Ownership::Shared;
            }
            {
                let destination_identity = self.state.objects.get(&destination.object).map(|state| state.identity);
                let MemoryState { objects, range_states, .. } = &mut self.state;
                range_states.retain(|identity, _| {
                    objects.get(&identity.object).map(|state| state.identity) != destination_identity
                });
            }
            self.state.range_states.insert(
                RangeIdentity { object: destination.object, start: destination.start, length: destination.length },
                RangeState {
                    element: destination.element,
                    ownership: Ownership::Shared,
                    allocation_class: cloned.allocation_class.or(source.allocation_class),
                    initialized: true,
                    zero_width: self.is_zero(count),
                    zero_start: self.is_zero(destination.start),
                },
            );
            return Ok(());
        }
                if self.descriptor(destination.element)?.kind() == &LayoutKind::Scalar(ScalarKind::Unit) {
            let state = self.live_object(source.object)?;
            let complete = match state.extent {
                Extent::Counted { count, .. } => self.unit_prefix_initialized(source.object, count),
                _ => false,
            };
            if !complete && !(self.is_zero(source.start) && self.unit_prefix_initialized(source.object, count)) {
                return self.unsupported("CloneRange(uninitialized Unit)");
            }
            self.state.range_states.insert(
                RangeIdentity { object: destination.object, start: destination.start, length: count },
                RangeState { element: destination.element, ownership: Ownership::Unique, allocation_class: None,
                    initialized: true, zero_width: self.is_zero(count), zero_start: self.is_zero(destination.start) },
            );
        }
        // Scalar cloning is an ordinary value copy.

        self.require_scalar_range_element(destination.element, "CloneRange")?;
        Ok(())
    }

    fn check_typed_range(
        &mut self,
        place: &Place,
        required_access: AccessMode,
        operation: &'static str,
    ) -> Result<CheckedRange, InitialCheckError> {
        let Projection::Slice { start, length } = place.projection() else {
            return self.unsupported("range(non-slice)");
        };
        if place.access() != required_access {
            return self.unsupported("range(access)");
        }
        self.promote_header_extent(place.object())?;
        let state = self.live_object(place.object())?.clone();
        self.require_ref_type(place.object(), state.layout)?;
        self.require_type(*start, ValueType::Size)?;
        self.require_type(*length, ValueType::Size)?;
        let Extent::Counted {
            count: logical_count,
            capacity,
            stride_bytes,
            ..
        } = state.extent
        else {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: place.object(),
            });
        };
        let descriptor = self.descriptor(state.layout)?;
        let Some(element) = descriptor.kind().packed_element_layout() else {
            return self.unsupported("range(non-packed-layout)");
        };
        // A pointer transfer names the value stored in the authenticated slot,
        // not a reference to the containing list. Scalar range copies retain
        // their collection-view type; they carry no child ownership tokens.
        let expected = if operation == "MoveRange"
            && matches!(self.descriptor(element)?.kind(), LayoutKind::RuntimePointerValueSlot { .. })
        {
            ValueType::Ref(self.owned_pointer_slot_pointee(element)?)
        } else {
            ValueType::Ref(state.layout)
        };
        if place.value_type() != expected {
            return Err(InitialCheckError::TypeMismatch {
                function: self.function.id,
                value: place.object(),
                expected,
                actual: place.value_type(),
            });
        }
        let LayoutSize::Dynamic { stride, .. } = descriptor.size() else {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: place.object(),
            });
        };
        if stride != stride_bytes {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: place.object(),
            });
        }
        let bound = if required_access == AccessMode::Write { capacity } else { logical_count };
        if !self.range_within(*start, *length, bound) {
            return Err(InitialCheckError::MissingBoundsGuard {
                function: self.function.id,
                index: *start,
                count: logical_count,
            });
        }
        let _ = operation;
        Ok(CheckedRange {
            object: place.object(),
            start: *start,
            length: *length,
            layout: state.layout,
            element,
            identity: state.identity,
            ownership: state.ownership,
            allocation_class: state.allocation_class,
        })
    }

    fn check_range_pair(
        &self,
        destination: CheckedRange,
        source: CheckedRange,
        count: ValueId,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
        if destination.layout != source.layout
            || destination.element != source.element
            || destination.length != count
            || source.length != count
        {
            return self.unsupported(match operation {
                "MoveRange" => "MoveRange(incompatible-range)",
                "CloneRange" => "CloneRange(incompatible-range)",
                _ => "range(incompatible-range)",
            });
        }
        // There is no overlap mode in the v1 operation. Distinct allocation
        // identities are therefore required; same-root aliases are rejected
        // even when a caller claims that their slices do not overlap.
        if destination.identity == source.identity {
            return self.unsupported(match operation {
                "MoveRange" => "MoveRange(overlap)",
                "CloneRange" => "CloneRange(overlap)",
                _ => "range(overlap)",
            });
        }
        Ok(())
    }

    fn require_scalar_range_element(
        &self,
        element: crate::layout::LayoutId,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
        let element_type = self.value_type_for_layout(element)?;
        if element_type.is_memory_bearing() {
            return self.unsupported(match operation {
                "MoveRange" => "MoveRange(pointer-bearing element)",
                "CloneRange" => "CloneRange(pointer-bearing element)",
                _ => "range(pointer-bearing element)",
            });
        }
        Ok(())
    }

    fn range_within(&self, start: ValueId, length: ValueId, count: ValueId) -> bool {
        let from_body_start = self.state.constants.get(&start) == Some(&0)
            && self.less_equal(length, count);
        let guarded_exact_end = self.state.exact_additions.iter().any(
            |(end, (left, right))| {
                ((*left == start && *right == length) || (*left == length && *right == start))
                    && self.less_equal(*end, count)
            },
        );
        let constant_range = self
            .state
            .constants
            .get(&start)
            .zip(self.state.constants.get(&length))
            .zip(self.state.constants.get(&count))
            .is_some_and(|((start, length), count)| {
                start.checked_add(*length).is_some_and(|end| end <= *count)
            });
        from_body_start || guarded_exact_end || constant_range
    }

    fn check_byte_slice(
        &mut self,
        place: &Place,
        required_access: AccessMode,
    ) -> Result<ValueId, InitialCheckError> {
        let Projection::Slice { start, length } = place.projection() else {
            return self.unsupported("CopyBytes(non-slice)");
        };
        if place.access() != required_access {
            return self.unsupported("CopyBytes(access)");
        }
        self.promote_header_extent(place.object())?;
        let object = self.live_object(place.object())?.clone();
        self.require_ref_type(place.object(), object.layout)?;
        if place.value_type() != ValueType::Ref(object.layout)
            || !self.is_raw_byte_buffer(object.layout)?
        {
            return self.unsupported("CopyBytes(non-byte layout)");
        }
        if required_access == AccessMode::Write
            && (object.ownership != Ownership::Unique
                || self.state.static_data_objects.contains(&place.object()))
        {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: place.object(),
            });
        }
        self.require_type(*start, ValueType::Size)?;
        self.require_type(*length, ValueType::Size)?;
        let Extent::Counted { count, stride_bytes: 1, .. } = object.extent else {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: place.object(),
            });
        };
        let logical_count = self
            .state
            .header_values
            .get(&(place.object(), HeaderField::Length))
            .copied()
            .unwrap_or(count);
        if !self.range_within(*start, *length, logical_count) {
            return Err(InitialCheckError::MissingBoundsGuard {
                function: self.function.id,
                index: *start,
                count: logical_count,
            });
        }
        Ok(*length)
    }

    fn is_raw_byte_buffer(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<bool, InitialCheckError> {
        let LayoutKind::RuntimeBuffer { element } = self.descriptor(layout)?.kind() else {
            return Ok(false);
        };
        Ok(matches!(
            self.descriptor(*element)?.kind(),
            LayoutKind::Scalar(ScalarKind::RawByte)
        ))
    }

    fn consume_call_move(
        &mut self,
        source: ValueId,
        parameter: ParameterOwnership,
    ) -> Result<(), InitialCheckError> {
        let state = self.live_object(source)?;
        let permitted = match parameter {
            ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack => state.ownership == Ownership::Unique,
            ParameterOwnership::Shared | ParameterOwnership::WriteBack => {
                matches!(state.ownership, Ownership::Unique | Ownership::Shared)
            }
            _ => false,
        };
        if !permitted {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: source,
            });
        }
        if matches!(parameter, ParameterOwnership::Shared | ParameterOwnership::WriteBack)
            && self.is_shared_immutable_storage(source, state) {
            self.state.objects
                .get_mut(&source)
                .expect("live object exists")
                .ownership = Ownership::Consumed;
            return Ok(());
        }
        // Unique ABI parameters remain concrete RC authority only.
        if state.allocation_class != Some(AllocationClass::ReferenceCounted) {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object: source,
            });
        }
        if matches!(parameter, ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack)
            && !self.is_sole_live_token(source, state.identity)
        {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: source,
            });
        }
        self.state.objects
            .get_mut(&source)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        Ok(())
    }

    /// Returns true when this operation belongs to the memory judgment.
    pub(super) fn check(&mut self, operation: &Operation) -> Result<bool, InitialCheckError> {
        match operation {
            Operation::ClosureOwner(owner) => {
                use crate::canonical_safewir::closure_owner_contract::OwnerOperation;
                match owner {
                    OwnerOperation::Construct { captures, .. } => {
                        for capture in captures {
                            if matches!(value_type(self.function, *capture)?, ValueType::Ref(_)) {
                                self.require_owned_slots_initialized(*capture, "Construct(uninitialized managed capture)")?;
                                self.live_object(*capture)?;
                            }
                        }
                    }
                    OwnerOperation::CloneEnvironment { destination, .. } => {
                        let ValueType::Ref(layout) = value_type(self.function, *destination)? else {
                            return self.unsupported("CloneEnvironment(non-reference)");
                        };
                        // The owner registry check establishes the exact
                        // environment layout before this memory judgment.
                        let extent = self.parameter_extent(layout)?;
                        self.state.objects.insert(*destination, ObjectState {
                            identity: *destination, layout, extent, ownership: Ownership::Shared,
                            allocation_class: Some(AllocationClass::ReferenceCounted),
                            shared_immutable: true, initialized_owned_slots: true,
                        });
                    }
                    OwnerOperation::ReadCapability { .. }
                    | OwnerOperation::ReadOwner { .. }
                    | OwnerOperation::ReadTag { .. }
                    | OwnerOperation::ReadCallable { .. }
                    | OwnerOperation::Clone { .. }
                    | OwnerOperation::Drop { .. }
                    | OwnerOperation::NewList { .. }
                    | OwnerOperation::ListGet { .. }
                    | OwnerOperation::ListLength { .. }
                    | OwnerOperation::CloneList { .. }
                    | OwnerOperation::ListPush { .. }
                    | OwnerOperation::ListSet { .. }
                    | OwnerOperation::DropList { .. } => {}
                }
                Ok(true)
            }
            Operation::ConstInt { destination, value } => {
                if let Ok(value) = u64::try_from(*value) {
                    self.state.constants.insert(*destination, value);
                }
                Ok(false)
            }
            Operation::ConstSize { destination, value } => {
                self.state.constants.insert(*destination, *value);
                Ok(false)
            }
            Operation::NarrowSize { destination, value } => {
                if let Some(value) = self.state.constants.get(value).copied()
                    && value <= u64::from(u32::MAX)
                {
                    self.state.constants.insert(*destination, value);
                }
                Ok(false)
            }
            Operation::Arithmetic {
                destination,
                operator: ArithmeticOperator::Add,
                left,
                right,
                overflow: OverflowBehavior::Trap,
            } => {
                self.state.exact_additions.insert(*destination, (*left, *right));
                Ok(false)
            }
            Operation::Arithmetic {
                destination,
                operator: ArithmeticOperator::Add,
                left,
                right,
                overflow: OverflowBehavior::Wrap,
            } => {
                if self.proven_nonwrapping_size_add(*left, *right) {
                    self.state.exact_additions.insert(*destination, (*left, *right));
                }
                Ok(false)
            }
            Operation::Compare { destination, operator, left, right } => {
                self.state.comparisons.insert(
                    *destination,
                    Comparison {
                        operator: *operator,
                        left: *left,
                        right: *right,
                    },
                );
                Ok(false)
            }
            Operation::Guard { condition, kind } => {
                let comparison = self.state.comparisons.get(condition).copied().ok_or(
                    InitialCheckError::InvalidGuard {
                        function: self.function.id,
                        condition: *condition,
                        kind: *kind,
                    },
                )?;
                if matches!(*kind, GuardKind::Bounds | GuardKind::Arithmetic) {
                    if !matches!(
                        comparison.operator,
                        ComparisonOperator::LessUnsigned
                            | ComparisonOperator::LessEqualUnsigned
                    ) {
                        return Err(InitialCheckError::InvalidGuard {
                            function: self.function.id,
                            condition: *condition,
                            kind: *kind,
                        });
                    }
                    self.state.guards
                        .insert((comparison.operator, comparison.left, comparison.right));
                }
                Ok(false)
            }
            Operation::Alloc { destination, layout, extent, class } => {
                self.allocate(*destination, *layout, extent, *class)?;
                Ok(true)
            }
            Operation::StaticData { destination, layout, extent, .. } => {
                self.static_data(*destination, *layout, extent)?;
                Ok(true)
            }
            Operation::Load { destination, place, transfer } => {
                self.load(*destination, place, *transfer)?;
                Ok(true)
            }
            Operation::InitNullableOwned { place } => {
                self.init_nullable_owned(place)?;
                Ok(true)
            }
            Operation::Store { place, value, transfer } => {
                self.store(place, *value, *transfer)?;
                Ok(true)
            }
            Operation::UpdateCount { object, count } => {
                self.update_count(*object, *count)?;
                Ok(true)
            }
            Operation::MoveValue { destination, source }
                if value_type(self.function, *source)?.is_memory_bearing() =>
            {
                self.move_value(*destination, *source)?;
                Ok(true)
            }
            Operation::CloneRef { destination, source } => {
                self.clone_ref(*destination, *source)?;
                Ok(true)
            }
            Operation::Release { object } => {
                self.release(*object)?;
                Ok(true)
            }
            Operation::DropRef { object } => {
                self.drop_ref(*object)?;
                Ok(true)
            }
            Operation::Reclaim { object } => {
                self.reclaim(*object)?;
                Ok(true)
            }
            Operation::CopyBytes { destination, source, length } => {
                self.copy_bytes(destination, source, *length)?;
                Ok(true)
            }
            Operation::MoveRange { destination, source, count } => {
                self.move_range(destination, source, *count)?;
                Ok(true)
            }
            Operation::CloneRange { destination, source, count } => {
                self.clone_range(destination, source, *count)?;
                Ok(true)
            }
            Operation::Retain { .. } => Err(InitialCheckError::UnsupportedMemoryOperation {
                function: self.function.id,
                operation: operation_name(operation),
            }),
            _ => Ok(false),
        }
    }

    fn allocate(
        &mut self,
        destination: ValueId,
        layout: crate::layout::LayoutId,
        extent: &Extent,
        class: AllocationClass,
    ) -> Result<(), InitialCheckError> {
        if class == AllocationClass::Scratch {
            return Err(InitialCheckError::UnsupportedMemoryOperation {
                function: self.function.id,
                operation: "Alloc(Scratch)",
            });
        }
        self.require_ref_type(destination, layout)?;
        let descriptor = self.descriptor(layout)?;
        self.check_extent(destination, extent, descriptor, false)?;
        let pointer_element = descriptor
            .kind()
            .packed_element_layout()
            .filter(|element| {
                self.value_type_for_layout(*element)
                    .is_ok_and(|value_type| value_type.is_memory_bearing())
            });
        self.state.objects.insert(
            destination,
            ObjectState {
                identity: destination,
                layout,
                extent: extent.clone(),
                ownership: Ownership::Unique,
                allocation_class: Some(class),
                shared_immutable: false,
                initialized_owned_slots: false,
            },
        );
        if let Some(element) = pointer_element {
            let Extent::Counted { count, .. } = extent else {
                return self.unsupported("Alloc(pointer collection without counted extent)");
            };
            if self.is_zero(*count) {
                self.state.range_states.insert(
                    RangeIdentity { object: destination, start: *count, length: *count },
                    RangeState {
                        element,
                        ownership: Ownership::Unique,
                        allocation_class: None,
                        initialized: true,
                        zero_width: true,
                        zero_start: true,
                    },
                );
            }
        }
        Ok(())
    }

    fn static_data(
        &mut self,
        destination: ValueId,
        layout: crate::layout::LayoutId,
        extent: &Extent,
    ) -> Result<(), InitialCheckError> {
        self.require_ref_type(destination, layout)?;
        // Static bytes cannot authenticate a closure table index. Closure
        // records must initialize their code lane through the checked path.
        if self.closure_layouts.contains_key(&layout) {
            return Err(InitialCheckError::InvalidClosureSlot {
                function: self.function.id,
                object: destination,
            });
        }
        let descriptor = self.descriptor(layout)?;
        self.check_extent(destination, extent, descriptor, true)?;
        self.state.objects.insert(
            destination,
            ObjectState {
                identity: destination,
                layout,
                extent: extent.clone(),
                ownership: Ownership::Shared,
                allocation_class: None,
                shared_immutable: false,
                initialized_owned_slots: false,
            },
        );
        self.state.static_data_objects.insert(destination);
        Ok(())
    }

    fn check_extent(
        &self,
        object: ValueId,
        extent: &Extent,
        descriptor: &LayoutDescriptor,
        allow_prefix: bool,
    ) -> Result<(), InitialCheckError> {
        let valid = match (extent, descriptor.size()) {
            (Extent::FixedBytes(actual), LayoutSize::Fixed(expected)) => {
                *actual == u64::from(expected)
            }
            (
                Extent::Counted {
                    header_bytes,
                    count,
                    capacity,
                    stride_bytes,
                },
                LayoutSize::Dynamic { base, stride },
            ) => {
                *header_bytes == base
                    && *stride_bytes == stride
                    && value_type(self.function, *count)? == ValueType::Size
                    && value_type(self.function, *capacity)? == ValueType::Size
                    && self.less_equal(*count, *capacity)
                    && self.allocation_fits(*capacity, base, stride)
            }
            (Extent::HeaderPrefix { minimum_bytes }, LayoutSize::Dynamic { base, .. }) => {
                allow_prefix && *minimum_bytes == base
            }
            _ => false,
        };
        if !valid {
            if let Extent::Counted { count, capacity, .. } = extent {
                if !self.less_equal(*count, *capacity) {
                    return Err(InitialCheckError::MissingCapacityGuard {
                        function: self.function.id,
                        count: *count,
                        capacity: *capacity,
                    });
                }
                if let LayoutSize::Dynamic { base, stride } = descriptor.size() {
                    if !self.allocation_fits(*capacity, base, stride) {
                        let maximum = (u64::from(u32::MAX) - u64::from(base))
                            / u64::from(stride);
                        return Err(InitialCheckError::MissingAllocationSizeGuard {
                            function: self.function.id,
                            capacity: *capacity,
                            maximum,
                        });
                    }
                }
            }
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            });
        }
        Ok(())
    }

    /// One atomic ownership transition. Live prefix facts are replaced only
    /// after proving completeness and absence of unpublished owned edges.
    /// Moved-out history is not a live edge. Unknown bounds grant no authority.
    fn take_last(&mut self, destination: ValueId, place: &Place, expected: ValueType) -> Result<(), InitialCheckError> {
        self.require_type(destination, expected)?;
        let Projection::Element { index } = place.projection() else { return self.unsupported("TakeLast(non-element)"); };
        let object = place.object();
        let state = self.live_object(object)?.clone();
        if state.ownership != Ownership::Unique
            || state.allocation_class != Some(AllocationClass::ReferenceCounted)
            || !self.is_sole_live_token(object, state.identity)
        { return Err(InitialCheckError::InvalidOwnership { function: self.function.id, object }); }
        let Extent::Counted { header_bytes, count, capacity, stride_bytes } = state.extent else {
            return self.unsupported("TakeLast(uncounted)");
        };
        let Some(element) = self.descriptor(state.layout)?.kind().packed_element_layout() else {
            return self.unsupported("TakeLast(non-list)");
        };
        // check_place already requires index < current logical count. The
        // unique SSA definition must additionally identify that exact tail.
        let last = self.constant_relation(*index, count, |index, count| index.checked_add(1) == Some(count))
            || self.function.blocks.iter().flat_map(|block| &block.operations).any(|operation| {
                matches!(operation, Operation::Arithmetic { destination, operator: ArithmeticOperator::Subtract, left, right, .. }
                    if destination == index && self.same_scalar(*left, count) && self.state.constants.get(right) == Some(&1))
            });
        if !last { return self.unsupported("TakeLast(not-last)"); }
        self.require_owned_slots_initialized(object, "TakeLast(uninitialized prefix)")?;
        if !matches!(expected, ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Size | ValueType::Unit | ValueType::Ref(_)) {
            return self.unsupported("TakeLast(element type)");
        }
        let containers = self.state.objects.iter().filter_map(|(value, candidate)|
            (candidate.identity == state.identity).then_some(*value)).collect::<BTreeSet<_>>();
        self.state.stored_objects.retain(|(container, _), _| !containers.contains(container));
        self.state.range_states.retain(|range, _| !containers.contains(&range.object));
        self.state.initialized_scalar_slots.retain(|(container, _)| !containers.contains(container));
        for candidate in self.state.objects.values_mut().filter(|candidate| candidate.identity == state.identity) {
            candidate.extent = Extent::Counted { header_bytes, count: *index, capacity, stride_bytes };
            candidate.initialized_owned_slots = true;
        }
        if matches!(expected, ValueType::Ref(_)) {
            // Remaining slots are live owning edges, not vacant capacity.
            // Preserve shared children through stores, moves and publication.
            self.state.range_states.insert(
                RangeIdentity { object, start: *index, length: *index },
                RangeState { element, ownership: Ownership::Shared,
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    initialized: true, zero_width: self.is_zero(*index), zero_start: true },
            );
        }
        self.state.header_values.retain(|(container, field), _| !containers.contains(container) || *field != HeaderField::Length);
        self.state.header_values.insert((object, HeaderField::Length), *index);
        if let ValueType::Ref(layout) = expected {
            let extent = self.parameter_extent(layout)?;
            // Transfer one owned reference, never manufacture child exclusivity.
            self.state.objects.insert(destination, ObjectState {
                identity: destination, layout, extent, ownership: Ownership::Shared,
                allocation_class: None, shared_immutable: true, initialized_owned_slots: true,
            });
        }
        Ok(())
    }
    fn load(
        &mut self,
        destination: ValueId,
        place: &Place,
        transfer: LoadTransfer,
    ) -> Result<(), InitialCheckError> {
        if place.access() != AccessMode::Read {
            return self.unsupported("Load(access)");
        }
        let expected = self.check_place(place)?;
        if transfer == LoadTransfer::TakeLast {
            return self.take_last(destination, place, expected);
        }
        if expected == ValueType::Unit && !self.checked_scalar_slot_initialized(place)?
            && self.require_owned_slots_initialized(place.object(), "Load(uninitialized Unit)").is_err() {
            return self.unsupported("Load(uninitialized Unit)");
        }
        // A callable lane is memory-bearing in the ABI sense (it is not an
        // ordinary scalar at a call boundary) but it holds a table index, so
        // its load is a plain scalar copy like any other four-byte lane.
        if let ValueType::Callable(_) = expected {
            if transfer != LoadTransfer::CopyScalar {
                return self.unsupported("Load(closure slot transfer)");
            }
            // The slot must have been stored on every path: a fresh record's
            // code slot holds nothing a call may dispatch through.
            if !self.checked_scalar_slot_initialized(place)? {
                return Err(InitialCheckError::InvalidClosureSlot {
                    function: self.function.id,
                    object: place.object(),
                });
            }
            self.require_type(destination, expected)?;
            return Ok(());
        }
        if !expected.is_memory_bearing() {
            if transfer != LoadTransfer::CopyScalar {
                return self.unsupported("Load(scalar transfer)");
            }
            self.require_type(destination, expected)?;
            if let Projection::Header(field) = place.projection() {
                self.state
                    .header_values
                    .insert((place.object(), *field), destination);
            }
            return Ok(());
        }
        let ValueType::Ref(layout) = expected else {
            return self.unsupported("Load(non-linear reference)");
        };
        self.require_ref_type(destination, layout)?;
        let key = self.reference_slot(place)?;
        let nullable = self.nullable_fixed_field(place)?.is_some();
        if nullable != (transfer == LoadTransfer::ClonePresent) {
            return self.unsupported("Load(nullable transfer mismatch)");
        }
        if transfer == LoadTransfer::ClonePresent {
            let root = self.live_object(place.object())?.clone();
            let stored = self.stored_slot_by_identity(key.0, key.1).cloned();
            if !root.initialized_owned_slots && stored.is_none()
                && !self.nullable_slot_absent(key.0, key.1)
            {
                return self.unsupported("Load(uninitialized nullable slot)");
            }
            if !Self::is_managed_storage(&root) {
                return self.unsupported("Load(nullable unmanaged root)");
            }
            // ClonePresent's runtime presence check dominates both retain and
            // the result. Even a definitely absent slot is legal: that execution
            // traps, so it never reaches the continuation described here.
            let mut result = if let Some(stored) = stored {
                if !Self::is_managed_storage(&stored)
                    || !matches!(stored.ownership, Ownership::Unique | Ownership::Shared)
                {
                    return self.unsupported("Load(nullable invalid child)");
                }
                stored
            } else {
                let extent = match self.descriptor(layout)?.size() {
                    LayoutSize::Fixed(bytes) => Extent::FixedBytes(u64::from(bytes)),
                    LayoutSize::Dynamic { base, .. } => Extent::HeaderPrefix { minimum_bytes: base },
                };
                ObjectState {
                    identity: destination, layout, extent, ownership: Ownership::Shared,
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    shared_immutable: false, initialized_owned_slots: true,
                }
            };
            result.ownership = Ownership::Shared;
            // A recorded present child must no longer appear uniquely owned
            // through any alias of its parent after the clone.
            let aliases = self.state.stored_objects.keys().copied().filter(|(container, slot)| {
                *slot == key.1 && self.same_allocation(key.0, *container)
            }).collect::<Vec<_>>();
            for alias in aliases {
                self.state.stored_objects.get_mut(&alias).expect("recorded slot").ownership = Ownership::Shared;
            }
            self.state.objects.insert(destination, result);
            return Ok(());
        }
        let recorded = self.state.stored_objects.get(&key).cloned().or_else(|| {
            // CloneRef preserves allocation identity. A shared read may use
            // the initialized slot recorded under its original owning token,
            // even after that token was dropped. Do not extend move authority.
            (transfer == LoadTransfer::CloneShared)
                .then(|| self.stored_slot_by_identity(key.0, key.1).cloned())
                .flatten()
        });
        let mut stored = if let Some(stored) = recorded {
            stored
        } else {
            if transfer != LoadTransfer::CloneShared {
                return self.unsupported("Load(dynamic unique move)");
            }
            let index = match place.projection() {
                Projection::Element { index } => *index,
                // A complete fixed-layout object's pointer field holds a
                // reference the object owns; the load takes a shared clone.
                Projection::Field { index } => {
                    let state = self.live_object(place.object())?.clone();
                    if !state.initialized_owned_slots {
                        return self.unsupported("Load(uninitialized reference)");
                    }
                    let pointee = self
                        .fixed_pointer_fields(state.layout)?
                        .into_iter()
                        .find(|(field, _)| *field == u64::from(*index))
                        .map(|(_, pointee)| pointee)
                        .ok_or_else(|| InitialCheckError::InvalidProjection {
                            function: self.function.id,
                            object: place.object(),
                        })?;
                    if !Self::is_managed_storage(&state) {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: place.object(),
                        });
                    }
                    let extent = match self.descriptor(pointee)?.size() {
                        LayoutSize::Fixed(bytes) => Extent::FixedBytes(u64::from(bytes)),
                        LayoutSize::Dynamic { base, .. } => Extent::HeaderPrefix { minimum_bytes: base },
                    };
                    // The child was stored complete (`Store` requires it),
                    // so its own owned slots need no per-slot facts.
                    self.state.objects.insert(destination, ObjectState {
                        identity: destination,
                        layout: pointee,
                        extent,
                        ownership: Ownership::Shared,
                        allocation_class: Some(AllocationClass::ReferenceCounted),
                        shared_immutable: false,
                        initialized_owned_slots: true,
                    });
                    return Ok(());
                }
                _ => return self.unsupported("Load(uninitialized reference)"),
            };
            // A managed parameter's prefix is seeded from its contract once
            // its runtime count is known.
            if let (Extent::Counted { count, .. }, Some(element)) = (
                self.live_object(place.object())?.extent.clone(),
                self.descriptor(self.live_object(place.object())?.layout)?.kind().packed_element_layout(),
            ) {
                let _ = self.seed_managed_prefix(place.object(), count, element)?;
            }
            let range = self.state.range_states.iter().find_map(|(identity, state)| {
                (self.same_allocation(identity.object, place.object())
                    && state.initialized
                    && !state.zero_width
                    && state.zero_start
                    && self.strict_less(index, identity.length))
                    .then_some((*identity, *state))
            }).ok_or(InitialCheckError::UnsupportedMemoryOperation {
                function: self.function.id,
                operation: "Load(uninitialized reference)",
            })?;
            if !matches!(range.1.ownership, Ownership::Unique | Ownership::Shared)
                || range.1.allocation_class != Some(AllocationClass::ReferenceCounted)
            {
                if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                    eprintln!("canonical element load refused: object {:?} prefix {:?} {:?}", place.object(), range.0, range.1);
                }
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: place.object(),
                });
            }
            self.state.range_states.get_mut(&range.0).expect("range exists").ownership = Ownership::Shared;
            let extent = match self.descriptor(layout)?.size() {
                LayoutSize::Fixed(bytes) => Extent::FixedBytes(u64::from(bytes)),
                LayoutSize::Dynamic { base, .. } => Extent::HeaderPrefix { minimum_bytes: base },
            };
            // Every element of an initialized prefix was stored complete
            // (`Store` requires it), so the loaded element is complete.
            let stored = ObjectState {
                identity: destination,
                layout,
                extent,
                ownership: Ownership::Shared,
                allocation_class: range.1.allocation_class,
                shared_immutable: false,
                initialized_owned_slots: true,
            };
            self.state.objects.insert(destination, stored);
            return Ok(());
        };
        match transfer {
            LoadTransfer::ClonePresent | LoadTransfer::TakeLast => unreachable!("handled before ordinary reference loads"),
            LoadTransfer::CloneShared => {
                if !matches!(stored.ownership, Ownership::Unique | Ownership::Shared)
                    || !Self::is_managed_storage(&stored)
                {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: place.object(),
                    });
                }
                stored.ownership = Ownership::Shared;
                // Every recorded name for this physical slot must lose its
                // unique-child claim when the load retains the child.
                let aliases = self.state.stored_objects.keys().copied()
                    .filter(|(container, slot)| {
                        *slot == key.1 && self.same_allocation(key.0, *container)
                    }).collect::<Vec<_>>();
                for alias in aliases {
                    self.state.stored_objects.get_mut(&alias)
                        .expect("recorded slot").ownership = Ownership::Shared;
                }
                self.state.objects.insert(destination, stored);
                Ok(())
            }
            LoadTransfer::MoveUnique => {
                if self.live_object(place.object())?.ownership != Ownership::Unique
                    || stored.ownership != Ownership::Unique
                {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: place.object(),
                    });
                }
                self.state.stored_objects.remove(&key);
                self.state.objects.insert(destination, stored);
                Ok(())
            }
            LoadTransfer::CopyScalar => self.unsupported("Load(reference copy)"),
        }
    }

    fn store(
        &mut self,
        place: &Place,
        value: ValueId,
        transfer: StoreTransfer,
    ) -> Result<(), InitialCheckError> {
        if place.access() != AccessMode::Write {
            return self.unsupported("Store(access)");
        }
        if matches!(place.projection(), Projection::Header(_)) {
            return self.unsupported("Store(header)");
        }
        if self.state.static_data_objects.contains(&place.object()) {
            return Err(InitialCheckError::WritableStaticData {
                function: self.function.id,
                object: place.object(),
            });
        }
        if self.live_object(place.object())?.ownership != Ownership::Unique {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: place.object(),
            });
        }
        let expected = self.check_place(place)?;
        // The only producer of a `Callable` value is `FunctionValue`, so a
        // closure slot store copies a table index into the lane.
        if let ValueType::Callable(_) = expected {
            if transfer != StoreTransfer::CopyScalar {
                return self.unsupported("Store(closure slot transfer)");
            }
            self.require_type(value, expected)?;
            let slot = self.reference_slot(place)?;
            self.state.initialized_scalar_slots.insert(slot);
            return Ok(());
        }
        if !expected.is_memory_bearing() {
            if transfer != StoreTransfer::CopyScalar {
                return self.unsupported("Store(scalar transfer)");
            }
            self.require_type(value, expected)?;
            if expected == ValueType::Unit {
                let slot = self.reference_slot(place)?;
                self.state.initialized_scalar_slots.insert(slot);
            }
            // A byte slot takes the low byte of a `Size`; the path must
            // prove the value fits so no bits are silently dropped.
            if self.is_raw_byte_element_place(place)? && !self.proven_at_most(value, 255) {
                return Err(InitialCheckError::MissingBoundsGuard {
                    function: self.function.id,
                    index: value,
                    count: value,
                });
            }
            return Ok(());
        }
        let ValueType::Ref(layout) = expected else {
            return self.unsupported("Store(non-linear reference)");
        };
        self.require_ref_type(value, layout)?;
        // A stored reference is complete: its own owned slots are initialized,
        // so a later load or drop of it needs no per-slot facts.
        self.require_owned_slots_initialized(value, "Store(uninitialized owned slot)")?;
        let key = self.reference_slot(place)?;
        if self.stored_slot_by_identity(key.0, key.1).is_some()
            || (matches!(place.projection(), Projection::Field { .. })
                && self.live_object(place.object())?.initialized_owned_slots
                && !self.nullable_slot_absent(key.0, key.1))
        {
            return self.unsupported("Store(overwrite reference)");
        }
        if self.same_allocation(place.object(), value) {
            return self.unsupported("Store(self-owned edge)");
        }
        if let Projection::Element { index } = place.projection() {
            let root = self.live_object(place.object())?;
            if root.initialized_owned_slots {
                let Extent::Counted { count, .. } = root.extent else {
                    return self.unsupported("Store(unknown initialized prefix)");
                };
                if !self.less_equal(count, *index) {
                    return self.unsupported("Store(overwrite initialized prefix)");
                }
            }
            if self.state.range_states.iter().any(|(identity, state)| {
                self.same_allocation(identity.object, place.object())
                    && state.initialized
                    && state.ownership != Ownership::Consumed
                    && !state.zero_width && !self.is_zero(identity.length)
                    // Live ranges forbid overwrite; moved-out ranges do not.
                    // Require proved disjointness. Unknown is not permission.
                    && !(if state.zero_start {
                        self.less_equal(identity.length, *index)
                    } else {
                        self.strict_less(*index, identity.start)
                            || self.range_within(identity.start, identity.length, *index)
                    })
            }) {
                return self.unsupported("Store(overwrite initialized prefix)");
            }
        }
        let mut stored = self.live_object(value)?.clone();
        let source_is_static = self.state.static_data_objects.contains(&value);
        if !self.is_shared_immutable_storage(value, &stored) {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object: value,
            });
        }
        // `static_data_objects` is SSA-local. Once stored, retain its managed
        // immutable provenance in the slot state so a later load cannot turn a
        // static child into unproven classless storage.
        stored.shared_immutable |= source_is_static;
        match transfer {
            StoreTransfer::MoveUnique => {
                if stored.ownership != Ownership::Unique {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: value,
                    });
                }
                self.state
                    .objects
                    .get_mut(&value)
                    .expect("live source exists")
                    .ownership = Ownership::Consumed;
            }
            StoreTransfer::CloneShared => {
                if stored.ownership != Ownership::Shared
                    || !Self::is_managed_storage(&stored)
                {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: value,
                    });
                }
                stored.ownership = Ownership::Shared;
            }
            StoreTransfer::CopyScalar => return self.unsupported("Store(reference copy)"),
        }
        let absent_aliases = self.state.nullable_absent_slots.iter().copied().filter(|(container, slot)| {
            *slot == key.1 && self.same_allocation(key.0, *container)
        }).collect::<Vec<_>>();
        for absent in absent_aliases {
            self.state.nullable_absent_slots.remove(&absent);
        }
        self.state.stored_objects.insert(key, stored);
        self.consolidate_fixed_fields(place.object())?;
        Ok(())
    }

    fn init_nullable_owned(&mut self, place: &Place) -> Result<(), InitialCheckError> {
        if place.access() != AccessMode::Write {
            return self.unsupported("InitNullableOwned(access)");
        }
        self.check_place(place)?;
        let root = self.live_object(place.object())?;
        if root.ownership != Ownership::Unique
            || root.allocation_class != Some(AllocationClass::ReferenceCounted)
            || !self.is_sole_live_token(place.object(), root.identity)
        {
            return Err(InitialCheckError::InvalidOwnership { function: self.function.id, object: place.object() });
        }
        let Some(_) = self.nullable_fixed_field(place)? else {
            return self.unsupported("InitNullableOwned(non-nullable slot)");
        };
        let key = self.reference_slot(place)?;
        if self.live_object(place.object())?.initialized_owned_slots
            || self.stored_slot_by_identity(key.0, key.1).is_some()
        {
            return self.unsupported("InitNullableOwned(overwrite live edge)");
        }
        if self.nullable_slot_absent(key.0, key.1) {
            return self.unsupported("InitNullableOwned(duplicate)");
        }
        self.state.nullable_absent_slots.insert(key);
        self.consolidate_fixed_fields(place.object())?;
        Ok(())
    }

    fn nullable_fixed_field(&self, place: &Place) -> Result<Option<crate::layout::LayoutId>, InitialCheckError> {
        let Projection::Field { index } = place.projection() else { return Ok(None) };
        let descriptor = self.descriptor(self.live_object(place.object())?.layout)?;
        Ok(descriptor.fields().get(*index as usize).and_then(|field| match field.kind() {
            crate::layout::FieldKind::Pointer { pointee, slot: PointerSlot::OwnedNullable } => Some(pointee),
            crate::layout::FieldKind::Inline(slot) => match self.layouts.get(slot).map(|slot| slot.kind()) {
                Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::OwnedNullable }) => Some(*pointee),
                _ => None,
            },
            _ => None,
        }))
    }

    /// The owned pointer fields of a fixed layout: `(field index, pointee)`.
    fn fixed_pointer_fields(&self, layout: crate::layout::LayoutId) -> Result<Vec<(u64, crate::layout::LayoutId)>, InitialCheckError> {
        let descriptor = self.descriptor(layout)?;
        if !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. }) {
            return Ok(Vec::new());
        }
        Ok(descriptor
            .fields()
            .iter()
            .enumerate()
            .filter_map(|(index, field)| match field.kind() {
                crate::layout::FieldKind::Pointer { pointee, slot: PointerSlot::Owned | PointerSlot::OwnedNullable } => Some((index as u64, pointee)),
                // A record child interned as an owned pointer value slot is
                // an inline field whose layout is that slot.
                crate::layout::FieldKind::Inline(slot) => match self.layouts.get(slot).map(|slot| slot.kind()) {
                    Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned | PointerSlot::OwnedNullable }) => Some((index as u64, *pointee)),
                    _ => None,
                },
                _ => None,
            })
            .collect())
    }

    /// Once every owned pointer field of a fixed-layout object holds a
    /// stored reference, the object is complete: the summary flag replaces
    /// the per-slot facts (which a join could not reconcile across fresh
    /// identities), and loads and drops rely on the flag from then on.
    fn unit_fields_initialized(&self, object: ValueId, layout: crate::layout::LayoutId) -> Result<bool, InitialCheckError> {
        Ok(self.descriptor(layout)?.fields().iter().enumerate().all(|(index, field)| {
            field.kind() != FieldKind::Scalar(ScalarKind::Unit)
                || self.checked_scalar_slot_stored(object, SlotIdentity::Constant(index as u64))
        }))
    }

    fn consolidate_fixed_fields(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        let layout = self.live_object(object)?.layout;
        let pointer_fields = self.fixed_pointer_fields(layout)?;
        if pointer_fields.is_empty() {
            return Ok(());
        }
        let complete = pointer_fields
            .iter()
            .all(|(index, _)| {
                let slot = SlotIdentity::Constant(*index);
                self.stored_slot_by_identity(object, slot).is_some()
                    || self.nullable_slot_absent(object, slot)
            });
        if !complete || !self.unit_fields_initialized(object, layout)? {
            return Ok(());
        }
        for (index, _) in &pointer_fields {
            self.state.stored_objects.remove(&(object, SlotIdentity::Constant(*index)));
        }
        self.state.objects.get_mut(&object).expect("live object exists").initialized_owned_slots = true;
        Ok(())
    }

    fn reference_slot(
        &self,
        place: &Place,
    ) -> Result<(ValueId, SlotIdentity), InitialCheckError> {
        // A record field is a constant slot; a list element is its index.
        let index = match place.projection() {
            Projection::Element { index } => index,
            Projection::Field { index } => return Ok((place.object(), SlotIdentity::Constant(u64::from(*index)))),
            _ => return self.unsupported("reference slot(non-element)"),
        };
        let slot = self
            .state
            .constants
            .get(index)
            .copied()
            .map(SlotIdentity::Constant)
            .unwrap_or(SlotIdentity::Dynamic(*index));
        Ok((place.object(), slot))
    }

    fn check_place(&mut self, place: &Place) -> Result<ValueType, InitialCheckError> {
        if matches!(place.projection(), Projection::Element { .. }) {
            self.promote_header_extent(place.object())?;
        }
        let object = self.live_object(place.object())?;
        let descriptor = self.descriptor(object.layout)?;
        let derived = match place.projection() {
            Projection::Header(field) => {
                match descriptor.header() {
                    HeaderLayout::PackedList {
                        length_offset,
                        capacity_offset,
                        data_offset,
                        ..
                    } => {
                        let offset = match field {
                            HeaderField::Length => length_offset,
                            HeaderField::Capacity => capacity_offset,
                        };
                        if offset >= data_offset {
                            return self.invalid_projection(place.object());
                        }
                    }
                    HeaderLayout::None
                        if *field == HeaderField::Length
                            && matches!(descriptor.kind(), LayoutKind::RuntimeBuffer { element }
                                if self.descriptor(*element).is_ok_and(|element| element.kind() == &LayoutKind::Scalar(ScalarKind::RawByte)))
                            && descriptor.size() == (LayoutSize::Dynamic { base: 4, stride: 1 }) => {}
                    _ => return self.invalid_projection(place.object()),
                }
                ValueType::Size
            }
            Projection::Element { index } => {
                let Extent::Counted { count, capacity, .. } = &object.extent else {
                    return self.invalid_projection(place.object());
                };
                let count = if place.access() == AccessMode::Write {
                    capacity
                } else {
                    self.state
                        .header_values
                        .get(&(place.object(), HeaderField::Length))
                        .unwrap_or(count)
                };
                self.require_type(*index, ValueType::Size)?;
                if !self.less_than(*index, *count) {
                    return Err(InitialCheckError::MissingBoundsGuard {
                        function: self.function.id,
                        index: *index,
                        count: *count,
                    });
                }
                let element = descriptor
                    .kind()
                    .packed_element_layout()
                    .ok_or(InitialCheckError::InvalidProjection {
                        function: self.function.id,
                        object: place.object(),
                    })?;
                // A raw byte has no value type of its own: a read of one is a
                // zero-extended `Size` (the byte loops behind string equality
                // and character counting), and a write stores a `Size` the
                // path proves fits a byte (`store` demands that proof).
                let raw_byte = self.descriptor(element).is_ok_and(|element| {
                    element.kind() == &LayoutKind::Scalar(ScalarKind::RawByte)
                });
                if matches!(self.descriptor(element)?.kind(), LayoutKind::RuntimePointerValueSlot {
                    semantics: PointerSlot::OwnedNullable, ..
                }) {
                    return self.unsupported("Place(nullable collection element)");
                }
                if raw_byte {
                    ValueType::Size
                } else {
                    self.value_type_for_layout(element)?
                }
            }
            Projection::Field { index } => {
                let field = descriptor.fields().get(*index as usize).ok_or(
                    InitialCheckError::InvalidProjection {
                        function: self.function.id,
                        object: place.object(),
                    },
                )?;
                // The callable lane carries no value type of its own: the
                // closure registry names the signature slot 0 must hold, and
                // an unregistered layout has no readable lane at all.
                match field.kind() {
                    FieldKind::Scalar(ScalarKind::Callable) => {
                        let derived = self.closure_slot_type(place.object(), object.layout)?;
                        if place.value_type() != derived {
                            return Err(InitialCheckError::InvalidClosureSlot {
                                function: self.function.id,
                                object: place.object(),
                            });
                        }
                        derived
                    }
                    kind => self.value_type_for_field(kind)?,
                }
            }
            Projection::Slice { .. } => return self.unsupported("Place(Slice)"),
        };
        if place.value_type() != derived {
            return Err(InitialCheckError::TypeMismatch {
                function: self.function.id,
                value: place.object(),
                expected: derived,
                actual: place.value_type(),
            });
        }
        Ok(derived)
    }

    fn promote_header_extent(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        let state = self.live_object(object)?.clone();
        let Extent::HeaderPrefix { minimum_bytes } = state.extent else {
            return Ok(());
        };
        let LayoutSize::Dynamic { base, stride } = self.descriptor(state.layout)?.size() else {
            return self.invalid_projection(object);
        };
        if minimum_bytes != base || stride == 0 {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            });
        }
        let count = self
            .state.header_values
            .get(&(object, HeaderField::Length))
            .copied()
            .ok_or(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            })?;
        let is_raw_runtime_buffer = matches!(self.descriptor(state.layout)?.kind(), LayoutKind::RuntimeBuffer { element }
            if self.descriptor(*element).is_ok_and(|element| element.kind() == &LayoutKind::Scalar(ScalarKind::RawByte)));
        if is_raw_runtime_buffer {
            if !self.allocation_fits(count, base, stride) {
                return Err(InitialCheckError::MissingAllocationSizeGuard {
                    function: self.function.id,
                    capacity: count,
                    maximum: (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride),
                });
            }
            self.state.objects.get_mut(&object).expect("live object exists").extent = Extent::Counted {
                header_bytes: base,
                count,
                capacity: count,
                stride_bytes: stride,
            };
            return Ok(());
        }
        let capacity = self
            .state.header_values
            .get(&(object, HeaderField::Capacity))
            .copied()
            .ok_or(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            })?;
        if !self.less_equal(count, capacity) {
            return Err(InitialCheckError::MissingCapacityGuard {
                function: self.function.id,
                count,
                capacity,
            });
        }
        if !self.allocation_fits(capacity, base, stride) {
            return Err(InitialCheckError::MissingAllocationSizeGuard {
                function: self.function.id,
                capacity,
                maximum: (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride),
            });
        }
        self.consolidate_pointer_prefix(object, count)?;
        self.state.objects.get_mut(&object).expect("live object exists").extent = Extent::Counted {
            header_bytes: base,
            count,
            capacity,
            stride_bytes: stride,
        };
        Ok(())
    }

    fn update_count(&mut self, object: ValueId, count: ValueId) -> Result<(), InitialCheckError> {
        self.require_type(count, ValueType::Size)?;
        if self.state.static_data_objects.contains(&object) {
            return Err(InitialCheckError::WritableStaticData {
                function: self.function.id,
                object,
            });
        }
        let state = self.live_object(object)?.clone();
        if state.ownership != Ownership::Unique {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object,
            });
        }
        let Extent::Counted { header_bytes, count: old_count, capacity, stride_bytes } = state.extent else {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            });
        };
        if self
            .state.header_values
            .get(&(object, HeaderField::Capacity))
            .is_some_and(|loaded| *loaded != capacity)
        {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            });
        }
        if !self.less_equal(count, capacity) {
            return Err(InitialCheckError::MissingCapacityGuard {
                function: self.function.id,
                count,
                capacity,
            });
        }
        if !self.same_scalar(old_count, count) {
            // A complete value proves only its old logical prefix. Materialize
            // that prefix before invalidating the summary; newly added slots
            // must be justified by the ordinary store/range rules below.
            if let Some(element) = self.descriptor(state.layout)?.kind().packed_element_layout()
                && matches!(self.descriptor(element)?.kind(), LayoutKind::RuntimePointerValueSlot { .. } | LayoutKind::Scalar(ScalarKind::Unit))
            {
                self.seed_managed_prefix(object, old_count, element)?;
            }
            for candidate in self.state.objects.values_mut() {
                if candidate.identity == state.identity {
                    candidate.initialized_owned_slots = false;
                }
            }
        }
        self.consolidate_pointer_prefix(object, count)?;
        self.state.objects.get_mut(&object).expect("live object exists").extent = Extent::Counted {
            header_bytes,
            count,
            capacity,
            stride_bytes,
        };
        self.state.header_values.insert((object, HeaderField::Length), count);
        Ok(())
    }

    /// A managed ABI entry parameter (`initialized_owned_slots`) denotes a
    /// complete logical value: every slot below its runtime count holds an
    /// owned reference-counted element. When such an object has no range
    /// fact yet (its count is read at runtime, never built by stores), seed
    /// the whole initialized prefix from that contract. Returns whether a
    /// range was seeded.
    fn seed_managed_prefix(
        &mut self,
        object: ValueId,
        count: ValueId,
        element: crate::layout::LayoutId,
    ) -> Result<bool, InitialCheckError> {
        let state = self.live_object(object)?;
        let current_count = match state.extent {
            Extent::Counted { count, .. } => Some(count),
            Extent::HeaderPrefix { .. } => self.state.header_values.get(&(object, HeaderField::Length)).copied(),
            _ => None,
        };
        if !state.initialized_owned_slots
            || !current_count.is_some_and(|current| self.same_scalar(current, count))
            || self.state.range_states.iter().any(|(identity, range)| {
                identity.object == object && range.initialized && range.zero_start && !range.zero_width
            })
        {
            return Ok(false);
        }
        self.state.range_states.insert(
            RangeIdentity { object, start: count, length: count },
            RangeState {
                element,
                ownership: Ownership::Unique,
                allocation_class: Some(AllocationClass::ReferenceCounted),
                initialized: true,
                zero_width: false,
                zero_start: true,
            },
        );
        Ok(true)
    }

    fn consolidate_pointer_prefix(
        &mut self,
        object: ValueId,
        count: ValueId,
    ) -> Result<(), InitialCheckError> {
        let layout = self.live_object(object)?.layout;
        let Some(element) = self.descriptor(layout)?.kind().packed_element_layout() else {
            return Ok(());
        };
        if self.descriptor(element)?.kind() == &LayoutKind::Scalar(ScalarKind::Unit) {
            if !self.unit_prefix_initialized(object, count) { return self.unsupported("UpdateCount(uninitialized Unit)"); }
            self.state.objects.get_mut(&object).expect("live object exists").initialized_owned_slots = true;
            return Ok(());
        }
        // Only the catalog's owned pointer slot is a v1 aggregate whose

        // initialized prefix carries object ownership. Raw bytes and boolean
        // value slots are pointer-free storage, not SafeWIR value types, and
        // therefore have no prefix ownership facts to consolidate.
        if !matches!(self.descriptor(element)?.kind(), LayoutKind::RuntimePointerValueSlot { .. }) {
            return Ok(());
        }
        let pointee = self.owned_pointer_slot_pointee(element)?;
        if self.is_zero(count) {
            // The zero-prefix replacement below erases facts by allocation
            // identity, so its precondition must cover exactly that identity.
            // A moved root cannot hide live edges under its old SSA key.
            if self.state.stored_objects.iter().any(|((stored_object, _), stored)|
                self.same_allocation(object, *stored_object) && stored.ownership != Ownership::Consumed)
                || self.state.range_states.iter().any(|(identity, state)| {
                    self.same_allocation(object, identity.object)
                        && state.initialized && state.ownership != Ownership::Consumed
                        && !state.zero_width && !self.is_zero(identity.length)
                })
            {
                return self.unsupported("UpdateCount(pointer shrink with live elements)");
            }
            {
                let object_identity = self.state.objects.get(&object).map(|state| state.identity);
                let MemoryState { objects, range_states, .. } = &mut self.state;
                range_states.retain(|identity, _| {
                    objects.get(&identity.object).map(|state| state.identity) != object_identity
                });
            }
            self.state.range_states.insert(
                RangeIdentity { object, start: count, length: count },
                RangeState {
                    element,
                    ownership: Ownership::Unique,
                    allocation_class: None,
                    initialized: true,
                    zero_width: true,
                    zero_start: true,
                },
            );
            return Ok(());
        }
        if self.seed_managed_prefix(object, count, element)? {
            return Ok(());
        }
        let existing = self.state.range_states.iter().find_map(|(identity, state)| {
            (identity.object == object && state.initialized && state.zero_start)
                .then_some((*identity, *state))
        }).ok_or(InitialCheckError::UnsupportedMemoryOperation {
            function: self.function.id,
            operation: "UpdateCount(pointer prefix missing)",
        })?;
        if self.same_scalar(existing.0.length, count) {
            if existing.0.length != count {
                self.state.range_states.remove(&existing.0);
                self.state.range_states.insert(
                    RangeIdentity { object, start: existing.0.start, length: count },
                    existing.1,
                );
            }
            return Ok(());
        }

        let one_step = self.state.exact_additions.get(&count).is_some_and(|(left, right)| {
            (self.same_scalar(*left, existing.0.length) && self.state.constants.get(right) == Some(&1))
                || (self.same_scalar(*right, existing.0.length) && self.state.constants.get(left) == Some(&1))
        }) || self.constant_relation(existing.0.length, count, |old, new| {
            old.checked_add(1) == Some(new)
        });
        if one_step {
            let key = self.slot_key(object, existing.0.length);
            let stored = self.state.stored_objects.remove(&key).ok_or(
                InitialCheckError::UnsupportedMemoryOperation {
                    function: self.function.id,
                    operation: "UpdateCount(pointer slot uninitialized)",
                },
            )?;
            if stored.layout != pointee {
                return self.unsupported("UpdateCount(pointer slot layout)");
            }
            if stored.ownership != Ownership::Unique {
                return self.unsupported("UpdateCount(pointer slot ownership)");
            }
            if existing.1.allocation_class.is_some()
                && existing.1.allocation_class != stored.allocation_class
            {
                return self.unsupported("UpdateCount(pointer allocation class)");
            }
            self.state.range_states.remove(&existing.0);
            self.state.range_states.insert(
                RangeIdentity { object, start: existing.0.start, length: count },
                RangeState {
                    element,
                    // Appending a unique child cannot make the already
                    // published children unique if that prefix was shared.
                    ownership: existing.1.ownership,
                    allocation_class: stored.allocation_class,
                    initialized: true,
                    zero_width: self.is_zero(count),
                    zero_start: existing.1.zero_start,
                },
            );
            return Ok(());
        }

        let Some(total) = self.state.constants.get(&count).copied() else {
            return self.unsupported("UpdateCount(pointer prefix gap)");
        };
        if !self.is_zero(existing.0.length) {
            return self.unsupported("UpdateCount(pointer prefix gap)");
        }
        let mut allocation_class = None;
        let mut keys = Vec::new();
        for index in 0..total {
            let key = (object, SlotIdentity::Constant(index));
            let stored = self.state.stored_objects.get(&key).ok_or(
                InitialCheckError::UnsupportedMemoryOperation {
                    function: self.function.id,
                    operation: "UpdateCount(pointer slot uninitialized)",
                },
            )?;
            if stored.layout != pointee {
                return self.unsupported("UpdateCount(pointer slot layout)");
            }
            if stored.ownership != Ownership::Unique {
                return self.unsupported("UpdateCount(pointer slot ownership)");
            }
            if allocation_class.is_some() && allocation_class != stored.allocation_class {
                return self.unsupported("UpdateCount(pointer allocation class)");
            }
            allocation_class = stored.allocation_class;
            keys.push(key);
        }
        for key in keys {
            self.state.stored_objects.remove(&key);
        }
        self.state.range_states.remove(&existing.0);
        self.state.range_states.insert(
            RangeIdentity { object, start: existing.0.start, length: count },
            RangeState { element, ownership: Ownership::Unique, allocation_class, initialized: true, zero_width: self.is_zero(count), zero_start: existing.1.zero_start },
        );
        Ok(())
    }

    fn slot_key(&self, object: ValueId, index: ValueId) -> (ValueId, SlotIdentity) {
        (
            object,
            self.state.constants.get(&index).copied()
                .map(SlotIdentity::Constant)
                .unwrap_or(SlotIdentity::Dynamic(index)),
        )
    }

    fn move_value(&mut self, destination: ValueId, source: ValueId) -> Result<(), InitialCheckError> {
        let source_type = value_type(self.function, source)?;
        self.require_type(destination, source_type)?;
        let state = self.live_object(source)?.clone();
        if state.ownership != Ownership::Unique
            || !self.is_sole_live_token(source, state.identity)
        {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: source,
            });
        }
        self.state.objects.insert(destination, state);
        self.state.objects
            .get_mut(&source)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        Ok(())
    }

    fn is_shared_immutable_storage(&self, object: ValueId, state: &ObjectState) -> bool {
        Self::is_managed_storage(state) || self.state.static_data_objects.contains(&object)
    }

    fn is_managed_storage(state: &ObjectState) -> bool {
        match state.allocation_class {
            Some(AllocationClass::ReferenceCounted) => true,
            Some(AllocationClass::Bump | AllocationClass::Scratch) => false,
            None => state.shared_immutable,
        }
    }

    fn clone_ref(
        &mut self,
        destination: ValueId,
        source: ValueId,
    ) -> Result<(), InitialCheckError> {
        let source_type = value_type(self.function, source)?;
        let ValueType::Ref(layout) = source_type else {
            return self.unsupported("CloneRef(scalar)");
        };
        self.require_ref_type(destination, layout)?;
        let source_is_static = self.state.static_data_objects.contains(&source);
        let mut state = self.live_object(source)?.clone();
        if !matches!(state.ownership, Ownership::Unique | Ownership::Shared) {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: source,
            });
        }
        if !self.is_shared_immutable_storage(source, &state) {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object: source,
            });
        }
        state.ownership = Ownership::Shared;
        self.state.objects.insert(destination, state);
        // The clone names the same storage: the source's loaded header facts
        // hold for it, so a later promotion of the clone uses the same ids.
        let header_values = self
            .state
            .header_values
            .iter()
            .filter(|((object, _), _)| *object == source)
            .map(|((_, field), value)| ((destination, *field), *value))
            .collect::<Vec<_>>();
        self.state.header_values.extend(header_values);
        if source_is_static {
            self.state.static_data_objects.insert(destination);
        }
        self.state.objects
            .get_mut(&source)
            .expect("live object exists")
            .ownership = Ownership::Shared;
        Ok(())
    }

    fn release(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        let state = self.live_object(object)?;
        if state.ownership != Ownership::Shared {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object,
            });
        }
        if !self.is_shared_immutable_storage(object, state) {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object,
            });
        }
        self.require_owned_slots_initialized(object, "Release(uninitialized owned slot)")?;
        self.state.objects
            .get_mut(&object)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        Ok(())
    }

    fn drop_ref(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        let state = self.live_object(object)?;
        if !self.is_shared_immutable_storage(object, state) {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object,
            });
        }
        self.require_owned_slots_initialized(object, "DropRef(uninitialized owned slot)")?;
        let descriptor = self.layouts.get(state.layout).ok_or(
            InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object,
            },
        )?;
        if !drop_ref_layout_supported(self.layouts, descriptor.layout_id(), &mut BTreeSet::new()) {
            return self.unsupported("DropRef(pointer-bearing recursive destructor)");
        }
        self.state.objects
            .get_mut(&object)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        Ok(())
    }

    fn reclaim(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        let state = self.live_object(object)?;
        if state.ownership != Ownership::Unique {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object,
            });
        }
        if state.allocation_class != Some(AllocationClass::ReferenceCounted) {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object,
            });
        }
        self.require_owned_slots_initialized(object, "Reclaim(uninitialized owned slot)")?;
        if !self.fixed_pointer_fields(state.layout)?.is_empty() {
            return self.unsupported("Reclaim(pointer-bearing layout requires DropRef)");
        }
        let descriptor = self.descriptor(state.layout)?;
        if let Some(element) = descriptor.kind().packed_element_layout() {
            let element_descriptor = self.descriptor(element)?;
            if matches!(element_descriptor.kind(), LayoutKind::RuntimePointerValueSlot { .. }) {
                return self.unsupported("Reclaim(pointer-bearing layout requires DropRef)");
            }
        }
        if self.state.range_states.iter().any(|(range, state)| {
            range.object == object && state.initialized && !self.is_zero(range.length)
        }) {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object,
            });
        }
        self.state.objects.get_mut(&object).expect("live object exists").ownership = Ownership::Consumed;
        Ok(())
    }

    fn live_object(&self, object: ValueId) -> Result<&ObjectState, InitialCheckError> {
        let state = self.state.objects.get(&object).ok_or(InitialCheckError::InvalidOwnership {
            function: self.function.id,
            object,
        })?;
        if state.ownership == Ownership::Consumed {
            return Err(InitialCheckError::UseAfterConsume {
                function: self.function.id,
                object,
            });
        }
        Ok(state)
    }

    fn less_than(&self, left: ValueId, right: ValueId) -> bool {
        self.state.guards
            .iter()
            .any(|(operator, guarded_left, guarded_right)| {
                *operator == ComparisonOperator::LessUnsigned
                    && self.same_scalar(left, *guarded_left)
                    && self.same_scalar(right, *guarded_right)
            })
    }

    fn strict_less(&self, left: ValueId, right: ValueId) -> bool {
        self.constant_relation(left, right, |left, right| left < right)
            || self.less_than(left, right)
    }

    fn same_scalar(&self, left: ValueId, right: ValueId) -> bool {
        left == right
            || self.state.constants.get(&left).zip(self.state.constants.get(&right))
                .is_some_and(|(left, right)| left == right)
            || self.state.header_values.iter().any(|((object, field), loaded)| {
                let Some(state) = self.state.objects.get(object) else {
                    return false;
                };
                let Extent::Counted { count, .. } = state.extent else {
                    return false;
                };
                let authoritative = match field {
                    HeaderField::Length => count,
                    // Shared meets may retain only capacity >= count, not
                    // equality with the physical capacity header. A capacity
                    // load must never become a logical-length proof here.
                    HeaderField::Capacity => return false,
                };
                (*loaded == left && authoritative == right)
                    || (*loaded == right && authoritative == left)
            })
    }

    /// A range identity's start matches a place's start by scalar identity,
    /// or because both are zero: a zero-start range that came through a loop
    /// header keeps its `zero_start` fact while its start value (a constant
    /// defined in the body) is renamed away.
    fn range_start_matches(&self, identity: &RangeIdentity, state: &RangeState, start: ValueId) -> bool {
        self.same_scalar(identity.start, start) || (state.zero_start && self.is_zero(start))
    }

    fn is_zero(&self, value: ValueId) -> bool {
        self.state.constants.get(&value) == Some(&0)
    }

    /// Whether `place` is an element of a raw-byte buffer (a string byte).
    fn is_raw_byte_element_place(&self, place: &Place) -> Result<bool, InitialCheckError> {
        if !matches!(place.projection(), Projection::Element { .. }) {
            return Ok(false);
        }
        let layout = self.live_object(place.object())?.layout;
        self.is_raw_byte_buffer(layout)
    }

    /// Whether the path proves `value <= bound`: a constant, or an unsigned
    /// guard against a constant no larger than the bound.
    fn proven_at_most(&self, value: ValueId, bound: u64) -> bool {
        if self.state.constants.get(&value).is_some_and(|constant| *constant <= bound) {
            return true;
        }
        self.state.guards.iter().any(|(operator, left, right)| {
            self.same_scalar(value, *left)
                && match operator {
                    ComparisonOperator::LessEqualUnsigned => self.state.constants.get(right).is_some_and(|limit| *limit <= bound),
                    ComparisonOperator::LessUnsigned => self.state.constants.get(right).is_some_and(|limit| *limit <= bound + 1),
                    _ => false,
                }
        })
    }

    fn less_equal(&self, left: ValueId, right: ValueId) -> bool {
        self.constant_relation(left, right, |left, right| left <= right)
            || self.is_zero(left)
            || self
                .state.guards
                .iter()
                .any(|(operator, guarded_left, guarded_right)| {
                    *operator == ComparisonOperator::LessEqualUnsigned
                        && self.same_scalar(left, *guarded_left)
                        && self.same_scalar(right, *guarded_right)
                })
            || self.same_scalar(left, right)
    }

    /// A wrapping addition is exact only when the current path proves that its
    /// mathematical sum fits the `Size` domain.  The operation remains a wrap
    /// in the IR; this merely permits consumers of `exact_additions` to use the
    /// relation when the wrap is unreachable.
    fn proven_nonwrapping_size_add(&self, left: ValueId, right: ValueId) -> bool {
        if !matches!(value_type(self.function, left), Ok(ValueType::Size))
            || !matches!(value_type(self.function, right), Ok(ValueType::Size))
        {
            return false;
        }
        let maximum = u64::from(u32::MAX);
        if let (Some(left), Some(right)) = (
            self.state.constants.get(&left).copied(),
            self.state.constants.get(&right).copied(),
        ) {
            return left.checked_add(right).is_some_and(|sum| sum <= maximum);
        }
        let Some((value, delta)) = self.state.constants.get(&left).copied()
            .map(|delta| (right, delta))
            .or_else(|| self.state.constants.get(&right).copied().map(|delta| (left, delta)))
        else {
            return false;
        };
        self.state.guards.iter().any(|(operator, guarded, bound)| {
            if *guarded != value {
                return false;
            }
            let Some(bound) = self.state.constants.get(bound).copied() else {
                return false;
            };
            match operator {
                ComparisonOperator::LessUnsigned => bound
                    .checked_sub(1)
                    .and_then(|maximum_value| maximum_value.checked_add(delta))
                    .is_some_and(|sum| sum <= maximum),
                ComparisonOperator::LessEqualUnsigned => bound
                    .checked_add(delta)
                    .is_some_and(|sum| sum <= maximum),
                _ => false,
            }
        })
    }

    fn allocation_fits(&self, capacity: ValueId, base: u32, stride: u32) -> bool {
        if stride == 0 {
            return false;
        }
        let maximum = (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride);
        self.state.constants
            .get(&capacity)
            .is_some_and(|capacity| *capacity <= maximum)
            || self.state.guards.iter().any(|(operator, left, right)| {
                *operator == ComparisonOperator::LessEqualUnsigned
                    && *left == capacity
                    && self.state.constants.get(right).copied() == Some(maximum)
            })
    }

    fn constant_relation(
        &self,
        left: ValueId,
        right: ValueId,
        predicate: impl FnOnce(u64, u64) -> bool,
    ) -> bool {
        self.state.constants
            .get(&left)
            .copied()
            .zip(self.state.constants.get(&right).copied())
            .is_some_and(|(left, right)| predicate(left, right))
    }

    fn descriptor(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<&LayoutDescriptor, InitialCheckError> {
        let physical = self.layouts.resolve_nominal(layout)
            .map_err(|_| InitialCheckError::UnknownLayout(layout))?;
        self.layouts
            .get(physical)
            .ok_or(InitialCheckError::UnknownLayout(layout))
    }

    fn value_type_for_layout(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<ValueType, InitialCheckError> {
        match self.descriptor(layout)?.kind() {
            LayoutKind::Scalar(kind) => self.value_type_for_scalar(*kind),
            LayoutKind::RuntimePointerValueSlot {
                pointee,
                semantics: PointerSlot::Owned | PointerSlot::OwnedNullable,
            } => Ok(ValueType::Ref(*pointee)),
            LayoutKind::RuntimePointerValueSlot { .. } => {
                self.unsupported("borrowed or raw pointer slot")
            }
            _ => Ok(ValueType::Ref(layout)),
        }
    }

    /// A range records its storage-slot layout, while its stored token has the
    /// layout of the object referenced by an owned slot. Borrowed and raw slots
    /// never carry transferable v1 ownership tokens.
    fn owned_pointer_slot_pointee(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<crate::layout::LayoutId, InitialCheckError> {
        match self.descriptor(layout)?.kind() {
            LayoutKind::RuntimePointerValueSlot {
                pointee,
                semantics: PointerSlot::Owned,
            } => Ok(*pointee),
            LayoutKind::RuntimePointerValueSlot { .. } => {
                self.unsupported("borrowed or raw pointer slot")
            }
            _ => self.unsupported("owned pointer slot"),
        }
    }

    /// The value type of a closure object's callable lane: the signature the
    /// module registered for that exact layout. Fail-closed -- an unregistered
    /// layout refuses rather than inventing a signature.
    fn closure_slot_type(
        &self,
        object: ValueId,
        layout: crate::layout::LayoutId,
    ) -> Result<ValueType, InitialCheckError> {
        self.closure_layouts
            .get(&layout)
            .map(|signature| ValueType::Callable(*signature))
            .ok_or(InitialCheckError::InvalidClosureSlot {
                function: self.function.id,
                object,
            })
    }

    fn value_type_for_field(&self, field: FieldKind) -> Result<ValueType, InitialCheckError> {
        match field {
            FieldKind::Scalar(kind) => self.value_type_for_scalar(kind),
            FieldKind::Inline(layout) => self.value_type_for_layout(layout),
            FieldKind::Pointer { pointee, .. } => Ok(ValueType::Ref(pointee)),
        }
    }

    fn value_type_for_scalar(&self, kind: ScalarKind) -> Result<ValueType, InitialCheckError> {
        match kind {
            ScalarKind::Unit => Ok(ValueType::Unit),
            ScalarKind::Bool => Ok(ValueType::Bool),
            ScalarKind::Int | ScalarKind::Duration => Ok(ValueType::Int),
            ScalarKind::Float => Ok(ValueType::Float),
            ScalarKind::U32 | ScalarKind::Tag8 | ScalarKind::Tag16 | ScalarKind::Tag32 => {
                Ok(ValueType::Size)
            }
            ScalarKind::V128 => Ok(ValueType::V128),
            ScalarKind::RawByte | ScalarKind::BoolValueSlot | ScalarKind::Callable => {
                self.unsupported("layout scalar")
            }
        }
    }

    fn require_ref_type(
        &self,
        value: ValueId,
        layout: crate::layout::LayoutId,
    ) -> Result<(), InitialCheckError> {
        self.require_type(value, ValueType::Ref(layout))
    }

    fn require_type(&self, value: ValueId, expected: ValueType) -> Result<(), InitialCheckError> {
        let actual = value_type(self.function, value)?;
        if actual != expected {
            return Err(InitialCheckError::TypeMismatch {
                function: self.function.id,
                value,
                expected,
                actual,
            });
        }
        Ok(())
    }

    fn invalid_projection<T>(&self, object: ValueId) -> Result<T, InitialCheckError> {
        Err(InitialCheckError::InvalidProjection {
            function: self.function.id,
            object,
        })
    }

    fn unsupported<T>(&self, operation: &'static str) -> Result<T, InitialCheckError> {
        Err(InitialCheckError::UnsupportedMemoryOperation {
            function: self.function.id,
            operation,
        })
    }
}

fn drop_ref_layout_supported(
    layouts: &LayoutInterner,
    layout: crate::layout::LayoutId,
    visited: &mut BTreeSet<crate::layout::LayoutId>,
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

#[test]
fn recursive_drop_layout_graph_requires_bound_supported_children() {
    let mut layouts = LayoutInterner::new();
    let nominal = layouts.intern_opaque_nominal([23; 32]).unwrap();
    let child = layouts.intern_runtime_pointer_value_slot(nominal, PointerSlot::OwnedNullable).unwrap();
    let physical = layouts.intern_packed_record(vec![child]).unwrap();
    assert!(!drop_ref_layout_supported(&layouts, nominal, &mut BTreeSet::new()));
    layouts.bind_nominal(nominal, physical).unwrap();
    assert!(drop_ref_layout_supported(&layouts, nominal, &mut BTreeSet::new()));
    let missing = layouts.intern_opaque_nominal([24; 32]).unwrap();
    let missing_child = layouts.intern_runtime_pointer_value_slot(missing, PointerSlot::Owned).unwrap();
    let with_missing = layouts.intern_packed_record(vec![missing_child, child]).unwrap();
    assert!(!drop_ref_layout_supported(&layouts, with_missing, &mut BTreeSet::new()), "a closed recursive edge must not hide another unbound child");
    let nullable_list = layouts.intern_packed_list(child, crate::layout::RcHeader::Required).unwrap();
    assert!(!drop_ref_layout_supported(&layouts, nullable_list, &mut BTreeSet::new()), "nullable collection slots remain unsupported");
}

fn remap_value(renames: &BTreeMap<ValueId, ValueId>, value: ValueId) -> ValueId {
    renames.get(&value).copied().unwrap_or(value)
}

fn remap_extent(renames: &BTreeMap<ValueId, ValueId>, extent: &Extent) -> Extent {
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
/// initialized only if both sides say so.
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

fn intersect_map<K: Ord + Copy, V: Clone + PartialEq>(
    left: &BTreeMap<K, V>,
    right: &BTreeMap<K, V>,
) -> BTreeMap<K, V> {
    left
        .iter()
        .filter_map(|(key, value)| (right.get(key) == Some(value)).then(|| (*key, value.clone())))
        .collect()
}

fn meet_shared_phi_origins(
    left: &MemoryState,
    right: &MemoryState,
) -> BTreeMap<ValueId, BTreeSet<ValueId>> {
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
) -> BTreeMap<RangeIdentity, RangeState> {
    existing
        .range_states
        .iter()
        .filter_map(|(identity, current)| {
            let mut candidates = incoming.range_states.iter().filter(|(candidate, next)| {
                candidate.object == identity.object
                    && next.element == current.element
                    // Two zero-start prefixes match by that fact alone: a
                    // start constant defined inside a loop body is renamed
                    // away by the time the prefix reaches the next header.
                    && (current.zero_width
                        || next.zero_width
                        || (((current.zero_start && next.zero_start)
                            || same_scalar_across(
                                existing,
                                identity.start,
                                incoming,
                                candidate.start,
                            ))
                            && same_scalar_across(
                                existing,
                                identity.length,
                                incoming,
                                candidate.length,
                            )))
            });
            let (next_identity, next) = candidates.next()?;
            if candidates.next().is_some() {
                return None;
            }
            if current.zero_width && !next.zero_width {
                return next.initialized.then_some((*next_identity, *next));
            }
            if next.zero_width && !current.zero_width {
                return current.initialized.then_some((*identity, *current));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

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
                .intern_runtime_buffer(element)
                .expect("scalar buffer layout")
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

    #[test]
    fn unique_phi_rebases_distinct_fresh_identities_before_join() {
        with_checker(false, |checker, list| {
            let phi = ValueId::new(99);
            let target = unique_phi_target(phi, list);
            // Each incoming path allocates only its own forwarded object.
            let initial = checker.snapshot();
            checker.state.objects.remove(&DESTINATION);
            let left = checker.transfer_edge(&target, &[SOURCE]).expect("left unique phi");
            checker.restore(initial);
            checker.state.objects.remove(&SOURCE);
            let right = checker.transfer_edge(&target, &[DESTINATION]).expect("right unique phi");

            let joined = checker.meet(&target, &left, &right).expect("fresh identities join through phi");
            assert_eq!(joined.objects[&phi].identity, phi);
            assert_eq!(joined.objects[&phi].ownership, Ownership::Unique);
        });
    }

    #[test]
    fn same_identity_ownership_meet_loses_only_unique_authority() {
        with_checker(false, |checker, list| {
            checker.state.objects.remove(&DESTINATION);
            let unique = checker.snapshot();
            checker.clone_ref(DESTINATION, SOURCE).expect("clone on one predecessor");
            checker.drop_ref(DESTINATION).expect("callee releases its clone");
            let shared = checker.snapshot();
            let mut target = unique_phi_target(ValueId::new(99), list);
            target.parameters.clear();
            let met = checker.meet(&target, &unique, &shared).expect("same object weakens at ordinary join");
            assert_eq!(met.objects[&SOURCE].identity, SOURCE);
            assert_eq!(met.objects[&SOURCE].ownership, Ownership::Shared);
            checker.restore(met);
            assert!(matches!(checker.move_value(DESTINATION, SOURCE), Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })));
            let place = Place::projected(
                SOURCE,
                Projection::Element { index: START },
                ValueType::Size,
                AccessMode::Write,
            );
            assert!(matches!(checker.store(&place, COUNT, super::StoreTransfer::CopyScalar), Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })));
        });
    }

    #[test]
    fn same_identity_ownership_meet_rejects_identity_consumption_and_alias_changes() {
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

    #[test]
    fn unique_phi_rejects_stored_alias() {
        with_checker(false, |checker, list| {
            let phi = ValueId::new(99);
            let target = unique_phi_target(phi, list);
            let alias = checker.state.objects[&SOURCE].clone();
            checker
                .state
                .stored_objects
                .insert((DESTINATION, SlotIdentity::Constant(0)), alias);

            assert!(matches!(
                checker.transfer_edge(&target, &[SOURCE]),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    fn shared_extent_target(layout: crate::layout::LayoutId) -> BasicBlock {
        let mut target = unique_phi_target(ValueId::new(99), layout);
        target.parameters[0].ownership = Some(Ownership::Shared);
        for value in [ValueId::new(100), ValueId::new(101)] {
            target.parameters.push(crate::canonical_safewir::BlockParameter {
                value,
                value_type: ValueType::Size,
                ownership: None,
            });
        }
        target
    }

    #[test]
    fn owned_prefix_summary_does_not_authorize_count_growth() {
        with_checker_capacity(true, 4, |checker, _| {
            checker.state.objects.get_mut(&SOURCE).unwrap().initialized_owned_slots = true;
            checker.state.range_states.retain(|identity, _| identity.object != SOURCE);
            assert!(checker.update_count(SOURCE, LONG_LENGTH).is_err(), "old prefix cannot initialize new slots");
        });
    }

    #[test]
    fn empty_range_cannot_replace_current_owned_prefix_evidence() {
        with_checker_capacity(true, 4, |checker, list| {
            let element = checker.layouts.get(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.retain(|identity, _| identity.object != SOURCE);
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: START },
                RangeState {
                    element, ownership: Ownership::Unique, allocation_class: None,
                    initialized: true, zero_width: true, zero_start: true,
                },
            );
            assert!(!checker.seed_managed_prefix(SOURCE, COUNT, element).unwrap(),
                "an empty initialized range proves no nonempty slot");
            checker.state.objects.get_mut(&SOURCE).unwrap().initialized_owned_slots = true;
            assert!(!checker.seed_managed_prefix(SOURCE, LONG_LENGTH, element).unwrap(),
                "whole-value evidence covers the current count only");
            assert!(checker.seed_managed_prefix(SOURCE, COUNT, element).unwrap());
            assert!(checker.state.range_states.iter().any(|(identity, range)|
                identity.object == SOURCE && identity.length == COUNT
                    && range.initialized && !range.zero_width
                    && range.allocation_class == Some(AllocationClass::ReferenceCounted)));
            assert!(checker.update_count(SOURCE, LONG_LENGTH).is_err(),
                "reconstruction must not authorize growing the initialized prefix");
        });
    }

    #[test]
    fn owned_prefix_summary_is_invalidated_by_move() {
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
            assert!(checker.require_owned_slots_initialized(SOURCE, "moved source").is_err());
            checker.update_count(DESTINATION, COUNT).expect("moved destination is initialized");
            checker.require_owned_slots_initialized(DESTINATION, "moved destination").expect("complete destination");
        });
    }

    #[test]
    fn owned_prefix_summary_at_shared_phi_requires_predecessor_evidence() {
        for empty in [false, true] {
            with_checker(true, |checker, list| {
                let count = if empty {
                    checker.update_count(SOURCE, START).expect("empty initialized prefix");
                    START
                } else {
                    COUNT
                };
                let target = shared_extent_target(list);
                let edge = checker.transfer_edge(&target, &[SOURCE, count, CAPACITY]).expect("shared phi");
                assert_eq!(edge.objects[&target.parameters[0].value].initialized_owned_slots, empty);
            });
        }
    }

    #[test]
    fn shared_extent_meet_preserves_count_before_header_demotion() {
        with_checker_capacity(true, 4, |checker, list| {
            let mut left = checker.snapshot();
            left.objects.get_mut(&SOURCE).unwrap().ownership = Ownership::Shared;
            let mut right = left.clone();
            let Extent::Counted { capacity, .. } = &mut right.objects.get_mut(&SOURCE).unwrap().extent else { unreachable!() };
            *capacity = LONG_LENGTH;
            let mut target = unique_phi_target(ValueId::new(99), list);
            target.parameters.clear();
            let joined = checker.meet(&target, &left, &right).expect("shared count survives capacity disagreement");
            assert!(matches!(joined.objects[&SOURCE].extent, Extent::Counted { count: COUNT, capacity: COUNT, .. }));
            assert_eq!(joined.objects[&SOURCE].ownership, Ownership::Shared);
            assert_eq!(joined.objects[&SOURCE].identity, SOURCE);
        });
    }

    #[test]
    fn shared_phi_capacity_guard_cannot_authorize_read_past_length() {
        with_checker_capacity(true, 4, |checker, list| {
            let mut left = checker.state.objects[&SOURCE].clone();
            left.ownership = Ownership::Shared;
            let mut right = left.clone();
            let Extent::Counted { capacity, .. } = &mut right.extent else { unreachable!() };
            *capacity = LONG_LENGTH;
            let joined = shared_phi_meet(&left, &right).expect("shared capacities meet at count");
            assert!(matches!(joined.extent, Extent::Counted { count: COUNT, capacity: COUNT, .. }));
            checker.state.objects.insert(SOURCE, joined);
            checker.state.header_values.remove(&(SOURCE, HeaderField::Length));
            checker.state.constants.remove(&LENGTH);
            checker.check(&Operation::Load {
                destination: LENGTH,
                place: Place::projected(SOURCE, Projection::Header(HeaderField::Capacity), ValueType::Size, AccessMode::Read),
                transfer: LoadTransfer::CopyScalar,
            }).expect("physical capacity can be read");
            checker.check(&Operation::ConstSize { destination: END, value: 2 }).expect("index between length and capacity");
            checker.state.guards.insert((ComparisonOperator::LessUnsigned, END, LENGTH));
            assert!(!checker.same_scalar(LENGTH, COUNT), "physical capacity is not logical length");
            let LayoutKind::PackedList { element, .. } = checker.layouts.get(list).unwrap().kind() else { unreachable!() };
            let LayoutKind::RuntimePointerValueSlot { pointee, .. } = checker.layouts.get(*element).unwrap().kind() else { unreachable!() };
            let place = Place::projected(SOURCE, Projection::Element { index: END }, ValueType::Ref(*pointee), AccessMode::Read);
            assert!(matches!(checker.check_place(&place), Err(InitialCheckError::MissingBoundsGuard { index: END, .. })));
        });
    }

    #[test]
    fn shared_phi_extent_transport_accepts_exact_and_authenticated_length() {
        with_checker(true, |checker, list| {
            let target = shared_extent_target(list);
            checker.transfer_edge(&target, &[SOURCE, COUNT, CAPACITY]).expect("exact extent lanes");
            checker.state.constants.clear();
            checker.check(&Operation::Load {
                destination: LENGTH,
                place: Place::projected(SOURCE, Projection::Header(HeaderField::Length), ValueType::Size, AccessMode::Read),
                transfer: LoadTransfer::CopyScalar,
            }).expect("authenticated length load");
            let edge = checker.transfer_edge(&target, &[SOURCE, LENGTH, CAPACITY]).expect("equivalent count lane");
            assert!(matches!(edge.objects[&ValueId::new(99)].extent,
                Extent::Counted { count, capacity, .. }
                    if count == ValueId::new(100) && capacity == ValueId::new(101)));
        });
    }

    #[test]
    fn shared_phi_extent_transport_accepts_one_count_identity_and_constants() {
        with_checker(true, |checker, list| {
            checker.state.constants.clear();
            let object = checker.state.objects.get_mut(&SOURCE).unwrap();
            let Extent::Counted { capacity, .. } = &mut object.extent else { unreachable!() };
            *capacity = COUNT;
            checker.check(&Operation::Load {
                destination: LENGTH,
                place: Place::projected(SOURCE, Projection::Header(HeaderField::Length), ValueType::Size, AccessMode::Read),
                transfer: LoadTransfer::CopyScalar,
            }).expect("authenticated shared count");
            let edge = checker.transfer_edge(&shared_extent_target(list), &[SOURCE, LENGTH, START]).expect("one scalar transports both identical bounds");
            assert!(matches!(edge.objects[&ValueId::new(99)].extent,
                Extent::Counted { count, capacity, .. }
                    if count == ValueId::new(100) && capacity == ValueId::new(100)));
        });
        with_checker(true, |checker, list| {
            let edge = checker.transfer_edge(&shared_extent_target(list), &[SOURCE, LENGTH, CAPACITY]).expect("checked equal constants transport count");
            assert!(matches!(edge.objects[&ValueId::new(99)].extent,
                Extent::Counted { count, .. } if count == ValueId::new(100)));
        });
    }

    #[test]
    fn shared_phi_extent_transport_rejects_unproved_replacements() {
        for (read_object, read_field) in [(DESTINATION, HeaderField::Length), (SOURCE, HeaderField::Capacity)] {
            with_checker(true, |checker, list| {
                checker.state.constants.clear();
                checker.check(&Operation::Load {
                    destination: LENGTH,
                    place: Place::projected(read_object, Projection::Header(read_field), ValueType::Size, AccessMode::Read),
                    transfer: LoadTransfer::CopyScalar,
                }).expect("valid but unrelated header read");
                assert!(checker.transfer_edge(&shared_extent_target(list), &[SOURCE, LENGTH, CAPACITY]).is_err());
            });
        }
        with_checker(true, |checker, list| {
            checker.state.constants.clear();
            assert!(checker.transfer_edge(&shared_extent_target(list), &[SOURCE, LENGTH, CAPACITY]).is_err(), "arbitrary Size is not a count proof");
        });
        with_checker(true, |checker, list| {
            checker.state.constants.clear();
            checker.check(&Operation::Load {
                destination: LONG_LENGTH,
                place: Place::projected(SOURCE, Projection::Header(HeaderField::Capacity), ValueType::Size, AccessMode::Read),
                transfer: LoadTransfer::CopyScalar,
            }).expect("physical capacity read");
            assert!(checker.transfer_edge(&shared_extent_target(list), &[SOURCE, COUNT, LONG_LENGTH]).is_err(), "physical capacity is not a replacement for a conservative bound");
        });
        with_checker(true, |checker, list| {
            checker.state.constants.clear();
            checker.state.header_values.clear();
            let object = checker.state.objects.get_mut(&SOURCE).unwrap();
            let Extent::Counted { header_bytes, .. } = object.extent else { unreachable!() };
            object.extent = Extent::HeaderPrefix { minimum_bytes: header_bytes };
            assert!(checker.transfer_edge(&shared_extent_target(list), &[SOURCE, COUNT, CAPACITY]).is_err(), "unpromoted prefix is not a counted extent");
        });
    }

    #[test]
    fn move_value_rejects_shared_source() {
        with_checker(false, |checker, _list| {
            checker.state.objects.remove(&DESTINATION);
            checker.state.objects.get_mut(&SOURCE).expect("source").ownership = Ownership::Shared;

            assert!(matches!(
                checker.move_value(DESTINATION, SOURCE),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn clone_ref_tracks_two_affine_shared_tokens() {
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

    #[test]
    fn clone_ref_rejects_storage_without_refcount_authority() {
        with_checker(false, |checker, _list| {
            checker.state.objects.remove(&DESTINATION);
            checker.state.objects.get_mut(&SOURCE).expect("source").allocation_class = None;

            assert!(matches!(
                checker.clone_ref(DESTINATION, SOURCE),
                Err(InitialCheckError::InvalidAllocationClass { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn constant_successor_publication_keeps_the_existing_prefix_ownership() {
        for ownership in [Ownership::Unique, Ownership::Shared] {
            with_pointer_slot_checker(|checker, list, pointee| {
                // Seed a complete one-element prefix with spare capacity.
                // The new stored child below is a separate unique token.
                checker.state.constants.insert(CAPACITY, 3);
                checker.state.constants.insert(ALIAS_LENGTH, 2);
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
                    RangeState { element, ownership, allocation_class: Some(AllocationClass::ReferenceCounted),
                        initialized: true, zero_width: false, zero_start: true },
                );
                checker.check(&Operation::Store {
                    place: Place::projected(SOURCE, Projection::Element { index: COUNT },
                        ValueType::Ref(pointee), AccessMode::Write),
                    value: CHILD, transfer: StoreTransfer::MoveUnique,
                }).expect("store the next owned child");
                checker.check(&Operation::UpdateCount { object: SOURCE, count: ALIAS_LENGTH })
                    .expect("constant one-step publication");
                assert_eq!(checker.state.range_states[&RangeIdentity {
                    object: SOURCE, start: START, length: ALIAS_LENGTH,
                }].ownership, ownership);
            });
        }
    }

    #[test]
    fn constant_prefix_publication_rejects_gaps_and_missing_children() {
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

    #[test]
    fn cloned_pointer_list_reads_initialized_slot_after_original_token_drop() {
        with_pointer_slot_checker(|checker, _list, pointee| {
            checker.check(&Operation::Store {
                place: Place::projected(SOURCE, Projection::Element { index: START },
                    ValueType::Ref(pointee), AccessMode::Write),
                value: CHILD,
                transfer: StoreTransfer::MoveUnique,
            }).expect("initialize owned child");
            checker.clone_ref(DESTINATION, SOURCE).expect("retain list");
            checker.drop_ref(SOURCE).expect("retire original token");
            checker.check(&Operation::Load {
                destination: LOADED,
                place: Place::projected(DESTINATION, Projection::Element { index: START },
                    ValueType::Ref(pointee), AccessMode::Read),
                transfer: LoadTransfer::CloneShared,
            }).expect("read through live retained token");
            assert_eq!(checker.state.objects[&LOADED].ownership, Ownership::Shared);
            assert_eq!(checker.stored_slot_by_identity(SOURCE, SlotIdentity::Constant(0))
                .expect("same physical slot").ownership, Ownership::Shared);
            assert!(checker.check(&Operation::Load {
                destination: CHILD,
                place: Place::projected(DESTINATION, Projection::Element { index: START },
                    ValueType::Ref(pointee), AccessMode::Read),
                transfer: LoadTransfer::MoveUnique,
            }).is_err(), "retaining the list never grants a unique move");
        });
    }

    #[test]
    fn cloned_pointer_list_reads_only_live_initialized_prefixes() {
        for initialized in [false, true] {
            with_pointer_slot_checker(|checker, list, pointee| {
                let element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
                checker.state.range_states.insert(
                    RangeIdentity { object: SOURCE, start: START, length: COUNT },
                    RangeState { element, ownership: if initialized { Ownership::Unique } else { Ownership::Consumed },
                        allocation_class: Some(AllocationClass::ReferenceCounted), initialized,
                        zero_width: false, zero_start: true },
                );
                checker.clone_ref(DESTINATION, SOURCE).expect("retain list token");
                let result = checker.check(&Operation::Load {
                    destination: LOADED,
                    place: Place::projected(DESTINATION, Projection::Element { index: START },
                        ValueType::Ref(pointee), AccessMode::Read),
                    transfer: LoadTransfer::CloneShared,
                });
                assert_eq!(result.is_ok(), initialized,
                    "a consumed or uninitialized prefix supplies no read authority: {result:?}");
            });
        }
    }

    #[test]
    fn pointer_slot_initialization_does_not_follow_layout_equality() {
        with_pointer_slot_checker(|checker, list, pointee| {
            checker.check(&Operation::Store {
                place: Place::projected(SOURCE, Projection::Element { index: START },
                    ValueType::Ref(pointee), AccessMode::Write),
                value: CHILD,
                transfer: StoreTransfer::MoveUnique,
            }).expect("initialize source child");
            let extent = checker.state.objects[&SOURCE].extent.clone();
            checker.check(&Operation::Alloc { destination: DESTINATION, layout: list, extent,
                class: AllocationClass::ReferenceCounted }).expect("independent allocation");
            assert!(matches!(checker.check(&Operation::Load {
                destination: LOADED,
                place: Place::projected(DESTINATION, Projection::Element { index: START },
                    ValueType::Ref(pointee), AccessMode::Read),
                transfer: LoadTransfer::CloneShared,
            }), Err(InitialCheckError::UnsupportedMemoryOperation {
                operation: "Load(uninitialized reference)", ..
            })));
        });
    }

    #[test]
    fn shared_static_storage_clones_and_drops_without_mutation_authority() {
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

    #[test]
    fn shared_boundary_union_allows_only_immutable_token_operations() {
        with_checker(false, |checker, _list| {
            checker.state.objects.remove(&DESTINATION);
            let source = checker.state.objects.get_mut(&SOURCE).expect("source");
            source.ownership = Ownership::Shared;
            source.allocation_class = None;
            source.shared_immutable = true;

            checker.clone_ref(DESTINATION, SOURCE).expect("RC-or-static union clones safely");
            assert!(matches!(
                checker.reclaim(SOURCE),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
            checker.drop_ref(SOURCE).expect("union drop dispatches safely at runtime");
            checker.drop_ref(DESTINATION).expect("union clone drop dispatches safely at runtime");
        });
    }

    #[test]
    fn release_rejects_shared_value_without_refcount_authority() {
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

    #[test]
    fn move_value_rejects_live_object_alias() {
        with_checker(false, |checker, _list| {
            let alias = checker.state.objects[&SOURCE].clone();
            checker.state.objects.insert(DESTINATION, alias);

            assert!(matches!(
                checker.move_value(DESTINATION, SOURCE),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn move_value_rejects_stored_alias() {
        with_checker(false, |checker, _list| {
            checker.state.objects.remove(&DESTINATION);
            let alias = checker.state.objects[&SOURCE].clone();
            checker
                .state
                .stored_objects
                .insert((DESTINATION, SlotIdentity::Constant(0)), alias);

            assert!(matches!(
                checker.move_value(DESTINATION, SOURCE),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn return_contract_accepts_only_sole_unique_fresh_object() {
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

    #[test]
    fn return_contract_rejects_fresh_allocation_class_mismatch() {
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

    #[test]
    fn affine_boundary_rejects_duplicate_owned_return_lanes() {
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

    #[test]
    fn affine_boundary_rejects_invented_call_storage_class() {
        with_checker(false, |checker, _list| {
            for class in [None, Some(AllocationClass::Bump), Some(AllocationClass::Scratch)] {
                checker.state.objects.get_mut(&SOURCE).unwrap().allocation_class = class;
                for ownership in [ParameterOwnership::Unique, ParameterOwnership::Shared] {
                    assert!(matches!(
                        checker.consume_call_move(SOURCE, ownership),
                        Err(InitialCheckError::InvalidAllocationClass { object: SOURCE, .. })
                    ));
                }
            }
            checker.state.objects.get_mut(&SOURCE).unwrap().allocation_class =
                Some(AllocationClass::ReferenceCounted);
            checker.consume_call_move(SOURCE, ParameterOwnership::Shared).unwrap();
            assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Consumed);
        });
    }

    #[test]
    fn affine_boundary_rejects_invented_shared_result_class() {
        with_checker(false, |checker, list| {
            let source = checker.state.objects.get_mut(&SOURCE).unwrap();
            source.ownership = Ownership::Shared;
            source.allocation_class = Some(AllocationClass::Bump);
            assert!(matches!(
                checker.check_return(&[SOURCE], &[ResultContract::Shared { layout: list }]),
                Err(InitialCheckError::InvalidAllocationClass { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn owned_result_contract_accepts_unique_rc_without_inventing_a_clone() {
        with_checker(false, |checker, list| {
            checker.drop_ref(DESTINATION).unwrap();
            checker.check_return(&[SOURCE], &[ResultContract::Shared { layout: list }]).unwrap();
            checker.check_exit(&[SOURCE]).unwrap();
            assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Unique);
            assert!(matches!(
                checker.check_return(&[SOURCE, SOURCE], &[
                    ResultContract::Shared { layout: list },
                    ResultContract::Shared { layout: list },
                ]),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn affine_boundary_join_preserves_release_obligations_on_both_edges() {
        with_checker(false, |checker, _list| {
            let block = &checker.function.blocks[0];
            let left = checker.snapshot();
            let mut right = left.clone();
            right.objects.remove(&SOURCE);
            for (a, b) in [(&left, &right), (&right, &left)] {
                assert!(matches!(
                    checker.meet(block, a, b),
                    Err(InitialCheckError::IncompatibleMemoryJoin { object: SOURCE, .. })
                ));
            }
            checker.drop_ref(SOURCE).unwrap();
            let consumed = checker.snapshot();
            checker.meet(block, &consumed, &right).unwrap();
        });
    }

    #[test]
    fn affine_boundary_join_does_not_forget_owned_heap_slots() {
        with_checker(false, |checker, _list| {
            let block = &checker.function.blocks[0];
            let left = checker.snapshot();
            let mut right = left.clone();
            right.stored_objects.insert(
                (DESTINATION, SlotIdentity::Constant(0)),
                right.objects[&SOURCE].clone(),
            );
            for (a, b) in [(&left, &right), (&right, &left)] {
                assert!(matches!(
                    checker.meet(block, a, b),
                    Err(InitialCheckError::IncompatibleMemoryJoin { object: DESTINATION, .. })
                ));
            }
        });
    }

    #[test]
    fn affine_boundary_tail_call_requires_no_untransferred_tokens() {
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
            let functions = BTreeMap::from([(function.id, function)]);
            assert!(matches!(
                super::super::check_terminator(
                    function, &terminator, &signature, &signatures, &functions, &BTreeMap::new(), checker,
                ),
                Err(InitialCheckError::UnconsumedReference { .. })
            ));
            checker.drop_ref(SOURCE).unwrap();
            checker.drop_ref(DESTINATION).unwrap();
            super::super::check_terminator(
                function, &terminator, &signature, &signatures, &functions, &BTreeMap::new(), checker,
            ).unwrap();
        });
    }



    #[test]
    fn guarded_wrapping_size_add_records_an_exact_relation() {
        with_checker(false, |checker, _list| {
            checker.state.constants.remove(&START);
            checker.state.guards.insert((
                ComparisonOperator::LessUnsigned,
                START,
                CAPACITY,
            ));
            checker
                .check(&Operation::Arithmetic {
                    destination: END,
                    operator: ArithmeticOperator::Add,
                    left: START,
                    right: LENGTH,
                    overflow: OverflowBehavior::Wrap,
                })
                .expect("guarded wrapping addition is checked");
            assert_eq!(checker.state.exact_additions.get(&END), Some(&(START, LENGTH)));
        });
    }

    #[test]
    fn unchecked_wrapping_size_add_does_not_record_an_exact_relation() {
        with_checker(false, |checker, _list| {
            checker.state.constants.remove(&START);
            checker
                .check(&Operation::Arithmetic {
                    destination: END,
                    operator: ArithmeticOperator::Add,
                    left: START,
                    right: LENGTH,
                    overflow: OverflowBehavior::Wrap,
                })
                .expect("unchecked wrapping addition remains representable");
            assert!(!checker.state.exact_additions.contains_key(&END));
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

    #[test]
    fn nullable_owned_checker_requires_initialization_and_checked_presence() {
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
    }

    #[test]
    fn nullable_owned_checker_store_replaces_absence_not_a_live_edge_after_move() {
        with_nullable_checker(|checker, child| {
            checker.check(&Operation::InitNullableOwned { place: nullable_field(SOURCE, child, AccessMode::Write) }).unwrap();
            checker.check(&Operation::MoveValue { destination: DESTINATION, source: SOURCE }).unwrap();
            let write = nullable_field(DESTINATION, child, AccessMode::Write);
            checker.check(&Operation::Store { place: write.clone(), value: CHILD, transfer: StoreTransfer::MoveUnique }).unwrap();
            assert!(!checker.nullable_slot_absent(DESTINATION, SlotIdentity::Constant(0)));
            checker.check(&Operation::Load {
                destination: LOADED, place: nullable_field(DESTINATION, child, AccessMode::Read), transfer: LoadTransfer::ClonePresent,
            }).unwrap();
            assert!(matches!(checker.check(&Operation::Store { place: write.clone(), value: LOADED, transfer: StoreTransfer::CloneShared }),
                Err(InitialCheckError::UnsupportedMemoryOperation { operation: "Store(overwrite reference)", .. })));
            assert!(checker.check(&Operation::InitNullableOwned { place: write }).is_err());
            checker.check(&Operation::DropRef { object: LOADED }).unwrap();
            checker.check(&Operation::DropRef { object: DESTINATION }).unwrap();
        });
    }

    #[test]
    fn nullable_owned_checker_keeps_ordinary_completed_fields_non_overwritable() {
        with_fixed_pointer_checker(PointerSlot::Owned, |checker, child| {
            let write = nullable_field(SOURCE, child, AccessMode::Write);
            checker.check(&Operation::Store { place: write.clone(), value: CHILD, transfer: StoreTransfer::MoveUnique }).unwrap();
            assert!(checker.state.objects[&SOURCE].initialized_owned_slots);
            assert!(checker.state.stored_objects.is_empty());
            checker.check(&Operation::Alloc {
                destination: LOADED, layout: child, extent: Extent::FixedBytes(8),
                class: AllocationClass::ReferenceCounted,
            }).unwrap();
            assert!(matches!(checker.check(&Operation::Store { place: write, value: LOADED, transfer: StoreTransfer::MoveUnique }),
                Err(InitialCheckError::UnsupportedMemoryOperation { operation: "Store(overwrite reference)", .. })));
        });
    }

    #[test]
    fn nullable_owned_checker_refuses_nonnullable_and_shared_initialization() {
        with_pointer_slot_checker(|checker, _list, pointee| {
            assert!(checker.check(&Operation::InitNullableOwned {
                place: Place::projected(SOURCE, Projection::Element { index: START }, ValueType::Ref(pointee), AccessMode::Write),
            }).is_err());
        });
        with_nullable_checker(|checker, child| {
            checker.check(&Operation::CloneRef { destination: DESTINATION, source: SOURCE }).unwrap();
            assert!(checker.check(&Operation::InitNullableOwned { place: nullable_field(SOURCE, child, AccessMode::Write) }).is_err());
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
            (COUNT, 1),
            (CAPACITY, 1),
            (START, 0),
            (LENGTH, 1),
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

    #[test]
    fn scalar_move_range_is_bounds_checked_and_admitted() {
        with_checker(false, |checker, list| {
            assert_eq!(
                checker.check(&Operation::MoveRange {
                    destination: range_place(DESTINATION, list, LENGTH, AccessMode::Write),
                    source: range_place(SOURCE, list, LENGTH, AccessMode::Read),
                    count: LENGTH,
                }),
                Ok(true)
            );
        });
    }

    #[test]
    fn scalar_clone_range_is_ownership_safe_and_admitted() {
        with_checker(false, |checker, list| {
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

    #[test]
    fn range_transfer_rejects_a_slice_past_logical_length() {
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

    #[test]
    fn range_transfer_rejects_same_storage_without_overlap_policy() {
        with_checker(false, |checker, list| {
            assert!(matches!(
                checker.check(&Operation::MoveRange {
                    destination: range_place(SOURCE, list, LENGTH, AccessMode::Write),
                    source: range_place(SOURCE, list, LENGTH, AccessMode::Read),
                    count: LENGTH,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "MoveRange(overlap)",
                    ..
                })
            ));
        });
    }

    #[test]
    fn pointer_bearing_ranges_fail_closed_without_element_transfer_state() {
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
                Err(InitialCheckError::UnsupportedMemoryOperation { .. })
            ));
        });
    }

    #[test]
    fn complete_managed_abi_range_can_be_cloned_without_local_slot_facts() {
        with_pointer_slot_checker(|checker, list, _pointee| {
            let pointer_element = checker
                .descriptor(list)
                .unwrap()
                .kind()
                .packed_element_layout()
                .unwrap();
            let source = checker.state.objects.get_mut(&SOURCE).unwrap();
            source.ownership = Ownership::Shared;
            source.allocation_class = None;
            source.shared_immutable = true;
            source.initialized_owned_slots = true;
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: START, length: ZERO },
                RangeState {
                    element: pointer_element,
                    ownership: Ownership::Unique,
                    allocation_class: None,
                    initialized: true,
                    zero_width: true,
                    zero_start: true,
                },
            );
            assert_eq!(
                checker.check(&Operation::CloneRange {
                    destination: pointer_range_place(DESTINATION, list, COUNT, AccessMode::Write),
                    source: pointer_range_place(SOURCE, list, COUNT, AccessMode::Read),
                    count: COUNT,
                }),
                Ok(true)
            );
        });
    }

    #[test]
    fn static_data_cannot_authenticate_registered_closure_layout() {
        let closure = crate::layout::LayoutId::from_bytes([0x63; 32]);
        let layouts = LayoutInterner::new();
        let function = Function {
            id: crate::canonical_safewir::FunctionId::new(0),
            signature: crate::canonical_safewir::FunctionSignatureId::new(0),
            entry: crate::canonical_safewir::BlockId::new(0),
            value_types: [(SOURCE, ValueType::Ref(closure))].into_iter().collect(),
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
        let closure_layouts = [(closure, signature.id)].into_iter().collect();
        let mut checker = MemoryChecker::new(&function, &layouts, &signature, &closure_layouts)
            .expect("checker construction");
        assert_eq!(
            checker.static_data(SOURCE, closure, &Extent::FixedBytes(8)),
            Err(InitialCheckError::InvalidClosureSlot {
                function: function.id,
                object: SOURCE,
            })
        );
    }

    #[test]
    fn dynamic_pointer_clone_range_shares_initialized_elements() {
        with_pointer_slot_checker(|checker, list, pointee| {
            let pointer_element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: START, length: ZERO },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: None, initialized: true, zero_width: true, zero_start: true },
            );
            checker.state.constants.remove(&COUNT);
            checker.state.guards.insert((ComparisonOperator::LessEqualUnsigned, COUNT, CAPACITY));
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: COUNT },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
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
        });
    }

    #[test]
    fn pointer_move_range_rejects_container_as_element_type() {
        with_pointer_slot_checker(|checker, list, _pointee| {
            assert!(matches!(
                checker.check(&Operation::MoveRange {
                    destination: pointer_range_place(DESTINATION, list, COUNT, AccessMode::Write),
                    source: pointer_range_place(SOURCE, list, COUNT, AccessMode::Read),
                    count: COUNT,
                }),
                Err(InitialCheckError::TypeMismatch { .. })
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

    #[test]
    fn dynamic_pointer_move_range_transfers_initialized_ownership_once() {
        with_pointer_slot_checker(|checker, list, pointee| {
            let pointer_element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: START, length: ZERO },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: None, initialized: true, zero_width: true, zero_start: true },
            );
            checker.state.constants.remove(&COUNT);
            checker.state.guards.insert((ComparisonOperator::LessEqualUnsigned, COUNT, CAPACITY));
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: COUNT },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
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
        });
    }

    #[test]
    fn pointer_bearing_reclaim_requires_recursive_drop() {
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

    #[test]
    fn nominal_pointer_element_resolves_to_pointer_bearing_physical_descriptor() {
        with_nominal_pointer_slot_checker( |checker, _list, _pointee, nominal| {
            assert!(matches!(
                checker.descriptor(nominal).unwrap().kind(),
                LayoutKind::RuntimePointerValueSlot { .. }
            ));
        });
    }

    #[test]
    fn admitted_pointer_list_reclaim_still_requires_recursive_drop() {
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

    // The Fresh-result case models a previously checked producer's ABI, using
    // the real call-result transition rather than assigning completeness bits.
    fn with_unpublished_owned_tail(
        prior_completeness: bool,
        test: impl FnOnce(&mut MemoryChecker<'_>, crate::layout::LayoutId),
    ) {
        let mut layouts = LayoutInterner::new();
        let child_layout = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let slot = layouts.intern_runtime_pointer_value_slot(child_layout, PointerSlot::Owned).unwrap();
        let list = layouts.intern_packed_list(slot, RcHeader::Required).unwrap();
        let LayoutSize::Dynamic { base, stride } = layouts.get(list).unwrap().size() else {
            panic!("counted list layout");
        };
        let LayoutSize::Fixed(child_bytes) = layouts.get(child_layout).unwrap().size() else {
            panic!("fixed child layout");
        };
        let maximum = ValueId::new(100);
        let conditions = [ValueId::new(101), ValueId::new(102), ValueId::new(103), ValueId::new(104)];
        let mut value_types = BTreeMap::from([
            (SOURCE, ValueType::Ref(list)), (DESTINATION, ValueType::Ref(list)), (CHILD, ValueType::Ref(child_layout)),
            (COUNT, ValueType::Size), (CAPACITY, ValueType::Size),
            (ZERO, ValueType::Size), (LENGTH, ValueType::Size), (maximum, ValueType::Size),
        ]);
        value_types.extend(conditions.into_iter().map(|value| (value, ValueType::Bool)));
        let function = Function {
            id: crate::canonical_safewir::FunctionId::new(0),
            signature: crate::canonical_safewir::FunctionSignatureId::new(0),
            entry: crate::canonical_safewir::BlockId::new(0),
            value_types,
            blocks: vec![BasicBlock {
                id: crate::canonical_safewir::BlockId::new(0),
                parameters: Vec::new(), operations: Vec::new(),
                terminator: crate::canonical_safewir::Terminator::Return { values: Vec::new() },
            }],
        };
        let signature = FunctionSignature {
            id: function.signature, parameters: Vec::new(), results: Vec::new(),
            contract: CallableContract {
                parameters: Vec::new(), results: Vec::new(), parameter_effects: BTreeMap::new(),
            },
        };
        let closure_layouts = BTreeMap::new();
        let mut checker = MemoryChecker::new(&function, &layouts, &signature, &closure_layouts).unwrap();
        checker.check(&Operation::ConstSize { destination: ZERO, value: 0 }).unwrap();
        checker.check(&Operation::ConstSize { destination: LENGTH, value: 1 }).unwrap();
        if prior_completeness {
            checker.check_call(&[], &[], &[SOURCE], &[ResultContract::Fresh {
                layout: list, class: AllocationClass::ReferenceCounted,
                extent: ReturnedExtent::Counted { header_bytes: base, stride_bytes: stride },
            }]).unwrap();
            for (destination, field) in [(COUNT, HeaderField::Length), (CAPACITY, HeaderField::Capacity)] {
                checker.check(&Operation::Load {
                    destination,
                    place: Place::projected(SOURCE, Projection::Header(field), ValueType::Size, AccessMode::Read),
                    transfer: LoadTransfer::CopyScalar,
                }).unwrap();
            }
            checker.check(&Operation::ConstSize {
                destination: maximum, value: (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride),
            }).unwrap();
            for (destination, (operator, left, right)) in conditions.into_iter().zip([
                (ComparisonOperator::LessEqualUnsigned, COUNT, CAPACITY),
                (ComparisonOperator::LessEqualUnsigned, CAPACITY, maximum),
                (ComparisonOperator::LessUnsigned, ZERO, COUNT),
                (ComparisonOperator::LessUnsigned, COUNT, CAPACITY),
            ]) {
                checker.check(&Operation::Compare { destination, operator, left, right }).unwrap();
                checker.check(&Operation::Guard { condition: destination, kind: GuardKind::Bounds }).unwrap();
            }
            checker.promote_header_extent(SOURCE).unwrap();
            assert!(checker.state.objects[&SOURCE].initialized_owned_slots);
        } else {
            checker.check(&Operation::ConstSize { destination: COUNT, value: 0 }).unwrap();
            checker.check(&Operation::ConstSize { destination: CAPACITY, value: 1 }).unwrap();
            checker.check(&Operation::Compare {
                destination: conditions[0], operator: ComparisonOperator::LessUnsigned,
                left: COUNT, right: CAPACITY,
            }).unwrap();
            checker.check(&Operation::Guard { condition: conditions[0], kind: GuardKind::Bounds }).unwrap();
            checker.check(&Operation::Alloc {
                destination: SOURCE, layout: list,
                extent: Extent::Counted { header_bytes: base, count: COUNT, capacity: CAPACITY, stride_bytes: stride },
                class: AllocationClass::ReferenceCounted,
            }).unwrap();
        }
        checker.check(&Operation::Alloc {
            destination: CHILD, layout: child_layout, extent: Extent::FixedBytes(u64::from(child_bytes)),
            class: AllocationClass::ReferenceCounted,
        }).unwrap();
        test(&mut checker, child_layout);
    }

    #[test]
    fn zero_count_drop_ref_refuses_initialized_unpublished_child() {
        with_unpublished_owned_tail(false, |checker, child_layout| {
            checker.check(&Operation::Store {
                place: Place::projected(SOURCE, Projection::Element { index: COUNT }, ValueType::Ref(child_layout), AccessMode::Write),
                value: CHILD, transfer: StoreTransfer::MoveUnique,
            }).expect("initialization within capacity is legal before publication");
            let pending = checker.snapshot();
            assert!(matches!(checker.check(&Operation::DropRef { object: SOURCE }),
                Err(InitialCheckError::UnsupportedMemoryOperation { operation: "DropRef(uninitialized owned slot)", .. })),
                "length zero must not hide a stored owned edge");
            checker.restore(pending);
            checker.check(&Operation::UpdateCount { object: SOURCE, count: LENGTH })
                .expect("publishing the initialized slot repairs the obligation");
            checker.check(&Operation::DropRef { object: SOURCE }).unwrap();
            checker.check_exit(&[]).unwrap();
        });
    }

    #[test]
    fn prior_completeness_drop_ref_refuses_new_unpublished_child() {
        with_unpublished_owned_tail(true, |checker, child_layout| {
            checker.check(&Operation::Store {
                place: Place::projected(SOURCE, Projection::Element { index: COUNT }, ValueType::Ref(child_layout), AccessMode::Write),
                value: CHILD, transfer: StoreTransfer::MoveUnique,
            }).expect("append slot is outside the old prefix but inside capacity");
            assert!(matches!(checker.check(&Operation::DropRef { object: SOURCE }),
                Err(InitialCheckError::UnsupportedMemoryOperation { operation: "DropRef(uninitialized owned slot)", .. })),
                "prior completeness covers only the published prefix");
        });
    }

    #[test]
    fn prior_completeness_count_decrease_refuses_live_owned_prefix() {
        with_unpublished_owned_tail(true, |checker, _| {
            assert!(matches!(checker.check(&Operation::UpdateCount { object: SOURCE, count: ZERO }),
                Err(InitialCheckError::UnsupportedMemoryOperation { operation: "UpdateCount(pointer shrink with live elements)", .. })),
                "changing a header is not a release of the old prefix");
        });
    }

    #[test]
    fn owned_tail_obligation_follows_allocation_across_root_move() {
        for prior in [false, true] {
            with_unpublished_owned_tail(prior, |checker, child_layout| {
                checker.check(&Operation::Store {
                    place: Place::projected(SOURCE, Projection::Element { index: COUNT }, ValueType::Ref(child_layout), AccessMode::Write),
                    value: CHILD, transfer: StoreTransfer::MoveUnique,
                }).unwrap();
                checker.check(&Operation::MoveValue { destination: DESTINATION, source: SOURCE }).unwrap();
                assert!(checker.check(&Operation::DropRef { object: DESTINATION }).is_err(),
                    "an old SSA key must not hide the moved root's pending child");
            });
        }
    }

    #[test]
    fn owned_tail_zero_shrink_cannot_erase_a_moved_roots_live_prefix() {
        with_unpublished_owned_tail(false, |checker, child_layout| {
            checker.check(&Operation::Store {
                place: Place::projected(SOURCE, Projection::Element { index: COUNT }, ValueType::Ref(child_layout), AccessMode::Write),
                value: CHILD, transfer: StoreTransfer::MoveUnique,
            }).unwrap();
            checker.check(&Operation::UpdateCount { object: SOURCE, count: LENGTH }).unwrap();
            checker.check(&Operation::MoveValue { destination: DESTINATION, source: SOURCE }).unwrap();
            assert!(matches!(checker.check(&Operation::UpdateCount { object: DESTINATION, count: ZERO }),
                Err(InitialCheckError::UnsupportedMemoryOperation { operation: "UpdateCount(pointer shrink with live elements)", .. })),
                "the old root's published range still owns its child");
        });
    }

    #[test]
    fn owned_tail_range_preflight_distinguishes_live_and_moved_out_edges() {
        for moved_out in [false, true] {
            with_unpublished_owned_tail(false, |checker, _| {
                let element = checker.descriptor(checker.state.objects[&SOURCE].layout).unwrap()
                    .kind().packed_element_layout().unwrap();
                // Boundary-state fixture: the same nonempty range either
                // still owns its edges or records a completed move-out.
                checker.state.range_states.insert(
                    RangeIdentity { object: SOURCE, start: ZERO, length: LENGTH },
                    RangeState { element, ownership: if moved_out { Ownership::Consumed } else { Ownership::Unique },
                        allocation_class: Some(AllocationClass::ReferenceCounted), initialized: !moved_out,
                        zero_width: false, zero_start: true },
                );
                assert_eq!(checker.check(&Operation::DropRef { object: SOURCE }).is_ok(), moved_out);
            });
        }
    }

    #[test]
    fn owned_tail_nonprefix_range_requires_a_checked_end_bound() {
        with_unpublished_owned_tail(true, |checker, _| {
            let element = checker.descriptor(checker.state.objects[&SOURCE].layout).unwrap()
                .kind().packed_element_layout().unwrap();
            let identity = RangeIdentity { object: SOURCE, start: ZERO, length: COUNT };
            checker.state.range_states.insert(identity, RangeState {
                element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted),
                initialized: true, zero_width: false, zero_start: false,
            });
            checker.require_owned_slots_initialized(SOURCE, "bounded range").unwrap();
            checker.state.range_states.remove(&identity);
            // Model a join that retained a range but lost its start constant.
            checker.state.constants.remove(&LENGTH);
            checker.state.range_states.insert(RangeIdentity { object: SOURCE, start: LENGTH, length: COUNT }, RangeState {
                element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted),
                initialized: true, zero_width: false, zero_start: false,
            });
            assert!(checker.require_owned_slots_initialized(SOURCE, "unknown range end").is_err());
        });
    }

    #[test]
    fn dynamic_pointer_prefix_cannot_hide_an_unaccounted_child_slot() {
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

    #[test]
    fn pointer_move_range_rejects_uninitialized_source() {
        with_pointer_slot_checker(|checker, list, pointee| {
            assert!(matches!(
                checker.check(&Operation::MoveRange {
                    destination: pointer_range_place(DESTINATION, pointee, COUNT, AccessMode::Write),
                    source: pointer_range_place(SOURCE, pointee, COUNT, AccessMode::Read),
                    count: COUNT,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "MoveRange(uninitialized-source)",
                    ..
                })
            ));
        });
    }

    #[test]
    fn pointer_move_range_rejects_source_reuse_after_consumption() {
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

    #[test]
    fn pointer_move_range_rejects_overlapping_storage_before_transfer() {
        with_pointer_slot_checker(|checker, list, pointee| {
            let pointer_element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: COUNT },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            assert!(matches!(
                checker.check(&Operation::MoveRange {
                    destination: pointer_range_place(SOURCE, pointee, COUNT, AccessMode::Write),
                    source: pointer_range_place(SOURCE, pointee, COUNT, AccessMode::Read),
                    count: COUNT,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "MoveRange(overlap)",
                    ..
                })
            ));
        });
    }

    #[test]
    fn pointer_move_range_rejects_initialized_destination() {
        with_pointer_slot_checker(|checker, list, pointee| {
            let pointer_element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: COUNT },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: START, length: COUNT },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            assert!(matches!(
                checker.check(&Operation::MoveRange {
                    destination: pointer_range_place(DESTINATION, pointee, COUNT, AccessMode::Write),
                    source: pointer_range_place(SOURCE, pointee, COUNT, AccessMode::Read),
                    count: COUNT,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "MoveRange(destination-initialized)",
                    ..
                })
            ));
        });
    }

    #[test]
    fn pointer_move_range_rejects_reload_through_old_equal_constant_identity() {
        with_pointer_slot_checker(|checker, list, pointee| {
            let pointer_element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: LENGTH },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: START, length: ZERO },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: None, initialized: true, zero_width: true, zero_start: true },
            );
            assert_eq!(
                checker.check(&Operation::MoveRange {
                    destination: pointer_range_place(DESTINATION, pointee, ALIAS_LENGTH, AccessMode::Write),
                    source: Place::projected(
                        SOURCE,
                        Projection::Slice { start: ALIAS_START, length: ALIAS_LENGTH },
                        ValueType::Ref(pointee),
                        AccessMode::Read,
                    ),
                    count: ALIAS_LENGTH,
                }),
                Ok(true)
            );
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
            ));
        });
    }

    #[test]
    fn pointer_slot_move_and_clone_preserve_structural_ownership() {
        with_pointer_slot_checker(|checker, _list, pointee| {
            let place = Place::projected(
                SOURCE,
                Projection::Element { index: START },
                ValueType::Ref(pointee),
                AccessMode::Write,
            );
            assert_eq!(
                checker.check(&Operation::Store {
                    place,
                    value: CHILD,
                    transfer: StoreTransfer::MoveUnique,
                }),
                Ok(true)
            );
            assert_eq!(checker.state.objects[&CHILD].ownership, Ownership::Consumed);

            assert_eq!(
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
                Ok(true)
            );
            assert_eq!(checker.state.objects[&LOADED].ownership, Ownership::Shared);
            assert_eq!(
                checker.state.stored_objects[&(SOURCE, SlotIdentity::Constant(0))].ownership,
                Ownership::Shared
            );
        });
    }

    #[test]
    fn store_cannot_overwrite_a_stored_reference() {
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

    #[test]
    fn retain_is_refused_in_v1() {
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

    #[test]
    fn pointer_slot_rejects_scalar_copy_transfer() {
        with_pointer_slot_checker(|checker, _list, pointee| {
            assert!(matches!(
                checker.check(&Operation::Store {
                    place: Place::projected(
                        SOURCE,
                        Projection::Element { index: START },
                        ValueType::Ref(pointee),
                        AccessMode::Write,
                    ),
                    value: CHILD,
                    transfer: StoreTransfer::CopyScalar,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "Store(reference copy)",
                    ..
                })
            ));
        });
    }

    #[test]
    fn byte_copy_requires_a_dominating_dynamic_end_guard() {
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

        assert!(matches!(
            checker.check(&operation),
            Err(InitialCheckError::MissingBoundsGuard { index: START, count: COUNT, .. })
        ));
        checker
            .state
            .guards
            .insert((ComparisonOperator::LessEqualUnsigned, END, COUNT));
        assert_eq!(checker.check(&operation), Ok(true));
    }
}
