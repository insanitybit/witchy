use crate::runtime::{Capabilities, Runtime, Vm};
use crate::value::NativeValue as Value;
use witchy_syntax::intrinsics;

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

fn native_hash(name: &str, args: &[Value]) -> String {
    match crate::native::lookup(name).unwrap()(args).unwrap() {
        Value::Str(value) => value,
        _ => panic!("native digest must be a String"),
    }
}

fn assert_digest(vm: &mut Vm, name: &str, bytes: &[u8], digest: &str) {
    for capacity in [0, digest.len() - 1, digest.len(), digest.len() + 8] {
        let mut expected = input(vm, bytes);
        assert_eq!(invoke(vm, name, [INPUT as i32, bytes.len() as i32, OUTPUT as i32, capacity as i32]).unwrap(), digest.len() as i32);
        if capacity >= digest.len() {
            expected[OUTPUT..OUTPUT + digest.len()].copy_from_slice(digest.as_bytes());
        }
        memory_equals(vm, &expected);
    }
}

#[test]
fn sha256_slices_hash_raw_bytes_and_write_only_complete_digests() {
    let mut vm = setup();
    assert_digest(&mut vm, "sha", b"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_digest(&mut vm, "sha", b"abc", abc);
    let raw = [0, 0xff, 0x80, b'\r', b'\n'];
    let expected = native_hash(intrinsics::CRYPTO_SHA256_BYTES, &[Value::Bytes(raw.to_vec())]);
    assert_digest(&mut vm, "sha", &raw, &expected);
    let mut expected = input(&mut vm, b"abc");
    assert_eq!(invoke(&mut vm, "sha", [INPUT as i32, 3, INPUT as i32, 64]).unwrap(), 64);
    expected[INPUT..INPUT + 64].copy_from_slice(abc.as_bytes());
    memory_equals(&mut vm, &expected);
    let expected = input(&mut vm, b"");
    assert_eq!(invoke(&mut vm, "sha", [65536, 0, 65536, 0]).unwrap(), 64);
    memory_equals(&mut vm, &expected);
}

#[test]
fn rune_hash_slices_preserve_native_unicode_order_duplicates_and_normalization() {
    let mut vm = setup();
    for (paths, contents) in [
        (vec![], vec![]),
        (vec!["z.witchy", "\u{e9}.witchy", "a.witchy"], vec!["a\r\nb\r", "\u{1f980}\0", ""]),
        (vec!["same", "same", "../data\0"], vec!["first", "second", "path is data"]),
        (vec!["same", "same"], vec!["second", "first"]),
    ] {
        let list = |values: &[&str]| Value::List(values.iter().map(|value| Value::Str((*value).into())).collect());
        let expected = native_hash(intrinsics::CRYPTO_RUNE_HASH, &[list(&paths), list(&contents)]);
        let bytes = transport(&paths, &contents);
        assert_digest(&mut vm, "rune", &bytes, &expected);
        // Input/output overlap must not affect parsing or native normalization.
        let mut memory = input(&mut vm, &bytes);
        assert_eq!(invoke(&mut vm, "rune", [INPUT as i32, bytes.len() as i32, INPUT as i32, 71]).unwrap(), 71);
        memory[INPUT..INPUT + 71].copy_from_slice(expected.as_bytes());
        memory_equals(&mut vm, &memory);
    }
}

#[test]
fn rune_hash_slices_reject_malformed_transport_and_preserve_native_length_error() {
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

#[test]
fn crypto_hash_slices_preflight_full_input_and_output_ranges() {
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
