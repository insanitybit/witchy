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
    assert_eq!(state.receive(&socket, Receive::All, RECEIVE_LIMIT), Err(InterpretError::UnsupportedOperation("Net fixture post-close receive")));
    assert_eq!(state.receive(&socket, Receive::Bytes(0), RECEIVE_LIMIT).unwrap(), 0);
    assert_eq!(take(&mut state, &socket), b"");
    assert_eq!(state.open(&state.token(Resource::Grant(0)), "fixture:80", false), Err(refused(OracleNetError::DuplicateResource)));
}

#[test]
fn net_fixture_close_cannot_skip_obligations_or_mutate_a_refused_transcript() {
    for barrier in [
        Some(OracleNetEvent::Send { socket: 7, bytes: b"required".to_vec() }),
        Some(OracleNetEvent::Send { socket: 8, bytes: b"other".to_vec() }),
        Some(OracleNetEvent::Receive { socket: 8, bytes: b"other".to_vec() }),
        Some(OracleNetEvent::Close { socket: 8 }),
        Some(OracleNetEvent::Accept { listener: 3, socket: 8 }),
        Some(OracleNetEvent::Eof { socket: 7 }),
        Some(OracleNetEvent::Timeout { socket: 7 }),
        Some(OracleNetEvent::ReadError { socket: 7, message: "reset".into() }),
        None,
    ] {
        let mut events = vec![
            OracleNetEvent::Receive { socket: 7, bytes: b"ab".to_vec() },
            OracleNetEvent::Receive { socket: 7, bytes: b"cd".to_vec() },
        ];
        if let Some(barrier) = barrier {
            events.push(barrier);
            events.push(OracleNetEvent::Close { socket: 7 });
        }
        let (mut state, socket) = connected(events);
        state.receive(&socket, Receive::Bytes(1), RECEIVE_LIMIT).unwrap();
        let before_events = state.events.clone();
        let before_observed = state.observed.clone();
        assert_eq!(state.close(&socket), Err(refused(OracleNetError::TranscriptMismatch)));
        assert_eq!(state.events, before_events);
        assert_eq!(state.observed, before_observed);
        assert_eq!(state.chunk_offset, 1);
        assert!(!state.sockets[&7].closed);
        assert!(!state.sockets[&7].eof);
        assert_eq!(state.sockets[&7].pending.as_deref(), Some(b"a".as_slice()));
        assert_eq!(state.finish(), Err(refused(OracleNetError::IncompleteTranscript)));
        assert_eq!(take(&mut state, &socket), b"a");
        state.receive(&socket, Receive::Bytes(1), RECEIVE_LIMIT).unwrap();
        assert_eq!(take(&mut state, &socket), b"b", "refused close cannot discard unread bytes");
    }
    let (mut state, socket) = connected(vec![
        OracleNetEvent::Receive { socket: 7, bytes: b"ab".to_vec() },
        OracleNetEvent::Close { socket: 7 },
        OracleNetEvent::Close { socket: 8 },
    ]);
    let before = state.events.clone();
    assert_eq!(state.close(&CapabilityToken::root(SOCKET)), Err(refused(OracleNetError::Unauthenticated)));
    assert_eq!(state.close(&CapabilityToken::root(NET)), Err(refused(OracleNetError::WrongKind)));
    let (_, foreign) = connected(vec![]);
    assert_eq!(state.close(&foreign), Err(refused(OracleNetError::Unauthenticated)));
    assert_eq!(state.events, before);
    state.close(&socket).unwrap();
    assert_eq!(state.events, VecDeque::from([OracleNetEvent::Close { socket: 8 }]));
    assert_eq!(state.finish(), Err(refused(OracleNetError::IncompleteTranscript)), "matching close cannot consume later events");
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
    let (mut state, socket) = connected(vec![OracleNetEvent::Receive { socket: 7, bytes: b"probe-unread".to_vec() }, OracleNetEvent::Close { socket: 7 }]);
    state.receive(&socket, Receive::Bytes(5), RECEIVE_LIMIT).unwrap();
    state.close(&socket).unwrap();
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
