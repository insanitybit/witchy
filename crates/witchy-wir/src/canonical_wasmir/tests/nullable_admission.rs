use super::*;
use crate::canonical_safewir::{AllocationClass, ValueType};

const FUNCTION: FunctionId = FunctionId::new(101);

fn candidate(present: bool) -> CanonicalModule<LoweredCandidate> {
    candidate_with_transfer(present, false, true, false)
}

fn candidate_with_transfer(present: bool, shared: bool, nullable: bool, take: bool) -> CanonicalModule<LoweredCandidate> {
    let mut layouts = LayoutInterner::new();
    let int = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let child_layout = layouts.intern_packed_record(vec![int]).unwrap();
    let slot = layouts.intern_runtime_pointer_value_slot(child_layout,
        if nullable { PointerSlot::OwnedNullable } else { PointerSlot::Owned }).unwrap();
    let parent_layout = layouts.intern_packed_record(vec![slot]).unwrap();
    let [parent, child, literal, loaded, result] = [1, 2, 3, 4, 5].map(ValueId::new);
    let external = ValueId::new(6);
    let after_drop = ValueId::new(7);
    let field = |object, value_type, access| Place::projected(object, Projection::Field { index: 0 }, value_type, access);
    let alloc = |destination, layout| {
        let LayoutSize::Fixed(bytes) = layouts.get(layout).unwrap().size() else { panic!("fixed fixture"); };
        Operation::Alloc { destination, layout, extent: Extent::FixedBytes(u64::from(bytes)), class: AllocationClass::ReferenceCounted }
    };
    let mut operations = vec![alloc(parent, parent_layout)];
    if nullable {
        operations.push(Operation::InitNullableOwned { place: field(parent, ValueType::Ref(child_layout), AccessMode::Write) });
    }
    let mut value_types = BTreeMap::from([
        (parent, ValueType::Ref(parent_layout)), (loaded, ValueType::Ref(child_layout)), (result, ValueType::Int),
    ]);
    if present {
        value_types.extend([(child, ValueType::Ref(child_layout)), (literal, ValueType::Int)]);
        operations.extend([
            alloc(child, child_layout),
            Operation::ConstInt { destination: literal, value: 42 },
            Operation::Store { place: field(child, ValueType::Int, AccessMode::Write), value: literal, transfer: StoreTransfer::CopyScalar },
        ]);
        if shared {
            value_types.extend([(external, ValueType::Ref(child_layout)), (after_drop, ValueType::Int)]);
            operations.push(Operation::CloneRef { destination: external, source: child });
        }
        operations.push(Operation::Store {
            place: field(parent, ValueType::Ref(child_layout), AccessMode::Write), value: child,
            transfer: if shared { StoreTransfer::CloneShared } else { StoreTransfer::MoveUnique },
        });
        if shared { operations.push(Operation::DropRef { object: child }); }
    }
    operations.extend([
        Operation::Load { destination: loaded, place: field(parent, ValueType::Ref(child_layout), AccessMode::Read),
            transfer: if take { LoadTransfer::TakeOwnedField }
                else if nullable { LoadTransfer::ClonePresent } else { LoadTransfer::CloneShared } },
        Operation::Load { destination: result, place: field(loaded, ValueType::Int, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
    ]);
    if take {
        operations.push(Operation::Store {
            place: field(parent, ValueType::Ref(child_layout), AccessMode::Write),
            value: loaded, transfer: StoreTransfer::CloneShared,
        });
    }
    operations.extend([Operation::DropRef { object: loaded }, Operation::DropRef { object: parent }]);
    if shared {
        operations.extend([
            Operation::Load { destination: after_drop, place: field(external, ValueType::Int, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
            Operation::DropRef { object: external },
        ]);
    }
    let signature = FunctionSignature {
        id: FunctionSignatureId::new(101), parameters: vec![], results: vec![ValueType::Int],
        contract: CallableContract { parameters: vec![], results: vec![ResultContract::Value(ValueType::Int)], parameter_effects: BTreeMap::new() },
    };
    CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0), checked_source: CheckedSourceIdentity::from_digest([101; 32]),
        layouts: LayoutBundle::from_interner(&layouts, [parent_layout]).unwrap(),
        signatures: vec![signature], imports: vec![], proper_tail_sites: vec![],
        functions: vec![Function {
            id: FUNCTION, signature: FunctionSignatureId::new(101), entry: BlockId::new(0), value_types,
            blocks: vec![BasicBlock { id: BlockId::new(0), parameters: vec![], operations,
                terminator: Terminator::Return { values: vec![if shared { after_drop } else { result }] } }],
        }],
    })
}

fn validated(present: bool) -> ValidatedSafeWir {
    let initial = check_initial_with_test_witness(candidate(present)).expect("initial nullable check");
    validate_final_for_test(initial).expect("final nullable check")
}

pub fn nullable_admission_rejects_valid_but_mutated_bytes() {
    let artifact = admit_canonical_for_test(validated(true)).unwrap();
    let mut bytes = artifact.wasm().to_vec();
    let literal = bytes.windows(2).position(|bytes| bytes == [0x42, 42]).expect("i64.const 42");
    bytes[literal + 1] = 43;
    wasmparser::validate(&bytes).expect("mutation is structurally valid Wasm");
    assert!(admit_canonical_with_bytes_for_test(validated(true), bytes).is_err());
}

pub fn owned_field_take_admits_shared_child() {
    let candidate = candidate_with_transfer(true, true, false, true);
    let initial = check_initial_with_test_witness(candidate).expect("shared child owned take checks");
    let validated = validate_final_for_test(initial).expect("shared child owned take validates");
    admit_canonical_for_test(validated).expect("shared child owned take admits to Wasm");
}
