use super::*;

fn connected(events: Vec<OracleNetEvent>) -> (NetState, CapabilityToken) {
    let mut transcript = vec![OracleNetEvent::Connect { grant: 0, address: "fixture:80".into(), socket: 7 }, OracleNetEvent::Buffered { socket: 7 }];
    transcript.extend(events);
    let mut state = NetState::new(OracleNetFixture { grants: vec![OracleNetGrant { connect: true, listen: false, addresses: ["fixture:80".into()].into_iter().collect() }], transcript });
    let socket = state.open(&state.token(Resource::Grant(0)), "fixture:80", false).unwrap();
    (state, socket)
}
fn arrive(bytes: &[u8]) -> OracleNetEvent { OracleNetEvent::Arrive { socket: 7, bytes: bytes.to_vec() } }
fn read(outcome: OracleNetRead) -> OracleNetEvent { OracleNetEvent::StreamRead { socket: 7, outcome } }
fn take(state: &mut NetState) -> Vec<u8> { state.sockets.get_mut(&7).unwrap().pending.take().unwrap() }

#[test]
fn buffered_protocol_refusal_rolls_back_but_io_error_consumes_read_bytes() {
    for barrier in [OracleNetEvent::Close { socket: 7 }, OracleNetEvent::Send { socket: 7, bytes: vec![] },
        OracleNetEvent::StreamRead { socket: 8, outcome: OracleNetRead::Data(1) },
        read(OracleNetRead::Data(0)), read(OracleNetRead::Data(8193)), read(OracleNetRead::Data(3))] {
        let (mut state, socket) = connected(vec![arrive(b"ab"), read(OracleNetRead::Data(1)), barrier]);
        let events = state.events.clone();
        let observed = state.observed.clone();
        assert_eq!(state.receive(&socket, Receive::Bytes(2), RECEIVE_LIMIT), Err(refused(OracleNetError::TranscriptMismatch)));
        assert_eq!(state.events, events);
        assert_eq!(state.observed, observed);
        assert!(state.sockets[&7].pending.is_none());
        let buffered = state.sockets[&7].buffered.as_ref().unwrap();
        assert!(buffered.transport.is_empty() && buffered.ahead.is_empty());
    }
    for outcome in [OracleNetRead::Error("reset".into()), OracleNetRead::Interrupted] {
        let (mut state, socket) = connected(vec![arrive(b"ab"), read(OracleNetRead::Data(1)), read(outcome)]);
        assert!(matches!(state.receive(&socket, Receive::Bytes(2), RECEIVE_LIMIT), Err(InterpretError::NetFixture(OracleNetError::ReadFailed(_)))));
        assert!(state.events.is_empty());
        assert_eq!(state.sockets[&7].buffered.as_ref().unwrap().transport, VecDeque::from(b"b".to_vec()));
        assert!(state.sockets[&7].pending.is_none());
    }
}

#[test]
fn buffered_close_retains_residency_and_staging_without_skipping_events() {
    let (mut state, socket) = connected(vec![arrive(b"abcd"), read(OracleNetRead::Data(2)),
        OracleNetEvent::BufferedClose { socket: 7, keep_transport_prefix: 1 }, read(OracleNetRead::Data(1)), read(OracleNetRead::Eof)]);
    state.receive(&socket, Receive::Bytes(1), RECEIVE_LIMIT).unwrap();
    state.close(&socket).unwrap();
    assert_eq!(state.receive(&socket, Receive::Line, RECEIVE_LIMIT), Err(refused(OracleNetError::PendingReceive)));
    assert_eq!(take(&mut state), b"a");
    state.receive(&socket, Receive::All, RECEIVE_LIMIT).unwrap();
    assert_eq!(take(&mut state), b"bc");
    state.finish().unwrap();
    for barrier in [OracleNetEvent::BufferedClose { socket: 8, keep_transport_prefix: 0 },
        OracleNetEvent::BufferedClose { socket: 7, keep_transport_prefix: 3 },
        OracleNetEvent::Send { socket: 7, bytes: vec![] }, read(OracleNetRead::Data(1)),
        OracleNetEvent::Arrive { socket: 8, bytes: vec![] }] {
        let (mut state, socket) = connected(vec![arrive(b"ab"), barrier]);
        let events = state.events.clone();
        assert_eq!(state.close(&socket), Err(refused(OracleNetError::TranscriptMismatch)));
        assert_eq!(state.events, events);
        assert!(!state.sockets[&7].closed);
    }
}

#[test]
fn buffered_decode_caps_timeouts_and_interrupted_all_are_independent() {
    for terminal in [OracleNetRead::Eof, OracleNetRead::Timeout, OracleNetRead::WouldBlock] {
        let (mut state, socket) = connected(vec![arrive(b"\xf0\x9f"), arrive(b"\xa6\x80\xff"),
            read(OracleNetRead::Data(3)), read(OracleNetRead::Interrupted), read(OracleNetRead::Data(2)), read(terminal)]);
        state.receive(&socket, Receive::All, 5).unwrap();
        assert_eq!(take(&mut state), "\u{1f980}\u{fffd}".as_bytes());
        state.finish().unwrap();
    }
    for (bytes, mode, result) in [(b"ab\n".as_slice(), Receive::Line, Ok(b"ab".as_slice())),
        (b"abc", Receive::Line, Err(OracleNetError::ReceiveLimit)),
        (b"abcd", Receive::All, Err(OracleNetError::ReceiveLimit))] {
        let (mut state, socket) = connected(vec![arrive(bytes), read(OracleNetRead::Data(bytes.len()))]);
        match result {
            Ok(expected) => { state.receive(&socket, mode, 3).unwrap(); assert_eq!(take(&mut state), expected); }
            Err(error) => { assert_eq!(state.receive(&socket, mode, 3), Err(refused(error))); assert!(state.sockets[&7].pending.is_none()); }
        }
        state.finish().unwrap();
    }
}
