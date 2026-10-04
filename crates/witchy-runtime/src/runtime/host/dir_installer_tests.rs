//! Raw Wasm boundary probes; source/admission execution is tested by integration.
use super::*;
use crate::runtime::{Capabilities, Runtime, Vm};

type Token = Option<Rooted<ExternRef>>;
type Args = (Token, i32, i32, i32, i32);
const WAT: &str = r#"(module
  (import "witchy" "dir_append_bytes_at" (func $append (param externref i32 i32 i32 i32) (result externref)))
  (import "witchy" "dir_set_executable_at" (func $chmod (param externref i32 i32) (result externref)))
  (import "witchy" "dir_rename_at" (func $rename (param externref i32 i32 i32 i32) (result externref)))
  (import "witchy" "cap_response_status" (func $status (param externref) (result i32)))
  (memory (export "memory") 1)
  (func (export "run"))
  (func (export "append") (param externref i32 i32 i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $append call $status)
  (func (export "chmod") (param externref i32 i32 i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 call $chmod call $status)
  (func (export "rename") (param externref i32 i32 i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $rename call $status))"#;

fn setup(with_memory: bool) -> (tempfile::TempDir, Vm, Token) {
    let root = tempfile::tempdir().unwrap();
    let wat = if with_memory { WAT.to_owned() } else { WAT.replace("(memory (export \"memory\") 1)", "") };
    let caps = Capabilities { dir_root: Some(root.path().to_owned()), dir_read: true, dir_write: true, ..Capabilities::none() };
    let mut vm = Runtime::new().unwrap().spawn(&wat, caps, 1).unwrap();
    let authority = dir_grant(vm.store.data(), 0).unwrap();
    let token = Some(ExternRef::new(&mut vm.store, authority).unwrap());
    (root, vm, token)
}

fn attenuate(vm: &mut Vm, grant: Token, read: bool, write: bool, policy: &str) -> Token {
    let mut authority = grant.unwrap().data(&vm.store).unwrap().unwrap().downcast_ref::<DirAuthority>().unwrap().clone();
    authority.rights = FsRights { read, write };
    authority.policy = policy.to_owned();
    Some(ExternRef::new(&mut vm.store, authority).unwrap())
}

fn put(vm: &mut Vm, offset: usize, bytes: &[u8]) {
    vm.instance.get_memory(&mut vm.store, "memory").unwrap().write(&mut vm.store, offset, bytes).unwrap();
}

fn inputs(vm: &mut Vm, grant: Token, path: &str, payload: &[u8]) -> Args {
    put(vm, 17, path.as_bytes());
    put(vm, 1025, payload);
    (grant, 17, path.len() as i32, 1025, payload.len() as i32)
}

/// An installer export on `args`, answered as its response's status
/// (0 success, 1 Denied, 2 NotFound, ...); a trap is the `Err`.
fn call(vm: &mut Vm, name: &str, args: Args) -> Result<i32> {
    let memory = vm.instance.get_memory(&mut vm.store, "memory");
    let before = memory.map(|memory| memory.data(&vm.store).to_vec());
    let function = vm.instance.get_typed_func::<Args, i32>(&mut vm.store, name).unwrap();
    let result = function.call(&mut vm.store, args);
    if let (Some(memory), Some(before)) = (memory, before) {
        assert_eq!(memory.data(&vm.store), before.as_slice(), "installer imports must not write guest memory");
    }
    assert!(vm.store.data().pending.is_none(), "no staging protocol");
    assert!(vm.output().is_empty(), "host operation must not print");
    result
}

fn error(error: Error, expected: &str) {
    let actual = format!("{error:#}");
    assert!(actual.contains(expected), "expected {expected:?}, got {actual}");
}

pub fn dir_installer_authentication_and_write_right_precede_memory() {
    let (_root, mut vm, grant) = setup(false);
    let read_only = attenuate(&mut vm, grant, true, false, "");
    let forged = Some(ExternRef::new(&mut vm.store, 1u32).unwrap());
    for operation in ["append", "chmod", "rename"] {
        for (token, expected) in [(None, "null"), (forged, "wrong host data"), (read_only, "does not grant Write")] {
            error(call(&mut vm, operation, (token, -1, -1, -1, -1)).unwrap_err(), expected);
        }
        assert!(call(&mut vm, operation, (grant, -1, -1, -1, -1)).is_err(), "valid authority still requires memory");
    }
}

pub fn dir_installer_full_buffer_preflight_prevents_filesystem_effects() {
    let (root, mut vm, grant) = setup(true);
    std::fs::write(root.path().join("keep"), b"original").unwrap();
    std::fs::write(root.path().join("dest"), b"destination").unwrap();
    let mode = std::fs::metadata(root.path().join("keep")).unwrap().permissions();
    for operation in ["append", "chmod", "rename"] {
        for (ptr, len) in [(-1, 0), (0, -1), (-1, -1), (65535, 2), (65537, 0), (i32::MAX, i32::MAX)] {
            for second in [false, true] {
                if operation == "chmod" && second { continue; }
                let mut args = inputs(&mut vm, grant, "keep", b"dest");
                if second { args.3 = ptr; args.4 = len; } else { args.1 = ptr; args.2 = len; }
                assert!(call(&mut vm, operation, args).is_err(), "{operation} second={second}, {ptr}/{len}");
                assert_eq!(std::fs::read(root.path().join("keep")).unwrap(), b"original");
                assert_eq!(std::fs::read(root.path().join("dest")).unwrap(), b"destination");
                assert_eq!(std::fs::metadata(root.path().join("keep")).unwrap().permissions(), mode);
            }
        }
        let mut args = inputs(&mut vm, grant, "keep", b"dest");
        put(&mut vm, 17, &[0xff]);
        args.2 = 1;
        assert!(call(&mut vm, operation, args).is_err());
    }
    let args = inputs(&mut vm, grant, "keep", &[0xff]);
    assert!(call(&mut vm, "rename", args).is_err(), "destination path must be UTF-8");
    for path in ["../outside", "/outside", "bad\0name", "missing/child"] {
        let args = inputs(&mut vm, grant, path, b"dest");
        for operation in ["append", "chmod", "rename"] {
            assert_ne!(call(&mut vm, operation, args).unwrap(), 0, "{operation}: {path:?}");
        }
        let args = inputs(&mut vm, grant, "keep", path.as_bytes());
        assert_ne!(call(&mut vm, "rename", args).unwrap(), 0);
    }
    assert_eq!(std::fs::read(root.path().join("keep")).unwrap(), b"original");
    assert_eq!(std::fs::read(root.path().join("dest")).unwrap(), b"destination");
    assert_eq!(std::fs::metadata(root.path().join("keep")).unwrap().permissions(), mode);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
}

pub fn dir_installer_entry_policy_applies_to_both_rename_paths() {
    let (root, mut vm, grant) = setup(true);
    std::fs::write(root.path().join("keep.bin"), b"original").unwrap();
    std::fs::write(root.path().join("deny.txt"), b"denied").unwrap();
    let filtered = attenuate(&mut vm, grant, false, true, "ext:.bin");
    for operation in ["append", "chmod"] {
        let args = inputs(&mut vm, filtered, "deny.txt", b"data");
        assert_eq!(call(&mut vm, operation, args).unwrap(), 1, "{operation}: a refused name is Denied");
    }
    // A refused destination is Denied; an extension policy admits any
    // directory name, so a refused file name as the source is invisible.
    for (from, to, status) in [("keep.bin", "deny.txt", 1), ("deny.txt", "fresh.bin", 2)] {
        let args = inputs(&mut vm, filtered, from, to.as_bytes());
        assert_eq!(call(&mut vm, "rename", args).unwrap(), status, "{from} -> {to}");
    }
    assert_eq!(std::fs::read(root.path().join("keep.bin")).unwrap(), b"original");
    assert_eq!(std::fs::read(root.path().join("deny.txt")).unwrap(), b"denied");
    let args = inputs(&mut vm, filtered, "keep.bin", b"ok.bin");
    assert_eq!(call(&mut vm, "rename", args).unwrap(), 0);
    assert_eq!(std::fs::read(root.path().join("ok.bin")).unwrap(), b"original");
    std::fs::create_dir(root.path().join("directory")).unwrap();
    let files_only = attenuate(&mut vm, grant, false, true, "kind:file");
    let args = inputs(&mut vm, files_only, "directory", b"moved");
    assert_eq!(call(&mut vm, "rename", args).unwrap(), 2, "a directory a files-only grant cannot see");
    let dirs_only = attenuate(&mut vm, grant, false, true, "kind:dir");
    let args = inputs(&mut vm, dirs_only, "directory", b"moved");
    assert_eq!(call(&mut vm, "rename", args).unwrap(), 0);
    assert!(root.path().join("moved").is_dir());
}

#[cfg(unix)]
pub fn dir_installer_symlink_behavior_preserves_confined_provider_policy() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let (root, mut vm, grant) = setup(true);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"outside").unwrap();
    std::fs::set_permissions(outside.path().join("secret"), std::fs::Permissions::from_mode(0o600)).unwrap();
    let mode = std::fs::metadata(outside.path().join("secret")).unwrap().permissions();
    symlink(outside.path(), root.path().join("parent-link")).unwrap();
    symlink(outside.path().join("secret"), root.path().join("leaf.bin")).unwrap();
    std::fs::write(root.path().join("keep.bin"), b"local").unwrap();
    for path in ["leaf.bin", "parent-link/secret"] {
        let args = inputs(&mut vm, grant, path, b"changed");
        assert_eq!(call(&mut vm, "append", args).unwrap(), 1, "{path}: a link out of the root is Denied");
        assert_eq!(call(&mut vm, "chmod", args).unwrap(), 1, "{path}: a link out of the root is Denied");
    }
    for (from, to) in [("parent-link/secret", "stolen"), ("keep.bin", "parent-link/secret")] {
        let args = inputs(&mut vm, grant, from, to.as_bytes());
        assert_eq!(call(&mut vm, "rename", args).unwrap(), 1, "{from} -> {to}");
    }
    let filtered = attenuate(&mut vm, grant, false, true, "ext:.bin");
    for (from, to) in [("leaf.bin", "new.bin"), ("keep.bin", "leaf.bin")] {
        let args = inputs(&mut vm, filtered, from, to.as_bytes());
        assert_eq!(call(&mut vm, "rename", args).unwrap(), 2, "{from} -> {to}: a filtered alias is invisible");
    }
    // Unrestricted rename moves/replaces the link entry, not the target.
    let args = inputs(&mut vm, grant, "leaf.bin", b"moved.bin");
    assert_eq!(call(&mut vm, "rename", args).unwrap(), 0);
    assert!(std::fs::symlink_metadata(root.path().join("moved.bin")).unwrap().file_type().is_symlink());
    let args = inputs(&mut vm, grant, "keep.bin", b"moved.bin");
    assert_eq!(call(&mut vm, "rename", args).unwrap(), 0);
    assert!(!std::fs::symlink_metadata(root.path().join("moved.bin")).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(root.path().join("moved.bin")).unwrap(), b"local");
    assert_eq!(std::fs::read(outside.path().join("secret")).unwrap(), b"outside");
    assert_eq!(std::fs::metadata(outside.path().join("secret")).unwrap().permissions(), mode);
}

pub fn dir_installer_imports_require_existing_write_grant_linkage() {
    let root = tempfile::tempdir().unwrap();
    let read_only = Capabilities { dir_root: Some(root.path().to_owned()), dir_read: true, ..Capabilities::none() };
    let mut runtime = Runtime::new().unwrap();
    for (name, params) in [
        ("dir_append_bytes_at", "externref i32 i32 i32 i32"),
        ("dir_set_executable_at", "externref i32 i32"),
        ("dir_rename_at", "externref i32 i32 i32 i32"),
    ] {
        let wat = format!("(module (import \"witchy\" \"{name}\" (func (param {params}) (result externref))) (func (export \"run\")))");
        assert!(runtime.spawn(&wat, Capabilities::none(), 1).is_err(), "no grants: {name}");
        assert!(runtime.spawn(&wat, read_only.clone(), 1).is_err(), "Read is not Write: {name}");
    }
}
