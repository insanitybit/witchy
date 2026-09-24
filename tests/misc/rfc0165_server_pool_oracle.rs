//! Finite pool execution, not a canned server response or a new scheduler.
use witchy::codegen;
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterConfig,
    InterpreterValue, OracleFixture, OracleNetEvent as Event, OracleNetFixture,
    OracleNetGrant, OracleServerPoolFixture};
use witchy_wir::canonical_safewir::FunctionId;

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

