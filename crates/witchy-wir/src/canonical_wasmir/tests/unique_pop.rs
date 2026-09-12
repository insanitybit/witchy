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
        make.push(Operation::Load { destination: v(8), place: Place::projected(v(3), Projection::Element { index: v(1) }, payload, AccessMode::Read), transfer: LoadTransfer::CloneShared });
    }
    let function = |id, types, parameters, operations, returns| Function {
        id: FunctionId::new(id), signature: FunctionSignatureId::new(id), entry: BlockId::new(0), value_types: types,
        blocks: vec![BasicBlock { id: BlockId::new(0), parameters, operations, terminator: Terminator::Return { values: returns } }],
    };
    let mut make_types = BTreeMap::from([(v(0), ValueType::Size), (v(1), ValueType::Size), (v(2), ValueType::Size), (v(3), root), (v(4), payload), (v(5), ValueType::Bool), (v(6), ValueType::Bool)]);
    if pointer { make_types.extend([(v(7), payload), (v(8), payload)]); }
    let make = function(0, make_types, vec![], make, vec![v(3), if pointer { v(8) } else { v(4) }]);
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
    let closed = function(4, BTreeMap::from([(v(0), root), (v(1), payload), (v(2), payload), (v(3), root), (v(4), payload), (v(5), root), (v(6), ValueType::Size)]), vec![], vec![
        Operation::Call { destinations: vec![v(0), v(1)], callee: FunctionId::new(0), arguments: vec![] },
        Operation::Call { destinations: vec![v(2), v(3)], callee: FunctionId::new(1), arguments: vec![CallArgument::Move(v(0))] },
        Operation::Call { destinations: vec![v(4), v(5)], callee: FunctionId::new(1), arguments: vec![CallArgument::Move(v(3))] },
        Operation::Call { destinations: vec![], callee: FunctionId::new(2), arguments: vec![CallArgument::Move(v(5))] },
        Operation::Call { destinations: vec![], callee: FunctionId::new(3), arguments: vec![drop_argument(v(2))] },
        Operation::Call { destinations: vec![], callee: FunctionId::new(3), arguments: vec![drop_argument(v(4))] },
        Operation::Call { destinations: vec![], callee: FunctionId::new(3), arguments: vec![drop_argument(v(1))] },
        Operation::ConstSize { destination: v(6), value: 42 },
    ], vec![v(6)]);
    match mutation {
        Some(0) => {
            signatures[1].contract.parameters[0].ownership = ParameterOwnership::WriteBack;
            take.blocks[0].parameters[0].ownership = Some(Ownership::Shared);
        }
        Some(1) => take.blocks[0].operations.retain(|op| !matches!(op, Operation::Guard { .. })),
        Some(2) => take.blocks[0].operations[2] = Operation::ConstSize { destination: v(3), value: 0 },
        None => {},
        _ => panic!("unknown negative fixture"),
    }
    CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(1), checked_source: CheckedSourceIdentity::from_digest([0x51; 32]),
        layouts: LayoutBundle::from_interner(&interner, [list, child]).unwrap(), signatures, imports: vec![],
        functions: vec![make, take, drop_list, drop_child, closed], proper_tail_sites: vec![],
    })
}

#[test]
fn unique_pop_oracle_complete_call_chain_reclaims_every_reference() {
    for pointer in [false, true] {
        let validated = finalize(candidate(pointer, None));
        let result = crate::canonical_interpreter::interpret_program(
            &validated, FunctionId::new(4), vec![], Default::default(),
        ).unwrap();
        assert_eq!(result.results, vec![crate::canonical_interpreter::InterpreterValue::Size(42)]);
        assert_eq!(result.leaked_objects, 0);
    }
}

#[test]
fn unique_pop_byte_plan_rejects_capacity_write_instead_of_tail_publication() {
    let artifact = admit_canonical(finalize(candidate(false, None))).unwrap();
    let mut bytes = artifact.wasm().to_vec();
    let mut code_index = 0;
    let mut store_offset = None;
    for section in wasmparser::Parser::new(0).parse_all(&bytes) {
        if let wasmparser::Payload::CodeSectionEntry(body) = section.unwrap() {
            if code_index == 1 {
                let mut operators = body.get_operators_reader().unwrap();
                while !operators.eof() {
                    let position = operators.original_position();
                    if let wasmparser::Operator::I32Store { memarg } = operators.read().unwrap() {
                        assert_eq!(memarg.offset, 0);
                        store_offset = Some(position);
                    }
                }
            }
            code_index += 1;
        }
    }
    let position = store_offset.expect("TakeLast publishes exactly one header count");
    assert_eq!(&bytes[position..position + 3], &[0x36, 2, 0]);
    bytes[position + 2] = 4;
    wasmparser::Validator::new().validate_all(&bytes).expect("mutation remains structurally valid Wasm");
    assert!(admit_canonical_with_bytes_for_test(finalize(candidate(false, None)), bytes).is_err());
}

#[test]
fn unique_pop_checker_refuses_shared_container_missing_bounds_and_non_tail() {
    for pointer in [false, true] {
        check_initial_with_test_witness(candidate(pointer, None)).expect("positive initial check");
        for mutation in 0..3 {
            let module = candidate(pointer, Some(mutation));
            assert!(check_initial_with_test_witness(module).is_err(), "mutation {mutation}, pointer={pointer}");
        }
    }
}

#[cfg(feature = "native")]
#[test]
fn unique_pop_native_preserves_receiver_address_heap_top_and_shared_child() {
    for pointer in [false, true] {
        let artifact = admit_canonical(finalize(candidate(pointer, None))).expect("initial/final/byte-plan admission");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let make = instance.get_typed_func::<(), (i32, i32)>(&mut store, "f0").unwrap();
        let take = instance.get_typed_func::<i32, (i32, i32)>(&mut store, "f1").unwrap();
        let drop_list = instance.get_typed_func::<i32, ()>(&mut store, "f2").unwrap();
        let drop_child = instance.get_typed_func::<i32, ()>(&mut store, "f3").unwrap();
        let (root, child) = make.call(&mut store, ()).unwrap();
        let bump = instance.get_global(&mut store, HEAP_TOP_EXPORT).unwrap();
        let before = bump.get(&mut store).i32().unwrap();
        let (last, after_one) = take.call(&mut store, root).unwrap();
        let (first, after_two) = take.call(&mut store, after_one).unwrap();
        assert_eq!((after_one, after_two), (root, root), "receiver storage never changes");
        assert_eq!(bump.get(&mut store).i32().unwrap(), before, "callee allocates no replacement or payload copy");
        assert_eq!(last, child);
        if pointer { assert_ne!(first, child, "independently allocated first child"); }
        else { assert_eq!(first, child); }
        // A direct empty extraction traps, and must not consume a cell or alter
        // the count. Source pop handles this case before issuing TakeLast.
        assert!(take.call(&mut store, after_two).is_err());
        drop_list.call(&mut store, after_two).unwrap();
        drop_child.call(&mut store, last).unwrap();
        drop_child.call(&mut store, first).unwrap();
        if pointer {
            let memory = instance.get_memory(&mut store, "memory").unwrap();
            assert_eq!(u32::from_le_bytes(memory.data(&store)[child as usize - 8..child as usize - 4].try_into().unwrap()), 1, "external child survives both transfers and list destruction");
        }
        drop_child.call(&mut store, child).unwrap();
    }
}
