use super::*;
use crate::canonical_safewir::{BlockId, FunctionId, Terminator};
use crate::layout::LayoutId;

const ZERO: ValueId = ValueId::new(0);
const ONE: ValueId = ValueId::new(1);
const LEFT_LENGTH: ValueId = ValueId::new(2);
const RIGHT_LENGTH: ValueId = ValueId::new(3);
const LEFT_COUNT: ValueId = ValueId::new(4);
const RIGHT_COUNT: ValueId = ValueId::new(5);
const CAPACITY: ValueId = ValueId::new(6);
const COUNT: ValueId = ValueId::new(7);
const LOADED_CAPACITY: ValueId = ValueId::new(8);
const LIMIT: ValueId = ValueId::new(9);
const NEXT: ValueId = ValueId::new(10);
const SOURCE: ValueId = ValueId::new(11);
const DESTINATION: ValueId = ValueId::new(12);
const CHILD: ValueId = ValueId::new(13);
const ALIAS: ValueId = ValueId::new(14);
const MAXIMUM: ValueId = ValueId::new(15);
const RELOADED_COUNT: ValueId = ValueId::new(17);
const RELOADED_CAPACITY: ValueId = ValueId::new(18);

#[derive(Clone, Copy)]
struct Layouts {
    child: LayoutId,
    element: LayoutId,
    list: LayoutId,
    base: u32,
    stride: u32,
}

impl Layouts {
    fn extent(self, count: ValueId, capacity: ValueId) -> Extent {
        Extent::Counted {
            header_bytes: self.base, count, capacity, stride_bytes: self.stride,
        }
    }

    fn slice(self, object: ValueId, length: ValueId, access: AccessMode) -> Place {
        Place::projected(object, Projection::Slice { start: ZERO, length },
            ValueType::Ref(self.child), access)
    }
}

fn with_checker(test: impl FnOnce(&mut MemoryChecker<'_>, Layouts)) {
    let mut layouts = LayoutInterner::new();
    let child = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let element = layouts.intern_runtime_pointer_value_slot(child, PointerSlot::Owned).unwrap();
    let list = layouts.intern_packed_list(element, RcHeader::Required).unwrap();
    let LayoutSize::Dynamic { base, stride } = layouts.get(list).unwrap().size() else {
        panic!("a pointer list has a counted extent");
    };
    let fixture = Layouts { child, element, list, base, stride };
    let mut value_types: BTreeMap<_, _> = (0..20)
        .map(|id| (ValueId::new(id), ValueType::Size)).collect();
    for object in [SOURCE, DESTINATION, ALIAS] {
        value_types.insert(object, ValueType::Ref(list));
    }
    value_types.insert(CHILD, ValueType::Ref(child));
    value_types.extend((20..30).map(|id| (ValueId::new(id), ValueType::Bool)));
    let function = Function {
        id: FunctionId::new(0), signature: FunctionSignatureId::new(0),
        entry: BlockId::new(0), value_types,
        blocks: vec![BasicBlock {
            id: BlockId::new(0), parameters: Vec::new(), operations: Vec::new(),
            terminator: Terminator::Return { values: Vec::new() },
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
    checker.state.constants.extend([
        (ZERO, 0), (ONE, 1), (CAPACITY, 3), (MAXIMUM, u64::from(u32::MAX)),
        (LIMIT, (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride)),
    ]);
    test(&mut checker, fixture);
}

// The incoming ranges are initialized child-edge facts, not ABI summaries.
// Their count equality is known separately on each edge and disappears at
// meet. This is the same fact loss as duplicate scalar lanes at a source join.
fn edges(
    checker: &MemoryChecker<'_>,
    layout: Layouts,
    left_count: u64,
    right_count: u64,
    ownership: Ownership,
) -> (MemoryState, MemoryState) {
    let edge = |length, count, count_value| {
        let mut state = checker.snapshot();
        state.constants.extend([(length, 1), (count, count_value)]);
        state.objects.insert(SOURCE, ObjectState {
            identity: SOURCE, owner_generation: 0, layout: layout.list, extent: layout.extent(count, CAPACITY),
            ownership, allocation_class: Some(AllocationClass::ReferenceCounted),
            shared_immutable: false, initialized_owned_slots: false,
        });
        state.range_states.insert(RangeIdentity { object: SOURCE, start: ZERO, length }, RangeState {
            element: layout.element, ownership: Ownership::Shared,
            allocation_class: Some(AllocationClass::ReferenceCounted),
            initialized: true, zero_width: false, zero_start: true,
        });
        state
    };
    (edge(LEFT_LENGTH, LEFT_COUNT, left_count), edge(RIGHT_LENGTH, RIGHT_COUNT, right_count))
}

fn meet(checker: &mut MemoryChecker<'_>, left: &MemoryState, right: &MemoryState) {
    let state = checker.meet(&checker.function.blocks[0], left, right).unwrap();
    checker.restore(state);
}

fn joined(checker: &mut MemoryChecker<'_>, layout: Layouts, ownership: Ownership) {
    let (left, right) = edges(checker, layout, 1, 1, ownership);
    meet(checker, &left, &right);
    assert!(matches!(checker.state.objects[&SOURCE].extent, Extent::HeaderPrefix { .. }));
    assert!(!checker.state.constants.contains_key(&LEFT_LENGTH));
    assert!(!checker.state.constants.contains_key(&RIGHT_LENGTH));
    assert_eq!(checker.state.full_current_prefixes.len(), 1);
}

fn guard(checker: &mut MemoryChecker<'_>, id: u32, left: ValueId, right: ValueId, strict: bool) {
    let condition = ValueId::new(id);
    checker.check(&Operation::Compare {
        destination: condition,
        operator: if strict { ComparisonOperator::LessUnsigned } else { ComparisonOperator::LessEqualUnsigned },
        left, right,
    }).unwrap();
    checker.check(&Operation::Guard { condition, kind: GuardKind::Bounds }).unwrap();
}

fn reload(checker: &mut MemoryChecker<'_>, object: ValueId, count: ValueId, capacity: ValueId) {
    for (field, destination) in [(HeaderField::Length, count), (HeaderField::Capacity, capacity)] {
        checker.check(&Operation::Load {
            destination,
            place: Place::projected(object, Projection::Header(field), ValueType::Size, AccessMode::Read),
            transfer: LoadTransfer::CopyScalar,
        }).unwrap();
    }
    // Real header loads plus ordinary explicit bounds, not invented count
    // equality or initialized-prefix assertions.
    guard(checker, 20, count, capacity, false);
    guard(checker, 21, capacity, LIMIT, false);
    guard(checker, 22, count, MAXIMUM, true);
    guard(checker, 23, count, capacity, true);
}

fn destination(checker: &mut MemoryChecker<'_>, layout: Layouts) {
    checker.check(&Operation::Alloc {
        destination: DESTINATION, layout: layout.list,
        extent: layout.extent(ZERO, LOADED_CAPACITY), class: AllocationClass::ReferenceCounted,
    }).unwrap();
}

fn clone_prefix(checker: &mut MemoryChecker<'_>, layout: Layouts) {
    destination(checker, layout);
    checker.check(&Operation::CloneRange {
        // CloneRange uses collection views; pointer MoveRange uses the
        // child-value views supplied by Layouts::slice instead.
        destination: Place::projected(DESTINATION, Projection::Slice { start: ZERO, length: COUNT },
            ValueType::Ref(layout.list), AccessMode::Write),
        source: Place::projected(SOURCE, Projection::Slice { start: ZERO, length: COUNT },
            ValueType::Ref(layout.list), AccessMode::Read),
        count: COUNT,
    }).unwrap();
}

fn store_tail(checker: &mut MemoryChecker<'_>, layout: Layouts, object: ValueId) {
    checker.check(&Operation::Alloc {
        destination: CHILD, layout: layout.child, extent: Extent::FixedBytes(8),
        class: AllocationClass::ReferenceCounted,
    }).unwrap();
    checker.check(&Operation::Store {
        place: Place::projected(object, Projection::Element { index: COUNT },
            ValueType::Ref(layout.child), AccessMode::Write),
        value: CHILD, transfer: StoreTransfer::MoveUnique,
    }).unwrap();
}

fn assert_shared_prefix(checker: &MemoryChecker<'_>, object: ValueId, length: ValueId) {
    let range = checker.state.range_states.iter().find_map(|(identity, range)|
        (identity.object == object && identity.length == length && range.zero_start).then_some(range))
        .expect("the existing prefix must survive under its current count");
    assert!(range.initialized);
    assert_eq!(range.ownership, Ownership::Shared);
    assert_eq!(range.allocation_class, Some(AllocationClass::ReferenceCounted));
}

#[test]
fn full_prefix_meet_header_reload_clone_append_preserves_shared_children() {
    with_checker(|checker, layout| {
        joined(checker, layout, Ownership::Shared);
        reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
        // CloneRange itself requests lazy header promotion, as in the source
        // regression. The old prefix length has no remaining scalar equality.
        clone_prefix(checker, layout);
        assert_shared_prefix(checker, SOURCE, COUNT);
        assert_shared_prefix(checker, DESTINATION, COUNT);
        assert!(!checker.state.full_current_prefixes.iter().any(|range| range.object == DESTINATION),
            "a newly constructed clone does not inherit the source proof");
        checker.check(&Operation::Arithmetic {
            destination: NEXT, operator: ArithmeticOperator::Add,
            left: COUNT, right: ONE, overflow: OverflowBehavior::Wrap,
        }).unwrap();
        assert_eq!(checker.state.exact_additions.get(&NEXT), Some(&(COUNT, ONE)));
        guard(checker, 24, NEXT, LOADED_CAPACITY, false);
        store_tail(checker, layout, DESTINATION);
        checker.check(&Operation::UpdateCount { object: DESTINATION, count: NEXT }).unwrap();
        assert_shared_prefix(checker, DESTINATION, NEXT);
        assert_eq!(checker.state.objects[&CHILD].ownership, Ownership::Consumed);
    });
}

#[test]
fn full_prefix_survives_repeated_header_prefix_meets() {
    with_checker(|checker, layout| {
        joined(checker, layout, Ownership::Shared);
        let edge = checker.snapshot();
        meet(checker, &edge, &edge);
        assert_eq!(checker.state.full_current_prefixes.len(), 1);
        reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
        checker.promote_header_extent(SOURCE).unwrap();
        assert_shared_prefix(checker, SOURCE, COUNT);
    });
}

#[test]
fn partial_prefix_on_either_incoming_edge_is_not_complete() {
    for (left_count, right_count) in [(1, 2), (2, 1), (2, 2)] {
        with_checker(|checker, layout| {
            let (left, right) = edges(checker, layout, left_count, right_count, Ownership::Shared);
            meet(checker, &left, &right);
            assert_eq!(checker.state.range_states.len(), 1, "partial evidence is still retained");
            assert!(checker.state.full_current_prefixes.is_empty());
            reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
            assert!(checker.promote_header_extent(SOURCE).is_err(),
                "only complete evidence on EVERY incoming edge may rebind a prefix");
        });
    }
}

#[test]
fn only_live_initialized_zero_start_ranges_can_be_complete() {
    for case in 0..4 {
        with_checker(|checker, layout| {
            let (mut left, mut right) = edges(checker, layout, 1, 1, Ownership::Shared);
            for edge in [&mut left, &mut right] {
                let range = edge.range_states.values_mut().next().unwrap();
                match case {
                    0 => range.initialized = false,
                    1 => range.ownership = Ownership::Consumed,
                    2 => range.zero_start = false,
                    3 => range.zero_width = true,
                    _ => unreachable!(),
                }
            }
            meet(checker, &left, &right);
            assert!(checker.state.full_current_prefixes.is_empty());
            reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
            assert!(checker.promote_header_extent(SOURCE).is_err());
        });
    }
}

#[test]
fn prior_proof_cannot_authorize_a_consumed_or_changed_range() {
    for case in 0..3 {
        with_checker(|checker, layout| {
            joined(checker, layout, Ownership::Shared);
            let range = checker.state.range_states.values_mut().next().unwrap();
            match case {
                0 => range.ownership = Ownership::Consumed,
                1 => range.initialized = false,
                2 => range.zero_start = false,
                _ => unreachable!(),
            }
            // Even an artificially retained proof cannot resurrect a range
            // whose current ownership/initialization state no longer agrees.
            reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
            assert!(checker.promote_header_extent(SOURCE).is_err());
        });
    }
}

#[test]
fn store_invalidates_current_prefix_proof_before_later_reload() {
    with_checker(|checker, layout| {
        joined(checker, layout, Ownership::Unique);
        reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
        checker.promote_header_extent(SOURCE).unwrap();
        assert!(!checker.state.full_current_prefixes.is_empty());
        store_tail(checker, layout, SOURCE);
        assert!(checker.state.full_current_prefixes.is_empty());
        // A later demotion must not revive the old derived proof after Store.
        checker.state.objects.get_mut(&SOURCE).unwrap().extent = Extent::HeaderPrefix { minimum_bytes: layout.base };
        checker.state.header_values.clear();
        reload(checker, SOURCE, RELOADED_COUNT, RELOADED_CAPACITY);
        assert!(checker.promote_header_extent(SOURCE).is_err());
    });
}

#[test]
fn consumed_call_and_drop_tokens_invalidate_current_prefix_proof() {
    for call in [false, true] {
        with_checker(|checker, layout| {
            joined(checker, layout, Ownership::Shared);
            reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
            checker.promote_header_extent(SOURCE).unwrap();
            if call {
                checker.consume_call_move(SOURCE, ParameterOwnership::Shared).unwrap();
            } else {
                checker.check(&Operation::DropRef { object: SOURCE }).unwrap();
            }
            assert!(checker.state.full_current_prefixes.is_empty());
            assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Consumed);
        });
    }
}

#[test]
fn take_last_invalidates_proof_and_keeps_remaining_child_ownership_shared() {
    with_checker(|checker, layout| {
        joined(checker, layout, Ownership::Unique);
        reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
        checker.promote_header_extent(SOURCE).unwrap();
        // This separate white-box tail test supplies the known one-element
        // count, so the ordinary exact-tail rule needs no subtraction fixture.
        checker.state.constants.insert(COUNT, 1);
        guard(checker, 25, ZERO, COUNT, true);
        checker.check(&Operation::Load {
            destination: CHILD,
            place: Place::projected(SOURCE, Projection::Element { index: ZERO },
                ValueType::Ref(layout.child), AccessMode::Read),
            transfer: LoadTransfer::TakeLast,
        }).unwrap();
        assert!(checker.state.full_current_prefixes.is_empty());
        assert_shared_prefix(checker, SOURCE, ZERO);
        assert_eq!(checker.state.objects[&CHILD].ownership, Ownership::Shared);
        assert!(checker.check(&Operation::UpdateCount { object: SOURCE, count: ONE }).is_err(),
            "the removed slot cannot be republished without initialization");
    });
}

#[test]
fn full_prefix_proof_does_not_allow_partial_move_shrink_or_uninitialized_append() {
    for case in 0..3 {
        with_checker(|checker, layout| {
            joined(checker, layout, Ownership::Unique);
            reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
            checker.promote_header_extent(SOURCE).unwrap();
            let result = match case {
                0 => {
                    destination(checker, layout);
                    checker.check(&Operation::MoveRange {
                        destination: layout.slice(DESTINATION, ZERO, AccessMode::Write),
                        source: layout.slice(SOURCE, ZERO, AccessMode::Read), count: ZERO,
                    })
                }
                1 => checker.check(&Operation::UpdateCount { object: SOURCE, count: ZERO }),
                2 => {
                    checker.check(&Operation::Arithmetic {
                        destination: NEXT, operator: ArithmeticOperator::Add,
                        left: COUNT, right: ONE, overflow: OverflowBehavior::Wrap,
                    }).unwrap();
                    guard(checker, 24, NEXT, LOADED_CAPACITY, false);
                    checker.check(&Operation::UpdateCount { object: SOURCE, count: NEXT })
                }
                _ => unreachable!(),
            };
            assert!(result.is_err(), "complete current prefix is not permission to consume or grow a different range");
        });
    }
}

#[test]
fn full_move_replaces_ranges_without_inheriting_prefix_proof() {
    with_checker(|checker, layout| {
        joined(checker, layout, Ownership::Unique);
        reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
        checker.promote_header_extent(SOURCE).unwrap();
        destination(checker, layout);
        checker.check(&Operation::MoveRange {
            destination: layout.slice(DESTINATION, COUNT, AccessMode::Write),
            source: layout.slice(SOURCE, COUNT, AccessMode::Read), count: COUNT,
        }).unwrap();
        assert!(checker.state.full_current_prefixes.is_empty());
        assert_shared_prefix(checker, DESTINATION, COUNT);
        assert!(checker.state.range_states.iter().any(|(identity, range)|
            identity.object == SOURCE && range.ownership == Ownership::Consumed && !range.initialized));
    });
}

#[test]
fn move_value_preserves_prefix_only_through_actual_allocation_identity() {
    with_checker(|checker, layout| {
        joined(checker, layout, Ownership::Unique);
        checker.check(&Operation::MoveValue { destination: ALIAS, source: SOURCE }).unwrap();
        assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Consumed);
        assert_eq!(checker.state.objects[&ALIAS].identity, SOURCE);
        reload(checker, ALIAS, COUNT, LOADED_CAPACITY);
        checker.promote_header_extent(ALIAS).unwrap();
        assert_shared_prefix(checker, ALIAS, COUNT);
    });
    with_checker(|checker, layout| {
        joined(checker, layout, Ownership::Shared);
        let mut unrelated = checker.state.objects[&SOURCE].clone();
        unrelated.identity = ALIAS;
        checker.state.objects.insert(ALIAS, unrelated);
        let origins = BTreeSet::from([SOURCE, ALIAS]);
        checker.state.shared_phi_origins.extend([(SOURCE, origins.clone()), (ALIAS, origins)]);
        reload(checker, ALIAS, COUNT, LOADED_CAPACITY);
        assert!(checker.promote_header_extent(ALIAS).is_err(),
            "overlapping phi origins do not authenticate another allocation's initialized prefix");
    });
}

#[test]
fn shared_clone_drop_reload_preserves_current_prefix_proof() {
    for release in [false, true] {
        with_checker(|checker, layout| {
            joined(checker, layout, Ownership::Shared);
            // Clone before promotion: ALIAS still has HeaderPrefix when the
            // first token is retired, so it needs the retained range proof.
            checker.check(&Operation::CloneRef { destination: ALIAS, source: SOURCE }).unwrap();
            reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
            checker.promote_header_extent(SOURCE).unwrap();
            assert_shared_prefix(checker, SOURCE, COUNT);
            checker.check(&if release {
                Operation::Release { object: SOURCE }
            } else {
                Operation::DropRef { object: SOURCE }
            }).unwrap();
            assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Consumed);
            assert!(!checker.state.full_current_prefixes.is_empty());
            assert!(matches!(checker.state.objects[&ALIAS].extent, Extent::HeaderPrefix { .. }));
            reload(checker, ALIAS, RELOADED_COUNT, RELOADED_CAPACITY);
            checker.promote_header_extent(ALIAS).unwrap();
            assert_shared_prefix(checker, ALIAS, RELOADED_COUNT);
            assert_eq!(checker.state.objects[&ALIAS].ownership, Ownership::Shared);
        });
    }
}

#[test]
fn final_shared_token_retirement_clears_proof_without_resurrection() {
    for release in [false, true] {
        with_checker(|checker, layout| {
            joined(checker, layout, Ownership::Shared);
            checker.check(&Operation::CloneRef { destination: ALIAS, source: SOURCE }).unwrap();
            reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
            checker.promote_header_extent(SOURCE).unwrap();
            checker.check(&Operation::DropRef { object: SOURCE }).unwrap();
            assert!(!checker.state.full_current_prefixes.is_empty());
            reload(checker, ALIAS, RELOADED_COUNT, RELOADED_CAPACITY);
            checker.promote_header_extent(ALIAS).unwrap();
            checker.check(&if release {
                Operation::Release { object: ALIAS }
            } else {
                Operation::DropRef { object: ALIAS }
            }).unwrap();
            assert!(checker.state.full_current_prefixes.is_empty());
            assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Consumed);
            assert_eq!(checker.state.objects[&ALIAS].ownership, Ownership::Consumed);
            assert!(checker.state.range_states.iter().all(|(identity, range)|
                !range_is_full_current_prefix(&checker.state, identity, range)));
            assert!(checker.check(&Operation::CloneRef {
                destination: DESTINATION, source: ALIAS,
            }).is_err(), "an old storage identity cannot resurrect its last retired token");
        });
    }
}

#[test]
fn shared_retirement_does_not_preserve_proof_through_phi_origin_overlap() {
    with_checker(|checker, layout| {
        joined(checker, layout, Ownership::Shared);
        let mut unrelated = checker.state.objects[&SOURCE].clone();
        unrelated.identity = ALIAS;
        checker.state.objects.insert(ALIAS, unrelated);
        let origins = BTreeSet::from([SOURCE, ALIAS]);
        checker.state.shared_phi_origins.extend([(SOURCE, origins.clone()), (ALIAS, origins)]);
        reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
        checker.promote_header_extent(SOURCE).unwrap();
        checker.check(&Operation::DropRef { object: SOURCE }).unwrap();
        assert!(checker.state.full_current_prefixes.is_empty());
        reload(checker, ALIAS, RELOADED_COUNT, RELOADED_CAPACITY);
        assert!(checker.promote_header_extent(ALIAS).is_err(),
            "a live but physically distinct phi cannot retain another allocation's prefix");
    });
}

#[test]
fn mutation_through_moved_token_invalidates_retired_root_prefix_proof() {
    with_checker(|checker, layout| {
        joined(checker, layout, Ownership::Unique);
        checker.check(&Operation::MoveValue { destination: ALIAS, source: SOURCE }).unwrap();
        reload(checker, ALIAS, COUNT, LOADED_CAPACITY);
        checker.promote_header_extent(ALIAS).unwrap();
        assert!(!checker.state.full_current_prefixes.is_empty());
        assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Consumed);
        assert_eq!(checker.state.objects[&ALIAS].identity, SOURCE);
        store_tail(checker, layout, ALIAS);
        assert!(checker.state.full_current_prefixes.is_empty(),
            "actual mutation still invalidates evidence for the whole allocation");
        checker.state.objects.get_mut(&ALIAS).unwrap().extent = Extent::HeaderPrefix { minimum_bytes: layout.base };
        checker.state.header_values.clear();
        reload(checker, ALIAS, RELOADED_COUNT, RELOADED_CAPACITY);
        assert!(checker.promote_header_extent(ALIAS).is_err());
    });
}

#[test]
fn destruction_promotes_authenticated_prefix_without_prior_body_access() {
    for release in [false, true] {
        with_checker(|checker, layout| {
            joined(checker, layout, Ownership::Shared);
            reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
            checker.check(&Operation::CloneRef { destination: ALIAS, source: SOURCE }).unwrap();
            assert!(matches!(checker.state.objects[&SOURCE].extent, Extent::HeaderPrefix { .. }));
            // Neither a body access nor an explicit promotion has occurred.
            checker.check(&if release {
                Operation::Release { object: SOURCE }
            } else {
                Operation::DropRef { object: SOURCE }
            }).unwrap();
            assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Consumed);
            assert_shared_prefix(checker, SOURCE, COUNT);
            assert!(!checker.state.full_current_prefixes.is_empty());
            assert!(matches!(checker.state.objects[&ALIAS].extent, Extent::HeaderPrefix { .. }));
            // Fresh header SSA names on the surviving alias must also work
            // when its very next operation is destruction, not a body read.
            reload(checker, ALIAS, RELOADED_COUNT, RELOADED_CAPACITY);
            checker.check(&Operation::DropRef { object: ALIAS }).unwrap();
            assert_shared_prefix(checker, ALIAS, RELOADED_COUNT);
            assert_eq!(checker.state.objects[&ALIAS].ownership, Ownership::Consumed);
            assert!(checker.state.full_current_prefixes.is_empty());
        });
    }
}

#[test]
fn destruction_rejects_unauthenticated_or_partial_prefix() {
    for case in 0..3 {
        for release in [false, true] {
            with_checker(|checker, layout| {
                if case == 2 {
                    let (left, right) = edges(checker, layout, 1, 2, Ownership::Shared);
                    meet(checker, &left, &right);
                    assert!(checker.state.full_current_prefixes.is_empty());
                } else {
                    joined(checker, layout, Ownership::Shared);
                }
                reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
                match case {
                    0 => { checker.state.header_values.remove(&(SOURCE, HeaderField::Capacity)); }
                    1 => checker.state.guards.clear(),
                    2 => {}
                    _ => unreachable!(),
                }
                let error = checker.check(&if release {
                    Operation::Release { object: SOURCE }
                } else {
                    Operation::DropRef { object: SOURCE }
                }).unwrap_err();
                match case {
                    0 => assert!(matches!(error, InitialCheckError::InvalidExtent { object: SOURCE, .. }), "{error:?}"),
                    1 => assert!(matches!(error, InitialCheckError::MissingCapacityGuard {
                        count: COUNT, capacity: LOADED_CAPACITY, ..
                    }), "{error:?}"),
                    2 => assert!(matches!(error, InitialCheckError::UnsupportedMemoryOperation {
                        operation: "UpdateCount(pointer prefix gap)", ..
                    }), "{error:?}"),
                    _ => unreachable!(),
                }
                assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Shared,
                    "a rejected destructor must not retire the token");
            });
        }
    }
}

#[test]
fn second_meet_synchronizes_promoted_count_before_clone_and_drop() {
    with_checker(|checker, layout| {
        joined(checker, layout, Ownership::Shared);
        reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
        let left = checker.snapshot();
        let right = left.clone();
        assert!(matches!(left.objects[&SOURCE].extent, Extent::HeaderPrefix { .. }));
        assert!(matches!(right.objects[&SOURCE].extent, Extent::HeaderPrefix { .. }));
        let old = RangeIdentity { object: SOURCE, start: ZERO, length: LEFT_LENGTH };
        let original_range = left.range_states[&old];
        // This SECOND meet speculatively promotes both objects from the
        // authenticated headers. It must also synchronize the retained range.
        meet(checker, &left, &right);
        assert!(matches!(checker.state.objects[&SOURCE].extent,
            Extent::Counted { count: COUNT, capacity: LOADED_CAPACITY, .. }));
        let fresh = RangeIdentity { length: COUNT, ..old };
        assert!(!checker.state.range_states.contains_key(&old));
        assert!(!checker.state.full_current_prefixes.contains(&old));
        assert_eq!(checker.state.range_states.get(&fresh), Some(&original_range));
        assert!(checker.state.full_current_prefixes.contains(&fresh));
        assert_shared_prefix(checker, SOURCE, COUNT);
        clone_prefix(checker, layout);
        checker.check(&Operation::UpdateCount { object: DESTINATION, count: COUNT }).unwrap();
        assert_shared_prefix(checker, DESTINATION, COUNT);
        checker.check(&Operation::DropRef { object: SOURCE }).unwrap();
        checker.check(&Operation::DropRef { object: DESTINATION }).unwrap();
        assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Consumed);
        assert_eq!(checker.state.objects[&DESTINATION].ownership, Ownership::Consumed);
    });
}

#[test]
fn second_meet_does_not_retag_partial_or_consumed_prefixes() {
    for consumed in [false, true] {
        with_checker(|checker, layout| {
            if consumed {
                joined(checker, layout, Ownership::Shared);
            } else {
                // Only one predecessor established a complete prefix.
                let (left, right) = edges(checker, layout, 1, 2, Ownership::Shared);
                meet(checker, &left, &right);
            }
            reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
            let left = checker.snapshot();
            let mut right = left.clone();
            if consumed {
                let range = right.range_states.values_mut().next().unwrap();
                range.ownership = Ownership::Consumed;
                range.initialized = false;
                right.full_current_prefixes.clear();
            }
            meet(checker, &left, &right);
            assert!(checker.state.full_current_prefixes.is_empty());
            assert!(!checker.state.range_states.keys().any(|range|
                range.object == SOURCE && range.length == COUNT));
            assert!(checker.check(&Operation::DropRef { object: SOURCE }).is_err(),
                "neither a partial nor a consumed prefix authorizes complete initialization");
        });
    }
}

#[test]
fn joined_prefix_sync_rejects_collision_and_ambiguity_without_erasing_ranges() {
    for ambiguous in [false, true] {
        with_checker(|checker, layout| {
            joined(checker, layout, Ownership::Shared);
            reload(checker, SOURCE, COUNT, LOADED_CAPACITY);
            let edge = checker.snapshot();
            let mut objects = (*edge.objects).clone();
            // Model the precise intermediate meet stage: checked promoted
            // ObjectState plus original-edge range/proof collections.
            objects.insert(SOURCE, checker.promoted_object(&edge, SOURCE).unwrap());
            let mut ranges = (*edge.range_states).clone();
            let mut proofs = (*edge.full_current_prefixes).clone();
            let old = *proofs.first().unwrap();
            let range = ranges[&old];
            if ambiguous {
                // Two roots name the SAME allocation, not overlapping phi
                // origin sets. Neither complete range may overwrite the other.
                objects.insert(ALIAS, objects[&SOURCE].clone());
                let alias = RangeIdentity { object: ALIAS, ..old };
                ranges.insert(alias, range);
                proofs.insert(alias);
            } else {
                let occupied = RangeIdentity { length: COUNT, ..old };
                ranges.insert(occupied, RangeState { initialized: false, ..range });
            }
            let before_ranges = ranges.clone();
            let before_proofs = proofs.clone();
            let error = checker.synchronize_joined_full_prefixes(
                &checker.function.blocks[0], &objects, &mut ranges, &mut proofs,
            ).unwrap_err();
            assert!(matches!(error, InitialCheckError::IncompatibleMemoryJoin { .. }), "{error:?}");
            assert_eq!(ranges, before_ranges);
            assert_eq!(proofs, before_proofs);
        });
    }
}
