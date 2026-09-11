use super::*;

fn grant(connect: bool, listen: bool) -> OracleNetGrant {
    OracleNetGrant { connect, listen, addresses: ["fixture:80".into()].into_iter().collect() }
}

fn connected(events: Vec<OracleNetEvent>) -> (NetState, CapabilityToken) {
    let mut transcript = vec![OracleNetEvent::Connect { grant: 0, address: "fixture:80".into(), socket: 7 }];
    transcript.extend(events);
    let mut state = NetState::new(OracleNetFixture { grants: vec![grant(true, false)], transcript });
    let socket = state.open(&state.token(Resource::Grant(0)), "fixture:80", false).unwrap();
    (state, socket)
}

fn take(state: &mut NetState, socket: &CapabilityToken) -> Vec<u8> {
    let id = state.socket(socket).unwrap();
    state.sockets.get_mut(&id).unwrap().pending.take().unwrap()
}

#[test]
fn net_fixture_tokens_require_kind_issuer_resource_and_explicit_rights() {
    let (mut state, socket) = connected(vec![]);
    assert_eq!(state.socket(&CapabilityToken::root(SOCKET)), Err(refused(OracleNetError::Unauthenticated)));
    assert_eq!(state.socket(&CapabilityToken::root(NET)), Err(refused(OracleNetError::WrongKind)));
    let (other, _) = connected(vec![]);
    assert_eq!(other.socket(&socket), Err(refused(OracleNetError::Unauthenticated)));
    assert_eq!(state.socket(&state.token(Resource::Socket(999))), Err(refused(OracleNetError::Unauthenticated)));
    let mut wrong_kind = socket.clone();
    wrong_kind.kind = NET;
    assert_eq!(state.authenticate(&wrong_kind, NET), Err(refused(OracleNetError::Unauthenticated)));
    assert_eq!(state.open(&CapabilityToken::root(NET), "fixture:80", false), Err(refused(OracleNetError::Unauthenticated)));
    let root = state.token(Resource::Grant(0));
    assert_eq!(state.open(&root, "fixture:80", true), Err(refused(OracleNetError::RightsDenied)));
    assert_eq!(state.open(&root, "elsewhere:80", false), Err(refused(OracleNetError::AddressDenied)));
    assert_eq!(state.open(&root, "fixture:80", false), Err(refused(OracleNetError::TranscriptMismatch)));
}

#[test]
fn net_fixture_transcript_is_ordered_exact_and_cannot_reuse_socket_ids() {
    let (mut state, socket) = connected(vec![
        OracleNetEvent::Send { socket: 7, bytes: b"request".to_vec() },
        OracleNetEvent::Close { socket: 7 },
        OracleNetEvent::Close { socket: 7 },
        OracleNetEvent::Connect { grant: 0, address: "fixture:80".into(), socket: 7 },
    ]);
    assert_eq!(state.send(&socket, b"wrong"), Err(refused(OracleNetError::TranscriptMismatch)));
    assert_eq!(state.finish(), Err(refused(OracleNetError::IncompleteTranscript)));
    state.send(&socket, b"request").unwrap();
    state.close(&socket).unwrap();
    state.close(&socket).unwrap();
    assert_eq!(state.send(&socket, b"x"), Err(refused(OracleNetError::ClosedSocket)));
    assert_eq!(state.receive(&socket, Receive::All, RECEIVE_LIMIT).unwrap(), 0);
    assert_eq!(take(&mut state, &socket), b"");
    assert_eq!(state.open(&state.token(Resource::Grant(0)), "fixture:80", false), Err(refused(OracleNetError::DuplicateResource)));
}

#[test]
fn net_fixture_reads_preserve_raw_chunk_tails_and_decode_per_receive() {
    let transcript = vec![
        OracleNetEvent::Receive { socket: 7, bytes: b"line\r\nAB\xf0\x9f".to_vec() },
        OracleNetEvent::Receive { socket: 7, bytes: b"\xa6\x80\xff\xe2".to_vec() },
        OracleNetEvent::Eof { socket: 7 },
    ];
    let (mut state, socket) = connected(transcript);
    assert_eq!(state.receive(&socket, Receive::Line, RECEIVE_LIMIT).unwrap(), 5);
    assert_eq!(take(&mut state, &socket), b"line\r");
    assert_eq!(state.receive(&socket, Receive::Bytes(2), RECEIVE_LIMIT).unwrap(), 2);
    assert_eq!(take(&mut state, &socket), b"AB");
    assert_eq!(state.receive(&socket, Receive::All, 6).unwrap(), 10);
    assert_eq!(take(&mut state, &socket), "\u{1f980}\u{fffd}\u{fffd}".as_bytes());
    assert_eq!(state.receive(&socket, Receive::All, 6).unwrap(), 0);
    assert_eq!(take(&mut state, &socket), b"");
    state.finish().unwrap();

    let (mut split, socket) = connected(vec![
        OracleNetEvent::Receive { socket: 7, bytes: b"\xf0\x9f\xa6\x80".to_vec() }, OracleNetEvent::Eof { socket: 7 },
    ]);
    split.receive(&socket, Receive::Bytes(2), RECEIVE_LIMIT).unwrap();
    assert_eq!(take(&mut split, &socket), "\u{fffd}".as_bytes());
    split.receive(&socket, Receive::All, RECEIVE_LIMIT).unwrap();
    assert_eq!(take(&mut split, &socket), "\u{fffd}\u{fffd}".as_bytes());
}

#[test]
fn net_fixture_limits_timeout_error_and_zero_count_are_not_canned_success() {
    assert_eq!(RECEIVE_LIMIT, 64 << 20);
    for (mode, bytes, terminal, expected) in [
        (Receive::All, b"abc".as_slice(), OracleNetEvent::Eof { socket: 7 }, Ok(b"abc".as_slice())),
        (Receive::All, b"abcd", OracleNetEvent::Eof { socket: 7 }, Err(OracleNetError::ReceiveLimit)),
        (Receive::Line, b"ab\n", OracleNetEvent::Eof { socket: 7 }, Ok(b"ab")),
        (Receive::Line, b"abc", OracleNetEvent::Eof { socket: 7 }, Err(OracleNetError::ReceiveLimit)),
        (Receive::All, b"ab", OracleNetEvent::Timeout { socket: 7 }, Ok(b"ab")),
        (Receive::Line, b"ab", OracleNetEvent::Timeout { socket: 7 }, Ok(b"ab")),
        (Receive::Bytes(10), b"ab", OracleNetEvent::Timeout { socket: 7 }, Ok(b"ab")),
        (Receive::All, b"ab", OracleNetEvent::ReadError { socket: 7, message: "reset".into() }, Err(OracleNetError::ReadFailed("reset".into()))),
    ] {
        let (mut state, socket) = connected(vec![OracleNetEvent::Receive { socket: 7, bytes: bytes.to_vec() }, terminal]);
        assert_eq!(state.receive(&socket, Receive::Bytes(0), 3).unwrap(), 0);
        assert_eq!(take(&mut state, &socket), b"");
        match expected {
            Ok(bytes) => { state.receive(&socket, mode, 3).unwrap(); assert_eq!(take(&mut state, &socket), bytes); }
            Err(error) => {
                assert_eq!(state.receive(&socket, mode, 3), Err(refused(error)));
                assert!(state.sockets[&7].pending.is_none());
            }
        }
    }
    let (mut state, socket) = connected(vec![OracleNetEvent::Receive { socket: 8, bytes: b"wrong socket".to_vec() }]);
    assert_eq!(state.receive(&socket, Receive::All, 32), Err(refused(OracleNetError::TranscriptMismatch)));
    let (mut state, socket) = connected(vec![]);
    assert_eq!(state.receive(&socket, Receive::All, 32), Err(refused(OracleNetError::TranscriptMismatch)), "unscripted EOF is not success");
}

#[test]
fn net_fixture_fill_checks_all_bounds_before_writing_or_consuming_staging() {
    use crate::canonical_safewir::{AccessMode, CheckedSourceIdentity, HostBufferExtent, LoweredModuleParts, Revision,
        check_initial_with_test_witness, validate_final_for_test};
    use crate::layout::{LayoutInterner, LayoutSize};
    let mut layouts = LayoutInterner::new();
    let byte = layouts.intern_scalar(ScalarKind::RawByte).unwrap();
    let layout = layouts.intern_runtime_buffer(byte).unwrap();
    let LayoutSize::Dynamic { base: header_bytes, .. } = layouts.get(layout).unwrap().size() else { panic!("buffer layout"); };
    let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0), checked_source: CheckedSourceIdentity::from_digest([71; 32]),
        layouts: LayoutBundle::from_interner(&layouts, [layout]).unwrap(), signatures: vec![], imports: vec![],
        functions: vec![], proper_tail_sites: vec![],
    });
    let checked = validate_final_for_test(check_initial_with_test_witness(candidate).unwrap()).unwrap();
    let mut engine = Engine::new(checked.module(), InterpreterConfig::default());
    let (mut state, socket) = connected(vec![OracleNetEvent::Receive { socket: 7, bytes: b"probe".to_vec() }, OracleNetEvent::Eof { socket: 7 }]);
    state.receive(&socket, Receive::All, RECEIVE_LIMIT).unwrap();
    assert_eq!(state.receive(&socket, Receive::Line, RECEIVE_LIMIT), Err(refused(OracleNetError::PendingReceive)));
    state.sockets.insert(8, Socket::default());
    let other = state.token(Resource::Socket(8));
    engine.net = Some(state);
    let minted = engine.bind_net_entry(vec![InterpreterValue::capability(NET)]).unwrap();
    assert_eq!(engine.bind_net_entry(minted), Err(refused(OracleNetError::Unauthenticated)), "minted tokens cannot be resubmitted as root requests");
    let sentinel: BTreeMap<_, _> = (0..8).map(|index| (CellKey::Element(index), InterpreterValue::Size(33))).collect();
    engine.objects.insert(0, Object { layout, storage: RuntimeStorage::Allocation(AllocationClass::ReferenceCounted),
        extent: RuntimeExtent::Counted { header_bytes, count: 8, capacity: 8, stride_bytes: 1 }, references: 1,
        immutable: false, cells: sentinel.clone(), absent_owned_slots: BTreeSet::new() });
    let token_id = ValueId::new(0);
    let object = ValueId::new(1);
    let start = ValueId::new(2);
    let capacity = ValueId::new(3);
    let result = ValueId::new(4);
    let mut values = BTreeMap::from([(token_id, InterpreterValue::Capability(socket.clone())),
        (object, InterpreterValue::ObjectRef(0)), (start, InterpreterValue::Size(1)), (capacity, InterpreterValue::Size(4))]);
    let args = [HostCallArgument::Capability(token_id), HostCallArgument::Buffer {
        place: Place::projected(object, Projection::Slice { start, length: capacity }, ValueType::Ref(layout), AccessMode::Write),
        extent: HostBufferExtent::Slice,
    }];
    assert_eq!(engine.net_host_call("net_take_received_into", &[result], &args, &mut values), Err(refused(OracleNetError::OutputTooSmall)));
    values.insert(capacity, InterpreterValue::Size(8));
    assert!(matches!(engine.net_host_call("net_take_received_into", &[result], &args, &mut values), Err(InterpretError::IndexOutOfBounds { .. })));
    assert_eq!(engine.objects[&0].cells, sentinel);
    values.insert(capacity, InterpreterValue::Size(5));
    values.insert(token_id, InterpreterValue::Capability(other));
    assert_eq!(engine.net_host_call("net_take_received_into", &[result], &args, &mut values), Err(refused(OracleNetError::NoPendingReceive)));
    values.insert(token_id, InterpreterValue::Capability(CapabilityToken::root(SOCKET)));
    assert_eq!(engine.net_host_call("net_take_received_into", &[result], &args, &mut values), Err(refused(OracleNetError::Unauthenticated)));
    for forged in [InterpreterValue::Int(7), InterpreterValue::NullableCapability(None), InterpreterValue::ObjectRef(0)] {
        values.insert(token_id, forged);
        assert_eq!(engine.net_host_call("net_take_received_into", &[result], &args, &mut values), Err(refused(OracleNetError::Unauthenticated)));
    }
    values.insert(token_id, InterpreterValue::Capability(socket));
    engine.net_host_call("net_take_received_into", &[result], &args, &mut values).unwrap();
    assert_eq!(values[&result], InterpreterValue::Size(5));
    for (index, byte) in b"!probe!!".iter().enumerate() { assert_eq!(engine.objects[&0].cells[&CellKey::Element(index as u64)], InterpreterValue::Size(u64::from(*byte))); }
    assert_eq!(engine.net_host_call("net_take_received_into", &[result], &args, &mut values), Err(refused(OracleNetError::NoPendingReceive)));
    assert_eq!(engine.net_host_call("serve_pool", &[], &[], &mut values), Err(InterpretError::UnsupportedOperation("Net fixture host call")));
    assert_eq!(engine.net_host_call("not_a_net_import", &[], &[], &mut values), Err(InterpretError::UnsupportedOperation("Net fixture host call")));
}
