use super::*;
use crate::runtime::{Capabilities, Runtime, Vm};

type Token = Option<Rooted<ExternRef>>;

const WAT: &str = r#"(module
  (import "witchy" "build_read_at" (func $read (param externref i32 i32) (result externref)))
  (import "witchy" "build_read_response_size" (func $size (param externref) (result i32)))
  (import "witchy" "build_read_response_into" (func $copy (param externref i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "run"))
  (func (export "read") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $read)
  (func (export "size") (param externref) (result i32) local.get 0 call $size)
  (func (export "copy") (param externref i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 call $copy))"#;

pub(in crate::runtime::host) fn setup() -> (tempfile::TempDir, Vm, Token) {
    let root = tempfile::tempdir().unwrap();
    for (name, contents) in [("a", "first"), ("b", "second")] {
        std::fs::create_dir(root.path().join(name)).unwrap();
        std::fs::write(root.path().join(name).join("sample.txt"), contents).unwrap();
    }
    let caps = Capabilities {
        build_read_roots: vec![root.path().join("a"), root.path().join("b")],
        ..Capabilities::none()
    };
    let mut runtime = Runtime::new().unwrap();
    let mut vm = runtime.spawn(WAT, caps, 1).unwrap();
    let authority = mint(vm.store.data()).unwrap();
    let grant = Some(ExternRef::new(&mut vm.store, authority).unwrap());
    (root, vm, grant)
}

pub(in crate::runtime::host) fn read(vm: &mut Vm, grant: Token, path: &str) -> Result<Token> {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.write(&mut vm.store, 0, path.as_bytes())?;
    let call = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "read")?;
    call.call(&mut vm.store, (grant, 0, path.len() as i32))
}

fn bytes(vm: &Vm, token: Token) -> &[u8] {
    token.unwrap().data(&vm.store).unwrap().unwrap().downcast_ref::<Response>().unwrap().bytes.as_deref().unwrap()
}

#[test]
fn build_read_authenticates_grants_before_memory_and_rejects_missing_grants() {
    let (root, _vm, _grant) = setup();
    let no_memory = WAT.replace("(memory (export \"memory\") 1)", "");
    let mut runtime = Runtime::new().unwrap();
    let mut vm = runtime.spawn(&no_memory, Capabilities {
        build_read_roots: vec![root.path().join("a")], ..Capabilities::none()
    }, 1).unwrap();
    let valid = mint(vm.store.data()).unwrap();
    let wrong_session = Authority { issuer: Arc::new(()), ..valid.clone() };
    let denied = Authority { read: false, ..valid.clone() };
    let invalid = [
        (None, "read_build: null capability"),
        (Some(ExternRef::new(&mut vm.store, 12u32).unwrap()), "read_build: wrong capability kind"),
        (Some(ExternRef::new(&mut vm.store, wrong_session).unwrap()), "read_build: capability belongs to another instance"),
        (Some(ExternRef::new(&mut vm.store, denied).unwrap()), "read_build: capability lacks read right"),
    ];
    let call = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "read").unwrap();
    for (token, expected) in invalid {
        let error = call.call(&mut vm.store, (token, -1, -1)).unwrap_err();
        assert_eq!(error.root_cause().to_string(), expected);
    }
    let valid = Some(ExternRef::new(&mut vm.store, valid).unwrap());
    assert!(call.call(&mut vm.store, (valid, 0, 0)).unwrap_err()
        .root_cause().to_string().contains("does not export a linear `memory`"));
    let ungranted = runtime.spawn(&no_memory, Capabilities::none(), 1).unwrap();
    assert_eq!(mint(ungranted.store.data()).unwrap_err().to_string(),
        "build step demands `BuildRead` but no read grant was provided");
    assert!(vm.output().is_empty());
}

#[test]
fn build_read_rejects_malformed_path_slices_before_filesystem_lookup() {
    let (_root, mut vm, grant) = setup();
    let call = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "read").unwrap();
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    for (ptr, len) in [(-1, 1), (0, -1), (65535, 2), (65536, 1), (i32::MAX, i32::MAX)] {
        let error = call.call(&mut vm.store, (grant, ptr, len)).unwrap_err();
        assert!(!error.root_cause().to_string().contains("not found in any granted read root"));
    }
    memory.write(&mut vm.store, 0, &[0xff]).unwrap();
    let before = memory.data(&vm.store).to_vec();
    assert!(call.call(&mut vm.store, (grant, 0, 1)).unwrap_err()
        .root_cause().to_string().contains("not valid UTF-8"));
    assert_eq!(memory.data(&vm.store), before.as_slice());
    assert!(vm.store.data().pending.is_none());
}

#[test]
fn build_read_response_kind_and_session_are_not_read_authority() {
    let (_root, mut vm, grant) = setup();
    let result = read(&mut vm, grant, "sample.txt").unwrap();
    let actual = result.unwrap().data(&vm.store).unwrap().unwrap().downcast_ref::<Response>().unwrap().clone();
    let foreign = Some(ExternRef::new(&mut vm.store, Response { issuer: Arc::new(()), ..actual }).unwrap());
    let forged = Some(ExternRef::new(&mut vm.store, 13u32).unwrap());
    let size = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap();
    let copy = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap();
    for (token, expected) in [
        (None, "BuildRead response is null"),
        (grant, "BuildRead response has wrong host kind"),
        (forged, "BuildRead response has wrong host kind"),
        (foreign, "BuildRead response belongs to another instance"),
    ] {
        assert_eq!(size.call(&mut vm.store, token).unwrap_err().root_cause().to_string(), expected);
        assert_eq!(copy.call(&mut vm.store, (token, -1, -1)).unwrap_err().root_cause().to_string(), expected);
    }
    let call = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "read").unwrap();
    assert_eq!(call.call(&mut vm.store, (result, -1, -1)).unwrap_err().root_cause().to_string(),
        "read_build: wrong capability kind");
    assert!(vm.output().is_empty());
}

#[test]
fn build_read_confines_absolute_and_parent_paths() {
    let (root, mut vm, grant) = setup();
    std::fs::write(root.path().join("outside.txt"), "outside").unwrap();
    for path in ["../outside.txt".to_owned(), root.path().join("outside.txt").to_str().unwrap().to_owned()] {
        let error = read(&mut vm, grant, &path).unwrap_err();
        assert!(error.root_cause().to_string().contains("not found in any granted read root"));
    }
}

#[cfg(unix)]
#[test]
fn build_read_confined_root_failure_can_fall_back_but_never_escape() {
    let (root, mut vm, grant) = setup();
    std::fs::write(root.path().join("outside.txt"), "outside").unwrap();
    std::os::unix::fs::symlink(root.path().join("outside.txt"), root.path().join("a/link")).unwrap();
    std::fs::write(root.path().join("b/link"), "safe").unwrap();
    let result = read(&mut vm, grant, "link").unwrap();
    assert_eq!(bytes(&vm, result), b"safe");
    std::fs::remove_file(root.path().join("b/link")).unwrap();
    assert!(read(&mut vm, grant, "link").is_err());
}

#[test]
fn build_read_response_length_never_wraps_the_i32_abi() {
    assert_eq!(checked_response_len(0).unwrap(), 0);
    assert_eq!(checked_response_len(i32::MAX as usize).unwrap(), i32::MAX);
    assert!(checked_response_len(i32::MAX as usize + 1).is_err());
    assert!(checked_response_len(usize::MAX).is_err());
}
