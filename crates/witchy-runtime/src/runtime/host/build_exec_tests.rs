use super::*;
use crate::runtime::{Capabilities, Runtime, Vm};

type Token = Option<Rooted<ExternRef>>;
type Arguments = (Token, i32, i32, i32, i32);

const WAT: &str = r#"(module
  (import "witchy" "build_exec_run" (func $exec (param externref i32 i32 i32 i32) (result externref)))
  (import "witchy" "build_response_is_some" (func $some (param externref) (result i32)))
  (import "witchy" "build_read_response_size" (func $size (param externref) (result i32)))
  (import "witchy" "build_read_response_into" (func $copy (param externref i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "run"))
  (func (export "exec") (param externref i32 i32 i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $exec)
  (func (export "some") (param externref) (result i32) local.get 0 call $some)
  (func (export "size") (param externref) (result i32) local.get 0 call $size)
  (func (export "copy") (param externref i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 call $copy))"#;

fn vm(caps: Capabilities, memory: bool) -> Vm {
    let wat = if memory { WAT.to_owned() } else {
        WAT.replace("(memory (export \"memory\") 1)", "")
    };
    Runtime::new().unwrap().spawn(&wat, caps, 1).unwrap()
}

pub(in crate::runtime::host) fn empty_grant_fixture() -> (Vm, Token) {
    let mut vm = vm(Capabilities { exec_allow: Some(Vec::new()), ..Capabilities::none() }, true);
    let authority = mint(vm.store.data()).unwrap();
    let grant = Some(ExternRef::new(&mut vm.store, authority).unwrap());
    (vm, grant)
}

fn call(vm: &mut Vm, grant: Token, tool: &str, input: &str) -> Result<Token> {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.write(&mut vm.store, 0, tool.as_bytes())?;
    memory.write(&mut vm.store, 1024, input.as_bytes())?;
    vm.instance.get_typed_func::<Arguments, Token>(&mut vm.store, "exec")?
        .call(&mut vm.store, (grant, 0, tool.len() as i32, 1024, input.len() as i32))
}

fn script_fixture(body: &str) -> (tempfile::TempDir, Vm, Token) {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("calls");
    let quoted_marker = marker.to_str().unwrap().replace('\'', "'\\''");
    let executable = dir.path().join("selected-tool");
    std::fs::write(&executable, format!(
        "#!/bin/sh\n[ \"$#\" -eq 0 ] || exit 91\nprintf x >> '{quoted_marker}'\n{body}\n"
    )).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut vm = vm(Capabilities {
        exec_allow: Some(vec!["worker".into()]),
        build_exec_tools: vec![("worker".into(), executable)],
        ..Capabilities::none()
    }, true);
    let authority = mint(vm.store.data()).unwrap();
    let grant = Some(ExternRef::new(&mut vm.store, authority).unwrap());
    (dir, vm, grant)
}

#[cfg(unix)]
#[test]
fn build_exec_runs_once_and_retains_independent_some_responses() {
    let (dir, mut vm, grant) = script_fixture("/bin/cat\nprintf 'ignored-stderr' >&2");
    let weak = {
        let data = grant.unwrap().data(&vm.store).unwrap().unwrap().downcast_ref::<Authority>().unwrap();
        Arc::downgrade(&data.tools)
    };
    let first_text = "first caf\u{e9}\n";
    let second_text = "second\0value";
    let first = call(&mut vm, grant, "worker", first_text).unwrap();
    let second = call(&mut vm, grant, "worker", second_text).unwrap();
    let empty = call(&mut vm, grant, "worker", "").unwrap();
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    let some = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "some").unwrap();
    let size = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap();
    let copy = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap();
    // Retain the results independently of subsequent invocations and grant
    // table changes. Repeated result access must not run the tool again.
    vm.store.data_mut().build_exec_tools.clear();
    vm.store.data_mut().caps.exec_allow = None;
    for (token, expected) in [(first, first_text), (second, second_text), (first, first_text), (empty, "")] {
        assert!(token.is_some());
        assert_eq!(some.call(&mut vm.store, token).unwrap(), 1);
        let length = expected.len() as i32;
        assert_eq!(size.call(&mut vm.store, token).unwrap(), length);
        memory.write(&mut vm.store, 4096, &[0xa5; 64]).unwrap();
        let before = memory.data(&vm.store).to_vec();
        if length > 0 {
            assert_eq!(copy.call(&mut vm.store, (token, 4096, length - 1)).unwrap(), length);
            assert_eq!(memory.data(&vm.store), before.as_slice());
        }
        for (ptr, capacity) in [(-1, 1), (0, -1), (65535, 2), (65537, 0), (4096, i32::MAX)] {
            assert!(copy.call(&mut vm.store, (token, ptr, capacity)).is_err());
            assert_eq!(memory.data(&vm.store), before.as_slice());
        }
        assert_eq!(copy.call(&mut vm.store, (token, 4096, 64)).unwrap(), length);
        assert_eq!(&memory.data(&vm.store)[4096..4096 + expected.len()], expected.as_bytes());
        assert!(memory.data(&vm.store)[4096 + expected.len()..4160].iter().all(|byte| *byte == 0xa5));
    }
    assert_eq!(std::fs::read(dir.path().join("calls")).unwrap(), b"xxx");
    assert!(vm.output().is_empty());
    assert!(vm.store.data().pending.is_none());
    // This host fixture has no canonical guest heap; audit the host-owned map
    // directly instead of claiming a synthetic zero-guest-leak result.
    drop(vm);
    assert!(weak.upgrade().is_none(), "Store teardown releases the tool-map root");
}

#[cfg(unix)]
#[test]
fn build_exec_preserves_lossy_stdout_only_and_nonzero_failure() {
    let (dir, mut vm, grant) = script_fixture("printf '\\377'\nprintf '\\302\\251' >&2");
    let response = call(&mut vm, grant, "worker", "").unwrap();
    let copy = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap();
    assert_eq!(copy.call(&mut vm.store, (response, 4096, 3)).unwrap(), 3);
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    assert_eq!(&memory.data(&vm.store)[4096..4099], "\u{fffd}".as_bytes());
    assert_eq!(std::fs::read(dir.path().join("calls")).unwrap(), b"x");
    assert!(vm.output().is_empty());
    let (failed_dir, mut failed_vm, failed_grant) = script_fixture("printf 'not-a-response'\nprintf 'private-stderr' >&2\nexit 7");
    let error = call(&mut failed_vm, failed_grant, "worker", "").unwrap_err().root_cause().to_string();
    assert!(error.starts_with("run_tool: `worker` exited with ") && error.contains('7'), "{error}");
    assert!(!error.contains("not-a-response") && !error.contains("private-stderr"));
    assert_eq!(std::fs::read(failed_dir.path().join("calls")).unwrap(), b"x");
    assert!(failed_vm.output().is_empty());
    assert!(failed_vm.store.data().pending.is_none());
}

#[test]
fn build_exec_authenticates_kind_session_and_execute_before_memory() {
    let mut vm = vm(Capabilities { exec_allow: Some(Vec::new()), ..Capabilities::none() }, false);
    let authority = mint(vm.store.data()).unwrap();
    let forged = Some(ExternRef::new(&mut vm.store, 15u32).unwrap());
    let ordinary = Some(ExternRef::new(&mut vm.store, crate::runtime::ExecAuthority { allow: None }).unwrap());
    let foreign = Some(ExternRef::new(&mut vm.store, Authority { issuer: Arc::new(()), ..authority.clone() }).unwrap());
    let denied = Some(ExternRef::new(&mut vm.store, Authority { execute: false, ..authority }).unwrap());
    let response = Some(super::super::build_read::new_response(&mut vm.store, Some("not authority")).unwrap());
    let run = vm.instance.get_typed_func::<Arguments, Token>(&mut vm.store, "exec").unwrap();
    for (token, expected) in [
        (None, "run_tool: null capability"), (forged, "run_tool: wrong capability kind"),
        (ordinary, "run_tool: wrong capability kind"), (response, "run_tool: wrong capability kind"),
        (foreign, "run_tool: capability belongs to another instance"),
        (denied, "run_tool: capability lacks execute right"),
    ] {
        assert_eq!(run.call(&mut vm.store, (token, -1, -1, -1, -1)).unwrap_err().root_cause().to_string(), expected);
    }
    assert!(vm.output().is_empty());
    assert!(vm.store.data().pending.is_none());
}

#[test]
fn build_exec_requires_explicit_resolved_grants_and_never_looks_up_guest_paths() {
    let absent = vm(Capabilities { exec: true, ..Capabilities::none() }, false);
    assert!(mint(absent.store.data()).unwrap_err().to_string().contains("no tool grant"));
    let unresolved = vm(Capabilities { exec_allow: Some(vec!["not-resolved".into()]), ..Capabilities::none() }, false);
    assert_eq!(mint(unresolved.store.data()).unwrap_err().to_string(),
        "run_tool: granted tool `not-resolved` has no resolved confined executable");
    let (dir, mut vm, grant) = script_fixture("exit 0");
    for tool in ["missing".to_owned(), "cat".to_owned(), dir.path().join("selected-tool").to_str().unwrap().to_owned()] {
        let error = call(&mut vm, grant, &tool, "").unwrap_err();
        assert_eq!(error.root_cause().to_string(), format!("run_tool: `{tool}` is not in this BuildExec grant's allow-list"));
    }
    vm.store.data_mut().caps.exec_allow = Some(Vec::new());
    let empty_authority = mint(vm.store.data()).unwrap();
    assert!(empty_authority.tools.is_empty(), "resolved files must not widen an empty grant");
    let empty = Some(ExternRef::new(&mut vm.store, empty_authority).unwrap());
    assert_eq!(call(&mut vm, empty, "worker", "").unwrap_err().root_cause().to_string(),
        "run_tool: `worker` is not in this BuildExec grant's allow-list");
    assert!(!dir.path().join("calls").exists());
    assert!(vm.output().is_empty());
}

#[test]
fn build_exec_validates_both_complete_utf8_slices_before_any_execution() {
    let (dir, mut vm, grant) = script_fixture("exit 0");
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.write(&mut vm.store, 0, b"worker").unwrap();
    let run = vm.instance.get_typed_func::<Arguments, Token>(&mut vm.store, "exec").unwrap();
    for (ptr, len) in [(-1, 1), (0, -1), (65535, 2), (65537, 0), (i32::MAX, i32::MAX)] {
        let before = memory.data(&vm.store).to_vec();
        assert!(run.call(&mut vm.store, (grant, ptr, len, 1024, 0)).is_err());
        assert!(run.call(&mut vm.store, (grant, 0, 6, ptr, len)).is_err());
        assert_eq!(memory.data(&vm.store), before.as_slice());
    }
    memory.write(&mut vm.store, 1024, &[0xff]).unwrap();
    assert!(run.call(&mut vm.store, (grant, 0, 6, 1024, 1)).unwrap_err().root_cause().to_string().contains("not valid UTF-8"));
    memory.write(&mut vm.store, 0, &[0xff]).unwrap();
    assert!(run.call(&mut vm.store, (grant, 0, 1, 2048, 0)).unwrap_err().root_cause().to_string().contains("not valid UTF-8"));
    assert!(!dir.path().join("calls").exists(), "no malformed input may start the tool");
    assert!(vm.store.data().pending.is_none());
    assert!(vm.output().is_empty());
}

#[test]
fn build_exec_response_is_not_a_root_and_requires_current_response_issuer() {
    let (mut vm, grant) = empty_grant_fixture();
    let response = Some(super::super::build_read::new_response(&mut vm.store, Some("output")).unwrap());
    assert_eq!(call(&mut vm, response, "worker", "").unwrap_err().root_cause().to_string(), "run_tool: wrong capability kind");
    let size = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap();
    let copy = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap();
    assert_eq!(size.call(&mut vm.store, grant).unwrap_err().root_cause().to_string(), "BuildRead response has wrong host kind");
    vm.store.data_mut().build_read_issuer = Arc::new(());
    assert_eq!(size.call(&mut vm.store, response).unwrap_err().root_cause().to_string(), "BuildRead response belongs to another instance");
    assert_eq!(copy.call(&mut vm.store, (response, -1, -1)).unwrap_err().root_cause().to_string(), "BuildRead response belongs to another instance");
}

#[cfg(not(unix))]
#[test]
fn build_exec_preserves_unsupported_platform_refusal() {
    let (dir, mut vm, grant) = script_fixture("exit 0");
    let error = call(&mut vm, grant, "worker", "").unwrap_err().root_cause().to_string();
    assert!(error.contains("race-free confined executable launch is unavailable on this target"), "{error}");
    assert!(!dir.path().join("calls").exists());
}
