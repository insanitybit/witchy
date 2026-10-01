use super::*;
use crate::canonical_safewir::{AllocationClass, CallArgument, InitialCheckError};
use crate::canonical_safewir::dictionary::{DictionaryOperation, DictionaryRehashTransfer, DictionarySlotField};

pub fn canonical_dictionary_scalar_insert_lookup_executes() {
    let mut layouts = LayoutInterner::new();
    let int = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let dict = layouts.intern_canonical_dictionary(int, int).unwrap();
    let [capacity, bucket, hash, key, value, table, loaded, grown_capacity, grown, removed] =
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10].map(ValueId::new);
    let module = CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0),
        checked_source: CheckedSourceIdentity::from_digest([167; 32]),
        layouts: LayoutBundle::from_interner(&layouts, [dict]).unwrap(),
        signatures: vec![FunctionSignature {
            id: FunctionSignatureId::new(167), parameters: vec![], results: vec![ValueType::Int, ValueType::Int],
            contract: CallableContract { parameters: vec![], results: vec![ResultContract::Value(ValueType::Int), ResultContract::Value(ValueType::Int)], parameter_effects: BTreeMap::new() },
        }],
        imports: vec![], proper_tail_sites: vec![],
        functions: vec![Function {
            id: FunctionId::new(167), signature: FunctionSignatureId::new(167), entry: BlockId::new(0),
            value_types: BTreeMap::from([
                (capacity, ValueType::Size), (bucket, ValueType::Size), (hash, ValueType::Int),
                (key, ValueType::Int), (value, ValueType::Int), (table, ValueType::Ref(dict)),
                (loaded, ValueType::Int), (grown_capacity, ValueType::Size),
                (grown, ValueType::Ref(dict)), (removed, ValueType::Int),
            ]),
            blocks: vec![BasicBlock { id: BlockId::new(0), parameters: vec![], operations: vec![
                Operation::ConstSize { destination: capacity, value: 8 },
                Operation::ConstSize { destination: bucket, value: 0 },
                Operation::ConstInt { destination: hash, value: 0 },
                Operation::ConstInt { destination: key, value: 11 },
                Operation::ConstInt { destination: value, value: 22 },
                Operation::Dictionary(DictionaryOperation::Initialize { destination: table, layout: dict, capacity }),
                Operation::Dictionary(DictionaryOperation::InsertSlot { dict: table, bucket, key, value, hash,
                    key_transfer: StoreTransfer::CopyScalar, value_transfer: StoreTransfer::CopyScalar }),
                Operation::ConstSize { destination: grown_capacity, value: 16 },
                Operation::Dictionary(DictionaryOperation::Rehash { destination: grown, source: table, capacity: grown_capacity,
                    transfer: DictionaryRehashTransfer::MoveUnique }),
                Operation::Dictionary(DictionaryOperation::ReadSlot { destination: loaded, dict: grown, bucket,
                    field: DictionarySlotField::Value, transfer: LoadTransfer::CopyScalar }),
                Operation::Dictionary(DictionaryOperation::RemoveSlot { removed_value: removed, dict: grown, bucket }),
                Operation::DropRef { object: grown },
            ], terminator: Terminator::Return { values: vec![loaded, removed] } }],
        }],
    });
    let checked = validate_final_for_test(check_initial_with_test_witness(module).expect("dictionary initial validation"))
        .expect("dictionary final validation");
    let artifact = admit_canonical_for_test(checked).expect("dictionary Wasm plan admission");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let actual = instance.get_typed_func::<(), (i64, i64)>(&mut store, "f167").unwrap().call(&mut store, ()).unwrap();
    assert_eq!(actual, (22, 22));
}

const SENTINEL: i64 = 0x123456789abcdef;

#[derive(Clone, Copy)]
enum Fault { None, WrongStore, BoolStore, Layout, Uninitialized, Oob, ShortExtent, Dead, Literal }

fn candidate(fault: Fault) -> CanonicalModule<LoweredCandidate> {
    let mut layouts = LayoutInterner::new();
    let unit = layouts.intern_scalar(ScalarKind::Unit).unwrap();
    let int = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let boolean = layouts.intern_scalar(ScalarKind::Bool).unwrap();
    let record = layouts.intern_packed_record(vec![if matches!(fault, Fault::Layout) { boolean } else { unit }, int]).unwrap();
    let [u, n, ru, rn, object, loaded, sentinel, flag] = [1,2,3,4,5,6,7,8].map(ValueId::new);
    let sig = |id, parameters: Vec<ValueType>| FunctionSignature {
        id: FunctionSignatureId::new(id), parameters: parameters.clone(), results: vec![ValueType::Unit, ValueType::Int],
        contract: CallableContract {
            parameters: parameters.into_iter().map(|value_type| ParameterContract { value_type, ownership: ParameterOwnership::Value }).collect(),
            results: vec![ResultContract::Value(ValueType::Unit), ResultContract::Value(ValueType::Int)], parameter_effects: BTreeMap::new(),
        },
    };
    let field = |index, ty, access| Place::projected(object, Projection::Field { index }, ty, access);
    let mut ops = vec![
        Operation::Alloc { destination: object, layout: record, extent: Extent::FixedBytes(if matches!(fault, Fault::ShortExtent) { 12 } else { 16 }), class: AllocationClass::ReferenceCounted },
        Operation::ConstBool { destination: flag, value: false },
        Operation::Store { place: field(1, ValueType::Int, AccessMode::Write), value: n, transfer: StoreTransfer::CopyScalar },
    ];
    if !matches!(fault, Fault::Uninitialized) {
        ops.push(Operation::Store { place: field(0, ValueType::Unit, AccessMode::Write), value: match fault { Fault::WrongStore => n, Fault::BoolStore => flag, _ => u }, transfer: StoreTransfer::CopyScalar });
    }
    if matches!(fault, Fault::Dead) { ops.push(Operation::DropRef { object }); }
    ops.extend([
        Operation::Load { destination: loaded, place: field(if matches!(fault, Fault::Oob) { 2 } else { 0 }, ValueType::Unit, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
        Operation::Load { destination: sentinel, place: field(1, ValueType::Int, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
        Operation::DropRef { object },
    ]);
    CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0), checked_source: CheckedSourceIdentity::from_digest([102;32]),
        layouts: LayoutBundle::from_interner(&layouts, [record]).unwrap(),
        signatures: vec![sig(101, vec![]), sig(102, vec![ValueType::Unit, ValueType::Int])], imports: vec![], proper_tail_sites: vec![],
        functions: vec![
            Function {
                id: FunctionId::new(101), signature: FunctionSignatureId::new(101), entry: BlockId::new(0),
                value_types: BTreeMap::from([(u, if matches!(fault, Fault::Literal) { ValueType::Int } else { ValueType::Unit }), (n, ValueType::Int), (ru, ValueType::Unit), (rn, ValueType::Int)]),
                blocks: vec![BasicBlock { id: BlockId::new(0), parameters: vec![], operations: vec![
                    Operation::ConstUnit { destination: u },
                    Operation::ConstInt { destination: n, value: SENTINEL },
                    Operation::Call { destinations: vec![ru, rn], callee: FunctionId::new(102), arguments: vec![CallArgument::Value(u), CallArgument::Value(n)] },
                ], terminator: Terminator::Return { values: vec![ru, rn] } }],
            },
            Function {
                id: FunctionId::new(102), signature: FunctionSignatureId::new(102), entry: BlockId::new(0),
                value_types: BTreeMap::from([(u, ValueType::Unit), (n, ValueType::Int), (object, ValueType::Ref(record)), (loaded, ValueType::Unit), (sentinel, ValueType::Int), (flag, ValueType::Bool)]),
                blocks: vec![BasicBlock { id: BlockId::new(0), parameters: vec![
                    BlockParameter { value: u, value_type: ValueType::Unit, ownership: None },
                    BlockParameter { value: n, value_type: ValueType::Int, ownership: None },
                ], operations: ops, terminator: Terminator::Return { values: vec![loaded, sentinel] } }],
            },
        ],
    })
}

fn validated() -> ValidatedSafeWir {
    validate_final_for_test(check_initial_with_test_witness(candidate(Fault::None)).expect("initial Unit validation")).expect("final Unit validation")
}

fn execute(bytes: &[u8]) -> (i32, i64) {
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let output = instance.get_typed_func::<(), (i32, i64)>(&mut store, "f101").unwrap().call(&mut store, ()).unwrap();
    // Even a noncanonical external carrier cannot be stored or returned as bits.
    let poisoned = instance.get_typed_func::<(i32, i64), (i32, i64)>(&mut store, "f102").unwrap().call(&mut store, (-1, SENTINEL)).unwrap();
    assert_eq!(poisoned, (0, SENTINEL));
    output
}

pub fn unit_storage_construct_call_native_oracle() {
    let checked = validated();
    let artifact = admit_canonical_for_test(checked).expect("Unit exact-byte plan admission");
    assert_eq!(execute(artifact.wasm()), (0, SENTINEL));
    let store = artifact.wasm().windows(3).position(|w| w == [0x41, 0, 0x36]).expect("Unit zero store");
    let mut poisoned = artifact.wasm().to_vec();
    poisoned[store + 1] = 42;
    assert_eq!(execute(&poisoned), (0, SENTINEL), "Unit load discards arbitrary stored bits after the real access");
    assert!(admit_canonical_with_bytes_for_test(validated(), poisoned).is_err(), "expected-byte plan binds the zero store");
}

pub fn unit_storage_rejects_type_layout_initialization_and_bounds() {
    for fault in [Fault::WrongStore, Fault::BoolStore, Fault::Layout, Fault::Uninitialized, Fault::Oob, Fault::ShortExtent, Fault::Dead, Fault::Literal] {
        assert!(check_initial_with_test_witness(candidate(fault)).is_err());
    }
    let error = crate::canonical_safewir::check_initial(candidate(Fault::Uninitialized)).unwrap_err();
    assert!(matches!(error, InitialCheckError::UnsupportedMemoryOperation { operation: "Load(uninitialized scalar)", .. }));
}

fn list_candidate(guarded: bool) -> CanonicalModule<LoweredCandidate> {
    let mut layouts = LayoutInterner::new();
    let unit = layouts.intern_scalar(ScalarKind::Unit).unwrap();
    let list = layouts.intern_packed_list(unit, RcHeader::Required).unwrap();
    let LayoutSize::Dynamic { base, stride } = layouts.get(list).unwrap().size() else { panic!("list extent") };
    assert_eq!(stride, 4);
    let [index, zero, one, two, u, source, destination, ok, loaded, length] = [1,2,3,4,5,6,7,8,9,10].map(ValueId::new);
    let element = |object, index, access| Place::projected(object, Projection::Element { index }, ValueType::Unit, access);
    let range = |object, access| Place::projected(object, Projection::Slice { start: zero, length: one }, ValueType::Ref(list), access);
    let alloc = |destination| Operation::Alloc { destination, layout: list,
        extent: Extent::Counted { header_bytes: base, count: zero, capacity: two, stride_bytes: stride }, class: AllocationClass::ReferenceCounted };
    let mut ops = vec![
        Operation::ConstSize { destination: zero, value: 0 },
        Operation::ConstSize { destination: one, value: 1 },
        Operation::ConstSize { destination: two, value: 2 },
        Operation::ConstUnit { destination: u },
        alloc(source),
        Operation::Store { place: element(source, zero, AccessMode::Write), value: u, transfer: StoreTransfer::CopyScalar },
        Operation::UpdateCount { object: source, count: one },
        alloc(destination),
        Operation::CloneRange { destination: range(destination, AccessMode::Write), source: range(source, AccessMode::Read), count: one },
        Operation::UpdateCount { object: destination, count: one },
        Operation::Store { place: element(destination, one, AccessMode::Write), value: u, transfer: StoreTransfer::CopyScalar },
        Operation::UpdateCount { object: destination, count: two },
        Operation::Compare { destination: ok, operator: ComparisonOperator::LessUnsigned, left: index, right: two },
    ];
    let [zero_bound, one_bound, range_bound, start_bound] = [11,12,13,14].map(ValueId::new);
    ops.splice(4..4, [
        Operation::Compare { destination: zero_bound, operator: ComparisonOperator::LessUnsigned, left: zero, right: two },
        Operation::Guard { condition: zero_bound, kind: GuardKind::Bounds },
        Operation::Compare { destination: one_bound, operator: ComparisonOperator::LessUnsigned, left: one, right: two },
        Operation::Guard { condition: one_bound, kind: GuardKind::Bounds },
        Operation::Compare { destination: range_bound, operator: ComparisonOperator::LessEqualUnsigned, left: one, right: two },
        Operation::Guard { condition: range_bound, kind: GuardKind::Bounds },
        Operation::Compare { destination: start_bound, operator: ComparisonOperator::LessEqualUnsigned, left: zero, right: one },
        Operation::Guard { condition: start_bound, kind: GuardKind::Bounds },
    ]);
    if guarded { ops.push(
Operation::Guard { condition: ok, kind: GuardKind::Bounds }); }
    ops.extend([
        Operation::Load { destination: loaded, place: element(destination, index, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
        Operation::Load { destination: length, place: Place::projected(destination, Projection::Header(crate::canonical_safewir::HeaderField::Length), ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
        Operation::DropRef { object: source }, Operation::DropRef { object: destination },
    ]);
    CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0), checked_source: CheckedSourceIdentity::from_digest([103;32]),
        layouts: LayoutBundle::from_interner(&layouts, [list]).unwrap(), imports: vec![], proper_tail_sites: vec![],
        signatures: vec![FunctionSignature {
            id: FunctionSignatureId::new(103), parameters: vec![ValueType::Size], results: vec![ValueType::Unit, ValueType::Size],
            contract: CallableContract { parameters: vec![ParameterContract { value_type: ValueType::Size, ownership: ParameterOwnership::Value }],
                results: vec![ResultContract::Value(ValueType::Unit), ResultContract::Value(ValueType::Size)], parameter_effects: BTreeMap::new() },
        }],
        functions: vec![Function {
            id: FunctionId::new(103), signature: FunctionSignatureId::new(103), entry: BlockId::new(0),
            value_types: BTreeMap::from([(index, ValueType::Size), (zero, ValueType::Size), (one, ValueType::Size), (two, ValueType::Size),
                (zero_bound, ValueType::Bool), (one_bound, ValueType::Bool), (range_bound, ValueType::Bool), (start_bound, ValueType::Bool), (u, ValueType::Unit), (source, ValueType::Ref(list)), (destination, ValueType::Ref(list)), (ok, ValueType::Bool), (loaded, ValueType::Unit), (length, ValueType::Size)]),
            blocks: vec![BasicBlock { id: BlockId::new(0), parameters: vec![BlockParameter { value: index, value_type: ValueType::Size, ownership: None }],
                operations: ops, terminator: Terminator::Return { values: vec![loaded, length] } }],
        }],
    })
}
