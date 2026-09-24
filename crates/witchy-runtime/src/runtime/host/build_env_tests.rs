use super::*;
use crate::runtime::{Capabilities, Runtime, Vm};

type Token = Option<Rooted<ExternRef>>;

const WAT: &str = r#"(module
  (import "witchy" "build_env_at" (func $get (param externref i32 i32) (result externref)))
  (import "witchy" "build_response_is_some" (func $some (param externref) (result i32)))
  (import "witchy" "build_read_response_size" (func $size (param externref) (result i32)))
  (import "witchy" "build_read_response_into" (func $copy (param externref i32 i32) (result i32)))
  (import "witchy" "build_read_at" (func $read (param externref i32 i32) (result externref)))
  (memory (export "memory") 1)
  (func (export "run"))
  (func (export "get") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $get)
  (func (export "some") (param externref) (result i32) local.get 0 call $some)
  (func (export "size") (param externref) (result i32) local.get 0 call $size)
  (func (export "copy") (param externref i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 call $copy)
  (func (export "read") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $read))"#;

fn vm(caps: Capabilities, memory: bool) -> Vm {
    let wat = if memory { WAT.to_owned() } else {
        WAT.replace("(memory (export \"memory\") 1)", "")
    };
    Runtime::new().unwrap().spawn(&wat, caps, 1).unwrap()
}

pub(in crate::runtime::host) fn setup() -> (Vm, Token) {
    let mut vm = vm(Capabilities {
        build_env: Some([
            ("VALUE".into(), Some("caf\u{e9}".repeat(100))),
            ("EMPTY".into(), Some(String::new())),
            ("UNSET".into(), None),
        ].into()),
        ..Capabilities::none()
    }, true);
    let authority = mint(vm.store.data()).unwrap();
    let token = Some(ExternRef::new(&mut vm.store, authority).unwrap());
    (vm, token)
}

pub(in crate::runtime::host) fn get(vm: &mut Vm, grant: Token, key: &str) -> Result<Token> {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.write(&mut vm.store, 0, key.as_bytes())?;
    let call = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "get")?;
    call.call(&mut vm.store, (grant, 0, key.len() as i32))
}

#[test]
fn build_env_preserves_some_empty_unset_and_denied() {
    let (mut vm, grant) = setup();
    let some = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "some").unwrap();
    let size = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap();
    for (key, present, length) in [("VALUE", 1, Some(500)), ("EMPTY", 1, Some(0)), ("UNSET", 0, None)] {
        let result = get(&mut vm, grant, key).unwrap();
        assert!(result.is_some(), "unset is a private response, not a null host result");
        assert_eq!(some.call(&mut vm.store, result).unwrap(), present);
        if let Some(length) = length {
            assert_eq!(size.call(&mut vm.store, result).unwrap(), length);
        } else {
            assert_eq!(size.call(&mut vm.store, result).unwrap_err().root_cause().to_string(), "Build response is None");
        }
    }
    for key in ["MISSING", "value", "", "VALUE\0"] {
        let error = get(&mut vm, grant, key).unwrap_err();
        assert_eq!(error.root_cause().to_string(), format!("get_build_env: `{key}` is not in this BuildEnv grant's allow-list"));
    }
    assert!(vm.output().is_empty());
    assert!(vm.store.data().pending.is_none());
}

#[test]
fn build_env_authenticates_kind_session_and_read_before_memory() {
    let mut vm = vm(Capabilities { build_env: Some(BTreeMap::new()), ..Capabilities::none() }, false);
    let authority = mint(vm.store.data()).unwrap();
    let wrong_kind = Some(ExternRef::new(&mut vm.store, 14u32).unwrap());
    let foreign = Some(ExternRef::new(&mut vm.store, Authority { issuer: Arc::new(()), ..authority.clone() }).unwrap());
    let unreadable = Some(ExternRef::new(&mut vm.store, Authority { read: false, ..authority }).unwrap());
    let response = Some(super::super::build_read::new_response(&mut vm.store, Some("data")).unwrap());
    let call = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "get").unwrap();
    for (token, expected) in [
        (None, "get_build_env: null capability"),
        (wrong_kind, "get_build_env: wrong capability kind"),
        (response, "get_build_env: wrong capability kind"),
        (foreign, "get_build_env: capability belongs to another instance"),
        (unreadable, "get_build_env: capability lacks read right"),
    ] {
        assert_eq!(call.call(&mut vm.store, (token, -1, -1)).unwrap_err().root_cause().to_string(), expected);
    }
    assert!(vm.output().is_empty());
    assert!(vm.store.data().pending.is_none());
}

#[test]
fn build_env_missing_grant_and_explicit_empty_snapshot_stay_distinct() {
    let denied = vm(Capabilities::none(), false);
    assert!(mint(denied.store.data()).unwrap_err().to_string().contains("no environment grant"));
    let mut empty = vm(Capabilities { build_env: Some(BTreeMap::new()), ..Capabilities::none() }, true);
    let authority = mint(empty.store.data()).unwrap();
    let root = Some(ExternRef::new(&mut empty.store, authority).unwrap());
    assert_eq!(get(&mut empty, root, "ANY").unwrap_err().root_cause().to_string(),
        "get_build_env: `ANY` is not in this BuildEnv grant's allow-list");
    let read = empty.instance.get_typed_func::<(Token, i32, i32), Token>(&mut empty.store, "read").unwrap();
    assert_eq!(read.call(&mut empty.store, (root, -1, -1)).unwrap_err().root_cause().to_string(),
        "read_build: wrong capability kind");
}

#[test]
fn build_env_validates_full_key_slice_and_strict_utf8_before_lookup() {
    let (mut vm, grant) = setup();
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    let call = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "get").unwrap();
    for (ptr, len) in [(-1, 1), (0, -1), (65535, 2), (65537, 0), (i32::MAX, i32::MAX)] {
        let error = call.call(&mut vm.store, (grant, ptr, len)).unwrap_err();
        assert!(!error.root_cause().to_string().contains("allow-list"));
    }
    memory.write(&mut vm.store, 0, &[0xff]).unwrap();
    let before = memory.data(&vm.store).to_vec();
    assert!(call.call(&mut vm.store, (grant, 0, 1)).unwrap_err().root_cause().to_string().contains("not valid UTF-8"));
    assert_eq!(memory.data(&vm.store), before.as_slice());
    assert!(vm.output().is_empty());
}

#[test]
fn build_env_none_and_foreign_responses_refuse_before_memory() {
    let mut vm = vm(Capabilities { build_env: Some(BTreeMap::new()), ..Capabilities::none() }, false);
    let none = Some(super::super::build_read::new_response(&mut vm.store, None).unwrap());
    let some = Some(super::super::build_read::new_response(&mut vm.store, Some("")).unwrap());
    let authority = mint(vm.store.data()).unwrap();
    let root = Some(ExternRef::new(&mut vm.store, authority).unwrap());
    let forged = Some(ExternRef::new(&mut vm.store, 13u32).unwrap());
    let size = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap();
    let copy = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap();
    let present = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "some").unwrap();
    assert_eq!(present.call(&mut vm.store, none).unwrap(), 0);
    assert_eq!(present.call(&mut vm.store, some).unwrap(), 1);
    assert_eq!(size.call(&mut vm.store, none).unwrap_err().root_cause().to_string(), "Build response is None");
    assert_eq!(copy.call(&mut vm.store, (none, -1, -1)).unwrap_err().root_cause().to_string(), "Build response is None");
    vm.store.data_mut().build_read_issuer = Arc::new(());
    for (token, expected) in [
        (None, "BuildRead response is null"),
        (root, "BuildRead response has wrong host kind"),
        (forged, "BuildRead response has wrong host kind"),
        (none, "BuildRead response belongs to another instance"),
        (some, "BuildRead response belongs to another instance"),
    ] {
        assert_eq!(present.call(&mut vm.store, token).unwrap_err().root_cause().to_string(), expected);
        assert_eq!(size.call(&mut vm.store, token).unwrap_err().root_cause().to_string(), expected);
        assert_eq!(copy.call(&mut vm.store, (token, -1, -1)).unwrap_err().root_cause().to_string(), expected);
    }
    assert!(vm.output().is_empty());
}
