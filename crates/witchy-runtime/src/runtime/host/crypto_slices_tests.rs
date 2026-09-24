use crate::runtime::{Capabilities, Runtime, Vm};

// RFC 8032 section 7.1, test 1: public verification material only.
const PUBLIC_KEY: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
const SIGNATURE: &str = concat!(
    "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555f",
    "b8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
);
const PK: usize = 17;
const MESSAGE: usize = 257;
const SIG: usize = 513;
const WAT: &str = r#"(module
  (import "witchy" "crypto_ed25519_verify_slices"
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
    // In particular, bytes preceding the unaligned inputs are not headers.
    memory.data_mut(&mut vm.store).fill(0xa5);
    vm
}

fn write_inputs(vm: &mut Vm, pk: &[u8], message: &[u8], signature: &[u8]) -> [i32; 6] {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    for (offset, bytes) in [(PK, pk), (MESSAGE, message), (SIG, signature)] {
        memory.write(&mut vm.store, offset, bytes).unwrap();
    }
    [PK as i32, pk.len() as i32, MESSAGE as i32, message.len() as i32,
        SIG as i32, signature.len() as i32]
}

fn invoke(vm: &mut Vm, [a, b, c, d, e, f]: [i32; 6]) -> wasmtime::Result<i64> {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    let before = memory.data(&vm.store).to_vec();
    let verify = vm.instance.get_typed_func::<(i32, i32, i32, i32, i32, i32), i64>(
        &mut vm.store, "verify").unwrap();
    let result = verify.call(&mut vm.store, (a, b, c, d, e, f));
    assert_eq!(memory.data(&vm.store), before.as_slice(), "verification must not write guest memory");
    assert!(vm.output().is_empty(), "verification must not emit output");
    result
}

#[test]
fn ed25519_slices_reject_invalid_extents_and_utf8_without_writes() {
    let mut vm = setup();
    let valid = write_inputs(&mut vm, PUBLIC_KEY.as_bytes(), b"", SIGNATURE.as_bytes());
    for slot in [0, 2, 4] {
        for (ptr, len) in [(-1, 1), (65535, 2), (0, -1), (i32::MAX, i32::MAX), (65537, 0)] {
            let mut args = valid;
            args[slot] = ptr;
            args[slot + 1] = len;
            assert_eq!(invoke(&mut vm, args).unwrap_err().root_cause().to_string(),
                "Ed25519 input slice lies outside guest memory");
        }
    }
    for slot in [0, 2, 4] {
        let mut args = write_inputs(&mut vm, PUBLIC_KEY.as_bytes(), b"", SIGNATURE.as_bytes());
        let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
        memory.write(&mut vm.store, args[slot] as usize, &[0xff]).unwrap();
        args[slot + 1] = 1;
        assert_eq!(invoke(&mut vm, args).unwrap_err().root_cause().to_string(),
            "Ed25519 input slice is not valid UTF-8");
    }
    // A malformed key cannot short-circuit validation of another input.
    let args = write_inputs(&mut vm, b"not-hex", b"", &[0xff]);
    assert_eq!(invoke(&mut vm, args).unwrap_err().root_cause().to_string(),
        "Ed25519 input slice is not valid UTF-8");
    let mut args = write_inputs(&mut vm, &[0xff], b"", SIGNATURE.as_bytes());
    args[4] = -1;
    args[5] = 1;
    assert_eq!(invoke(&mut vm, args).unwrap_err().root_cause().to_string(),
        "Ed25519 input slice lies outside guest memory");
}

#[test]
fn ed25519_slices_accept_exact_end_and_require_exported_memory() {
    let mut vm = setup();
    let mut args = write_inputs(&mut vm, PUBLIC_KEY.as_bytes(), b"", SIGNATURE.as_bytes());
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    let end = memory.data_size(&vm.store);
    memory.write(&mut vm.store, end - PUBLIC_KEY.len(), PUBLIC_KEY.as_bytes()).unwrap();
    args[0] = (end - PUBLIC_KEY.len()) as i32;
    args[2] = end as i32;
    assert_eq!(invoke(&mut vm, args).unwrap(), 1);

    let mut runtime = Runtime::new().unwrap();
    let wat = WAT.replace("(memory (export \"memory\") 1)", "");
    let mut vm = runtime.spawn(wat.as_bytes(), Capabilities::none(), 1).unwrap();
    let verify = vm.instance.get_typed_func::<(i32, i32, i32, i32, i32, i32), i64>(
        &mut vm.store, "verify").unwrap();
    assert!(verify.call(&mut vm.store, (0, 0, 0, 0, 0, 0)).is_err());
    assert!(vm.output().is_empty());
}
