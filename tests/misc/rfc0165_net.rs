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
      (import "witchy" "net_take_received_into" (func $fill (param externref i32 i32) (result i32)))
      (import "witchy" "net_send_slice" (func $send (param externref i32 i32)))
      (import "witchy" "net_close" (func $close (param externref)))
      (memory (export "memory") 1)
      (data (i32.const 0) "{address}")
      (data (i32.const 512) "\ff")
      (func (export "run") (local $listener externref) (local $sock externref) (local $other externref)
        {body}))"#)
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
    assert!(raw(module(address, &listen), caps("192.0.2.1:0")).is_err(), "grant cannot escape its address");
    let mut denied = caps(address);
    denied.net_allow = Some(vec![address.into(), format!("!{address}")]);
    assert!(raw(module(address, &listen), denied).is_err(), "deny floor survives an allow entry");
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
    let checked = witchy::resolve_std_only_checked("fn main(first: Net, second: Net):\n    Nil\n").unwrap();
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
