use crate::runtime::{Capabilities, Runtime, Vm};

const INPUT: usize = 17;
const OUTPUT: usize = 4097;
const WAT: &str = r#"(module
  (import "witchy" "crypto_sha256_slices" (func $sha (param i32 i32 i32 i32) (result i32)))
  (import "witchy" "crypto_rune_hash_slices" (func $rune (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "run"))
  (func (export "sha") (param i32 i32 i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 local.get 3 call $sha)
  (func (export "rune") (param i32 i32 i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 local.get 3 call $rune))"#;

fn setup() -> Vm {
    Runtime::new().unwrap().spawn(WAT, Capabilities::none(), 1).unwrap()
}

fn input(vm: &mut Vm, bytes: &[u8]) -> Vec<u8> {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.data_mut(&mut vm.store).fill(0xa5);
    memory.write(&mut vm.store, INPUT, bytes).unwrap();
    memory.data(&vm.store).to_vec()
}

fn invoke(vm: &mut Vm, name: &str, args: [i32; 4]) -> wasmtime::Result<i32> {
    let function = vm.instance.get_typed_func::<(i32, i32, i32, i32), i32>(&mut vm.store, name).unwrap();
    let result = function.call(&mut vm.store, (args[0], args[1], args[2], args[3]));
    assert!(vm.output().is_empty());
    result
}

fn memory_equals(vm: &mut Vm, expected: &[u8]) {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    assert_eq!(memory.data(&vm.store), expected);
}

fn transport(paths: &[&str], contents: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for values in [paths, contents] {
        bytes.extend_from_slice(&(values.len() as u32).to_le_bytes());
        for value in values {
            bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
            bytes.extend_from_slice(value.as_bytes());
        }
    }
    bytes
}

pub fn rune_hash_slices_reject_malformed_transport_and_preserve_native_length_error() {
    let mut vm = setup();
    let mut trailing = transport(&[], &[]);
    trailing.push(0);
    let mut bad_length = 1_u32.to_le_bytes().to_vec();
    bad_length.extend_from_slice(&u32::MAX.to_le_bytes());
    bad_length.extend_from_slice(&0_u32.to_le_bytes());
    let mut bad_utf8 = 1_u32.to_le_bytes().to_vec();
    bad_utf8.extend_from_slice(&1_u32.to_le_bytes());
    bad_utf8.push(0xff);
    bad_utf8.extend_from_slice(&0_u32.to_le_bytes());
    let mut bad_second_count = 0_u32.to_le_bytes().to_vec();
    bad_second_count.extend_from_slice(&u32::MAX.to_le_bytes());
    for bytes in [vec![], vec![0; 4], u32::MAX.to_le_bytes().to_vec(), bad_second_count, bad_length, bad_utf8, trailing] {
        for capacity in [0, 71] {
            let expected = input(&mut vm, &bytes);
            assert!(invoke(&mut vm, "rune", [INPUT as i32, bytes.len() as i32, OUTPUT as i32, capacity]).is_err());
            memory_equals(&mut vm, &expected);
        }
    }
    let bytes = transport(&["one"], &[]);
    let expected = input(&mut vm, &bytes);
    let error = invoke(&mut vm, "rune", [INPUT as i32, bytes.len() as i32, OUTPUT as i32, 71]).unwrap_err();
    assert_eq!(error.root_cause().to_string(), "crypto.rune_hash: paths and contents differ in length");
    memory_equals(&mut vm, &expected);
}

pub fn crypto_hash_slices_preflight_full_input_and_output_ranges() {
    let mut vm = setup();
    for name in ["sha", "rune"] {
        let bytes = transport(&[], &[]);
        for slot in [0, 2] {
            for (ptr, len) in [(-1, 1), (0, -1), (65535, 2), (65537, 0), (i32::MAX, i32::MAX)] {
                let expected = input(&mut vm, &bytes);
                let mut args = [INPUT as i32, bytes.len() as i32, OUTPUT as i32, 71];
                args[slot] = ptr;
                args[slot + 1] = len;
                let error = invoke(&mut vm, name, args).unwrap_err();
                assert_eq!(error.root_cause().to_string(), "crypto hash slice lies outside guest memory");
                memory_equals(&mut vm, &expected);
            }
        }
    }
    // Output preflight happens even before a malformed transport is decoded.
    let bytes = u32::MAX.to_le_bytes();
    let expected = input(&mut vm, &bytes);
    let error = invoke(&mut vm, "rune", [INPUT as i32, 4, -1, 0]).unwrap_err();
    assert_eq!(error.root_cause().to_string(), "crypto hash slice lies outside guest memory");
    memory_equals(&mut vm, &expected);
}
