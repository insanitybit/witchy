use super::*;
use crate::runtime::{Capabilities, Runtime, Vm};

type Token = Option<Rooted<ExternRef>>;
const WAT: &str = r#"(module
  (import "witchy" "mint_dir" (func $mint (param i32) (result externref)))
  (import "witchy" "dir_is_dir_at" (func $query (param externref i32 i32) (result i32)))
  (import "witchy" "dir_is_dir" (func $legacy (param externref i32) (result i32)))
  (memory (export "memory") 1)
  (export "mint" (func $mint))
  (func (export "query") (param externref i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 call $query)
  (func (export "legacy") (param externref i32) (result i32)
    local.get 0 local.get 1 call $legacy)
  (func (export "run")))"#;

fn setup(memory: bool) -> (tempfile::TempDir, Vm, Token) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("nested/leaf")).unwrap();
    std::fs::create_dir(root.path().join("\u{e9}-\u{1f980}")).unwrap();
    std::fs::write(root.path().join("file"), b"unchanged").unwrap();
    let caps = Capabilities { dir_root: Some(root.path().to_path_buf()), dir_read: true,
        ..Capabilities::none() };
    let wat = if memory { WAT.to_owned() } else { WAT.replace("(memory (export \"memory\") 1)", "") };
    let mut runtime = Runtime::new().unwrap();
    let mut vm = runtime.spawn(&wat, caps, 1).unwrap();
    let token = vm.instance.get_typed_func::<i32, Token>(&mut vm.store, "mint").unwrap()
        .call(&mut vm.store, 0).unwrap();
    (root, vm, token)
}

fn attenuate(vm: &mut Vm, token: Token, policy: &str, read: bool) -> Token {
    let mut authority = token.unwrap().data(&vm.store).unwrap().unwrap()
        .downcast_ref::<DirAuthority>().unwrap().clone();
    authority.policy = policy.to_owned();
    authority.rights.read = read;
    Some(ExternRef::new(&mut vm.store, authority).unwrap())
}

fn call(vm: &mut Vm, token: Token, ptr: i32, len: i32) -> Result<i32> {
    let memory = vm.instance.get_memory(&mut vm.store, "memory");
    let before = memory.map(|memory| memory.data(&vm.store).to_vec());
    let result = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "query").unwrap()
        .call(&mut vm.store, (token, ptr, len));
    if let (Some(memory), Some(before)) = (memory, before) {
        assert_eq!(memory.data(&vm.store), before.as_slice());
    }
    assert!(vm.output().is_empty());
    result
}

fn query(vm: &mut Vm, token: Token, path: &str, expected: i32) {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.data_mut(&mut vm.store).fill(0xa5);
    memory.write(&mut vm.store, 17, path.as_bytes()).unwrap();
    assert_eq!(call(vm, token, 17, path.len() as i32).unwrap(), expected, "{path:?}");
    // The frozen header-based ABI must give the same total predicate. This
    // fixture writes a header only for that independent legacy invocation.
    memory.write(&mut vm.store, 4096, &(path.len() as u32).to_le_bytes()).unwrap();
    memory.write(&mut vm.store, 4100, path.as_bytes()).unwrap();
    let before = memory.data(&vm.store).to_vec();
    let actual = vm.instance.get_typed_func::<(Token, i32), i32>(&mut vm.store, "legacy").unwrap()
        .call(&mut vm.store, (token, 4096)).unwrap();
    assert_eq!(actual, expected, "legacy {path:?}");
    assert_eq!(memory.data(&vm.store), before.as_slice());
    assert!(vm.output().is_empty());
}

#[test]
fn dir_is_dir_slices_preserve_entry_policy_without_minting_authority() {
    let (_root, mut vm, token) = setup(true);
    let directories = attenuate(&mut vm, token, "kind:dir", true);
    let files = attenuate(&mut vm, token, "kind:file", true);
    let deny_policy = witchy_caps::capabilities::dir_only("ext:.txt", "ext:.md");
    let denied = attenuate(&mut vm, token, &deny_policy, true);
    query(&mut vm, directories, "nested", 1);
    query(&mut vm, directories, "file", 0);
    query(&mut vm, files, "nested", 0);
    query(&mut vm, denied, "nested", 0);
    query(&mut vm, directories, "missing", 0);
    query(&mut vm, directories, "../", 0);
}

#[cfg(unix)]
#[test]
fn dir_is_dir_slices_preserve_confined_and_policy_alias_behavior() {
    let (root, mut vm, token) = setup(true);
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("nested", root.path().join("inside-link")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("outside-link")).unwrap();
    std::os::unix::fs::symlink("missing", root.path().join("dangling-link")).unwrap();
    query(&mut vm, token, "inside-link", 1);
    query(&mut vm, token, "outside-link", 0);
    query(&mut vm, token, "dangling-link", 0);
    let directories = attenuate(&mut vm, token, "kind:dir", true);
    query(&mut vm, directories, "inside-link", 0);
    query(&mut vm, directories, "outside-link", 0);
    query(&mut vm, directories, "nested", 1);
}

#[test]
fn dir_is_dir_slices_reject_invalid_transport_after_authentication() {
    let (_root, mut vm, token) = setup(true);
    let wrong = Some(ExternRef::new(&mut vm.store, "not a Dir").unwrap());
    let denied = attenuate(&mut vm, token, "", false);
    for (token, message) in [(None, "Dir externref is null"),
        (wrong, "Dir externref has wrong host data"),
        (denied, "this Dir capability does not grant Read")] {
        assert_eq!(call(&mut vm, token, -1, -1).unwrap_err().root_cause().to_string(), message);
    }
    for (ptr, len) in [(-1, 1), (0, -1), (65535, 2), (65537, 0), (i32::MAX, i32::MAX)] {
        assert!(call(&mut vm, token, ptr, len).is_err(), "({ptr}, {len})");
    }
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.write(&mut vm.store, 17, &[0xff]).unwrap();
    assert!(call(&mut vm, token, 17, 1).unwrap_err().root_cause().to_string().contains("not valid UTF-8"));

    let (_root, mut vm, token) = setup(false);
    let wrong = Some(ExternRef::new(&mut vm.store, 42_i64).unwrap());
    let denied = attenuate(&mut vm, token, "", false);
    for (token, message) in [(None, "Dir externref is null"),
        (wrong, "Dir externref has wrong host data"),
        (denied, "this Dir capability does not grant Read")] {
        assert_eq!(call(&mut vm, token, 0, 0).unwrap_err().root_cause().to_string(), message);
    }
    assert!(call(&mut vm, token, 0, 0).unwrap_err().root_cause().to_string().contains("memory"));
}

#[test]
fn dir_is_dir_slices_link_only_with_existing_read_grant() {
    let root = tempfile::tempdir().unwrap();
    let mut runtime = Runtime::new().unwrap();
    // No mint or legacy import can mask which import fails this boundary.
    let wat = r#"(module
      (import "witchy" "dir_is_dir_at" (func (param externref i32 i32) (result i32)))
      (func (export "run")))"#;
    for caps in [Capabilities::none(), Capabilities {
        dir_root: Some(root.path().to_path_buf()), dir_write: true, ..Capabilities::none()
    }] {
        let error = runtime.spawn(wat, caps, 1).err().expect("ungranted import must not link");
        assert!(error.to_string().contains("dir_is_dir_at"), "{error}");
    }
}
