use super::*;
use crate::canonical_safewir::{check_initial_with_test_witness, validate_final_for_test,
    AccessMode, CallableContract, CheckedSourceIdentity, LoweredModuleParts, Revision};

const KEY: &str = "048f81cd9fca785a42a6f5dd58972cc0f702e83b1c960b5912354471496597e227fec81ff1d52530b06d7091649e6beb49dba70968b4b727bb24e3ceb7dd01a039";
const SIG: &str = "304402203260029f4c6beb2e78afdd906c057c63f8828e2b03820de7053d97254577fb8c02204478b9b75f8fd7a1ce4298f0d119e12926dafda116ae4c197b0048dc117bc9de";

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn crypto_extensions_oracle_ecdsa_vectors_and_status_precedence() {
    let message = "webauthn-es256-test-message";
    let encoded = String::from_utf8(hex(message.as_bytes())).unwrap();
    assert_eq!(ecdsa_status(KEY, message, SIG, false), Ok(1));
    assert_eq!(ecdsa_status(KEY, &encoded, SIG, true), Ok(1));
    assert_eq!(ecdsa_status(&format!("+4{}", &KEY[2..]), message, SIG, false), Ok(1));
    assert_eq!(ecdsa_status(KEY, "tampered", SIG, false), Ok(0));
    assert_eq!(ecdsa_status(KEY, "+f", SIG, true), Ok(0));
    for key in ["", "bad", "04", "03"] { assert_eq!(ecdsa_status(key, "gg", "bad", true), Ok(-1)); }
    for message in ["g", "gg", "-0", " f"] { assert_eq!(ecdsa_status(KEY, message, "bad", true), Ok(-2)); }
    for signature in ["", "bad", "3006020180020101", "300702020001020101", "300602010102010100", "308006020101020101"] {
        assert_eq!(ecdsa_status(KEY, "", signature, true), Ok(-3));
    }
    assert_eq!(ecdsa_status(KEY, "", "3006020100020100", true), Ok(0));
}

fn module() -> (ValidatedSafeWir, LayoutId) {
    let mut layouts = LayoutInterner::new();
    let byte = layouts.intern_scalar(ScalarKind::RawByte).unwrap();
    let layout = layouts.intern_runtime_buffer(byte).unwrap();
    let module = CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0), checked_source: CheckedSourceIdentity::from_digest([130; 32]),
        layouts: LayoutBundle::from_interner(&layouts, [layout]).unwrap(),
        signatures: vec![FunctionSignature { id: FunctionSignatureId::new(0), parameters: vec![], results: vec![],
            contract: CallableContract { parameters: vec![], results: vec![], parameter_effects: BTreeMap::new() } }],
        imports: vec![], proper_tail_sites: vec![], functions: vec![Function {
            id: FunctionId::new(0), signature: FunctionSignatureId::new(0), entry: BlockId::new(0),
            value_types: BTreeMap::new(), blocks: vec![BasicBlock {
                id: BlockId::new(0), parameters: vec![], operations: vec![], terminator: Terminator::Return { values: vec![] },
            }],
        }],
    });
    (validate_final_for_test(check_initial_with_test_witness(module).unwrap()).unwrap(), layout)
}

fn buffer(engine: &mut Engine<'_>, layout: LayoutId, values: &mut BTreeMap<ValueId, InterpreterValue>, bytes: &[u8], access: AccessMode) -> HostCallArgument {
    let object = engine.next_object;
    engine.next_object += 1;
    let id = ValueId::new(100 + object as u32 * 3);
    let start = ValueId::new(id.get() + 1);
    let length = ValueId::new(id.get() + 2);
    engine.objects.insert(object, Object {
        layout, storage: RuntimeStorage::Allocation(AllocationClass::ReferenceCounted),
        extent: RuntimeExtent::Counted { header_bytes: 4, count: bytes.len() as u64, capacity: bytes.len() as u64, stride_bytes: 1 },
        references: 1, immutable: false,
        cells: bytes.iter().enumerate().map(|(index, byte)| (CellKey::Element(index as u64), InterpreterValue::Size(u64::from(*byte)))).collect(),
        absent_owned_slots: BTreeSet::new(),
    });
    values.insert(id, InterpreterValue::ObjectRef(object));
    values.insert(start, InterpreterValue::Size(0));
    values.insert(length, InterpreterValue::Size(bytes.len() as u64));
    HostCallArgument::Buffer { place: Place::projected(id, Projection::Slice { start, length }, ValueType::Ref(layout), access), extent: HostBufferExtent::Slice }
}

#[test]
fn crypto_extensions_oracle_checks_initialized_inputs_and_full_output_capacity() {
    let (module, layout) = module();
    let mut engine = Engine::new(module.module(), InterpreterConfig::default());
    for invalid in ["uninitialized", "output-bounds", "utf8", "length-type", "length-range"] {
        let mut values = BTreeMap::new();
        let input = buffer(&mut engine, layout, &mut values, &[255], AccessMode::Read);
        let output = buffer(&mut engine, layout, &mut values, &[0xa5; 16], AccessMode::Write);
        let HostCallArgument::Buffer { place: input_place, .. } = &input else { unreachable!() };
        let HostCallArgument::Buffer { place: output_place, .. } = &output else { unreachable!() };
        let InterpreterValue::ObjectRef(input_object) = values[&input_place.object()] else { unreachable!() };
        let InterpreterValue::ObjectRef(output_object) = values[&output_place.object()] else { unreachable!() };
        let before = engine.objects[&output_object].cells.clone();
        if invalid == "uninitialized" { engine.objects.get_mut(&input_object).unwrap().cells.clear(); }
        if invalid == "output-bounds" {
            values.insert(ValueId::new(output_place.object().get() + 2), InterpreterValue::Size(17));
        }
        let length = ValueId::new(9000);
        values.insert(length, if invalid == "length-type" { InterpreterValue::Size(16) }
            else { InterpreterValue::Int(if invalid == "length-range" { -1 } else { 16 }) });
        let (name, arguments) = if invalid == "utf8" { (CRYPTO_SHA512_IMPORT, vec![input, output]) }
            else { (CRYPTO_SHAKE128_IMPORT, vec![input, HostCallArgument::Value(length), output]) };
        let destination = ValueId::new(9001);
        values.insert(destination, InterpreterValue::Size(77));
        assert!(engine.crypto_extension_host_call(name, &[destination], &arguments, &mut values).is_err(), "{invalid}");
        assert_eq!(values[&destination], InterpreterValue::Size(77));
        assert_eq!(engine.objects[&output_object].cells, before);
    }
}
