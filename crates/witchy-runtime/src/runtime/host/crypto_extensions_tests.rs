use crate::runtime::{Capabilities, Runtime, Vm};

const WAT: &str = r#"(module
  (import "witchy" "crypto_sha512_slices" (func $sha512 (param i32 i32 i32 i32) (result i32)))
  (import "witchy" "crypto_sha3_256_slices" (func $sha3 (param i32 i32 i32 i32) (result i32)))
  (import "witchy" "crypto_hmac_sha256_slices" (func $hmac (param i32 i32 i32 i32 i32 i32) (result i32)))
  (import "witchy" "crypto_shake128_slices" (func $shake128 (param i32 i32 i64 i32 i32) (result i32)))
  (import "witchy" "crypto_shake256_slices" (func $shake256 (param i32 i32 i64 i32 i32) (result i32)))
  (import "witchy" "crypto_ecdsa_p256_verify_slices" (func $ecdsa (param i32 i32 i32 i32 i32 i32) (result i64)))
  (import "witchy" "crypto_ecdsa_p256_verify_hex_slices" (func $ecdsa_hex (param i32 i32 i32 i32 i32 i32) (result i64)))
  (memory (export "memory") 1)
  (func (export "run"))
  ;; Enter a guest frame so Caller resolves this instance's exported memory.
  (func (export "sha512") (param i32 i32 i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 local.get 3 call $sha512)
  (func (export "sha3") (param i32 i32 i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 local.get 3 call $sha3)
  (func (export "hmac") (param i32 i32 i32 i32 i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 local.get 5 call $hmac)
  (func (export "shake128") (param i32 i32 i64 i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $shake128)
  (func (export "shake256") (param i32 i32 i64 i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $shake256)
  (func (export "ecdsa") (param i32 i32 i32 i32 i32 i32) (result i64)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 local.get 5 call $ecdsa)
  (func (export "ecdsa_hex") (param i32 i32 i32 i32 i32 i32) (result i64)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 local.get 5 call $ecdsa_hex))"#;

fn setup() -> Vm { Runtime::new().unwrap().spawn(WAT, Capabilities::none(), 1).unwrap() }
fn seed(vm: &mut Vm, inputs: &[(usize, &[u8])]) -> Vec<u8> {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.data_mut(&mut vm.store).fill(0xa5);
    for &(offset, bytes) in inputs { memory.write(&mut vm.store, offset, bytes).unwrap(); }
    memory.data(&vm.store).to_vec()
}
fn unchanged(vm: &mut Vm, expected: &[u8]) {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    assert_eq!(memory.data(&vm.store), expected);
    assert!(vm.output().is_empty());
}
fn hash(vm: &mut Vm, name: &str, [a, b, c, d]: [i32; 4]) -> wasmtime::Result<i32> {
    vm.instance.get_typed_func::<(i32, i32, i32, i32), i32>(&mut vm.store, name).unwrap()
        .call(&mut vm.store, (a, b, c, d))
}
fn hmac(vm: &mut Vm, [a, b, c, d, e, f]: [i32; 6]) -> wasmtime::Result<i32> {
    vm.instance.get_typed_func::<(i32, i32, i32, i32, i32, i32), i32>(&mut vm.store, "hmac").unwrap()
        .call(&mut vm.store, (a, b, c, d, e, f))
}
fn shake(vm: &mut Vm, name: &str, (a, b, n, c, d): (i32, i32, i64, i32, i32)) -> wasmtime::Result<i32> {
    vm.instance.get_typed_func::<(i32, i32, i64, i32, i32), i32>(&mut vm.store, name).unwrap()
        .call(&mut vm.store, (a, b, n, c, d))
}
fn verify(vm: &mut Vm, name: &str, [a, b, c, d, e, f]: [i32; 6]) -> wasmtime::Result<i64> {
    vm.instance.get_typed_func::<(i32, i32, i32, i32, i32, i32), i64>(&mut vm.store, name).unwrap()
        .call(&mut vm.store, (a, b, c, d, e, f))
}

pub fn crypto_extensions_slices_preflight_every_range_before_decoding() {
    let mut vm = setup();
    for (pointer, length) in [(-1, 1), (0, -1), (65535, 2), (65537, 0), (i32::MAX, i32::MAX)] {
        for slot in [0, 2] {
            let memory = seed(&mut vm, &[(17, &[0xff])]);
            let mut args = [17, 1, 4097, 128];
            args[slot] = pointer; args[slot + 1] = length;
            for name in ["sha512", "sha3"] {
                let error = hash(&mut vm, name, args).unwrap_err();
                assert_eq!(error.root_cause().to_string(), "crypto extension slice lies outside guest memory");
                unchanged(&mut vm, &memory);
            }
            for name in ["shake128", "shake256"] {
                let error = shake(&mut vm, name, (args[0], args[1], -1, args[2], args[3])).unwrap_err();
                assert_eq!(error.root_cause().to_string(), "crypto extension slice lies outside guest memory");
                unchanged(&mut vm, &memory);
            }
        }
        for slot in [0, 2, 4] {
            let memory = seed(&mut vm, &[(17, &[0xff])]);
            let mut args = [17, 1, 1025, 0, 4097, 64];
            args[slot] = pointer; args[slot + 1] = length;
            assert_eq!(hmac(&mut vm, args).unwrap_err().root_cause().to_string(), "crypto extension slice lies outside guest memory");
            for name in ["ecdsa", "ecdsa_hex"] {
                assert_eq!(verify(&mut vm, name, args).unwrap_err().root_cause().to_string(), "crypto extension slice lies outside guest memory");
            }
            unchanged(&mut vm, &memory);
        }
    }
    for name in ["sha512", "sha3"] {
        let memory = seed(&mut vm, &[(17, &[0xff])]);
        assert!(hash(&mut vm, name, [17, 1, 4097, 0]).is_err());
        unchanged(&mut vm, &memory);
    }
    for key in [b"g".as_slice(), b"gg", b"-0", b" f", b"f "] {
        let memory = seed(&mut vm, &[(17, key), (1025, b"message")]);
        assert!(hmac(&mut vm, [17, key.len() as i32, 1025, 7, 4097, 0]).is_err());
        unchanged(&mut vm, &memory);
    }
    for name in ["ecdsa", "ecdsa_hex"] {
        let memory = seed(&mut vm, &[(17, b"bad"), (1025, &[0xff])]);
        assert_eq!(verify(&mut vm, name, [17, 3, 1025, 1, 4097, 0]).unwrap_err().root_cause().to_string(),
            "crypto extension input is not valid UTF-8");
        unchanged(&mut vm, &memory);
    }
}

