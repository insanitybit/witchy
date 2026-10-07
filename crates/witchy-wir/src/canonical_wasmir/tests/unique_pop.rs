use super::*;
use crate::canonical_safewir::{AllocationClass, Ownership};

fn candidate(pointer: bool, mutation: Option<u8>) -> CanonicalModule<LoweredCandidate> {
    let mut interner = LayoutInterner::new();
    let scalar = interner.intern_scalar(ScalarKind::U32).unwrap();
    let byte = interner.intern_scalar(ScalarKind::RawByte).unwrap();
    let child = interner.intern_runtime_buffer(byte).unwrap();
    let slot = if pointer {
        interner.intern_runtime_pointer_value_slot(child, PointerSlot::Owned).unwrap()
    } else { scalar };
    let list = interner.intern_packed_list(slot, RcHeader::Required).unwrap();
    let LayoutSize::Dynamic { base, stride } = interner.get(list).unwrap().size() else { panic!() };
    let LayoutSize::Dynamic { base: child_base, stride: child_stride } = interner.get(child).unwrap().size() else { panic!() };
    let payload = if pointer { ValueType::Ref(child) } else { ValueType::Size };
    let root = ValueType::Ref(list);
    let payload_result = if pointer { ResultContract::Shared { layout: child } } else { ResultContract::Value(payload) };
    let sig = |id, parameters: Vec<ParameterContract>, results: Vec<ValueType>, contracts, effects| FunctionSignature {
        id: FunctionSignatureId::new(id), parameters: parameters.iter().map(|p| p.value_type).collect(), results,
        contract: CallableContract { parameters, results: contracts, parameter_effects: effects },
    };
    let unique = ParameterContract { value_type: root, ownership: ParameterOwnership::UniqueWriteBack };
    let mut signatures = vec![
        sig(0, vec![], vec![root, payload], vec![ResultContract::Unique { layout: list }, payload_result.clone()], BTreeMap::new()),
        sig(1, vec![unique.clone()], vec![payload, root], vec![payload_result, ResultContract::Unique { layout: list }],
            BTreeMap::from([(0, vec![crate::canonical_safewir::MemoryEffect::Read, crate::canonical_safewir::MemoryEffect::Write])])),
        sig(2, vec![ParameterContract { value_type: root, ownership: ParameterOwnership::Shared }], vec![], vec![],
            BTreeMap::from([(0, vec![crate::canonical_safewir::MemoryEffect::Release])])),
        sig(3, vec![ParameterContract { value_type: payload, ownership: if pointer { ParameterOwnership::Shared } else { ParameterOwnership::Value } }], vec![], vec![],
            if pointer { BTreeMap::from([(0, vec![crate::canonical_safewir::MemoryEffect::Release])]) } else { BTreeMap::new() }),
        sig(4, vec![], vec![ValueType::Size], vec![ResultContract::Value(ValueType::Size)], BTreeMap::new()),
    ];
    let v = ValueId::new;
    let mut make = vec![
        Operation::ConstSize { destination: v(0), value: 0 },
        Operation::ConstSize { destination: v(1), value: 1 },
        Operation::ConstSize { destination: v(2), value: 2 },
        Operation::Alloc { destination: v(3), layout: list, extent: Extent::Counted { header_bytes: base, count: if pointer { v(0) } else { v(2) }, capacity: v(2), stride_bytes: stride }, class: AllocationClass::ReferenceCounted },
    ];
    if pointer {
        make.push(Operation::Alloc { destination: v(4), layout: child, extent: Extent::Counted { header_bytes: child_base, count: v(0), capacity: v(0), stride_bytes: child_stride }, class: AllocationClass::ReferenceCounted });
        make.push(Operation::Alloc { destination: v(7), layout: child, extent: Extent::Counted { header_bytes: child_base, count: v(0), capacity: v(0), stride_bytes: child_stride }, class: AllocationClass::ReferenceCounted });
    } else {
        make.push(Operation::ConstSize { destination: v(4), value: 42 });
    }
    for (index, condition, published) in [(0, 5, 1), (1, 6, 2)] {
        make.extend([
            Operation::Compare { destination: v(condition), operator: ComparisonOperator::LessUnsigned, left: v(index), right: v(2) },
            Operation::Guard { condition: v(condition), kind: GuardKind::Bounds },
            Operation::Store { place: Place::projected(v(3), Projection::Element { index: v(index) }, payload, AccessMode::Write), value: if pointer && index == 1 { v(7) } else { v(4) }, transfer: if pointer { StoreTransfer::MoveUnique } else { StoreTransfer::CopyScalar } },
        ]);
        if pointer { make.push(Operation::UpdateCount { object: v(3), count: v(published) }); }
    }
    if pointer {
        // Publish unique children first, then explicitly share the initialized
        // tail child. The caller retains this token across both extractions.
        make.push(Operation::Load { destination: v(8), place: Place::projected(v(3), Projection::Element { index: if matches!(mutation, Some(10..=12)) { v(0) } else { v(1) } }, payload, AccessMode::Read), transfer: LoadTransfer::CloneShared });
    }
    let mut returned_root = v(3);
    if matches!(mutation, Some(3..=6 | 10..=12)) {
        assert!(pointer);
        make.extend([
            Operation::Load { destination: v(9), place: Place::projected(v(3), Projection::Element { index: v(1) }, payload, AccessMode::Read), transfer: LoadTransfer::TakeLast },
            Operation::DropRef { object: v(9) },
            Operation::Alloc { destination: v(10), layout: child, extent: Extent::Counted { header_bytes: child_base, count: v(0), capacity: v(0), stride_bytes: child_stride }, class: AllocationClass::ReferenceCounted },
        ]);
        if matches!(mutation, Some(4 | 6 | 11)) {
            make.push(Operation::MoveValue { destination: v(11), source: v(3) });
            returned_root = v(11);
        }
        let append = matches!(mutation, Some(5 | 6 | 10..=12));
        make.push(Operation::Store { place: Place::projected(returned_root, Projection::Element { index: if append { v(1) } else { v(0) } }, payload, AccessMode::Write), value: v(10), transfer: StoreTransfer::MoveUnique });
        if mutation == Some(12) {
            make.push(Operation::MoveValue { destination: v(11), source: v(3) });
            returned_root = v(11);
        }
        if append { make.push(Operation::UpdateCount { object: returned_root, count: v(2) }); }
        if matches!(mutation, Some(10..=12)) {
            make.extend([
                Operation::Alloc { destination: v(12), layout: list, extent: Extent::Counted { header_bytes: base, count: v(0), capacity: v(2), stride_bytes: stride }, class: AllocationClass::ReferenceCounted },
                Operation::MoveRange {
                    destination: Place::projected(v(12), Projection::Slice { start: v(0), length: v(2) }, payload, AccessMode::Write),
                    source: Place::projected(returned_root, Projection::Slice { start: v(0), length: v(2) }, payload, AccessMode::Read),
                    count: v(2),
                },
                Operation::UpdateCount { object: v(12), count: v(2) },
                Operation::UpdateCount { object: returned_root, count: v(0) },
                Operation::DropRef { object: returned_root },
            ]);
            returned_root = v(12);
        }

    }
    if mutation == Some(8) {
        assert!(!pointer);
        for operation in &mut make {
            if let Operation::Store { value, .. } = operation {
                *value = v(0);
            }
        }
    }
    if mutation == Some(9) {
        assert!(!pointer);
        make.extend([
            Operation::Load { destination: v(7), place: Place::projected(v(3), Projection::Element { index: v(1) }, payload, AccessMode::Read), transfer: LoadTransfer::TakeLast },
            Operation::Store { place: Place::projected(v(3), Projection::Element { index: v(1) }, payload, AccessMode::Write), value: v(7), transfer: StoreTransfer::CopyScalar },
            Operation::UpdateCount { object: v(3), count: v(2) },
        ]);
    }
    let function = |id, types, parameters, operations, returns| Function {
        id: FunctionId::new(id), signature: FunctionSignatureId::new(id), entry: BlockId::new(0), value_types: types,
        blocks: vec![BasicBlock { id: BlockId::new(0), parameters, operations, terminator: Terminator::Return { values: returns } }],
    };
    let mut make_types = BTreeMap::from([(v(0), ValueType::Size), (v(1), ValueType::Size), (v(2), ValueType::Size), (v(3), root), (v(4), payload), (v(5), ValueType::Bool), (v(6), ValueType::Bool)]);
    if pointer { make_types.extend([(v(7), payload), (v(8), payload)]); }
    if matches!(mutation, Some(3..=6 | 10..=12)) { make_types.extend([(v(9), payload), (v(10), payload)]); }
    if matches!(mutation, Some(4 | 6 | 11 | 12)) { make_types.insert(v(11), root); }
    if mutation == Some(9) { make_types.insert(v(7), payload); }
    if matches!(mutation, Some(10..=12)) { make_types.insert(v(12), root); }
    let make = function(0, make_types, vec![], make, vec![returned_root, if pointer { v(8) } else { v(4) }]);

    let mut take = function(1, BTreeMap::from([(v(0), root), (v(1), ValueType::Size), (v(2), ValueType::Size), (v(3), ValueType::Size), (v(4), ValueType::Bool), (v(5), payload), (v(6), ValueType::Size), (v(7), ValueType::Bool), (v(8), ValueType::Size), (v(9), ValueType::Bool)]),
        vec![BlockParameter { value: v(0), value_type: root, ownership: Some(Ownership::Unique) }], vec![
            Operation::Load { destination: v(1), place: Place::projected(v(0), Projection::Header(crate::canonical_safewir::HeaderField::Length), ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
            Operation::ConstSize { destination: v(2), value: 1 },
            Operation::Arithmetic { destination: v(3), operator: ArithmeticOperator::Subtract, left: v(1), right: v(2), overflow: OverflowBehavior::Wrap },
            Operation::Compare { destination: v(4), operator: ComparisonOperator::LessUnsigned, left: v(3), right: v(1) },
            Operation::Guard { condition: v(4), kind: GuardKind::Bounds },
            Operation::Load { destination: v(6), place: Place::projected(v(0), Projection::Header(crate::canonical_safewir::HeaderField::Capacity), ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
            Operation::Compare { destination: v(7), operator: ComparisonOperator::LessEqualUnsigned, left: v(1), right: v(6) },
            Operation::Guard { condition: v(7), kind: GuardKind::Bounds },
            Operation::ConstSize { destination: v(8), value: (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride) },
            Operation::Compare { destination: v(9), operator: ComparisonOperator::LessEqualUnsigned, left: v(6), right: v(8) },
            Operation::Guard { condition: v(9), kind: GuardKind::Arithmetic },
            Operation::Load { destination: v(5), place: Place::projected(v(0), Projection::Element { index: v(3) }, payload, AccessMode::Read), transfer: LoadTransfer::TakeLast },
        ], vec![v(5), v(0)]);
    let drop_list = function(2, BTreeMap::from([(v(0), root)]), vec![BlockParameter { value: v(0), value_type: root, ownership: Some(Ownership::Shared) }], vec![Operation::DropRef { object: v(0) }], vec![]);
    let drop_child = function(3, BTreeMap::from([(v(0), payload)]), vec![BlockParameter { value: v(0), value_type: payload, ownership: pointer.then_some(Ownership::Shared) }], if pointer { vec![Operation::DropRef { object: v(0) }] } else { vec![] }, vec![]);
    let drop_argument = |value| if pointer { CallArgument::Move(value) } else { CallArgument::Value(value) };
    let mut closed = function(4, BTreeMap::from([(v(0), root), (v(1), payload), (v(2), payload), (v(3), root), (v(4), payload), (v(5), root), (v(6), ValueType::Size)]), vec![], vec![
        Operation::Call { destinations: vec![v(0), v(1)], callee: FunctionId::new(0), arguments: vec![] },
        Operation::Call { destinations: vec![v(2), v(3)], callee: FunctionId::new(1), arguments: vec![CallArgument::Move(v(0))] },
        Operation::Call { destinations: vec![v(4), v(5)], callee: FunctionId::new(1), arguments: vec![CallArgument::Move(v(3))] },
        Operation::Call { destinations: vec![], callee: FunctionId::new(2), arguments: vec![CallArgument::Move(v(5))] },
        Operation::Call { destinations: vec![], callee: FunctionId::new(3), arguments: vec![drop_argument(v(2))] },
        Operation::Call { destinations: vec![], callee: FunctionId::new(3), arguments: vec![drop_argument(v(4))] },
        Operation::Call { destinations: vec![], callee: FunctionId::new(3), arguments: vec![drop_argument(v(1))] },
        Operation::ConstSize { destination: v(6), value: 42 },
    ], vec![v(6)]);
    if matches!(mutation, Some(3..=6 | 10..=12)) {
        closed.value_types.retain(|value, _| matches!(value.get(), 0 | 1 | 6));
        closed.blocks[0].operations = vec![
            Operation::Call { destinations: vec![v(0), v(1)], callee: FunctionId::new(0), arguments: vec![] },
            Operation::Call { destinations: vec![], callee: FunctionId::new(2), arguments: vec![CallArgument::Move(v(0))] },
            Operation::Call { destinations: vec![], callee: FunctionId::new(3), arguments: vec![CallArgument::Move(v(1))] },
            Operation::ConstSize { destination: v(6), value: 42 },
        ];
    }
    if matches!(mutation, Some(8 | 9)) {
        closed.blocks[0].terminator = Terminator::Return { values: vec![v(4)] };
    }
    match mutation {
        Some(0) => {
            signatures[1].contract.parameters[0].ownership = ParameterOwnership::WriteBack;
            take.blocks[0].parameters[0].ownership = Some(Ownership::Shared);
        }
        Some(1) => take.blocks[0].operations.retain(|op| !matches!(op, Operation::Guard { .. })),
        Some(2) => take.blocks[0].operations[2] = Operation::ConstSize { destination: v(3), value: 0 },
        Some(7) => {
            assert!(pointer);
            take.value_types.extend([(v(10), ValueType::Size), (v(11), payload), (v(12), ValueType::Bool)]);
            take.blocks[0].operations.extend([
                Operation::ConstSize { destination: v(10), value: 0 },
                Operation::Compare { destination: v(12), operator: ComparisonOperator::LessUnsigned, left: v(10), right: v(6) },
                Operation::Guard { condition: v(12), kind: GuardKind::Bounds },
                Operation::Alloc { destination: v(11), layout: child, extent: Extent::Counted { header_bytes: child_base, count: v(10), capacity: v(10), stride_bytes: child_stride }, class: AllocationClass::ReferenceCounted },
                Operation::Store { place: Place::projected(v(0), Projection::Element { index: v(10) }, payload, AccessMode::Write), value: v(11), transfer: StoreTransfer::MoveUnique },
            ]);
        }
        Some(3..=6 | 8..=12) | None => {},

        _ => panic!("unknown negative fixture"),
    }
    CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(1), checked_source: CheckedSourceIdentity::from_digest([0x51; 32]),
        layouts: LayoutBundle::from_interner(&interner, [list, child]).unwrap(), signatures, imports: vec![],
        functions: vec![make, take, drop_list, drop_child, closed], proper_tail_sites: vec![],
    })
}

pub fn unique_pop_checker_refuses_shared_container_missing_bounds_and_non_tail() {
    check_initial_with_test_witness(candidate(false, None)).expect("scalar initial check");
    for mutation in 0..3 {
        assert!(check_initial_with_test_witness(candidate(false, Some(mutation))).is_err(), "scalar mutation {mutation}");
    }
    // A raw pointer-bearing parameter has no checked source element plan, so
    // its popped element is a shared token, never a unique one.
    check_initial_with_test_witness(candidate(true, None)).expect("pointer pop yields a shared token");
}
