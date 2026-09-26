//! Raw Wasm host-boundary tests; source/admission integration is a separate leg.
use super::*;
use crate::runtime::{Capabilities, Runtime, Vm};

type Token = Option<Rooted<ExternRef>>;
type WriteArgs = (Token, i32, i32, i32, i32);
const WAT: &str = r#"(module
  (import "witchy" "dir_create_new_at" (func $create (param externref i32 i32 i32 i32) (result i32)))
  (import "witchy" "dir_replace_at" (func $replace (param externref i32 i32 i32 i32)))
  (import "witchy" "dir_read_bytes_at" (func $read (param externref i32 i32) (result externref)))
  (import "witchy" "dir_bytes_response_size" (func $size (param externref) (result i32)))
  (import "witchy" "dir_bytes_response_into" (func $copy (param externref i32 i32) (result i32)))
  (import "witchy" "dir_write_bytes_at" (func $write (param externref i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (func (export "run"))
  (func (export "create") (param externref i32 i32 i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $create)
  (func (export "replace") (param externref i32 i32 i32 i32)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $replace)
  (func (export "write") (param externref i32 i32 i32 i32)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $write)
  (func (export "read") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $read)
  (func (export "size") (param externref) (result i32) local.get 0 call $size)
  (func (export "copy") (param externref i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 call $copy))"#;

fn vm_at(root: &std::path::Path, memory: bool) -> (Vm, Token) {
    let wat = if memory { WAT.to_owned() } else { WAT.replace("(memory (export \"memory\") 1)", "") };
    let caps = Capabilities { dir_root: Some(root.to_owned()), dir_read: true, dir_write: true, ..Capabilities::none() };
    let mut vm = Runtime::new().unwrap().spawn(&wat, caps, 1).unwrap();
    let authority = dir_grant(vm.store.data(), 0).unwrap();
    let token = Some(ExternRef::new(&mut vm.store, authority).unwrap());
    (vm, token)
}

fn setup(memory: bool) -> (tempfile::TempDir, Vm, Token) {
    let root = tempfile::tempdir().unwrap();
    let (vm, token) = vm_at(root.path(), memory);
    (root, vm, token)
}

fn attenuate(vm: &mut Vm, root: Token, read: bool, write: bool, policy: &str) -> Token {
    let mut authority = root.unwrap().data(&vm.store).unwrap().unwrap().downcast_ref::<DirAuthority>().unwrap().clone();
    authority.rights = FsRights { read, write };
    authority.policy = policy.to_owned();
    Some(ExternRef::new(&mut vm.store, authority).unwrap())
}

fn put(vm: &mut Vm, offset: usize, bytes: &[u8]) {
    vm.instance.get_memory(&mut vm.store, "memory").unwrap().write(&mut vm.store, offset, bytes).unwrap();
}
fn memory(vm: &mut Vm) -> Vec<u8> {
    vm.instance.get_memory(&mut vm.store, "memory").unwrap().data(&vm.store).to_vec()
}
fn inputs(vm: &mut Vm, root: Token, path: &str, data: &[u8]) -> WriteArgs {
    put(vm, 0, path.as_bytes());
    put(vm, 1024, data);
    (root, 0, path.len() as i32, 1024, data.len() as i32)
}
fn create(vm: &mut Vm, args: WriteArgs) -> Result<i32> {
    vm.instance.get_typed_func::<WriteArgs, i32>(&mut vm.store, "create").unwrap().call(&mut vm.store, args)
}
fn write(vm: &mut Vm, name: &str, args: WriteArgs) -> Result<()> {
    vm.instance.get_typed_func::<WriteArgs, ()>(&mut vm.store, name).unwrap().call(&mut vm.store, args)
}
fn read(vm: &mut Vm, root: Token, ptr: i32, len: i32) -> Result<Token> {
    vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "read").unwrap()
        .call(&mut vm.store, (root, ptr, len))
}
fn read_path(vm: &mut Vm, root: Token, path: &str) -> Token {
    put(vm, 0, path.as_bytes());
    read(vm, root, 0, path.len() as i32).unwrap()
}
fn size(vm: &mut Vm, token: Token) -> Result<i32> {
    vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap().call(&mut vm.store, token)
}
fn copy(vm: &mut Vm, token: Token, ptr: i32, cap: i32) -> Result<i32> {
    vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap()
        .call(&mut vm.store, (token, ptr, cap))
}
fn payload(vm: &mut Vm, token: Token) -> Vec<u8> {
    let length = size(vm, token).unwrap();
    assert_eq!(copy(vm, token, 4096, length).unwrap(), length);
    memory(vm)[4096..4096 + length as usize].to_vec()
}
fn error(error: Error, expected: &str) {
    let message = format!("{error:#}");
    assert!(message.contains(expected), "expected {expected:?}, got {message}");
}
fn names(root: &std::path::Path) -> std::collections::BTreeSet<String> {
    std::fs::read_dir(root).unwrap().map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect()
}

pub fn dir_storage_response_preflight_short_capacity_and_exact_prefix() {
    let (root, mut vm, grant) = setup(true);
    std::fs::write(root.path().join("bytes"), [0, 0xff, 9]).unwrap();
    let response = read_path(&mut vm, grant, "bytes");
    for capacity in [0, 1, 2, 3, 4, 8] {
        put(&mut vm, 4096, &[0xa5; 16]);
        let before = memory(&mut vm);
        assert_eq!(copy(&mut vm, response, 4096, capacity).unwrap(), 3);
        let mut expected = before;
        if capacity >= 3 { expected[4096..4099].copy_from_slice(&[0, 0xff, 9]); }
        assert_eq!(memory(&mut vm), expected);
    }
    let before = memory(&mut vm);
    for (ptr, cap) in [(-1, 0), (0, -1), (65535, 2), (65537, 0), (0, i32::MAX)] {
        assert!(copy(&mut vm, response, ptr, cap).is_err());
        assert_eq!(memory(&mut vm), before);
    }
    assert_eq!(copy(&mut vm, response, 65536, 0).unwrap(), 3);
    assert_eq!(memory(&mut vm), before);
    assert_eq!(dir_bytes_length(i32::MAX as usize).unwrap(), i32::MAX);
    assert!(dir_bytes_length(i32::MAX as usize + 1).is_err());
}

pub fn dir_storage_full_input_preflight_precedes_file_mutation() {
    let (root, mut vm, grant) = setup(true);
    std::fs::write(root.path().join("keep"), b"original").unwrap();
    for operation in ["create", "replace", "write"] {
        for (ptr, len) in [(-1, 0), (0, -1), (65535, 2), (65537, 0)] {
            for path_bad in [false, true] {
                let mut args = inputs(&mut vm, grant, "keep", b"overwrite");
                if path_bad { args.1 = ptr; args.2 = len; } else { args.3 = ptr; args.4 = len; }
                let before = memory(&mut vm);
                let result = if operation == "create" { create(&mut vm, args).map(|_| ()) } else { write(&mut vm, operation, args) };
                assert!(result.is_err(), "{operation}: ({ptr}, {len}), path_bad={path_bad}");
                assert_eq!(memory(&mut vm), before);
                assert_eq!(std::fs::read(root.path().join("keep")).unwrap(), b"original");
                assert_eq!(names(root.path()), ["keep".into()].into_iter().collect());
            }
        }
        let args = inputs(&mut vm, grant, "keep", &[0xff]);
        let before = memory(&mut vm);
        if operation == "create" { assert!(create(&mut vm, args).is_err()); }
        if operation == "replace" { assert!(write(&mut vm, operation, args).is_err()); }
        if operation != "write" {
            assert_eq!(memory(&mut vm), before);
            assert_eq!(std::fs::read(root.path().join("keep")).unwrap(), b"original");
        }
    }
    put(&mut vm, 0, &[0xff]);
    assert!(read(&mut vm, grant, 0, 1).is_err());
    for (ptr, len) in [(-1, 0), (0, -1), (65535, 2)] { assert!(read(&mut vm, grant, ptr, len).is_err()); }
    for operation in ["create", "replace", "write"] {
        let result = if operation == "create" { create(&mut vm, (grant, 0, 1, 1024, 0)).map(|_| ()) }
            else { write(&mut vm, operation, (grant, 0, 1, 1024, 0)) };
        assert!(result.is_err());
    }
    assert_eq!(names(root.path()), ["keep".into()].into_iter().collect());
}

pub fn dir_storage_kind_rights_and_response_issuer_precede_memory() {
    let (_root, mut vm, grant) = setup(false);
    let read_only = attenuate(&mut vm, grant, true, false, "");
    let write_only = attenuate(&mut vm, grant, false, true, "");
    let forged = Some(ExternRef::new(&mut vm.store, witchy_wir::canonical_safewir::DIR_BYTES_RESPONSE_KIND.get()).unwrap());
    for operation in ["create", "replace", "write"] {
        for (token, message) in [(None, "null"), (forged, "wrong host data"), (read_only, "does not grant Write")] {
            let args = (token, -1, -1, -1, -1);
            let result = if operation == "create" { create(&mut vm, args).map(|_| ()) } else { write(&mut vm, operation, args) };
            error(result.unwrap_err(), message);
        }
    }
    for (token, message) in [(None, "null"), (forged, "wrong host data"), (write_only, "does not grant Read")] {
        error(read(&mut vm, token, -1, -1).unwrap_err(), message);
    }
    for (token, message) in [(None, "null"), (forged, "wrong host kind"), (grant, "wrong host kind")] {
        error(size(&mut vm, token).unwrap_err(), message);
        error(copy(&mut vm, token, -1, -1).unwrap_err(), message);
    }
    let (other_root, mut other, other_grant) = setup(true);
    std::fs::write(other_root.path().join("bytes"), b"foreign").unwrap();
    let response = read_path(&mut other, other_grant, "bytes").unwrap();
    let foreign = response.data(&other.store).unwrap().unwrap().downcast_ref::<DirBytesResponse>().unwrap().clone();
    let foreign = Some(ExternRef::new(&mut vm.store, foreign).unwrap());
    error(size(&mut vm, foreign).unwrap_err(), "another instance");
    error(copy(&mut vm, foreign, -1, -1).unwrap_err(), "another instance");
    error(read(&mut vm, foreign, -1, -1).unwrap_err(), "wrong host data");
    other.store.data_mut().registry_issuer = Arc::new(());
    error(size(&mut other, Some(response)).unwrap_err(), "another instance");
}

pub fn dir_storage_entry_policy_is_preserved_for_every_file_operation() {
    let (root, mut vm, grant) = setup(true);
    std::fs::write(root.path().join("keep.bin"), b"original").unwrap();
    let text_only = attenuate(&mut vm, grant, true, true, "ext:.txt");
    let directories_only = attenuate(&mut vm, grant, true, true, "kind:dir");
    for token in [text_only, directories_only] {
        let args = inputs(&mut vm, token, "keep.bin", b"changed");
        error(create(&mut vm, args).unwrap_err(), "entry policy");
        error(write(&mut vm, "replace", args).unwrap_err(), "entry policy");
        error(write(&mut vm, "write", args).unwrap_err(), "entry policy");
        error(read(&mut vm, token, 0, 8).unwrap_err(), "entry policy");
        assert_eq!(std::fs::read(root.path().join("keep.bin")).unwrap(), b"original");
    }
    let args = inputs(&mut vm, text_only, "allowed.txt", b"allowed");
    assert_eq!(create(&mut vm, args).unwrap(), 1);
    write(&mut vm, "replace", args).unwrap();
    write(&mut vm, "write", args).unwrap();
    let response = read_path(&mut vm, text_only, "allowed.txt");
    assert_eq!(payload(&mut vm, response), b"allowed");
}

#[cfg(unix)]
pub fn dir_storage_symlinks_never_acquire_outside_authority() {
    let (root, mut vm, grant) = setup(true);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"outside").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("parent-link")).unwrap();
    let args = inputs(&mut vm, grant, "parent-link/secret", b"changed");
    assert!(create(&mut vm, args).is_err());
    assert!(write(&mut vm, "replace", args).is_err());
    assert!(write(&mut vm, "write", args).is_err());
    assert!(read(&mut vm, grant, 0, 18).is_err());
    std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("leaf.txt")).unwrap();
    let filtered = attenuate(&mut vm, grant, true, true, "ext:.txt");
    let args = inputs(&mut vm, filtered, "leaf.txt", b"changed");
    assert!(write(&mut vm, "replace", args).is_err(), "filtered aliases must not bypass entry policy");
    let args = inputs(&mut vm, grant, "leaf.txt", b"local replacement");
    assert_eq!(create(&mut vm, args).unwrap(), 0);
    assert!(write(&mut vm, "write", args).is_err());
    assert!(read(&mut vm, grant, 0, 8).is_err());
    // Unrestricted atomic rename replaces the link itself, not its target.
    write(&mut vm, "replace", args).unwrap();
    assert!(!std::fs::symlink_metadata(root.path().join("leaf.txt")).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(root.path().join("leaf.txt")).unwrap(), b"local replacement");
    assert_eq!(std::fs::read(outside.path().join("secret")).unwrap(), b"outside");
}

pub fn dir_storage_imports_remain_grant_gated() {
    let root = tempfile::tempdir().unwrap();
    for (name, read, write) in [("dir_create_new_at", true, false), ("dir_replace_at", true, false),
        ("dir_write_bytes_at", true, false), ("dir_read_bytes_at", false, true)]
    {
        let (parameters, result) = if name == "dir_read_bytes_at" { ("externref i32 i32", "(result externref)") }
            else if name == "dir_create_new_at" { ("externref i32 i32 i32 i32", "(result i32)") }
            else { ("externref i32 i32 i32 i32", "") };
        let wat = format!("(module (import \"witchy\" \"{name}\" (func (param {parameters}) {result})) (func (export \"run\")))");
        let caps = Capabilities { dir_root: Some(root.path().to_owned()), dir_read: read, dir_write: write, ..Capabilities::none() };
        let result = Runtime::new().unwrap().spawn(&wat, caps, 1);
        let Err(failure) = result else { panic!("{name} linked without its right") };
        assert!(format!("{failure:#}").contains(name));
    }
}
