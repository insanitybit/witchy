//! Independent wire expectations adjudicate the scripted semantic oracle and
//! admitted native Wasm. Only the native test peer opens real sockets.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_entry_program, interpret_program, InterpretError,
    InterpreterConfig, InterpreterValue, OracleNetError, OracleNetEvent as Event, OracleNetFixture, OracleNetGrant};
use witchy_wir::canonical_safewir::FunctionId;

fn grant(address: &str, connect: bool, listen: bool) -> OracleNetGrant {
    OracleNetGrant { connect, listen, addresses: [address.to_owned()].into_iter().collect() }
}

fn config(grants: Vec<OracleNetGrant>, transcript: Vec<Event>) -> InterpreterConfig {
    let mut config = InterpreterConfig::default();
    config.fixture.net = Some(OracleNetFixture { grants, transcript });
    config
}

#[test]
fn canonical_net_fixture_matches_native_wire_and_independent_expected_output() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let source = format!(r#"
fn main(console: Console, net: Net[Connect, Tcp]):
    let socket = net.connect("{address}")
    socket.send_bytes("ping")
    console.print(socket.recv_line())
    console.print(socket.recv_bytes(2))
    console.print(socket.recv_all())
    console.print(socket.recv_all())
    socket.close()
"#);
    let transcript = vec![
        Event::Connect { grant: 0, address: address.clone(), socket: 7 },
        Event::Send { socket: 7, bytes: b"ping".to_vec() },
        Event::Receive { socket: 7, bytes: b"hi\nAB\xf0\x9f".to_vec() },
        Event::Receive { socket: 7, bytes: b"\xa6\x80\xff".to_vec() },
        Event::Eof { socket: 7 },
        Event::Close { socket: 7 },
    ];
    let expected = ["hi", "AB", "\u{1f980}\u{fffd}", ""];
    let checked = witchy::resolve_std_only_checked(&source).unwrap();
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("scripted socket source admits");
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    for launch in [false, true] {
        let values = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let fixture = config(vec![grant(&address, true, false)], transcript.clone());
        let result = if launch { interpret_entry_program(&validated, function, values, &[], fixture) }
            else { interpret_program(&validated, function, values, fixture) }.unwrap();
        assert_eq!(result.output, expected);
        assert_eq!(result.net_trace, transcript);
        assert_eq!(result.leaked_objects, 0);
    }
    // Start the real peer only AFTER both oracle executions have completed.
    listener.set_nonblocking(true).unwrap();
    let peer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                    let mut request = [0; 4];
                    socket.read_exact(&mut request).unwrap();
                    assert_eq!(&request, b"ping");
                    socket.write_all(b"hi\nAB\xf0\x9f").unwrap();
                    socket.write_all(b"\xa6\x80\xff").unwrap();
                    socket.shutdown(std::net::Shutdown::Write).unwrap();
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                    std::thread::sleep(Duration::from_millis(5)),
                Err(error) => panic!("native fixture accept failed: {error}"),
            }
        }
    });
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, quiet: true,
        net_allow: Some(vec![address]), net_connect: true, ..Default::default() }, 256).unwrap();
    let result = vm.run();
    peer.join().unwrap();
    result.unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}

#[test]
fn canonical_net_fixture_listen_accept_and_interleaved_root_ordinals() {
    let source = r#"
fn main(first: Net, console: Console, net: Net[Listen, Tcp]):
    let listener = net.listen("fixture:80")
    let socket = listener.accept()
    let request = socket.recv_line()
    socket.send_bytes(request)
    socket.close()
    console.print(request)
"#;
    let checked = witchy::resolve_std_only_checked(source).unwrap();
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("listener fixture admits");
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let transcript = vec![Event::Listen { grant: 1, address: "fixture:80".into(), listener: 3 },
        Event::Accept { listener: 3, socket: 7 }, Event::Receive { socket: 7, bytes: b"probe\n".to_vec() },
        Event::Send { socket: 7, bytes: b"probe".to_vec() }, Event::Close { socket: 7 }];
    let grants = vec![grant("wrong:80", true, false), grant("fixture:80", false, true)];
    for launch in [false, true] {
        let run = |fixture: InterpreterConfig, args: Vec<InterpreterValue>| if launch {
            interpret_entry_program(&validated, function, args, &[], fixture)
        } else { interpret_program(&validated, function, args, fixture) };
        let tokens = || entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let output = run(config(grants.clone(), transcript.clone()), tokens()).unwrap();
        assert_eq!(output.output, ["probe"]);
        assert_eq!(output.net_trace, transcript);
        assert_eq!(output.leaked_objects, 0);
        let mut wrong = grants.clone();
        wrong.swap(0, 1);
        assert_eq!(run(config(wrong, transcript.clone()), tokens()), Err(InterpretError::NetFixture(OracleNetError::RightsDenied)));
        assert_eq!(run(config(vec![grants[0].clone()], transcript.clone()), tokens()), Err(InterpretError::ExplicitTrap));
        let mut wrong_tokens: Vec<_> = tokens();
        wrong_tokens[2] = InterpreterValue::capability(6);
        assert_eq!(run(config(grants.clone(), transcript.clone()), wrong_tokens), Err(InterpretError::TypeMismatch("capability parameter kind")));
    }
}

#[test]
fn canonical_net_fixture_refuses_unscripted_mismatched_or_unfinished_calls() {
    let source = "fn main(net: Net[Connect, Tcp]):\n    let socket = net.connect(\"fixture:80\")\n    socket.send_bytes(\"ping\")\n    socket.close()\n";
    let checked = witchy::resolve_std_only_checked(source).unwrap();
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("negative fixture admits");
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let run = |config| interpret_program(&validated, function, vec![InterpreterValue::capability(4)], config);
    let transcript = vec![Event::Connect { grant: 0, address: "fixture:80".into(), socket: 7 },
        Event::Send { socket: 7, bytes: b"ping".to_vec() }, Event::Close { socket: 7 }];
    let mut absent = InterpreterConfig::default();
    absent.fixture.net_grants = 1;
    assert_eq!(run(absent), Err(InterpretError::UnsupportedOperation("HostCall import")));
    assert_eq!(run(config(vec![grant("fixture:80", false, true)], transcript.clone())), Err(InterpretError::NetFixture(OracleNetError::RightsDenied)));
    assert_eq!(run(config(vec![grant("elsewhere:80", true, false)], transcript.clone())), Err(InterpretError::NetFixture(OracleNetError::AddressDenied)));
    assert_eq!(run(config(vec![grant("fixture:80", true, false)], vec![])), Err(InterpretError::NetFixture(OracleNetError::TranscriptMismatch)));
    let mut wrong = transcript.clone();
    wrong[1] = Event::Send { socket: 7, bytes: b"other".to_vec() };
    assert_eq!(run(config(vec![grant("fixture:80", true, false)], wrong)), Err(InterpretError::NetFixture(OracleNetError::TranscriptMismatch)));
    let mut unfinished = transcript;
    unfinished.push(Event::Close { socket: 7 });
    assert_eq!(run(config(vec![grant("fixture:80", true, false)], unfinished)), Err(InterpretError::NetFixture(OracleNetError::IncompleteTranscript)));
}

mod half_close {
    use super::*;
    use std::net::{Shutdown, TcpStream};
    use witchy_runtime::net::Stream;

    fn accept(listener: &TcpListener) -> TcpStream {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((socket, _)) => {
                    socket.set_nonblocking(false).unwrap();
                    socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                    return socket;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
                    && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
                Err(error) => panic!("bounded half-close peer accept: {error}"),
            }
        }
    }

    #[test]
    fn close_shuts_both_halves_with_retained_handles_after_peer_eof() {
        for peer_eof in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let peer = std::thread::spawn(move || {
                let mut socket = accept(&listener);
                socket.write_all(b"ab").unwrap();
                if peer_eof { socket.shutdown(Shutdown::Write).unwrap(); }
                let mut acknowledgement = [0; 3];
                socket.read_exact(&mut acknowledgement).unwrap();
                assert_eq!(&acknowledgement, b"ack");
                assert_eq!(socket.read(&mut [0; 1]).unwrap(), 0,
                    "explicit close must send FIN without dropping client handles");
            });
            let mut socket = TcpStream::connect(address).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut retained_alias = socket.try_clone().unwrap();
            let mut received = Vec::new();
            if peer_eof {
                socket.read_to_end(&mut received).unwrap();
            } else {
                let mut bytes = [0; 2];
                socket.read_exact(&mut bytes).unwrap();
                received.extend(bytes);
            }
            assert_eq!(received, b"ab");
            socket.write_all(b"ack").unwrap();
            Stream::shutdown(&mut socket);
            Stream::shutdown(&mut socket);
            peer.join().unwrap();
            assert!(retained_alias.write_all(b"extra").is_err(), "aliases cannot send after close");
            assert_eq!(retained_alias.read(&mut [0; 1]).unwrap(), 0);
            drop(retained_alias);
            drop(socket);
        }
    }

    #[test]
    fn full_source_half_close_barrier_agrees_before_source_or_vm_teardown() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let source = format!(r#"
fn exchange(net: Net[Connect, Tcp], address: String) -> String:
    let socket = net.connect(address)
    let barrier = net.connect(address)
    socket.send_bytes("ping")
    let body = socket.recv_all()
    socket.send_bytes("ack")
    socket.close()
    let observed = barrier.recv_bytes(6)
    barrier.close()
    body + "|" + observed

fn main(console: Console, net: Net[Connect, Tcp]):
    console.print(exchange(net, "{address}"))
    console.print("done")
"#);
        let expected = ["ab|closed", "done"];
        let checked = witchy::resolve_std_only_checked(&source).unwrap();
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("half-close barrier source admits");
        let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
        let entry = artifact.entry().unwrap();
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let transcript = vec![
            Event::Connect { grant: 0, address: address.clone(), socket: 7 },
            Event::Connect { grant: 0, address: address.clone(), socket: 8 },
            Event::Send { socket: 7, bytes: b"ping".to_vec() },
            Event::Receive { socket: 7, bytes: b"ab".to_vec() },
            Event::Eof { socket: 7 },
            Event::Send { socket: 7, bytes: b"ack".to_vec() },
            Event::Close { socket: 7 },
            Event::Receive { socket: 8, bytes: b"closed".to_vec() },
            Event::Close { socket: 8 },
        ];
        for launch in [false, true] {
            let values = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
            let fixture = config(vec![grant(&address, true, false)], transcript.clone());
            let result = if launch { interpret_entry_program(&validated, function, values, &[], fixture) }
                else { interpret_program(&validated, function, values, fixture) }.unwrap();
            assert_eq!(result.output, expected);
            assert_eq!(result.net_trace, transcript);
            assert_eq!(result.leaked_objects, 0);
        }
        assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        let peer = std::thread::spawn(move || {
            let mut completed = Vec::new();
            for leg in ["source", "Wasm"] {
                let mut socket = accept(&listener);
                let mut barrier = accept(&listener);
                let mut request = [0; 4];
                socket.read_exact(&mut request).unwrap();
                assert_eq!(&request, b"ping");
                socket.write_all(b"ab").unwrap();
                socket.shutdown(Shutdown::Write).unwrap();
                let mut acknowledgement = [0; 3];
                socket.read_exact(&mut acknowledgement).unwrap();
                assert_eq!(&acknowledgement, b"ack");
                assert_eq!(socket.read(&mut [0; 1]).unwrap_or_else(|error|
                    panic!("{leg} explicit close failed before completion barrier: {error}")), 0);
                // The source remains inside exchange with both Socket values
                // alive until this acknowledgement. Interpreter teardown cannot
                // make a broken close appear to work; the VM also remains live.
                barrier.write_all(b"closed").unwrap();
                assert_eq!(barrier.read(&mut [0; 1]).unwrap(), 0);
                completed.push(leg);
            }
            assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            completed
        });
        let interpreted = witchy::interpreter::run_checked_module(&checked, ".", vec![address.clone()]);
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
            print: true, quiet: true, net_allow: Some(vec![address]), net_connect: true,
            ..Default::default()
        }, 256).unwrap();
        let compiled = vm.run();
        let completed = peer.join().unwrap();
        assert_eq!(interpreted.unwrap(), expected);
        compiled.unwrap();
        assert_eq!(vm.output(), expected);
        assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
        assert_eq!(completed, ["source", "Wasm"]);
    }
}

fn two_byte_peer(listener: TcpListener, run: impl FnOnce() -> Result<(), String>) {
    listener.set_nonblocking(true).unwrap();
    let peer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                    socket.write_all(b"ab").unwrap();
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                    std::thread::sleep(Duration::from_millis(5)),
                Err(error) => panic!("two-byte peer accept failed: {error}"),
            }
        }
    });
    let result = run();
    peer.join().unwrap();
    result.unwrap();
}

#[test]
fn canonical_net_fixture_partial_receive_close_matches_native() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let source = format!(r#"
fn main(console: Console, net: Net[Connect, Tcp]):
    let socket = net.connect("{address}")
    let first = socket.recv_bytes(1)
    socket.close()
    console.print(first)
"#);
    let checked = witchy::resolve_std_only_checked(&source).unwrap();
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("partial receive then close admits");
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let transcript = vec![Event::Connect { grant: 0, address: address.clone(), socket: 7 },
        Event::Receive { socket: 7, bytes: b"ab".to_vec() }, Event::Close { socket: 7 }];
    for launch in [false, true] {
        let values = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let fixture = config(vec![grant(&address, true, false)], transcript.clone());
        let output = if launch { interpret_entry_program(&validated, function, values, &[], fixture) }
            else { interpret_program(&validated, function, values, fixture) }.unwrap();
        assert_eq!(output.output, ["a"]);
        assert_eq!(output.net_trace, transcript, "discarded b is accounted once, not silently removed from the script");
        assert_eq!(output.leaked_objects, 0);
    }
    // The deterministic oracle has finished before the real peer starts.
    two_byte_peer(listener, || {
        let mut runtime = Runtime::batch().map_err(|error| error.to_string())?;
        let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, quiet: true,
            net_allow: Some(vec![address]), net_connect: true, ..Default::default() }, 256)
            .map_err(|error| error.to_string())?;
        vm.run().map_err(|error| format!("{error:#}"))?;
        assert_eq!(vm.output(), ["a"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
        Ok(())
    });
}

#[test]
fn canonical_net_fixture_post_close_reads_refuse_native_buffered_remainder() {
    for method in ["recv_bytes(1)", "recv_all()", "recv_line()"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let source = format!(r#"
fn main(console: Console, net: Net[Connect, Tcp]):
    let socket = net.connect("{address}")
    console.print(socket.recv_bytes(1))
    socket.close()
    console.print(socket.{method})
"#);
        let checked = witchy::resolve_std_only_checked(&source).unwrap();
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("native post-close buffered read admits");
        let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
        let entry = artifact.entry().unwrap();
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let transcript = vec![Event::Connect { grant: 0, address: address.clone(), socket: 7 },
            Event::Receive { socket: 7, bytes: b"ab".to_vec() }, Event::Close { socket: 7 }];
        for launch in [false, true] {
            let values = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
            let fixture = config(vec![grant(&address, true, false)], transcript.clone());
            let result = if launch { interpret_entry_program(&validated, function, values, &[], fixture) }
                else { interpret_program(&validated, function, values, fixture) };
            assert_eq!(result, Err(InterpretError::UnsupportedOperation("Net fixture post-close receive")),
                "{method}: explicit oracle limitation, never a successful empty result");
        }
        two_byte_peer(listener, || {
            let mut runtime = Runtime::batch().map_err(|error| error.to_string())?;
            let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, quiet: true,
                net_allow: Some(vec![address]), net_connect: true, ..Default::default() }, 256)
                .map_err(|error| error.to_string())?;
            vm.run().map_err(|error| format!("{error:#}"))?;
            assert_eq!(vm.output(), ["a", "b"], "{method}: native read-ahead survives local shutdown");
            assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
            Ok(())
        });
    }
}

#[test]
fn canonical_net_fixture_native_close_preserves_pending_fill() {
    // Raw host-boundary probe only: source receives normally fill before
    // returning. The oracle counterpart exercises close-before-fill in its
    // private bounded-buffer test, including retry and stale-fill refusals.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let wat = format!(r#"(module
      (import "witchy" "mint_net" (func $mint (param i32) (result externref)))
      (import "witchy" "net_connect_at" (func $connect (param externref i32 i32) (result externref)))
      (import "witchy" "net_recv_bytes_size" (func $bytes (param externref i64) (result i32)))
      (import "witchy" "net_take_received_into" (func $fill (param externref i32 i32) (result i32)))
      (import "witchy" "net_close" (func $close (param externref)))
      (memory (export "memory") 1)
      (data (i32.const 0) "{address}")
      (func (export "run") (local $socket externref)
        (local.set $socket (call $connect (call $mint (i32.const 0)) (i32.const 0) (i32.const {})))
        (if (i32.ne (call $bytes (local.get $socket) (i64.const 1)) (i32.const 1)) (then unreachable))
        (call $close (local.get $socket))
        (i32.store8 (i32.const 63) (i32.const 33))
        (i32.store8 (i32.const 65) (i32.const 33))
        (if (i32.ne (call $fill (local.get $socket) (i32.const 64) (i32.const 1)) (i32.const 1)) (then unreachable))
        (if (i32.ne (i32.load8_u (i32.const 63)) (i32.const 33)) (then unreachable))
        (if (i32.ne (i32.load8_u (i32.const 64)) (i32.const 97)) (then unreachable))
        (if (i32.ne (i32.load8_u (i32.const 65)) (i32.const 33)) (then unreachable))))"#, address.len());
    two_byte_peer(listener, || {
        let mut runtime = Runtime::batch().map_err(|error| error.to_string())?;
        let mut vm = runtime.spawn(&wat, Capabilities { quiet: true, net_allow: Some(vec![address]),
            net_connect: true, ..Default::default() }, 16).map_err(|error| error.to_string())?;
        vm.run().map_err(|error| format!("{error:#}"))
    });
}

mod source_program {
//! One whole source program, explicit grants, two real peers, and an independent
//! oracle transcript. Wire chunks make no claims about kernel read boundaries.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{
    interpret_entry_program, interpret_program, InterpretError, InterpreterConfig,
    InterpreterValue, OracleNetError, OracleNetEvent as Event, OracleNetFixture, OracleNetGrant,
};
use witchy_wir::canonical_safewir::FunctionId;

#[test]
fn full_source_net_program_agrees_with_admitted_wasm_and_independent_oracle() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("controlled loopback endpoint");
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let source = format!(r#"
fn exchange(net: Net[Connect, Tcp], address: String) -> String:
    let socket = net.connect(address)
    socket.send_bytes("ping")
    let head = socket.recv_bytes(4)
    let line = socket.recv_line()
    let tail = socket.recv_bytes(4)
    socket.send_bytes("ack")
    socket.close()
    head + "|" + line + "|" + tail

fn main(console: Console, net: Net[Connect, Tcp]):
    console.print(exchange(net, "{address}"))
    console.print("closed")
"#);
    let expected = ["pong|line\r|tail", "closed"];
    let checked = witchy::resolve_std_only_checked(&source).expect("checked full source");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("full Net program admits without fixture-specific source changes");
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let transcript = vec![
        Event::Connect { grant: 0, address: address.clone(), socket: 7 },
        Event::Send { socket: 7, bytes: b"ping".to_vec() },
        Event::Receive { socket: 7, bytes: b"pongline\r\ntail".to_vec() },
        Event::Send { socket: 7, bytes: b"ack".to_vec() },
        Event::Close { socket: 7 },
    ];
    let grant = OracleNetGrant {
        connect: true, listen: false,
        addresses: [address.clone()].into_iter().collect(),
    };
    let config = |grants| {
        let mut config = InterpreterConfig::default();
        config.fixture.net = Some(OracleNetFixture { grants, transcript: transcript.clone() });
        config
    };
    // Both canonical launch APIs authenticate the normal entry signature, not
    // capability names supplied by this test. Neither oracle run opens sockets.
    for launch in [false, true] {
        let tokens = || entry.capabilities().iter().copied()
            .map(InterpreterValue::entry_capability).collect::<Vec<_>>();
        let run = |values, fixture| if launch {
            interpret_entry_program(&validated, function, values, &[], fixture)
        } else {
            interpret_program(&validated, function, values, fixture)
        };
        let result = run(tokens(), config(vec![grant.clone()])).unwrap();
        assert_eq!(result.output, expected);
        assert_eq!(result.net_trace, transcript, "consume every event exactly once");
        assert_eq!(result.leaked_objects, 0);
        assert_eq!(run(tokens(), config(vec![])), Err(InterpretError::ExplicitTrap));
        let mut denied = grant.clone();
        denied.connect = false;
        assert_eq!(run(tokens(), config(vec![denied])),
            Err(InterpretError::NetFixture(OracleNetError::RightsDenied)));
        let mut wrong = tokens();
        wrong[1] = wrong[0].clone();
        assert_eq!(run(wrong, config(vec![grant.clone()])),
            Err(InterpretError::TypeMismatch("capability parameter kind")));
        let mut denied = grant.clone();
        denied.addresses.clear();
        assert!(run(tokens(), config(vec![denied])).is_err());
    }

    let mut runtime = Runtime::batch().unwrap();
    // Real launchers must refuse absent authority and an explicit allowlist
    // that excludes this endpoint, before producing output or connecting.
    for allowed in [Vec::new(), vec!["127.0.0.1:1".to_owned()]] {
        assert!(witchy::interpreter::run_checked_module(&checked, ".", allowed.clone()).is_err());
        let missing = allowed.is_empty();
        let spawned = runtime.spawn_admitted(&artifact, Capabilities {
            print: true, quiet: true, net_connect: true,
            net_allow: (!allowed.is_empty()).then_some(allowed), ..Default::default()
        }, 256);
        if missing {
            let error = spawned.err().expect("ungranted Net imports must not link").to_string();
            assert!(error.contains("witchy::net_"), "{error}");
        } else {
            let mut vm = spawned.unwrap();
            assert!(vm.run().is_err());
            assert!(vm.output().is_empty());
        }
    }
    assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock,
        "refused source/Wasm and all oracle launches must leave the peer untouched");

    let peer = std::thread::spawn(move || {
        let mut effects = Vec::new();
        // The listener stays bound: source and Wasm use the identical source
        // and endpoint sequentially, not independently guessed free ports.
        for leg in ["source", "Wasm"] {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
                    Err(error) => panic!("bounded peer accept: {error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            socket.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut request = [0; 4];
            socket.read_exact(&mut request).unwrap();
            assert_eq!(&request, b"ping");
            for chunk in [&b"po"[..], &b"ngli"[..], &b"ne\r\n"[..], &b"tail"[..]] {
                socket.write_all(chunk).unwrap();
            }
            // Keep the peer write half open: the fixed-length protocol needs
            // no peer EOF, and the client's explicit close must cause EOF here.
            let mut acknowledgement = [0; 3];
            socket.read_exact(&mut acknowledgement).unwrap();
            assert_eq!(&acknowledgement, b"ack");
            let mut extra = [0; 1];
            assert_eq!(socket.read(&mut extra).unwrap_or_else(|error|
                panic!("{leg} peer did not observe close: {error}")), 0, "close, with no extra sends");
            effects.push((request, acknowledgement));
        }
        assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        effects
    });
    // This is the production checked-module source launcher, not the builtin
    // test driver or a fixture-injected interpreter transport.
    let interpreted = witchy::interpreter::run_checked_module(&checked, ".", vec![address.clone()]);
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, net_allow: Some(vec![address]), net_connect: true,
        ..Default::default()
    }, 256).unwrap();
    let compiled = vm.run();
    eprintln!("full source result: {interpreted:?}; admitted Wasm result: {compiled:?}; output: {:?}", vm.output());
    let effects = peer.join().expect("source and Wasm peer completed");
    assert_eq!(interpreted.expect("full source execution"), expected);
    compiled.expect("admitted Wasm execution");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
    assert_eq!(effects, vec![(*b"ping", *b"ack"); 2]);
}

}
