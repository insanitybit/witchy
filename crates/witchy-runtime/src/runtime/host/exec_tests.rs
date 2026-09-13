use wasmtime::{ExternRef, Rooted};
use crate::runtime::{Capabilities, Runtime, Vm};

const WAT: &str = r#"(module
  (import "witchy" "mint_exec" (func $exec (result externref)))
  (import "witchy" "mint_dir" (func $dir (param i32) (result externref)))
  (import "witchy" "exec_run_at" (func $invoke (param externref externref i32 i32 i32 i32 i32 i32) (result externref)))
  (import "witchy" "exec_response_size" (func $size (param externref) (result i32)))
  (import "witchy" "exec_response_into" (func $copy (param externref i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "probe")
  (func (export "run"))
  (export "exec" (func $exec)) (export "dir" (func $dir))
  (func (export "invoke") (param externref externref i32 i32 i32 i32 i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 local.get 5 local.get 6 local.get 7 call $invoke)
  (func (export "size") (param externref) (result i32) local.get 0 call $size)
  (func (export "copy") (param externref i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 call $copy))"#;

type Token = Option<Rooted<ExternRef>>;

fn setup() -> (tempfile::TempDir, Vm, Token, Token) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("probe");
    std::fs::write(&script, b"#!/bin/sh\nprintf x >> \"$1\"\nread line\nprintf 'args=%s,%s,%s;stdin=%s\\n' \"$2\" \"$3\" \"$4\" \"$line\"\nprintf 'stderr\\n' >&2\nexit 7\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let caps = Capabilities { exec: true, exec_allow: Some(vec!["probe".into()]),
        dir_root: Some(root.path().to_path_buf()), dir_read: true, ..Capabilities::none() };
    let mut runtime = Runtime::new().unwrap();
    let mut vm = runtime.spawn(WAT, caps, 1).unwrap();
    let exec = vm.instance.get_typed_func::<(), Token>(&mut vm.store, "exec").unwrap().call(&mut vm.store, ()).unwrap();
    let dir = vm.instance.get_typed_func::<i32, Token>(&mut vm.store, "dir").unwrap().call(&mut vm.store, 0).unwrap();
    (root, vm, exec, dir)
}

#[test]
fn exec_slices_preserve_status_streams_arguments_and_effect_once() {
    let (root, mut vm, exec, dir) = setup();
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    let marker = root.path().join("effects");
    let args = format!("{}\0two words\0\0tail", marker.display());
    memory.write(&mut vm.store, 64, args.as_bytes()).unwrap();
    memory.write(&mut vm.store, 2048, b"input\n").unwrap();
    let invoke = vm.instance.get_typed_func::<(Token, Token, i32, i32, i32, i32, i32, i32), Token>(&mut vm.store, "invoke").unwrap();
    let result = invoke.call(&mut vm.store, (exec, dir, 0, 5, 64, args.len() as i32, 2048, 6)).unwrap();
    let expected = b"7\nargs=two words,,tail;stdin=input\nstderr\n";
    let size = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap();
    let copy = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap();
    for _ in 0..2 {
        assert_eq!(size.call(&mut vm.store, result).unwrap(), expected.len() as i32);
        memory.write(&mut vm.store, 4096, &[0xa5; 64]).unwrap();
        assert_eq!(copy.call(&mut vm.store, (result, 4096, 1)).unwrap(), expected.len() as i32);
        assert_eq!(&memory.data(&vm.store)[4096..4160], &[0xa5; 64]);
        assert_eq!(copy.call(&mut vm.store, (result, 4096, 64)).unwrap(), expected.len() as i32);
        assert_eq!(&memory.data(&vm.store)[4096..4096 + expected.len()], expected);
        assert!(copy.call(&mut vm.store, (result, 65535, 2)).is_err());
    }
    assert_eq!(std::fs::read(marker).unwrap(), b"x");
    assert!(vm.output().is_empty());
    // This harness allocates no guest heap objects. Dropping the isolated Store
    // releases its host-held immutable response; no child process remains live.
    drop(vm);
}

#[test]
fn exec_slices_require_dir_read_before_invalid_buffers_or_effects() {
    let (root, mut vm, exec, dir) = setup();
    // Attenuate the real minted Dir token without changing its backing or
    // policy. The host must reject its missing right before inspecting slices.
    let mut authority = dir.unwrap().data(&vm.store).unwrap().unwrap()
        .downcast_ref::<crate::runtime::DirAuthority>().unwrap().clone();
    authority.rights.read = false;
    let denied = Some(ExternRef::new(&mut vm.store, authority).unwrap());
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    let before = memory.data(&vm.store).to_vec();
    let invoke = vm.instance.get_typed_func::<(Token, Token, i32, i32, i32, i32, i32, i32), Token>(
        &mut vm.store, "invoke").unwrap();
    let error = invoke.call(&mut vm.store, (exec, denied, -1, -1, -1, -1, -1, -1)).unwrap_err();
    assert_eq!(error.root_cause().to_string(), "this Dir capability does not grant Read");
    assert_eq!(memory.data(&vm.store), before.as_slice());
    assert!(vm.output().is_empty());
    assert!(!root.path().join("effects").exists());
}

#[test]
fn exec_slices_reject_wrong_authority_malformed_inputs_and_response_forgery() {
    let (root, mut vm, exec, dir) = setup();
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    let invoke = vm.instance.get_typed_func::<(Token, Token, i32, i32, i32, i32, i32, i32), Token>(&mut vm.store, "invoke").unwrap();
    for (exec, dir, expected) in [(None, dir, "Exec externref is null"),
        (dir, dir, "Exec externref has wrong host data"),
        (exec, exec, "Dir externref has wrong host data")] {
        let error = invoke.call(&mut vm.store, (exec, dir, -1, -1, -1, -1, -1, -1)).unwrap_err();
        assert_eq!(error.root_cause().to_string(), expected);
    }
    for (ptr, len) in [(-1, 1), (65535, 2), (0, -1)] {
        assert!(invoke.call(&mut vm.store, (exec, dir, ptr, len, 0, 0, 0, 0)).unwrap_err()
            .root_cause().to_string().contains("outside guest memory"));
    }
    memory.write(&mut vm.store, 32, &[0xff]).unwrap();
    // A malformed third input must fail before running the otherwise valid path.
    assert!(invoke.call(&mut vm.store, (exec, dir, 0, 5, 0, 0, 32, 1)).unwrap_err()
        .root_cause().to_string().contains("not valid UTF-8"));
    memory.write(&mut vm.store, 32, b"other").unwrap();
    assert!(invoke.call(&mut vm.store, (exec, dir, 32, 5, 0, 0, 0, 0)).unwrap_err()
        .root_cause().to_string().contains("allow-list"));
    let size = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap();
    assert_eq!(size.call(&mut vm.store, exec).unwrap_err().root_cause().to_string(), "Exec response has wrong host kind");
    assert_eq!(size.call(&mut vm.store, None).unwrap_err().root_cause().to_string(), "Exec response is null");
    assert!(!root.path().join("effects").exists());
    assert!(vm.output().is_empty());
}
