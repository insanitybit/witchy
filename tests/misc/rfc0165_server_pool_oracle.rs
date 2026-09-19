//! Finite pool execution, not a canned server response or a new scheduler.
use witchy::{codegen, runtime::{Capabilities, Runtime}};
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterConfig,
    InterpreterValue, OracleFixture, OracleNetEvent as Event, OracleNetFixture,
    OracleNetGrant, OracleServerPoolFixture};
use witchy_wir::canonical_safewir::FunctionId;

fn connection(socket: u64) -> Vec<Event> {
    vec![Event::Accept { listener: 1, socket },
        Event::Receive { socket, bytes: b"request\n".to_vec() },
        Event::Send { socket, bytes: b"copied:request".to_vec() },
        Event::Close { socket }]
}

#[test]
fn server_pool_oracle_rejects_unused_worker_scripts() {
    let checked = witchy::resolve_std_only_checked("fn main():\n    ()\n").unwrap();
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("ordinary empty entry admits");
    let function = FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).unwrap();
    for workers in [vec![], vec![connection(8)]] {
        let config = InterpreterConfig { fixture: OracleFixture {
            server_pool: Some(OracleServerPoolFixture { workers }), ..Default::default()
        }, ..Default::default() };
        assert_eq!(interpret_entry_program(&validated, function, vec![], &[], config),
            Err(witchy_wir::canonical_interpreter::InterpretError::NetFixture(
                witchy_wir::canonical_interpreter::OracleNetError::IncompleteTranscript)));
    }
}

#[test]
fn server_pool_oracle_refuses_scripted_self_connect_after_rendezvous() {
    let address = "127.0.0.1:12345";
    let checked = witchy::resolve_std_only_checked(&format!(r#"
fn main(net: Net):
    let listener = net.listen("{address}")
    listener.serve_pool()
    let socket = net.connect("{address}")
    let reply = socket.recv_line()
    if reply != "invented":
        fail("wrong response")
    socket.close()
"#)).unwrap();
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("existing Net operations admit");
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let scripted_reply = vec![Event::Connect { grant: 0, address: address.into(), socket: 7 },
        Event::Receive { socket: 7, bytes: b"invented\n".to_vec() }, Event::Close { socket: 7 }];
    for in_worker in [false, true] {
        let mut primary = vec![Event::Listen { grant: 0, address: address.into(), listener: 1 }];
        if !in_worker { primary.extend(scripted_reply.clone()); }
        let config = InterpreterConfig { fixture: OracleFixture {
            net: Some(OracleNetFixture { grants: vec![OracleNetGrant {
                listen: true, connect: true, addresses: [address.into()].into_iter().collect(),
            }], transcript: primary }),
            server_pool: Some(OracleServerPoolFixture {
                workers: if in_worker { vec![scripted_reply.clone()] } else { vec![] },
            }), ..Default::default()
        }, ..Default::default() };
        let capabilities = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        assert_eq!(interpret_entry_program(&validated, function, capabilities, &[], config),
            Err(witchy_wir::canonical_interpreter::InterpretError::UnsupportedOperation(
                "server pool outbound traffic")), "worker={in_worker}");
    }
    // The source is checked and encoded, but deliberately NOT executed as
    // native Wasm: its self-connect has no server able to produce this reply.
}

#[test]
fn server_pool_oracle_and_admitted_workers_execute_copied_capture_and_request() {
    use std::{io::{Read, Write}, net::{TcpListener, TcpStream}, time::{Duration, Instant}};
    let address = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let source = format!(r#"
fn main(console: Console, args: List(String), net: Net):
    let prefix = args[0]
    let reply = fn(message: String):
        console.print("served")
        prefix + message
    let listener = net.listen("{address}")
    listener.serve_pool()
    let socket = listener.accept()
    let request = socket.recv_line()
    socket.send_bytes(reply(request))
    socket.close()
"#);
    let checked = witchy::resolve_std_only_checked(&source).expect("source checks");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("pool admission and encoding");
    assert!(!artifact.owner_bridges().is_empty(), "captured string must use admitted owner recipes");
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let capabilities = || entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("independent graph validation");
    let count = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let mut primary = vec![Event::Listen { grant: 0, address: address.clone(), listener: 1 }];
    primary.extend(connection(7));
    let workers: Vec<_> = (1..count).map(|index| connection(7 + index as u64)).collect();
    let config = InterpreterConfig { max_steps: 10_000_000, fixture: OracleFixture {
        net: Some(OracleNetFixture { grants: vec![OracleNetGrant {
            connect: false, listen: true, addresses: [address.clone()].into_iter().collect(),
        }], transcript: primary.clone() }),
        server_pool: Some(OracleServerPoolFixture { workers: workers.clone() }),
        ..Default::default()
    }, ..Default::default() };
    let argv = vec!["copied:".to_owned()];
    let oracle = interpret_entry_program(&validated, function, capabilities(), &argv, config.clone()).expect("execute each worker body");
    assert_eq!(oracle.output, ["served"]);
    assert_eq!(oracle.leaked_objects, 0);
    assert_eq!(oracle.net_trace, primary);
    assert_eq!(oracle.server_workers.len(), count - 1);
    for (worker, transcript) in oracle.server_workers.iter().zip(workers) {
        assert_eq!(worker.output, ["served"]);
        assert_eq!(worker.net_trace, transcript);
        assert_eq!(worker.leaked_objects, 0);
    }
    let mut denied = config.clone();
    denied.fixture.net.as_mut().unwrap().grants[0].listen = false;
    assert!(interpret_entry_program(&validated, function, capabilities(), &argv, denied).is_err(), "Connect/absent Listen cannot start a pool");
    let mut incomplete = config;
    // A worker must execute its real send; absence of a scripted response is
    // not successful completion. Use one worker even on a single-core host.
    incomplete.fixture.server_pool = Some(OracleServerPoolFixture { workers: vec![vec![]] });
    assert!(interpret_entry_program(&validated, function, capabilities(), &argv, incomplete).is_err());

    let endpoint = address.clone();
    let client = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut responses = Vec::new();
        for _ in 0..count {
            let mut socket = loop {
                match TcpStream::connect(&endpoint) {
                    Ok(socket) => break socket,
                    Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                    Err(error) => panic!("server never listened: {error}"),
                }
            };
            socket.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
            socket.set_write_timeout(Some(Duration::from_secs(20))).unwrap();
            socket.write_all(b"request\n").unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).unwrap();
            responses.push(response);
        }
        responses
    });
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, args: argv, net_allow: Some(vec![address]), net_listen: true,
        ..Default::default()
    }, 256).unwrap();
    vm.run().expect("primary and fresh workers invoke admitted entry");
    assert_eq!(client.join().unwrap(), vec!["copied:request"; count]);
    assert_eq!(vm.output(), oracle.output);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
    // Native detached child heaps are not exposed by Vm's primary heap audit.
    // Zero child-owned leaks above are independent oracle evidence, not a
    // fabricated native per-worker heap measurement.
}
