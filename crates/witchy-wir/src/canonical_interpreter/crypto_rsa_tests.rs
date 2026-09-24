use super::*;
use crate::canonical_safewir::{
    check_initial_with_test_witness, validate_final_for_test, AccessMode, CallableContract, CheckedSourceIdentity,
    HostBufferExtent, LoweredModuleParts, Revision,
};

#[path = "crypto_rsa_testdata.rs"]
mod vector;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn crypto_rsa_oracle_known_answer_and_tampered_inputs() {
    assert_eq!(rsa_verify_status(vector::KEY, vector::MESSAGE, vector::SIGNATURE), Ok(1));
    assert_eq!(rsa_verify_status(&vector::KEY.to_uppercase(), vector::MESSAGE, &vector::SIGNATURE.to_uppercase()), Ok(1));
    assert_eq!(rsa_verify_status(vector::KEY, "test message!", vector::SIGNATURE), Ok(0));
    assert_eq!(rsa_verify_status(vector::KEY, "test message\0", vector::SIGNATURE), Ok(0));
    assert_eq!(rsa_verify_status(vector::KEY, "test message\u{e9}", vector::SIGNATURE), Ok(0));
    let mut signature = rsa_hex(vector::SIGNATURE).unwrap();
    signature[0] ^= 1;
    assert_eq!(rsa_verify_status(vector::KEY, vector::MESSAGE, &hex(&signature)), Ok(0));
    assert_eq!(rsa_verify_status(vector::KEY, vector::MESSAGE, "00"), Ok(0));
    // Every low-valued byte can use native radix parsing's plus spelling.
    let plus = |text: &str| rsa_hex(text).unwrap().into_iter().map(|byte|
        if byte < 16 { format!("+{byte:x}") } else { format!("{byte:02x}") }).collect::<String>();
    assert_eq!(rsa_verify_status(&plus(vector::KEY), vector::MESSAGE, &plus(vector::SIGNATURE)), Ok(1));
}

#[test]
fn crypto_rsa_oracle_pkcs1_encoding_boundaries() {
    let key = rsa_hex(vector::KEY).unwrap();
    assert!(rsa_key_encoding(&key));
    for end in 0..key.len() {
        assert!(!rsa_key_encoding(&key[..end]), "truncated at {end}");
    }
    let mut trailing = key.clone();
    trailing.push(0);
    assert!(!rsa_key_encoding(&trailing));
    let mut wrong_tag = key.clone();
    wrong_tag[0] = 0x31;
    assert!(!rsa_key_encoding(&wrong_tag));
    // Shape-only fixtures isolate native status classification from RSA math.
    let make_key = |modulus: &[u8], exponent: &[u8]| {
        let integer = |bytes: &[u8]| {
            let mut encoded = vec![2, 0x82, (bytes.len() >> 8) as u8, bytes.len() as u8];
            if bytes.len() < 128 { encoded = vec![2, bytes.len() as u8]; }
            encoded.extend_from_slice(bytes);
            encoded
        };
        let mut body = integer(modulus);
        body.extend(integer(exponent));
        let mut key = vec![0x30, 0x82, (body.len() >> 8) as u8, body.len() as u8];
        key.extend(body);
        key
    };
    for size in [255, 256, 1024, 1025] {
        assert_eq!(rsa_key_encoding(&make_key(&vec![1; size], &[3])), (256..=1024).contains(&size));
    }
    for exponent in [vec![], vec![0], vec![1], vec![2], vec![0, 3], vec![128], vec![1; 9]] {
        assert!(!rsa_key_encoding(&make_key(&vec![1; 256], &exponent)));
    }
    assert!(rsa_key_encoding(&make_key(&vec![1; 256], &[0, 129])));
    assert!(!rsa_integer(&[0, 1]));
    assert!(!rsa_integer(&[128]));
    assert!(rsa_integer(&[0, 128]));
    assert!(rsa_tlv(&[0x30, 0x80], 0x30).is_none());
    assert!(rsa_tlv(&[0x30, 0x81, 1, 0], 0x30).is_none());
    assert!(rsa_tlv(&[0x30, 0x85, 0, 0, 0, 0, 128], 0x30).is_none());
    let mut redundant_length = vec![0x30, 0x83, 0, 0, 128];
    redundant_length.extend([0; 128]);
    assert!(rsa_tlv(&redundant_length, 0x30).is_some(), "native accepts redundant long length octets");
}

fn module() -> (ValidatedSafeWir, LayoutId) {
    let mut layouts = LayoutInterner::new();
    let byte = layouts.intern_scalar(ScalarKind::RawByte).unwrap();
    let layout = layouts.intern_runtime_buffer(byte).unwrap();
    let module = CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0), checked_source: CheckedSourceIdentity::from_digest([129; 32]),
        layouts: LayoutBundle::from_interner(&layouts, [layout]).unwrap(),
        signatures: vec![FunctionSignature {
            id: FunctionSignatureId::new(0), parameters: vec![], results: vec![],
            contract: CallableContract { parameters: vec![], results: vec![], parameter_effects: BTreeMap::new() },
        }], imports: vec![], proper_tail_sites: vec![],
        functions: vec![Function {
            id: FunctionId::new(0), signature: FunctionSignatureId::new(0), entry: BlockId::new(0),
            value_types: BTreeMap::new(), blocks: vec![BasicBlock {
                id: BlockId::new(0), parameters: vec![], operations: vec![],
                terminator: Terminator::Return { values: vec![] },
            }],
        }],
    });
    (validate_final_for_test(check_initial_with_test_witness(module).unwrap()).unwrap(), layout)
}

fn buffer(engine: &mut Engine<'_>, layout: LayoutId, values: &mut BTreeMap<ValueId, InterpreterValue>, data: &[u8]) -> HostCallArgument {
    let object = engine.next_object;
    engine.next_object += 1;
    let id = ValueId::new(100 + object as u32 * 3);
    let start = ValueId::new(id.get() + 1);
    let length = ValueId::new(id.get() + 2);
    engine.objects.insert(object, Object {
        layout, storage: RuntimeStorage::Allocation(AllocationClass::ReferenceCounted),
        extent: RuntimeExtent::Counted { header_bytes: 8, count: data.len() as u64, capacity: data.len() as u64, stride_bytes: 1 },
        references: 1, immutable: false,
        cells: data.iter().enumerate().map(|(index, byte)| (CellKey::Element(index as u64), InterpreterValue::Size(u64::from(*byte)))).collect(),
        absent_owned_slots: BTreeSet::new(),
    });
    values.insert(id, InterpreterValue::ObjectRef(object));
    values.insert(start, InterpreterValue::Size(0));
    values.insert(length, InterpreterValue::Size(data.len() as u64));
    HostCallArgument::Buffer {
        place: Place::projected(id, Projection::Slice { start, length }, ValueType::Ref(layout), AccessMode::Read),
        extent: HostBufferExtent::Slice,
    }
}

#[test]
fn crypto_rsa_oracle_transport_refuses_before_assigning_status() {
    let (module, layout) = module();
    let mut engine = Engine::new(module.module(), InterpreterConfig::default());
    for invalid in ["utf8", "uninitialized", "bounds", "oversized-size", "arity"] {
        let mut values = BTreeMap::new();
        let key = buffer(&mut engine, layout, &mut values, b"bad key hex");
        let message = buffer(&mut engine, layout, &mut values, if invalid == "utf8" { &[255] } else { b"message" });
        let signature = buffer(&mut engine, layout, &mut values, b"00");
        let HostCallArgument::Buffer { place, .. } = &message else { unreachable!() };
        let InterpreterValue::ObjectRef(object) = values[&place.object()] else { unreachable!() };
        match invalid {
            "uninitialized" => { engine.objects.get_mut(&object).unwrap().cells.remove(&CellKey::Element(0)); }
            "bounds" | "oversized-size" => {
                // Buffer helper assigns the slice length at object ValueId+2.
                let length = ValueId::new(place.object().get() + 2);
                values.insert(length, InterpreterValue::Size(if invalid == "bounds" { 1000 } else { u64::MAX }));
                if invalid == "oversized-size" {
                    values.insert(ValueId::new(place.object().get() + 1), InterpreterValue::Size(1));
                }
            }
            _ => {}
        }
        let destination = ValueId::new(9000);
        values.insert(destination, InterpreterValue::Int(77));
        let arguments = if invalid == "arity" { vec![key, message] } else { vec![key, message, signature] };
        let result = engine.rsa_verify_host_call(&[destination], &arguments, &mut values);
        if invalid == "oversized-size" {
            // slice_place rejects a Size above u32::MAX before range addition.
            assert_eq!(result, Err(InterpretError::ArithmeticTrap));
        } else {
            assert!(result.is_err(), "{invalid}");
        }
        assert_eq!(values[&destination], InterpreterValue::Int(77), "{invalid}");
    }
}

#[test]
fn crypto_rsa_oracle_range_addition_overflow_is_not_ordinary_bounds() {
    let (module, layout) = module();
    let mut engine = Engine::new(module.module(), InterpreterConfig::default());
    let mut values = BTreeMap::new();
    let argument = buffer(&mut engine, layout, &mut values, b"message");
    let HostCallArgument::Buffer { place, .. } = argument else { unreachable!() };
    let InterpreterValue::ObjectRef(object) = values[&place.object()] else { unreachable!() };
    // White-box arithmetic control: this oversized length cannot pass the
    // handler's Size validation. Test checked_add itself without claiming it
    // is reached by the handler's oversized-Size probe above.
    assert_eq!(engine.check_range(object, 1, u64::MAX, false), Err(InterpretError::ArithmeticTrap));
    assert_eq!(engine.check_range(object, 0, u64::MAX, false),
        Err(InterpretError::IndexOutOfBounds { index: u64::MAX, length: 7 }));
}
