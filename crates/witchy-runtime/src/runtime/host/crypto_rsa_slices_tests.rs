use crate::runtime::{Capabilities, Runtime, Vm};

// Fixed RSA-2048 PKCS#1/SHA-256 vector generated locally with OpenSSL. Only
// public verification material is retained here; no runtime signing or network.
const PUBLIC_KEY: &str = concat!(
    "3082010a028201010097d2114ea48b4a9cdf17f78d5ef37cbf7d7c1fed64",
    "0b5fad2288ba034428e6da9b0bb0f8e5c0686e0ff3f894684ccbffa6f93e",
    "45d09e2874b62af6bd850785347df0452a6dd61bcedc10962e0de9bbbca2",
    "8b32d771e97796359e1ef09951c8eee3c084240da53dd742ea727e7228d4",
    "63c1852f8d936387db339980bef5d925885e51aa6ae8da97c2fd8b8be548",
    "f5423f94af99d06be7d28f8c7041650ddca3c883ddf7102696cac4d08785",
    "9b29098ea852b0c01914a08601804927a73100b0ca02d175e9bc2cdf779d",
    "9578ae1b8202aa17732657b4b620af887cece3cd07233f1eb8bbdffe27aa",
    "93e2731ba17ddc257c21ea2e26dfde673b8f80ca518acd9ab50203010001",
);
const MESSAGE: &str = "RSA bounded fixture\0caf\u{e9}\n";
const SIGNATURE: &str = concat!(
    "5b73f6d34e8b9895e1161a88da1c61663e365e5f025c8544a145f78bf8cd",
    "6624628415ec9652cd0f0209970f1e57b9db9a74be5713ce8b3e28f0ca78",
    "7b3e29a16d8a96c158e557c59ac7957dd139a301a92d93b233b1202b2fba",
    "cda7031399244bad665a19e72961fae2b1f57a3e00c33837cd025edd4887",
    "c0c05244d59fcf0f53d4ebca29dbf309027925b92a36e536bb6eb3927ac7",
    "306169524fb5e8f620f7fbadf3a831b9b1ed45c227074f0d38259eefcbaa",
    "5a1f0a82229d927554f576346a3842667948d287020c19055b5efc413ad9",
    "1c2359a0631fcbee35b05aad4e6f6f608d2a6cbe8c2d984b3265ccab8caa",
    "caf7535fb5bb3c5f729b3362d36b084a",
);
const WAT: &str = r#"(module
  (import "witchy" "crypto_rsa_pkcs1_sha256_verify_slices"
    (func $verify (param i32 i32 i32 i32 i32 i32) (result i64)))
  (memory (export "memory") 1)
  (func (export "run"))
  (func (export "verify") (param i32 i32 i32 i32 i32 i32) (result i64)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 local.get 5
    call $verify))"#;

fn setup() -> Vm {
    let mut runtime = Runtime::new().unwrap();
    let mut vm = runtime.spawn(WAT, Capabilities::none(), 1).unwrap();
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    // Unaligned inputs are raw slices, not plausible managed string headers.
    memory.data_mut(&mut vm.store).fill(0xa5);
    vm
}

fn inputs(vm: &mut Vm, key: &[u8], message: &[u8], signature: &[u8]) -> [i32; 6] {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    for (offset, bytes) in [(17, key), (1025, message), (2049, signature)] {
        memory.write(&mut vm.store, offset, bytes).unwrap();
    }
    [17, key.len() as i32, 1025, message.len() as i32, 2049, signature.len() as i32]
}

fn invoke(vm: &mut Vm, [a, b, c, d, e, f]: [i32; 6]) -> wasmtime::Result<i64> {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    let before = memory.data(&vm.store).to_vec();
    let verify = vm.instance.get_typed_func::<(i32, i32, i32, i32, i32, i32), i64>(
        &mut vm.store, "verify").unwrap();
    let result = verify.call(&mut vm.store, (a, b, c, d, e, f));
    assert_eq!(memory.data(&vm.store), before.as_slice(), "RSA verification must be guest-read-only");
    assert!(vm.output().is_empty(), "RSA verification must not emit output");
    result
}

#[test]
fn rsa_slices_known_answer_and_native_status_order() {
    let mut vm = setup();
    for (key, message, signature, expected) in [
        (PUBLIC_KEY, MESSAGE, SIGNATURE, 1),
        (PUBLIC_KEY, "altered", SIGNATURE, 0),
        (PUBLIC_KEY, "", SIGNATURE, 0),
        ("not-hex", MESSAGE, SIGNATURE, -1),
        ("00", MESSAGE, SIGNATURE, -1),
        ("", MESSAGE, SIGNATURE, -1),
        ("not-hex", MESSAGE, "not-hex", -1),
        (PUBLIC_KEY, MESSAGE, "not-hex", -3),
        (PUBLIC_KEY, MESSAGE, "0", -3),
        (PUBLIC_KEY, MESSAGE, "", -3),
        // Nonempty, well-formed hex with the wrong signature length is 0, not -3.
        (PUBLIC_KEY, MESSAGE, "00", 0),
        (PUBLIC_KEY, MESSAGE, SIGNATURE, 1),
    ] {
        let args = inputs(&mut vm, key.as_bytes(), message.as_bytes(), signature.as_bytes());
        assert_eq!(invoke(&mut vm, args).unwrap(), expected);
    }
}

#[test]
fn rsa_slices_preserve_native_plus_hex_spelling() {
    let mut vm = setup();
    let mut signature = SIGNATURE.as_bytes().to_vec();
    let pair = signature.chunks_exact_mut(2).find(|pair| *pair == b"0f")
        .expect("fixed signature includes a 0f byte");
    pair[0] = b'+';
    let args = inputs(&mut vm, PUBLIC_KEY.as_bytes(), MESSAGE.as_bytes(), &signature);
    assert_eq!(invoke(&mut vm, args).unwrap(), 1, "+f must decode identically to 0f");
    for signature in [b"0f".as_slice(), b"+f".as_slice()] {
        let args = inputs(&mut vm, PUBLIC_KEY.as_bytes(), MESSAGE.as_bytes(), signature);
        assert_eq!(invoke(&mut vm, args).unwrap(), 0, "valid hex, invalid signature");
    }
}

#[test]
fn rsa_slices_preflight_every_range_without_wraparound() {
    let mut vm = setup();
    let valid = inputs(&mut vm, PUBLIC_KEY.as_bytes(), MESSAGE.as_bytes(), SIGNATURE.as_bytes());
    for slot in [0, 2, 4] {
        for (ptr, len) in [
            (-1, 1), (0, -1), (-1, -1), (i32::MIN, 1), (0, i32::MIN),
            (i32::MAX, i32::MAX), (65535, 2), (65537, 0),
        ] {
            let mut args = valid;
            args[slot] = ptr;
            args[slot + 1] = len;
            assert_eq!(invoke(&mut vm, args).unwrap_err().root_cause().to_string(),
                "RSA input slice lies outside guest memory", "slot {slot}: {ptr}/{len}");
        }
    }
    // Even invalid key UTF-8 cannot hide a later out-of-bounds input.
    let mut args = inputs(&mut vm, &[0xff], b"", SIGNATURE.as_bytes());
    args[4] = 65535;
    args[5] = 2;
    assert_eq!(invoke(&mut vm, args).unwrap_err().root_cause().to_string(),
        "RSA input slice lies outside guest memory");
}

#[test]
fn rsa_slices_decode_all_utf8_before_native_status_short_circuit() {
    let mut vm = setup();
    for slot in [0, 2, 4] {
        for invalid in [b"\xff".as_slice(), b"\xc2".as_slice(), b"\xc0\x80".as_slice()] {
            let mut args = inputs(&mut vm, PUBLIC_KEY.as_bytes(), MESSAGE.as_bytes(), SIGNATURE.as_bytes());
            let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
            memory.write(&mut vm.store, args[slot] as usize, invalid).unwrap();
            args[slot + 1] = invalid.len() as i32;
            assert_eq!(invoke(&mut vm, args).unwrap_err().root_cause().to_string(),
                "RSA input slice is not valid UTF-8");
        }
    }
    for (message, signature) in [(b"\xff".as_slice(), b"".as_slice()), (b"".as_slice(), b"\xff".as_slice())] {
        let args = inputs(&mut vm, b"not-hex", message, signature);
        assert_eq!(invoke(&mut vm, args).unwrap_err().root_cause().to_string(),
            "RSA input slice is not valid UTF-8");
    }
}

#[test]
fn rsa_slices_exact_end_empty_aliases_and_missing_memory() {
    let mut vm = setup();
    for (slot, bytes) in [(0, PUBLIC_KEY.as_bytes()), (2, MESSAGE.as_bytes()), (4, SIGNATURE.as_bytes())] {
        let mut args = inputs(&mut vm, PUBLIC_KEY.as_bytes(), MESSAGE.as_bytes(), SIGNATURE.as_bytes());
        let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
        let end = memory.data_size(&vm.store);
        memory.write(&mut vm.store, end - bytes.len(), bytes).unwrap();
        args[slot] = (end - bytes.len()) as i32;
        assert_eq!(invoke(&mut vm, args).unwrap(), 1);
    }
    for (slot, expected) in [(0, -1), (2, 0), (4, -3)] {
        let mut args = inputs(&mut vm, PUBLIC_KEY.as_bytes(), MESSAGE.as_bytes(), SIGNATURE.as_bytes());
        args[slot] = 65536;
        args[slot + 1] = 0;
        assert_eq!(invoke(&mut vm, args).unwrap(), expected);
    }
    let mut args = inputs(&mut vm, PUBLIC_KEY.as_bytes(), MESSAGE.as_bytes(), SIGNATURE.as_bytes());
    args[2] = args[0];
    args[3] = args[1];
    assert_eq!(invoke(&mut vm, args).unwrap(), 0, "overlapping read slices need no write authority");

    let mut runtime = Runtime::new().unwrap();
    let wat = WAT.replace("(memory (export \"memory\") 1)", "");
    let mut vm = runtime.spawn(wat.as_bytes(), Capabilities::none(), 1).unwrap();
    let verify = vm.instance.get_typed_func::<(i32, i32, i32, i32, i32, i32), i64>(
        &mut vm.store, "verify").unwrap();
    assert!(verify.call(&mut vm.store, (0, 0, 0, 0, 0, 0)).is_err());
    assert!(vm.output().is_empty());
}
