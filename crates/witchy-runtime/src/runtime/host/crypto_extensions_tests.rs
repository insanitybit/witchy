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

#[test]
fn crypto_extensions_slices_hash_vectors_short_outputs_and_overlap() {
    let mut vm = setup();
    for (name, expected) in [
        ("sha512", "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"),
        ("sha3", "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532"),
    ] {
        for capacity in [0, expected.len() - 1, expected.len(), expected.len() + 8] {
            for output in [17, 4097] {
                let mut memory = seed(&mut vm, &[(17, b"abc")]);
                assert_eq!(hash(&mut vm, name, [17, 3, output, capacity as i32]).unwrap(), expected.len() as i32);
                if capacity >= expected.len() {
                    memory[output as usize..output as usize + expected.len()].copy_from_slice(expected.as_bytes());
                }
                unchanged(&mut vm, &memory);
            }
        }
    }
    for key in ["0b".repeat(20), "+b".repeat(20)] {
        for capacity in [0, 63, 64, 72] {
            let mut memory = seed(&mut vm, &[(17, key.as_bytes()), (1025, b"Hi There")]);
            assert_eq!(hmac(&mut vm, [17, key.len() as i32, 1025, 8, 17, capacity]).unwrap(), 64);
            if capacity >= 64 {
                memory[17..81].copy_from_slice(b"b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
            }
            unchanged(&mut vm, &memory);
        }
    }
}

#[test]
fn crypto_extensions_shake_slices_raw_zero_max_and_short_outputs() {
    let mut vm = setup();
    for (name, expected) in [
        ("shake128", [0x7f, 0x9c, 0x2b, 0xa4, 0xe8, 0x8f, 0x82, 0x7d, 0x61, 0x60, 0x45, 0x50, 0x76, 0x05, 0x85, 0x3e]),
        ("shake256", [0x46, 0xb9, 0xdd, 0x2b, 0x0b, 0xa8, 0x8d, 0x13, 0x23, 0x3b, 0x3f, 0xeb, 0x74, 0x3e, 0xeb, 0x24]),
    ] {
        for cap in [0, 15, 16, 32] {
            let mut memory = seed(&mut vm, &[]);
            assert_eq!(shake(&mut vm, name, (65536, 0, 16, 17, cap)).unwrap(), 16);
            if cap >= 16 { memory[17..33].copy_from_slice(&expected); }
            unchanged(&mut vm, &memory);
        }
        let memory = seed(&mut vm, &[(17, &[0xff, 0x80, 0])]);
        assert_eq!(shake(&mut vm, name, (17, 3, 0, 65536, 0)).unwrap(), 0);
        assert_eq!(shake(&mut vm, name, (17, 3, 1_048_576, 65536, 0)).unwrap(), 1_048_576);
        unchanged(&mut vm, &memory);
        for length in [-1, 1_048_577, i64::MIN, i64::MAX] {
            assert!(shake(&mut vm, name, (17, 3, length, 4097, 64)).is_err());
            unchanged(&mut vm, &memory);
        }
    }
    let mut memory = seed(&mut vm, &[(17, &[0xa3; 200])]);
    assert_eq!(shake(&mut vm, "shake128", (17, 200, 16, 17, 16)).unwrap(), 16);
    memory[17..33].copy_from_slice(&[0x13, 0x1a, 0xb8, 0xd2, 0xb5, 0x94, 0x94, 0x6b, 0x9c, 0x81, 0x33, 0x3f, 0x9b, 0xb6, 0xe0, 0xce]);
    unchanged(&mut vm, &memory);
}

#[test]
fn crypto_extensions_slices_preflight_every_range_before_decoding() {
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

const KEY: &str = "048f81cd9fca785a42a6f5dd58972cc0f702e83b1c960b5912354471496597e227fec81ff1d52530b06d7091649e6beb49dba70968b4b727bb24e3ceb7dd01a039";
const SIG: &str = "304402203260029f4c6beb2e78afdd906c057c63f8828e2b03820de7053d97254577fb8c02204478b9b75f8fd7a1ce4298f0d119e12926dafda116ae4c197b0048dc117bc9de";

#[test]
fn crypto_extensions_ecdsa_slices_preserve_exact_statuses_and_read_only_inputs() {
    let mut vm = setup();
    let message = "webauthn-es256-test-message";
    let encoded: String = message.bytes().map(|byte| format!("{byte:02x}")).collect();
    let plus = format!("+4{}", &KEY[2..]);
    for (name, key, message, signature, expected) in [
        ("ecdsa", KEY, message, SIG, 1), ("ecdsa_hex", KEY, encoded.as_str(), SIG, 1),
        ("ecdsa", plus.as_str(), message, SIG, 1),
        ("ecdsa", KEY, "tampered", SIG, 0), ("ecdsa_hex", KEY, "+f", SIG, 0),
        ("ecdsa_hex", "bad", "bad", "bad", -1), ("ecdsa_hex", KEY, "gg", "bad", -2),
        ("ecdsa_hex", KEY, "", "bad", -3),
        ("ecdsa", KEY, message, "3006020180020101", -3),
        ("ecdsa", KEY, message, "300702020001020101", -3),
        ("ecdsa", KEY, message, "3006020100020100", 0),
    ] {
        let memory = seed(&mut vm, &[(17, key.as_bytes()), (1025, message.as_bytes()), (2049, signature.as_bytes())]);
        assert_eq!(verify(&mut vm, name, [17, key.len() as i32, 1025, message.len() as i32, 2049, signature.len() as i32]).unwrap(), expected);
        unchanged(&mut vm, &memory);
    }
}
