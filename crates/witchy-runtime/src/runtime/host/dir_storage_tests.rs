//! Raw Wasm host-boundary tests; source/admission integration is a separate leg.
use super::*;
use crate::runtime::{Capabilities, Runtime, Vm};

type Token = Option<Rooted<ExternRef>>;
type WriteArgs = (Token, i32, i32, i32, i32);
const WAT: &str = r#"(module
  (import "witchy" "dir_create_new_at" (func $create (param externref i32 i32 i32 i32) (result externref)))
  (import "witchy" "dir_replace_at" (func $replace (param externref i32 i32 i32 i32) (result externref)))
  (import "witchy" "dir_read_bytes_at" (func $read (param externref i32 i32) (result externref)))
  (import "witchy" "cap_response_size" (func $size (param externref) (result i32)))
  (import "witchy" "cap_response_into" (func $copy (param externref i32 i32) (result i32)))
  (import "witchy" "dir_write_bytes_at" (func $write (param externref i32 i32 i32 i32) (result externref)))
  (import "witchy" "dir_write_at" (func $write_text (param externref i32 i32 i32 i32) (result externref)))
  (import "witchy" "dir_append_at" (func $append_text (param externref i32 i32 i32 i32) (result externref)))
  (import "witchy" "dir_append_bytes_at" (func $append (param externref i32 i32 i32 i32) (result externref)))
  (import "witchy" "dir_rename_at" (func $rename (param externref i32 i32 i32 i32) (result externref)))
  (import "witchy" "dir_make_dir_at" (func $make_dir (param externref i32 i32) (result externref)))
  (import "witchy" "dir_set_executable_at" (func $set_executable (param externref i32 i32) (result externref)))
  (import "witchy" "dir_exists_at" (func $exists (param externref i32 i32) (result externref)))
  (import "witchy" "dir_is_dir_at" (func $is_dir (param externref i32 i32) (result externref)))
  (import "witchy" "cap_response_flag" (func $flag (param externref) (result i32)))
  (import "witchy" "dir_read_at" (func $read_text (param externref i32 i32) (result externref)))
  (import "witchy" "cap_response_status" (func $status (param externref) (result i32)))
  (import "witchy" "dir_read_file_at" (func $open_file (param externref i32 i32) (result externref)))
  (import "witchy" "dir_subtree_open_at" (func $open_dir (param externref i32 i32) (result externref)))
  (import "witchy" "dir_write_file_at" (func $create_file (param externref i32 i32) (result externref)))
  (import "witchy" "cap_response_take_dir" (func $take_dir (param externref) (result externref)))
  (import "witchy" "cap_response_take_file" (func $take_file (param externref) (result externref)))
  (import "witchy" "file_read_at" (func $file_read (param externref) (result externref)))
  (import "witchy" "dir_list_at" (func $list (param externref) (result externref)))
  (import "witchy" "cap_response_status" (func $file_status (param externref) (result i32)))
  (import "witchy" "cap_response_size" (func $file_size (param externref) (result i32)))
  (import "witchy" "cap_response_into" (func $file_copy (param externref i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "run"))
  (func (export "create") (param externref i32 i32 i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $create)
  (func (export "replace") (param externref i32 i32 i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $replace)
  (func (export "write") (param externref i32 i32 i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $write)
  (func (export "write_text") (param externref i32 i32 i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $write_text)
  (func (export "append_text") (param externref i32 i32 i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $append_text)
  (func (export "append") (param externref i32 i32 i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $append)
  (func (export "rename") (param externref i32 i32 i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $rename)
  (func (export "make_dir") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $make_dir)
  (func (export "set_executable") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $set_executable)
  (func (export "exists") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $exists)
  (func (export "is_dir") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $is_dir)
  (func (export "flag") (param externref) (result i32) local.get 0 call $flag)
  (func (export "read") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $read)
  (func (export "read_text") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $read_text)
  (func (export "status") (param externref) (result i32) local.get 0 call $status)
  (func (export "open_file") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $open_file)
  (func (export "open_dir") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $open_dir)
  (func (export "create_file") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $create_file)
  (func (export "take_dir") (param externref) (result externref) local.get 0 call $take_dir)
  (func (export "take_file") (param externref) (result externref) local.get 0 call $take_file)
  (func (export "file_read") (param externref) (result externref) local.get 0 call $file_read)
  (func (export "list") (param externref) (result externref) local.get 0 call $list)
  (func (export "file_status") (param externref) (result i32) local.get 0 call $file_status)
  (func (export "file_size") (param externref) (result i32) local.get 0 call $file_size)
  (func (export "file_copy") (param externref i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 call $file_copy)
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
/// A write-family export on `args`, answered as its response's status.
fn write(vm: &mut Vm, name: &str, args: WriteArgs) -> Result<i32> {
    let response = vm.instance.get_typed_func::<WriteArgs, Token>(&mut vm.store, name).unwrap().call(&mut vm.store, args)?;
    vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "status").unwrap().call(&mut vm.store, response)
}
fn create(vm: &mut Vm, args: WriteArgs) -> Result<i32> {
    write(vm, "create", args)
}
fn read(vm: &mut Vm, root: Token, ptr: i32, len: i32) -> Result<Token> {
    vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "read").unwrap()
        .call(&mut vm.store, (root, ptr, len))
}
fn read_path(vm: &mut Vm, root: Token, path: &str) -> Token {
    put(vm, 0, path.as_bytes());
    read(vm, root, 0, path.len() as i32).unwrap()
}
/// `dir_read_at` on `path`, answered as (status, payload).
fn read_text(vm: &mut Vm, root: Token, path: &str) -> Result<(i32, Vec<u8>)> {
    put(vm, 0, path.as_bytes());
    let response = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "read_text").unwrap()
        .call(&mut vm.store, (root, 0, path.len() as i32))?;
    let status = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "status").unwrap()
        .call(&mut vm.store, response)?;
    Ok((status, payload(vm, response)))
}
fn size(vm: &mut Vm, token: Token) -> Result<i32> {
    vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap().call(&mut vm.store, token)
}
fn copy(vm: &mut Vm, token: Token, ptr: i32, cap: i32) -> Result<i32> {
    vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap()
        .call(&mut vm.store, (token, ptr, cap))
}
/// `dir_read_bytes_at` on `path`, answered as its status and payload.
fn read_bytes_status(vm: &mut Vm, root: Token, path: &str) -> (i32, Vec<u8>) {
    let response = read_path(vm, root, path);
    let status = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "status").unwrap()
        .call(&mut vm.store, response).unwrap();
    (status, payload(vm, response))
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
                let result = write(&mut vm, operation, args);
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
        let result = write(&mut vm, operation, (grant, 0, 1, 1024, 0));
        assert!(result.is_err());
    }
    assert_eq!(names(root.path()), ["keep".into()].into_iter().collect());
}

pub fn dir_storage_kind_rights_and_response_issuer_precede_memory() {
    let (_root, mut vm, grant) = setup(false);
    let read_only = attenuate(&mut vm, grant, true, false, "");
    let write_only = attenuate(&mut vm, grant, false, true, "");
    let forged = Some(ExternRef::new(&mut vm.store, witchy_wir::canonical_safewir::CAP_RESPONSE_KIND.get()).unwrap());
    for operation in ["create", "replace", "write"] {
        for (token, message) in [(None, "null"), (forged, "wrong host data"), (read_only, "does not grant Write")] {
            let args = (token, -1, -1, -1, -1);
            let result = write(&mut vm, operation, args);
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
    let foreign = response.data(&other.store).unwrap().unwrap().downcast_ref::<cap_response::CapResponse>().unwrap().clone();
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
        for operation in ["create", "replace", "write"] {
            assert_eq!(write(&mut vm, operation, args).unwrap(), 1, "{operation}: a refused write is Denied");
        }
        assert_eq!(read_bytes_status(&mut vm, token, "keep.bin").0, 1, "a refused read is Denied");
        assert_eq!(std::fs::read(root.path().join("keep.bin")).unwrap(), b"original");
    }
    let args = inputs(&mut vm, text_only, "allowed.txt", b"allowed");
    for operation in ["create", "replace", "write"] {
        assert_eq!(write(&mut vm, operation, args).unwrap(), 0, "{operation}");
    }
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
    for operation in ["create", "replace", "write"] {
        assert_eq!(write(&mut vm, operation, args).unwrap(), 1, "{operation} through a link out of the root is Denied");
    }
    let (status, data) = read_bytes_status(&mut vm, grant, "parent-link/secret");
    assert_eq!(status, 1, "a read through a link out of the root is Denied");
    assert!(!data.windows(7).any(|window| window == b"outside"));
    std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("leaf.txt")).unwrap();
    let filtered = attenuate(&mut vm, grant, true, true, "ext:.txt");
    let args = inputs(&mut vm, filtered, "leaf.txt", b"changed");
    assert_eq!(write(&mut vm, "replace", args).unwrap(), 2, "a filtered alias is invisible, never followed");
    let args = inputs(&mut vm, grant, "leaf.txt", b"local replacement");
    assert_eq!(create(&mut vm, args).unwrap(), 1, "a link leaf is Denied, whatever its target");
    assert_eq!(write(&mut vm, "write", args).unwrap(), 1, "a write never follows a link leaf");
    assert_eq!(read_bytes_status(&mut vm, grant, "leaf.txt").0, 1, "a link leaf out of the root is Denied");
    // Unrestricted atomic rename replaces the link itself, not its target.
    assert_eq!(write(&mut vm, "replace", args).unwrap(), 0);
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
            else { ("externref i32 i32 i32 i32", "(result externref)") };
        let wat = format!("(module (import \"witchy\" \"{name}\" (func (param {parameters}) {result})) (func (export \"run\")))");
        let caps = Capabilities { dir_root: Some(root.path().to_owned()), dir_read: read, dir_write: write, ..Capabilities::none() };
        let result = Runtime::new().unwrap().spawn(&wat, caps, 1);
        let Err(failure) = result else { panic!("{name} linked without its right") };
        assert!(format!("{failure:#}").contains(name));
    }
}

/// A covered read (`dir.read` and `dir.read_bytes` alike) answers status 0
/// and the contents; every refusal the grant
/// makes and every filesystem failure is a `CapError` status carrying the
/// message (1 Denied, 2 NotFound, 3 NotAFile, 5 Io). A missing right or a
/// forged token still traps.
///
/// No status is an existence oracle. An escape (`..`, absolute, or a symlink
/// leaving the root) is Denied whether or not its outside target exists, a
/// name the policy refuses is Denied whether or not it exists, and an entry a
/// policy cannot see answers like a missing one. No payload carries the
/// escaped file's contents or the host location of the root.
pub fn dir_read_at_answers_failures_as_statuses() {
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(outer.path().join("outside.txt"), b"escaped contents").unwrap();
    std::fs::create_dir(outer.path().join("outside-dir")).unwrap();
    std::fs::write(outer.path().join("outside-dir/x.txt"), b"escaped contents").unwrap();
    std::fs::write(root.join("a.txt"), b"hello").unwrap();
    std::fs::write(root.join("raw.txt"), [0xff, 0xfe]).unwrap();
    std::fs::write(root.join("keep.bin"), b"binary").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::create_dir(root.join("notes.txt")).unwrap();
    std::fs::write(root.join("sub/y.txt"), b"hello").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        assert!(std::process::Command::new("mkfifo").arg(root.join("pipe")).status().unwrap().success());
        symlink("../outside.txt", root.join("out.txt")).unwrap();
        symlink("../missing.txt", root.join("dangling.txt")).unwrap();
        symlink(outer.path().join("outside.txt"), root.join("absolute.txt")).unwrap();
        symlink("../outside-dir", root.join("outdir")).unwrap();
        symlink("../missing-dir", root.join("nodir")).unwrap();
        symlink("a.txt", root.join("alias.txt")).unwrap();
    }
    let (mut vm, grant) = vm_at(&root, true);
    let text_only = attenuate(&mut vm, grant, true, false, "ext:.txt");
    let files_only = attenuate(&mut vm, grant, true, false, "kind:file");
    let mut rows = vec![
        (grant, "a.txt", 0), (grant, "missing.txt", 2), (grant, "sub", 3), (grant, "raw.txt", 5),
        // Lexical escapes, outside target present or not.
        (grant, "../outside.txt", 1), (grant, "../missing.txt", 1), (grant, "/etc/passwd", 1),
        (grant, "/no/such/file", 1),
        // A name the policy refuses, present or not.
        (text_only, "keep.bin", 1), (text_only, "absent.bin", 1), (text_only, "sub", 1),
        // Entries a policy cannot see answer like a missing name.
        (files_only, "sub", 2), (files_only, "nothing", 2), (text_only, "nothing.txt", 2),
        (text_only, "notes.txt", 3),
        // A path through an entry the policy cannot traverse, present or not.
        (text_only, "keep.bin/x.txt", 2), (text_only, "absent.bin/x.txt", 2),
        (files_only, "sub/x.txt", 2), (files_only, "absent/x.txt", 2),
        (text_only, "sub/y.txt", 0), (files_only, "sub/y.txt", 0),
    ];
    #[cfg(unix)]
    rows.extend([
        // A special file must answer without waiting for another process.
        (grant, "pipe", 5),
        // A symlink leaving the root, outside target present or not.
        (grant, "out.txt", 1), (grant, "dangling.txt", 1), (grant, "absolute.txt", 1),
        (grant, "outdir/x.txt", 1), (grant, "nodir/x.txt", 1),
        // An in-root alias under a policy is invisible, like a missing name.
        (text_only, "alias.txt", 2), (text_only, "out.txt", 2), (text_only, "dangling.txt", 2),
        (text_only, "outdir/x.txt", 2),
    ]);
    let host_root = root.to_string_lossy().into_owned();
    let host_outer = outer.path().to_string_lossy().into_owned();
    for (token, path, status) in rows {
        let (answer, payload) = read_text(&mut vm, token, path).unwrap();
        let text = String::from_utf8_lossy(&payload);
        assert_eq!(answer, status, "{path}: {text}");
        // `dir.read_bytes` classifies identically; only UTF-8 decoding differs.
        let (bytes_answer, bytes_payload) = read_bytes_status(&mut vm, token, path);
        if path == "raw.txt" {
            assert_eq!((bytes_answer, bytes_payload.as_slice()), (0, &[0xff_u8, 0xfe][..]));
        } else {
            assert_eq!(bytes_answer, status, "read_bytes {path}");
            assert_eq!(bytes_payload, payload, "read_bytes {path}");
        }
        if status == 0 { assert_eq!(payload, b"hello"); continue; }
        assert!(!text.contains("escaped contents"), "{path} leaked the escaped file");
        // A failure message is guest data: never the host's location of the root.
        assert!(!text.contains(&host_root) && !text.contains(&host_outer), "{path} leaked a host path: {text}");
        assert!(text.starts_with(&format!("`{path}` ")), "{path}: {text}");
    }
    let write_only = attenuate(&mut vm, grant, false, true, "");
    error(read_text(&mut vm, write_only, "a.txt").unwrap_err(), "does not grant Read");
    let forged = Some(ExternRef::new(&mut vm.store, witchy_wir::canonical_safewir::CAP_RESPONSE_KIND.get()).unwrap());
    let status = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "status").unwrap();
    error(status.call(&mut vm.store, forged).unwrap_err(), "wrong host kind");
}

/// A navigation export (`open_file`, `open_dir`, `create_file`) on `path`,
/// answered as (status, message, response).
fn navigate(vm: &mut Vm, export: &str, root: Token, path: &str) -> Result<(i32, String, Token)> {
    put(vm, 0, path.as_bytes());
    let response = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, export).unwrap()
        .call(&mut vm.store, (root, 0, path.len() as i32))?;
    let status = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "status").unwrap()
        .call(&mut vm.store, response)?;
    let message = String::from_utf8(payload(vm, response)).unwrap();
    Ok((status, message, response))
}
fn take(vm: &mut Vm, export: &str, response: Token) -> Result<Token> {
    vm.instance.get_typed_func::<Token, Token>(&mut vm.store, export).unwrap().call(&mut vm.store, response)
}
fn open_file(vm: &mut Vm, root: Token, path: &str) -> Result<Token> {
    let (status, message, response) = navigate(vm, "open_file", root, path)?;
    assert_eq!(status, 0, "open {path}: {message}");
    take(vm, "take_file", response)
}
/// `file_read_at` on `file`, answered as (status, payload).
fn read_file(vm: &mut Vm, file: Token) -> Result<(i32, Vec<u8>)> {
    let response = vm.instance.get_typed_func::<Token, Token>(&mut vm.store, "file_read").unwrap()
        .call(&mut vm.store, file)?;
    let status = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "file_status").unwrap()
        .call(&mut vm.store, response)?;
    let size = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "file_size").unwrap()
        .call(&mut vm.store, response)?;
    let copied = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "file_copy").unwrap()
        .call(&mut vm.store, (response, 4096, size))?;
    assert_eq!(copied, size);
    Ok((status, memory(vm)[4096..4096 + size as usize].to_vec()))
}

/// `file.read()` answers its failures as statuses with a fixed message that
/// names no path (RFC-0126 rule 3): a deleted file is NotFound, a directory
/// swapped in is NotAFile, non-UTF-8 text is Io, and a symlink swapped in
/// is Denied whether or not its outside target exists. A forged or wrong-kind
/// token, a null File, and an out-of-range copy still trap.
pub fn file_read_at_answers_failures_as_statuses() {
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(outer.path().join("outside.txt"), b"escaped contents").unwrap();
    for name in ["ok.txt", "gone.txt", "dir.txt", "raw.txt", "out.txt", "dangling.txt"] {
        std::fs::write(root.join(name), b"hello").unwrap();
    }
    let (mut vm, grant) = vm_at(&root, true);
    let mut files = std::collections::BTreeMap::new();
    for name in ["ok.txt", "gone.txt", "dir.txt", "raw.txt", "out.txt", "dangling.txt"] {
        files.insert(name, open_file(&mut vm, grant, name).unwrap());
    }
    std::fs::remove_file(root.join("gone.txt")).unwrap();
    std::fs::remove_file(root.join("dir.txt")).unwrap();
    std::fs::create_dir(root.join("dir.txt")).unwrap();
    std::fs::write(root.join("raw.txt"), [0xff, 0xfe]).unwrap();
    let mut rows = vec![("ok.txt", 0), ("gone.txt", 2), ("dir.txt", 3), ("raw.txt", 5)];
    #[cfg(unix)]
    {
        for name in ["out.txt", "dangling.txt"] { std::fs::remove_file(root.join(name)).unwrap(); }
        std::os::unix::fs::symlink("../outside.txt", root.join("out.txt")).unwrap();
        std::os::unix::fs::symlink("../missing.txt", root.join("dangling.txt")).unwrap();
        rows.extend([("out.txt", 1), ("dangling.txt", 1)]);
    }
    let host = outer.path().to_string_lossy().into_owned();
    for (name, status) in rows {
        let (answer, payload) = read_file(&mut vm, files[name]).unwrap();
        let text = String::from_utf8_lossy(&payload);
        assert_eq!(answer, status, "{name}: {text}");
        if status == 0 { assert_eq!(payload, b"hello"); continue; }
        assert!(text.starts_with("this File "), "{name}: {text}");
        assert!(!text.contains("escaped contents") && !text.contains(&host) && !text.contains(name), "{name}: {text}");
    }
    // A File a policy admitted never reads through a link swapped in later,
    // even to an admitted sibling: it answers Denied.
    #[cfg(unix)]
    {
        std::fs::write(root.join("keep.bin"), b"binary").unwrap();
        std::fs::write(root.join("fenced.txt"), b"hello").unwrap();
        let text_only = attenuate(&mut vm, grant, true, false, "ext:.txt");
        let file = open_file(&mut vm, text_only, "fenced.txt").unwrap();
        std::fs::remove_file(root.join("fenced.txt")).unwrap();
        std::os::unix::fs::symlink("keep.bin", root.join("fenced.txt")).unwrap();
        let (status, payload) = read_file(&mut vm, file).unwrap();
        assert_eq!(status, 1, "{}", String::from_utf8_lossy(&payload));
        assert!(!payload.windows(6).any(|window| window == b"binary"));
    }
    error(read_file(&mut vm, grant).unwrap_err(), "File externref has wrong host data");
    error(read_file(&mut vm, None).unwrap_err(), "File externref is null");
    let response = vm.instance.get_typed_func::<Token, Token>(&mut vm.store, "file_read").unwrap()
        .call(&mut vm.store, files["ok.txt"]).unwrap();
    let copy = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "file_copy").unwrap();
    assert!(copy.call(&mut vm.store, (response, 65535, 2)).is_err(), "an out-of-range copy must trap");
}

/// `dir_list_at` on `dir`, answered as (status, NUL-split names or message).
fn list(vm: &mut Vm, dir: Token) -> (i32, Vec<String>) {
    let response = vm.instance.get_typed_func::<Token, Token>(&mut vm.store, "list").unwrap()
        .call(&mut vm.store, dir).unwrap();
    let status = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "status").unwrap()
        .call(&mut vm.store, response).unwrap();
    let bytes = payload(vm, response);
    let text = String::from_utf8(bytes).unwrap();
    let names = if text.is_empty() { Vec::new() } else { text.split('\0').map(str::to_owned).collect() };
    (status, names)
}

/// `dir.list()` answers the sorted names a grant can observe (RFC-0126 rule
/// 3). Under an entry policy it lists exactly what `dir.exists` reports: no
/// alias, no case-folded spelling, no directory a `files()` handle cannot
/// open. A directory the host refuses to list is a status with a fixed
/// message that names no path; a missing Read right still traps.
pub fn dir_list_at_lists_only_what_the_grant_observes() {
    let (root, mut vm, grant) = setup(true);
    std::fs::write(root.path().join("b.txt"), b"b").unwrap();
    std::fs::write(root.path().join("a.bin"), b"a").unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.bin", root.path().join("alias.txt")).unwrap();
    let text_only = attenuate(&mut vm, grant, true, false, "ext:.txt");
    let files_only = attenuate(&mut vm, grant, true, false, "kind:file");
    let all: Vec<&str> = if cfg!(unix) { vec!["a.bin", "alias.txt", "b.txt", "sub"] } else { vec!["a.bin", "b.txt", "sub"] };
    assert_eq!(list(&mut vm, grant), (0, all.iter().map(|n| n.to_string()).collect()));
    assert_eq!(list(&mut vm, text_only), (0, vec!["b.txt".to_string(), "sub".to_string()]));
    assert_eq!(list(&mut vm, files_only), (0, vec!["a.bin".to_string(), "b.txt".to_string()]));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let locked = root.path().join("sub");
        let opened = crate::confine::ConfinedDir::open_ambient(root.path()).unwrap().open_dir("sub").unwrap();
        let sub = Some(ExternRef::new(&mut vm.store, DirAuthority {
            backing: DirBacking::Fs(opened),
            policy: String::new(),
            rights: FsRights { read: true, write: false },
        }).unwrap());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let refused = list(&mut vm, sub);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Running as root bypasses permission bits; then the listing succeeds.
        if refused.0 != 0 {
            assert_eq!(refused, (1, vec!["this Dir cannot be listed under its capability".to_string()]));
        } else {
            assert_eq!(refused, (0, Vec::new()), "only root bypasses the refusal; sub is empty");
        }
    }
    let write_only = attenuate(&mut vm, grant, false, true, "");
    let call = vm.instance.get_typed_func::<Token, Token>(&mut vm.store, "list").unwrap();
    error(call.call(&mut vm.store, write_only).unwrap_err(), "does not grant Read");
}

/// `subtree`, `read_file` and `write_file` answer navigation failures as
/// statuses (RFC-0126 rule 3) with the same no-oracle rules as `dir.read`:
/// an escape or policy refusal is Denied whether or not anything is there,
/// a link leaving the root is Denied whatever its target is, and an entry a
/// policy cannot see answers like a missing one. Taking from a failed
/// response, or the wrong capability kind, traps; so does a missing right.
pub fn dir_navigation_answers_failures_as_statuses() {
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(outer.path().join("outside-dir")).unwrap();
    std::fs::write(outer.path().join("outside.txt"), b"escaped contents").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/y.txt"), b"hello").unwrap();
    std::fs::write(root.join("a.txt"), b"hello").unwrap();
    std::fs::write(root.join("keep.bin"), b"binary").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink("../outside-dir", root.join("outdir")).unwrap();
        symlink("../missing-dir", root.join("nodir")).unwrap();
        symlink("../outside.txt", root.join("out.txt")).unwrap();
        symlink("../missing.txt", root.join("dangling.txt")).unwrap();
        symlink("a.txt", root.join("alias.txt")).unwrap();
        symlink("sub", root.join("subalias")).unwrap();
    }
    let (mut vm, grant) = vm_at(&root, true);
    let text_only = attenuate(&mut vm, grant, true, true, "ext:.txt");
    let files_only = attenuate(&mut vm, grant, true, true, "kind:file");
    let mut rows = vec![
        ("open_dir", grant, "sub", 0), ("open_dir", grant, "missing", 2), ("open_dir", grant, "a.txt", 5),
        ("open_dir", grant, "..", 1), ("open_dir", grant, "../outside-dir", 1), ("open_dir", grant, "../missing-dir", 1),
        ("open_dir", grant, "/tmp", 1), ("open_dir", files_only, "sub", 1), ("open_dir", files_only, "missing", 1),
        ("open_file", grant, "a.txt", 0), ("open_file", grant, "sub/y.txt", 0), ("open_file", grant, "missing.txt", 2),
        ("open_file", grant, "sub", 3), ("open_file", grant, "../outside.txt", 1), ("open_file", grant, "../missing.txt", 1),
        ("open_file", text_only, "keep.bin", 1), ("open_file", text_only, "absent.bin", 1),
        ("open_file", text_only, "nothing.txt", 2), ("open_file", text_only, "keep.bin/x.txt", 2),
        ("open_file", text_only, "absent.bin/x.txt", 2),
        ("create_file", grant, "new.txt", 0), ("create_file", grant, "sub", 3), ("create_file", grant, "missing/x.txt", 2),
        ("create_file", grant, "../outside.txt", 1), ("create_file", grant, "../missing.txt", 1),
        ("create_file", text_only, "keep.bin", 1), ("create_file", text_only, "absent.bin", 1),
        // A file the policy hides is not "not a directory": it is invisible.
        ("open_dir", text_only, "keep.bin", 2), ("open_dir", text_only, "absent.bin", 2),
        ("open_dir", text_only, "a.txt", 5),
    ];
    #[cfg(unix)]
    rows.extend([
        ("open_dir", grant, "outdir", 1), ("open_dir", grant, "nodir", 1), ("open_dir", grant, "subalias", 0),
        ("open_dir", text_only, "subalias", 2), ("open_dir", text_only, "outdir", 2), ("open_dir", text_only, "nodir", 2),
        ("open_file", grant, "out.txt", 1), ("open_file", grant, "dangling.txt", 1),
        ("open_file", grant, "outdir/x.txt", 1), ("open_file", grant, "nodir/x.txt", 1),
        ("open_file", text_only, "alias.txt", 2), ("open_file", text_only, "out.txt", 2), ("open_file", text_only, "dangling.txt", 2),
        ("create_file", grant, "outdir/x.txt", 1), ("create_file", grant, "nodir/x.txt", 1),
        ("create_file", text_only, "outdir/x.txt", 2), ("create_file", text_only, "nodir/x.txt", 2),
    ]);
    let host = outer.path().to_string_lossy().into_owned();
    for (export, token, path, status) in rows {
        let (answer, message, response) = navigate(&mut vm, export, token, path).unwrap();
        assert_eq!(answer, status, "{export} {path}: {message}");
        let kind = if export == "open_dir" { "take_dir" } else { "take_file" };
        if status == 0 {
            assert!(message.is_empty());
            take(&mut vm, kind, response).unwrap();
            continue;
        }
        assert!(message.starts_with(&format!("`{path}` ")), "{export} {path}: {message}");
        assert!(!message.contains(&host) && !message.contains("escaped contents"), "{export} {path}: {message}");
        error(take(&mut vm, kind, response).unwrap_err(), "carries no");
    }
    assert!(!outer.path().join("outside-dir/x.txt").exists() && !outer.path().join("missing.txt").exists());
    let (_, _, opened) = navigate(&mut vm, "open_dir", grant, "sub").unwrap();
    error(take(&mut vm, "take_file", opened).unwrap_err(), "carries no File");
    let read_only = attenuate(&mut vm, grant, true, false, "");
    error(navigate(&mut vm, "create_file", read_only, "new.txt").unwrap_err(), "does not grant Write");
    let write_only = attenuate(&mut vm, grant, false, true, "");
    error(navigate(&mut vm, "open_file", write_only, "a.txt").unwrap_err(), "does not grant Read");
}

/// A write-only `Dir` links everything `write_file` needs: the response
/// getters and the File take link under any Dir grant.
pub fn write_only_dir_links_write_file() {
    let root = tempfile::tempdir().unwrap();
    let wat = r#"(module
      (import "witchy" "dir_write_file_at" (func (param externref i32 i32) (result externref)))
      (import "witchy" "cap_response_status" (func (param externref) (result i32)))
      (import "witchy" "cap_response_size" (func (param externref) (result i32)))
      (import "witchy" "cap_response_into" (func (param externref i32 i32) (result i32)))
      (import "witchy" "cap_response_take_file" (func (param externref) (result externref)))
      (memory (export "memory") 1)
      (func (export "run")))"#;
    let caps = Capabilities { dir_root: Some(root.path().to_owned()), dir_read: false, dir_write: true, ..Capabilities::none() };
    Runtime::new().unwrap().spawn(wat, caps, 1).expect("write-only Dir links write_file");
}

/// A write-family or probe export on `path` (and `data`, the rename target for
/// `rename`), answered as (status, flag, message).
fn effect(vm: &mut Vm, export: &str, root: Token, path: &str, data: &[u8]) -> Result<(i32, i32, String)> {
    let response = if matches!(export, "make_dir" | "set_executable" | "exists" | "is_dir") {
        put(vm, 0, path.as_bytes());
        vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, export).unwrap()
            .call(&mut vm.store, (root, 0, path.len() as i32))?
    } else {
        let args = inputs(vm, root, path, data);
        vm.instance.get_typed_func::<WriteArgs, Token>(&mut vm.store, export).unwrap().call(&mut vm.store, args)?
    };
    let status = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "status").unwrap().call(&mut vm.store, response)?;
    let flag = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "flag").unwrap().call(&mut vm.store, response)?;
    Ok((status, flag, String::from_utf8(payload(vm, response)).unwrap()))
}

/// The write family and the `exists`/`is_dir` probes answer failures as
/// statuses (RFC-0126 rule 3) under the same no-oracle rules as `dir.read`:
/// an escape (`..`, absolute, or a link leaving the root) is Denied whether or
/// not anything is there, a name the policy refuses is Denied present or
/// absent, a path through an entry a policy cannot see answers like a missing
/// parent, a write never follows a link leaf, and an exclusive create of a
/// taken name is AlreadyExists. A probe answers its flag with status 0, and an
/// entry a policy cannot see is `false`. No message names a host path. A
/// missing right still traps.
pub fn dir_write_family_answers_failures_as_statuses() {
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(outer.path().join("outside-dir")).unwrap();
    std::fs::write(outer.path().join("outside.txt"), b"escaped contents").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    for name in ["a.txt", "b.txt", "move.txt", "keep.bin", "secret.key", "classified.TXT"] {
        std::fs::write(root.join(name), b"hello").unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink("../outside-dir", root.join("outdir")).unwrap();
        symlink("../missing-dir", root.join("nodir")).unwrap();
        symlink("../outside.txt", root.join("out.txt")).unwrap();
        symlink("../missing.txt", root.join("dangling.txt")).unwrap();
        symlink("secret.key", root.join("alias.txt")).unwrap();
        symlink("alias.txt", root.join("multi.txt")).unwrap();
    }
    let (mut vm, grant) = vm_at(&root, true);
    let text_only = attenuate(&mut vm, grant, true, true, "ext:.txt");
    let files_only = attenuate(&mut vm, grant, true, true, "kind:file");
    let dirs_only = attenuate(&mut vm, grant, true, true, "kind:dir");
    // (export, grant, path, data, status, flag)
    let mut rows: Vec<(&str, Token, &str, &str, i32, i32)> = vec![
        ("write_text", grant, "new.txt", "x", 0, 0), ("write_text", grant, "missing/x.txt", "x", 2, 0),
        ("write_text", grant, "sub", "x", 3, 0), ("write_text", grant, "../outside.txt", "x", 1, 0),
        ("write_text", grant, "../missing.txt", "x", 1, 0), ("write_text", grant, "/tmp/x.txt", "x", 1, 0),
        ("write_text", text_only, "keep.bin", "x", 1, 0), ("write_text", text_only, "absent.bin", "x", 1, 0),
        ("write_text", text_only, "keep.bin/x.txt", "x", 2, 0), ("write_text", text_only, "sub", "x", 1, 0),
        ("append_text", grant, "a.txt", "x", 0, 0), ("append_text", grant, "sub", "x", 3, 0),
        ("append_text", grant, "../outside.txt", "x", 1, 0),
        ("write", grant, "blob.bin", "x", 0, 0), ("append", grant, "blob.bin", "x", 0, 0),
        ("append", grant, "../missing.txt", "x", 1, 0),
        ("create", grant, "fresh.txt", "x", 0, 0), ("create", grant, "a.txt", "x", 4, 0),
        ("create", grant, "sub", "x", 3, 0), ("create", grant, "../missing.txt", "x", 1, 0),
        ("create", text_only, "keep.bin", "x", 1, 0), ("create", text_only, "absent.bin", "x", 1, 0),
        ("replace", grant, "b.txt", "x", 0, 0), ("replace", grant, "../outside.txt", "x", 1, 0),
        ("make_dir", grant, "newdir", "", 0, 0), ("make_dir", grant, "sub", "", 0, 0),
        ("make_dir", grant, "../newdir", "", 1, 0),
        ("set_executable", grant, "a.txt", "", 0, 0), ("set_executable", grant, "missing.txt", "", 2, 0),
        ("set_executable", grant, "../outside.txt", "", 1, 0),
        ("rename", grant, "move.txt", "moved.txt", 0, 0), ("rename", grant, "missing.txt", "x.txt", 2, 0),
        ("rename", grant, "a.txt", "../stolen.txt", 1, 0), ("rename", grant, "../outside.txt", "here.txt", 1, 0),
        // An extension policy admits any directory name, so a refused file name
        // as a rename source is invisible rather than refused.
        ("rename", text_only, "keep.bin", "k.txt", 2, 0), ("rename", text_only, "absent.bin", "k.txt", 2, 0),
        ("rename", files_only, "sub", "moved", 2, 0),
        // SEC-056/062: an occupant the policy hides answers like a missing
        // name, never its kind, and is never written.
        ("write_text", files_only, "sub", "x", 2, 0), ("append", files_only, "sub", "x", 2, 0),
        ("create", files_only, "sub", "x", 2, 0), ("replace", files_only, "sub", "x", 2, 0),
        ("set_executable", files_only, "sub", "", 2, 0), ("rename", files_only, "a.txt", "sub", 2, 0),
        ("make_dir", dirs_only, "a.txt", "", 2, 0),
        ("rename", text_only, "a.txt", "a.bin", 1, 0),
        ("exists", grant, "a.txt", "", 0, 1), ("exists", grant, "missing", "", 0, 0), ("exists", grant, "sub", "", 0, 1),
        ("exists", grant, "../outside.txt", "", 1, 0), ("exists", grant, "../missing.txt", "", 1, 0),
        ("exists", grant, "/tmp", "", 1, 0),
        ("exists", text_only, "a.txt", "", 0, 1), ("exists", text_only, "keep.bin", "", 0, 0),
        ("exists", text_only, "absent.bin", "", 0, 0),
        ("is_dir", grant, "sub", "", 0, 1), ("is_dir", grant, "a.txt", "", 0, 0), ("is_dir", grant, "missing", "", 0, 0),
        ("is_dir", grant, "..", "", 1, 0),
    ];
    #[cfg(unix)]
    rows.extend([
        ("write_text", grant, "out.txt", "x", 1, 0), ("write_text", grant, "dangling.txt", "x", 1, 0),
        ("write_text", grant, "outdir/x.txt", "x", 1, 0), ("write_text", grant, "nodir/x.txt", "x", 1, 0),
        ("write_text", text_only, "outdir/x.txt", "x", 2, 0), ("write_text", text_only, "nodir/x.txt", "x", 2, 0),
        ("create", grant, "dangling.txt", "x", 1, 0), ("create", grant, "outdir/x.txt", "x", 1, 0),
        // SEC-056/062: an in-root alias (one or two hops) to a refused file
        // is invisible under the policy and is never written through.
        ("write_text", text_only, "alias.txt", "x", 2, 0), ("append_text", text_only, "alias.txt", "x", 2, 0),
        ("create", text_only, "alias.txt", "x", 2, 0), ("replace", text_only, "alias.txt", "x", 2, 0),
        ("set_executable", text_only, "alias.txt", "", 2, 0), ("append_text", text_only, "multi.txt", "x", 2, 0),
        ("replace", text_only, "multi.txt", "x", 2, 0), ("rename", text_only, "multi.txt", "m.txt", 2, 0),
        ("exists", text_only, "alias.txt", "", 0, 0), ("exists", text_only, "outdir/x.txt", "", 0, 0),
        ("make_dir", grant, "outdir/new", "", 1, 0), ("make_dir", grant, "nodir/new", "", 1, 0),
        ("exists", grant, "out.txt", "", 1, 0), ("exists", grant, "dangling.txt", "", 1, 0),
        ("exists", grant, "outdir", "", 1, 0), ("exists", grant, "nodir", "", 1, 0),
        ("exists", text_only, "out.txt", "", 0, 0), ("exists", text_only, "dangling.txt", "", 0, 0),
        ("is_dir", grant, "outdir", "", 1, 0), ("is_dir", grant, "nodir", "", 1, 0),
    ]);
    let host = outer.path().to_string_lossy().into_owned();
    for (export, token, path, data, status, flag) in rows {
        let (answer, set, message) = effect(&mut vm, export, token, path, data.as_bytes()).unwrap();
        assert_eq!((answer, set), (status, flag), "{export} {path}: {message}");
        if status == 0 {
            assert!(message.is_empty(), "{export} {path}: {message}");
            continue;
        }
        // A rename refused on its destination names the destination.
        let named = message.starts_with(&format!("`{path}` "))
            || (export == "rename" && message.starts_with(&format!("`{data}` ")));
        assert!(named, "{export} {path}: {message}");
        assert!(!message.contains(&host) && !message.contains("escaped contents"), "{export} {path}: {message}");
    }
    assert_eq!(std::fs::read(outer.path().join("outside.txt")).unwrap(), b"escaped contents");
    assert_eq!(std::fs::read(root.join("secret.key")).unwrap(), b"hello");
    assert!(root.join("sub").is_dir());
    // On a case-insensitive filesystem a case-folded spelling reaches
    // `classified.TXT`: it is invisible to an `ext:.txt` grant and never written.
    if root.join("classified.txt").exists() {
        for export in ["write_text", "append_text", "create", "replace"] {
            let (status, _, message) = effect(&mut vm, export, text_only, "classified.txt", b"x").unwrap();
            assert_eq!(status, 2, "{export} classified.txt: {message}");
        }
        assert_eq!(std::fs::read(root.join("classified.TXT")).unwrap(), b"hello");
    }
    for escaped in ["missing.txt", "stolen.txt", "newdir", "outside-dir/x.txt", "outside-dir/new", "missing-dir"] {
        assert!(!outer.path().join(escaped).exists(), "{escaped} was created outside the root");
    }
    assert_eq!(std::fs::read(root.join("new.txt")).unwrap(), b"x");
    assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"hellox");
    assert_eq!(std::fs::read(root.join("blob.bin")).unwrap(), b"xx");
    assert_eq!(std::fs::read(root.join("b.txt")).unwrap(), b"x");
    assert_eq!(std::fs::read(root.join("moved.txt")).unwrap(), b"hello");
    assert_eq!(std::fs::read(root.join("keep.bin")).unwrap(), b"hello");
    assert!(root.join("newdir").is_dir() && !root.join("move.txt").exists());
    let read_only = attenuate(&mut vm, grant, true, false, "");
    let write_only = attenuate(&mut vm, grant, false, true, "");
    for export in ["write_text", "append_text", "append", "create", "replace", "rename", "make_dir", "set_executable"] {
        error(effect(&mut vm, export, read_only, "new.txt", b"x").unwrap_err(), "does not grant Write");
    }
    for export in ["exists", "is_dir"] {
        error(effect(&mut vm, export, write_only, "a.txt", b"").unwrap_err(), "does not grant Read");
    }
}
