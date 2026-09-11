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
