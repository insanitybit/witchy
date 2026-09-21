//! Net acceptance: real wire behavior plus adversarial host boundaries.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use witchy::runtime::{Capabilities, Runtime};

fn fixture_address() -> String {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string()
}

fn caps(address: &str) -> Capabilities {
    Capabilities { net_allow: Some(vec![address.to_owned()]), net_listen: true, quiet: true, ..Default::default() }
}

fn exchange(address: String, request: &[u8], connections: usize, run: impl FnOnce() -> Result<(), String> + Send + 'static) -> Result<String, String> {
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || { let _ = tx.send(run()); });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut sockets = Vec::new();
    for _ in 0..connections {
        loop {
            match TcpStream::connect(&address) {
                Ok(mut socket) => {
                    socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                    socket.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
                    socket.write_all(request).unwrap();
                    // A rejected host call can close the connection before
                    // this fixture half-closes; the asserted VM error below
                    // remains authoritative for those adversarial cases.
                    if let Err(error) = socket.shutdown(std::net::Shutdown::Write) {
                        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected, "fixture shutdown: {error}");
                    }
                    sockets.push(socket);
                    break;
                }
                Err(_) if Instant::now() < deadline => {
                    if let Ok(result) = rx.try_recv() {
                        worker.join().unwrap();
                        return result.and(Err("server returned before listening".into()));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(format!("fixture server did not listen: {error}")),
            }
        }
    }
    let mut response = String::new();
    let read = sockets[0].read_to_string(&mut response);
    let result = rx.recv_timeout(Duration::from_secs(10)).map_err(|error| error.to_string())?;
    worker.join().unwrap();
    result?;
    read.map_err(|error| error.to_string())?;
    Ok(response)
}

fn module(address: &str, body: &str) -> String {
    format!(r#"(module
      (import "witchy" "mint_net" (func $mint (param i32) (result externref)))
      (import "witchy" "net_listen_at" (func $listen (param externref i32 i32) (result externref)))
      (import "witchy" "net_accept" (func $accept (param externref) (result externref)))
      (import "witchy" "net_recv_line_size" (func $line (param externref) (result i32)))
      (import "witchy" "net_recv_bytes_size" (func $bytes (param externref i64) (result i32)))
      (import "witchy" "net_recv_all_size" (func $all (param externref) (result i32)))
      (import "witchy" "net_take_received_into" (func $fill (param externref i32 i32) (result i32)))
      (import "witchy" "net_send_slice" (func $send (param externref i32 i32)))
      (import "witchy" "net_close" (func $close (param externref)))
      (memory (export "memory") 1)
      (data (i32.const 0) "{address}")
      (data (i32.const 512) "\ff")
      (func (export "run") (local $listener externref) (local $sock externref) (local $other externref)
        {body}))"#)
}

/// The same raw fixture with the canonical dial import, for the verb-right
/// assertions that need to call `connect` rather than `listen`.
fn connect_module(address: &str, body: &str) -> String {
    module(address, body).replace(
        r#"(import "witchy" "net_accept" (func $accept (param externref) (result externref)))"#,
        concat!(
            r#"(import "witchy" "net_accept" (func $accept (param externref) (result externref)))"#,
            "
      ",
            r#"(import "witchy" "net_connect_at" (func $connect (param externref i32 i32) (result externref)))"#,
        ),
    )
}

fn raw(wat: String, capabilities: Capabilities) -> Result<(), String> {
    let mut runtime = Runtime::batch().map_err(|error| error.to_string())?;
    let mut vm = runtime.spawn(&wat, capabilities, 16).map_err(|error| error.to_string())?;
    vm.run().map_err(|error| format!("{error:#}"))
}

fn accept(address: &str) -> String {
    format!("(local.set $listener (call $listen (call $mint (i32.const 0)) (i32.const 0) (i32.const {})))\n(local.set $sock (call $accept (local.get $listener)))", address.len())
}

#[test]
fn host_net_real_socket_receives_once_and_preserves_exact_buffers() {
    let address = fixture_address();
    let body = format!(r#"{}
      (if (i32.ne (call $line (local.get $sock)) (i32.const 5)) (then unreachable))
      (i32.store8 (i32.const 63) (i32.const 33))
      (i32.store8 (i32.const 69) (i32.const 33))
      (drop (call $fill (local.get $sock) (i32.const 64) (i32.const 5)))
      (if (i32.ne (i32.load8_u (i32.const 63)) (i32.const 33)) (then unreachable))
      (if (i32.ne (i32.load8_u (i32.const 69)) (i32.const 33)) (then unreachable))
      (call $send (local.get $sock) (i32.const 64) (i32.const 5))
      (if (i32.ne (call $bytes (local.get $sock) (i64.const 5)) (i32.const 5)) (then unreachable))
      (drop (call $fill (local.get $sock) (i32.const 96) (i32.const 5)))
      (call $send (local.get $sock) (i32.const 96) (i32.const 5))
      (drop (call $bytes (local.get $sock) (i64.const 0)))
      (drop (call $fill (local.get $sock) (i32.const 0) (i32.const 0)))
      (call $close (local.get $sock))"#, accept(&address));
    let wat = module(&address, &body);
    let grants = caps(&address);
    assert_eq!(exchange(address, b"probe\nhello", 1, move || raw(wat, grants)).unwrap(), "probehello");
}

#[test]
fn host_net_rejects_missing_authority_wrong_refs_and_invalid_input_slices() {
    let address = "127.0.0.1:0";
    let calls = [
        "(drop (call $listen (ref.null extern) (i32.const 0) (i32.const 11)))",
        "(call $send (call $mint (i32.const 0)) (i32.const 0) (i32.const 0))",
        "(drop (call $listen (call $mint (i32.const 0)) (i32.const -1) (i32.const 11)))",
        "(drop (call $listen (call $mint (i32.const 0)) (i32.const 0) (i32.const -1)))",
        "(drop (call $listen (call $mint (i32.const 0)) (i32.const 65535) (i32.const 2)))",
        "(drop (call $listen (call $mint (i32.const 0)) (i32.const 512) (i32.const 1)))",
    ];
    for call in calls { assert!(raw(module(address, call), caps(address)).is_err(), "{call}"); }
    let listen = format!("(drop (call $listen (call $mint (i32.const 0)) (i32.const 0) (i32.const {})))", address.len());
    assert!(raw(module(address, &listen), Capabilities::default()).is_err());
    let mut connect_only = caps(address);
    connect_only.net_listen = false;
    connect_only.net_connect = true;
    assert!(raw(module(address, &listen), connect_only).is_err(), "Connect cannot bind");
    // The converse, so neither verb can stand in for the other (SEC-097). The
    // canonical adapters are registered unconditionally, so the right is
    // checked where the token is authenticated rather than at registration.
    let connect = format!(
        "(drop (call $connect (call $mint (i32.const 0)) (i32.const 0) (i32.const {})))",
        address.len()
    );
    let mut listen_only = caps(address);
    listen_only.net_listen = true;
    listen_only.net_connect = false;
    let refusal = raw(connect_module(address, &connect), listen_only)
        .expect_err("Listen cannot dial");
    assert!(
        refusal.contains("Connect right"),
        "the refusal must name the missing verb right, not an incidental dial failure: {refusal}"
    );
    assert!(raw(module(address, &listen), caps("192.0.2.1:0")).is_err(), "grant cannot escape its address");
    let mut denied = caps(address);
    denied.net_allow = Some(vec![address.into(), format!("!{address}")]);
    assert!(raw(module(address, &listen), denied).is_err(), "deny floor survives an allow entry");
}

#[test]
fn canonical_net_listen_registration_does_not_mint_authority() {
    // Canonical Net imports are always linkable so a public, unreachable helper
    // cannot prevent its no-Net entry point from instantiating.  Calling the
    // adapters (including the legacy-name accept/close ABI entries) with no
    // host-minted token must still fail before any network action.
    let cases = [
        (
            "net_listen_at",
            "(param externref i32 i32) (result externref)",
            "(drop (call $host (ref.null extern) (i32.const 0) (i32.const 0)))",
            "Net externref is null",
        ),
        (
            "net_accept",
            "(param externref) (result externref)",
            "(drop (call $host (ref.null extern)))",
            "Listener externref is null",
        ),
        (
            "net_close",
            "(param externref)",
            "(call $host (ref.null extern))",
            "Socket externref is null",
        ),
        (
            "serve_pool",
            "(param externref)",
            "(call $host (ref.null extern))",
            "Listener externref is null",
        ),
    ];
    for (name, signature, call, expected) in cases {
        let wat = format!(r#"(module
          (import "witchy" "{name}" (func $host {signature}))
          (memory (export "memory") 1)
          (func (export "run") {call}))"#);
        let error = raw(wat, Capabilities::none()).expect_err("null Net token must be rejected");
        assert!(error.contains(expected), "{name}: unexpected denial: {error}");
    }
}

#[test]
fn host_net_receive_rejects_short_invalid_stale_and_cross_socket_fills() {
    let cases = [
        ("(drop (call $fill (local.get $sock) (i32.const 64) (i32.const 4)))", 1, "capacity is too small"),
        ("(drop (call $fill (local.get $sock) (i32.const -1) (i32.const 5)))", 1, "negative host out slice"),
        ("(drop (call $fill (local.get $sock) (i32.const 65534) (i32.const 5)))", 1, "outside guest memory"),
        ("(drop (call $line (local.get $sock)))", 1, "consume its previous result"),
        ("(drop (call $fill (local.get $sock) (i32.const 64) (i32.const 5))) (drop (call $fill (local.get $sock) (i32.const 64) (i32.const 5)))", 1, "no pending Net receive"),
        ("(local.set $other (call $accept (local.get $listener))) (drop (call $fill (local.get $other) (i32.const 64) (i32.const 5)))", 2, "no pending Net receive"),
    ];
    for (action, connections, expected) in cases {
        let address = fixture_address();
        let body = format!("{} (drop (call $line (local.get $sock))) {action}", accept(&address));
        let wat = module(&address, &body);
        let grants = caps(&address);
        let error = exchange(address, b"probe\n", connections, move || raw(wat, grants)).unwrap_err();
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

fn server_source(address: &str, parameters: &str) -> String {
    // The existing finite-request server API, with the same handler API as
    // serve_hello. No replacement parser, router, stdlib or scheduler.
    format!(r#"
from http import Request, Response
from server import Router
fn home(req: Request) -> Response:
    server.text(200, "hello, witchy")
fn main({parameters}):
    let app = server.router().get("/", home)
    server.serve_n(net, "{address}", app, 1)
"#)
}

#[test]
fn canonical_net_server_router_serves_one_real_request() {
    let address = fixture_address();
    let source = server_source(&address, "net: Net");
    let checked = witchy::resolve_std_only_checked(&source).expect("checked existing server API");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("Net server must cross canonical validation and encoding without fallback");
    let grants = caps(&address);
    let response = exchange(address, b"GET / HTTP/1.1\r\nHost: fixture\r\n\r\n", 1, move || {
        let mut runtime = Runtime::batch().map_err(|error| error.to_string())?;
        let mut vm = runtime.spawn_admitted(&artifact, grants, 256).map_err(|error| error.to_string())?;
        vm.run().map_err(|error| format!("{error:#}"))?;
        assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
        Ok(())
    }).expect("admitted server executes one real request");
    assert_eq!(response, "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\nContent-Length: 13\r\nConnection: close\r\n\r\nhello, witchy");
}

#[test]
fn canonical_net_missing_root_grants_reject_even_without_imports() {
    let checked = witchy::resolve_std_only_checked("fn main(first: Net, second: Net):\n    ()\n").unwrap();
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("Net entry admits");
    assert_eq!(artifact.entry().unwrap().capabilities(), &[witchy_wir::CanonicalEntryCapability::Net; 2]);
    assert_eq!(witchy_wir::canonical_entry_from_wasm(artifact.wasm()).unwrap().as_ref(), artifact.entry());
    for (grants, missing) in [(Capabilities::default(), 0), (caps("127.0.0.1:0"), 1)] {
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact, grants, 16).unwrap();
        let error = vm.run().unwrap_err().to_string();
        assert!(error.contains(&format!("Net capability for parameter {missing} that was not granted")), "{error}");
    }
    let mut runtime = Runtime::batch().unwrap();
    let mut grants = caps("127.0.0.1:0");
    grants.net_grants.push(vec!["127.0.0.1:1".into()]);
    runtime.spawn_admitted(&artifact, grants, 16).unwrap().run().unwrap();

    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let entry = artifact.entry().unwrap();
    let function = witchy_wir::canonical_safewir::FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    use witchy_wir::canonical_interpreter::{interpret_program, interpret_entry_program, InterpreterConfig, InterpreterValue};
    let tokens = || entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    assert!(interpret_program(&validated, function, tokens(), InterpreterConfig::default()).is_err());
    assert!(interpret_entry_program(&validated, function, tokens(), &[], InterpreterConfig::default()).is_err());
    let mut config = InterpreterConfig::default();
    config.fixture.net_grants = 2;
    interpret_program(&validated, function, tokens(), config.clone()).unwrap();
    interpret_entry_program(&validated, function, tokens(), &[], config).unwrap();
}

#[test]
fn canonical_net_oracle_entry_roles_require_validated_signature() {
    assert_oracle_entry_roles(
        "fn main(console: Console, net: Net):\n    console.print(\"ran\")\n",
        &[0, 4],
    );
}

#[test]
fn canonical_net_oracle_entry_roles_preserve_interleaved_ordinals() {
    assert_oracle_entry_roles(
        "fn main(first: Net, console: Console, second: Net[Listen, Tcp]):\n    console.print(\"ran\")\n",
        &[4, 0, 4],
    );
}

fn assert_oracle_entry_roles(source: &str, expected_kinds: &[u32]) {
    use witchy_wir::canonical_interpreter::{
        interpret_entry_program, interpret_program, InterpretError, InterpreterConfig, InterpreterValue,
    };
    let checked = witchy::resolve_std_only_checked(source).unwrap();
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("oracle entry role fixture admits");
    let entry = artifact.entry().unwrap();
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let tokens: Vec<_> = expected_kinds.iter().copied().map(InterpreterValue::capability).collect();
    assert_eq!(entry.capabilities().iter().copied().map(InterpreterValue::entry_capability)
        .collect::<Vec<_>>(), tokens);
    let required_grants = expected_kinds.iter().filter(|kind| **kind == 4).count();
    for launch in [false, true] {
        let run = |values: Vec<InterpreterValue>, grants: usize| {
            let mut config = InterpreterConfig::default();
            config.fixture.net_grants = grants;
            if launch {
                interpret_entry_program(&validated, function, values, &[], config)
            } else {
                interpret_program(&validated, function, values, config)
            }
        };
        // The original bypass: fake Console tokens must not erase the
        // signature's Net requirement, even though the Net is unused.
        for grants in 0..required_grants {
            assert_eq!(run(vec![InterpreterValue::capability(0); tokens.len()], grants),
                Err(InterpretError::ExplicitTrap), "launch={launch}, grants={grants}");
            assert_eq!(run(tokens.clone(), grants), Err(InterpretError::ExplicitTrap));
        }
        // Supplying enough grants cannot make a token of the wrong kind bind
        // to any parameter, including the non-Net role between Net ordinals.
        for (ordinal, expected) in expected_kinds.iter().enumerate() {
            for wrong_kind in (0..=7).filter(|kind| kind != expected) {
                let mut wrong = tokens.clone();
                wrong[ordinal] = InterpreterValue::capability(wrong_kind);
                assert_eq!(run(wrong, required_grants),
                    Err(InterpretError::TypeMismatch("capability parameter kind")),
                    "launch={launch}, ordinal={ordinal}, kind={wrong_kind}");
            }
            for fake in [InterpreterValue::Int(i64::from(*expected)),
                InterpreterValue::NullableCapability(None)] {
                let mut wrong = tokens.clone();
                wrong[ordinal] = fake;
                assert_eq!(run(wrong, required_grants),
                    Err(InterpretError::TypeMismatch("capability parameter kind")));
            }
            let mut missing = tokens.clone();
            missing.remove(ordinal);
            assert_eq!(run(missing, required_grants), Err(InterpretError::ArityMismatch {
                expected: tokens.len(), actual: tokens.len() - 1,
            }));
        }
        let mut extra = tokens.clone();
        extra.push(InterpreterValue::capability(4));
        assert_eq!(run(extra, required_grants), Err(InterpretError::ArityMismatch {
            expected: tokens.len(), actual: tokens.len() + 1,
        }));
        let mut misordered = tokens.clone();
        misordered.swap(0, 1);
        assert_eq!(run(misordered, required_grants),
            Err(InterpretError::TypeMismatch("capability parameter kind")));
        let result = run(tokens.clone(), required_grants).unwrap();
        assert_eq!(result.output, ["ran"]);
        assert_eq!(result.leaked_objects, 0);
    }
}

#[test]
fn canonical_net_grants_bind_by_supplied_ordinal_and_wrong_grant_rejects() {
    let address = fixture_address();
    let source = socket_source(&address, "first: Net, net: Net[Listen, Tcp]");
    let checked = witchy::resolve_std_only_checked(&source).unwrap();
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("ordinal Net server admits");
    // A permitted address in the FIRST grant cannot authorize the second Net.
    let mut wrong = caps(&address);
    wrong.net_grants.push(vec!["192.0.2.1:1".into()]);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, wrong, 256).unwrap();
    let error = format!("{:#}", vm.run().unwrap_err());
    assert!(error.contains("is not permitted by this Net capability"), "{error}");
    let mut correct = caps("192.0.2.1:1");
    correct.net_grants.push(vec![address.clone()]);
    let response = exchange(address, b"probe\nhello", 1, move || {
        let mut runtime = Runtime::batch().map_err(|error| error.to_string())?;
        let mut vm = runtime.spawn_admitted(&artifact, correct, 256).map_err(|error| error.to_string())?;
        vm.run().map_err(|error| format!("{error:#}"))?;
        assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
        Ok(())
    }).unwrap();
    assert_eq!(response, "probehello");
}

fn socket_source(address: &str, parameters: &str) -> String {
    format!(r#"
fn listen(net: Net[Listen, Tcp], address: String) -> Listener:
    net.listen(address)
fn close(sock: Socket):
    sock.close()
fn main({parameters}):
    let listener = listen(net, "{address}")
    let sock = listener.accept()
    let line = sock.recv_line()
    let body = sock.recv_bytes(5)
    sock.send_bytes(line + body)
    close(sock)
"#)
}

#[test]
fn host_net_recv_all_authentication_and_staging_boundaries() {
    for token in ["(ref.null extern)", "(call $mint (i32.const 0))"] {
        let error = raw(module("127.0.0.1:0", &format!("(drop (call $all {token}))")), caps("127.0.0.1:0")).unwrap_err();
        assert!(error.contains("Socket"), "{error}");
    }
    let cases = [
        ("(drop (call $fill (local.get $sock) (i32.const 64) (i32.const 4)))", 1, "capacity is too small"),
        ("(drop (call $fill (local.get $sock) (i32.const -1) (i32.const 5)))", 1, "negative host out slice"),
        ("(drop (call $fill (local.get $sock) (i32.const 65534) (i32.const 5)))", 1, "outside guest memory"),
        ("(drop (call $all (local.get $sock)))", 1, "consume its previous result"),
        ("(drop (call $fill (local.get $sock) (i32.const 64) (i32.const 5))) (drop (call $fill (local.get $sock) (i32.const 64) (i32.const 5)))", 1, "no pending Net receive"),
        ("(local.set $other (call $accept (local.get $listener))) (drop (call $fill (local.get $other) (i32.const 64) (i32.const 5)))", 2, "no pending Net receive"),
    ];
    for (action, connections, expected) in cases {
        let address = fixture_address();
        let wat = module(&address, &format!("{} (drop (call $all (local.get $sock))) {action}", accept(&address)));
        let grants = caps(&address);
        let error = exchange(address, b"probe", connections, move || raw(wat, grants)).unwrap_err();
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

#[test]
fn host_net_recv_all_fills_only_initialized_bytes_and_consumes_eof() {
    let address = fixture_address();
    let body = format!(r#"{}
      (if (i32.ne (call $all (local.get $sock)) (i32.const 8)) (then unreachable))
      (i32.store8 (i32.const 63) (i32.const 33))
      (i32.store8 (i32.const 72) (i32.const 33))
      (if (i32.ne (call $fill (local.get $sock) (i32.const 64) (i32.const 8)) (i32.const 8)) (then unreachable))
      (if (i32.ne (i32.load8_u (i32.const 63)) (i32.const 33)) (then unreachable))
      (if (i32.ne (i32.load8_u (i32.const 72)) (i32.const 33)) (then unreachable))
      (call $send (local.get $sock) (i32.const 64) (i32.const 8))
      (if (i32.ne (call $all (local.get $sock)) (i32.const 0)) (then unreachable))
      (drop (call $fill (local.get $sock) (i32.const 65536) (i32.const 0)))
      (call $close (local.get $sock))"#, accept(&address));
    let wat = module(&address, &body);
    let grants = caps(&address);
    assert_eq!(exchange(address, b"a\xf0\x9f\xa6\x80\xff", 1, move || raw(wat, grants)).unwrap(), "a\u{1f980}\u{fffd}");
}

#[test]
fn canonical_net_recv_all_fetch_real_socket_and_explicit_oracle_policy() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let source = format!(r#"
fn fetch(net: Net[Connect, Tcp], addr: String) -> String:
    net.connect(addr).recv_all()
fn main(console: Console, net: Net[Connect, Tcp]):
    console.print(fetch(net, "{address}"))
"#);
    let checked = witchy::resolve_std_only_checked(&source).unwrap();
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("recv_all must cross canonical admission without fallback");
    // Real network I/O has no deterministic canonical interpreter adapter.
    // Keep its refusal explicit; wire bytes and exact output are the oracle.
    // Convergence requires an authenticated socket fixture adapter before this
    // surface can claim canonical interpreter/Wasm parity.
    use witchy_wir::canonical_interpreter::{interpret_program, InterpretError, InterpreterConfig, InterpreterValue};
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let entry = artifact.entry().unwrap();
    let function = witchy_wir::canonical_safewir::FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let mut config = InterpreterConfig::default();
    config.fixture.net_grants = 1;
    assert_eq!(interpret_program(&validated, function, entry.capabilities().iter().copied()
        .map(InterpreterValue::entry_capability).collect(), config),
        Err(InterpretError::UnsupportedOperation("HostCall import")), "real network cannot silently return fixture data");

    // A successfully checked Connect signature cannot supply a missing or
    // mismatched host grant. Refusals happen before touching the fixture.
    for mut grants in [Capabilities::default(), caps("192.0.2.1:1")] {
        grants.print = true;
        grants.net_connect = true;
        grants.net_listen = false;
        // Without any Net grant the canonical entry has no token to mint, so
        // `main` is refused before it runs; with an allowlist that excludes the
        // endpoint the program runs and the dial itself is denied.
        let expected = if grants.net_allow.is_none() { "Net capability" } else { "not permitted" };
        let mut runtime = Runtime::batch().unwrap();
        let error = match runtime.spawn_admitted(&artifact, grants, 256) {
            Ok(mut vm) => vm.run().expect_err("recv_all cannot confer connect authority"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains(expected), "{expected}: {error:#}");
    }

    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                    socket.write_all(b"a\xf0\x9f").unwrap();
                    socket.write_all(b"\xa6\x80\xff\xe2").unwrap();
                    socket.shutdown(std::net::Shutdown::Write).unwrap();
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                    std::thread::sleep(Duration::from_millis(5)),
                Err(error) => panic!("fixture accept failed: {error}"),
            }
        }
    });
    let mut grants = caps(&address);
    grants.print = true;
    grants.net_connect = true;
    grants.net_listen = false;
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, grants, 256).unwrap();
    let result = vm.run();
    server.join().unwrap();
    result.unwrap();
    assert_eq!(vm.output(), ["a\u{1f980}\u{fffd}\u{fffd}"]);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}

#[test]
fn canonical_net_recv_all_unchanged_capability_rights_example_admits() {
    let source = include_str!("../../examples/capability_rights/src/capability_rights.witchy");
    let checked = witchy::resolve_std_only_checked(source).unwrap();
    witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("unchanged capability_rights example must admit recv_all");
}

#[test]
fn host_net_connect_slice_rejects_forged_authority_and_invalid_addresses() {
    let address = "127.0.0.1:0";
    let call = |token: &str, pointer: i32, length: i32| format!(
        "(drop (call $connect {token} (i32.const {pointer}) (i32.const {length})))");
    let module = |body: &str| module(address, body).replacen("(memory",
        "(import \"witchy\" \"net_connect_at\" (func $connect (param externref i32 i32) (result externref))) (memory", 1);
    let mut grants = caps(address);
    grants.net_connect = true;
    for (token, pointer, length, expected) in [
        ("(ref.null extern)", 0, 11, "Net externref is null"),
        ("(call $listen (call $mint (i32.const 0)) (i32.const 0) (i32.const 11))", 0, 11, "wrong host data"),
        ("(call $mint (i32.const 0))", -1, 11, "negative"),
        ("(call $mint (i32.const 0))", 0, -1, "negative"),
        ("(call $mint (i32.const 0))", 65535, 2, "out-of-bounds guest memory"),
        ("(call $mint (i32.const 0))", 512, 1, "UTF-8"),
    ] {
        let error = raw(module(&call(token, pointer, length)), grants.clone()).unwrap_err();
        assert!(error.contains(expected), "{expected}: {error}");
    }
    grants.net_connect = false;
    assert!(raw(module(&call("(call $mint (i32.const 0))", 0, 11)), grants).is_err(), "Listen does not confer Connect");
    assert!(witchy::resolve_std_only_checked(
        "fn main(net: Net[Listen, Tcp]):\n    let _ = net.connect(\"127.0.0.1:1\").recv_all()\n").is_err());
}
