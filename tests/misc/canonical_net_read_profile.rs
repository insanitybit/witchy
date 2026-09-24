//! The oracle has its own buffering implementation. This test transport only
//! supplies arrivals/completions to REAL std BufReader and source Net builtins.
use std::collections::VecDeque;
use std::io::{self, BufReader, Read, Write};
use std::sync::{Arc, Mutex};
use witchy::interpreter::net_fixture::{NetFixtureCall as Call, NetReceiveFixture};
use witchy_wir::canonical_interpreter::{interpret_entry_program, interpret_program,
    InterpretError, InterpreterConfig, InterpreterValue, OracleNetEvent as E, OracleNetFixture,
    OracleNetGrant, OracleNetRead as R, ORACLE_NET_BUFFER_CAPACITY as B};
use witchy_wir::canonical_safewir::FunctionId;

#[derive(Clone)]
struct ScriptStream(Arc<Mutex<Transport>>);
struct Transport {
    events: VecDeque<E>, wire: VecDeque<u8>, observed: Vec<E>, closed: bool,
    requests: Vec<(usize, usize)>, protocol_failure: bool,
}
impl ScriptStream {
    fn new(events: Vec<E>) -> Self {
        assert!(matches!(&events[..2], [E::Connect { socket: 7, .. }, E::Buffered { socket: 7 }]));
        Self(Arc::new(Mutex::new(Transport { observed: events[..2].to_vec(), events: events[2..].to_vec().into(),
            wire: VecDeque::new(), closed: false, requests: Vec::new(), protocol_failure: false })))
    }
    fn finish(&self, expected: &[E]) -> Vec<(usize, usize)> {
        let state = self.0.lock().unwrap();
        assert!(!state.protocol_failure, "transport protocol failure cannot pass as IO success");
        assert!(state.events.is_empty(), "unconsumed transport obligations: {:?}", state.events);
        assert_eq!(state.observed, expected);
        for &(requested, returned) in &state.requests {
            assert!(returned <= requested && returned <= B, "Data completion must fit ACTUAL native request");
        }
        state.requests.clone()
    }
}
impl Transport {
    fn mismatch(&mut self) -> io::Error {
        self.protocol_failure = true;
        io::Error::new(io::ErrorKind::InvalidInput, "fixture protocol mismatch")
    }
    fn arrivals(&self) -> (usize, usize) {
        let mut count = 0;
        let mut available = self.wire.len();
        for event in &self.events {
            match event {
                E::Arrive { socket: 7, bytes } if !self.closed => { count += 1; available += bytes.len(); }
                _ => break,
            }
        }
        (count, available)
    }
    fn pop(&mut self) { self.observed.push(self.events.pop_front().unwrap()); }
    fn ingest(&mut self, count: usize) {
        for _ in 0..count {
            if let E::Arrive { bytes, .. } = self.events.front().unwrap() { self.wire.extend(bytes); }
            self.pop();
        }
    }
}
impl Read for ScriptStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() { return Ok(0); }
        let mut state = self.0.lock().unwrap();
        let (arrivals, available) = state.arrivals();
        let outcome = match state.events.get(arrivals) {
            Some(E::StreamRead { socket: 7, outcome }) => outcome.clone(),
            _ => return Err(state.mismatch()),
        };
        if let R::Data(count) = outcome {
            if count == 0 || count > out.len() || count > B || count > available { return Err(state.mismatch()); }
        }
        state.ingest(arrivals);
        state.pop();
        match outcome {
            R::Data(count) => {
                state.requests.push((out.len(), count));
                for byte in &mut out[..count] { *byte = state.wire.pop_front().unwrap(); }
                Ok(count)
            }
            R::Eof => Ok(0),
            R::Timeout => Err(io::ErrorKind::TimedOut.into()),
            R::WouldBlock => Err(io::ErrorKind::WouldBlock.into()),
            R::Interrupted => Err(io::ErrorKind::Interrupted.into()),
            R::Error(message) => Err(io::Error::new(io::ErrorKind::ConnectionReset, message)),
        }
    }
}
impl Write for ScriptStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        if state.events.front() != Some(&E::Send { socket: 7, bytes: bytes.to_vec() }) { return Err(state.mismatch()); }
        state.pop();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}
impl witchy_runtime::net::Stream for ScriptStream {
    fn shutdown(&mut self) {
        let mut state = self.0.lock().unwrap();
        let (arrivals, available) = state.arrivals();
        let keep = match state.events.get(arrivals) {
            Some(E::BufferedClose { socket: 7, keep_transport_prefix }) if *keep_transport_prefix <= available => *keep_transport_prefix,
            _ => { let _ = state.mismatch(); return; }
        };
        state.ingest(arrivals);
        state.wire.truncate(keep);
        state.closed = true;
        state.pop();
    }
}

// Native conformance uses std buffering and the existing native line helper,
// not the oracle implementation. Byte/all adapters follow the actual host API.
fn native(stream: ScriptStream, calls: &[Call]) -> io::Result<Vec<String>> {
    use witchy_runtime::net::Stream;
    let mut reader = BufReader::new(stream);
    assert_eq!(reader.capacity(), B, "the fixture profile must match this native toolchain");
    let mut output = Vec::new();
    for call in calls {
        let raw = match call {
            Call::Close => { reader.get_mut().shutdown(); continue; }
            Call::Send(text) => { reader.get_mut().write_all(text.as_bytes())?; continue; }
            Call::Line => witchy_runtime::net::read_line_capped(&mut reader)?,
            Call::Bytes(count) => {
                let mut raw = Vec::new();
                let mut chunk = [0u8; 8192];
                while raw.len() < (*count).max(0) as usize {
                    let amount = ((*count).max(0) as usize - raw.len()).min(chunk.len());
                    match reader.read(&mut chunk[..amount]) {
                        Ok(0) => break,
                        Ok(count) => raw.extend_from_slice(&chunk[..count]),
                        Err(error) if witchy_runtime::net::is_read_timeout(&error) => break,
                        Err(error) => return Err(error),
                    }
                }
                raw
            }
            Call::All => {
                let mut raw = Vec::new();
                match reader.by_ref().take(witchy_runtime::net::MAX_RECV_BYTES + 1).read_to_end(&mut raw) {
                    Ok(_) => {},
                    Err(error) if witchy_runtime::net::is_read_timeout(&error) => {},
                    Err(error) => return Err(error),
                }
                assert!(raw.len() as u64 <= witchy_runtime::net::MAX_RECV_BYTES);
                raw
            }
        };
        let text = String::from_utf8_lossy(&raw);
        output.push(if matches!(call, Call::Line) { text.trim_end_matches('\n').to_string() } else { text.into_owned() });
    }
    Ok(output)
}

fn arrival(bytes: &[u8]) -> E { E::Arrive { socket: 7, bytes: bytes.to_vec() } }
fn read(outcome: R) -> E { E::StreamRead { socket: 7, outcome } }
fn tape(events: Vec<E>) -> Vec<E> {
    let mut tape = vec![E::Connect { grant: 0, address: "fixture:80".into(), socket: 7 }, E::Buffered { socket: 7 }];
    tape.extend(events);
    tape
}
fn source(calls: &[Call]) -> String {
    let mut source = "fn main(console: Console, net: Net[Connect, Tcp]):\n    let socket = net.connect(\"fixture:80\")\n".to_string();
    for call in calls {
        source.push_str(&match call {
            Call::Bytes(count) => format!("    console.print(socket.recv_bytes({count}))\n"),
            Call::Line => "    console.print(socket.recv_line())\n".into(),
            Call::All => "    console.print(socket.recv_all())\n".into(),
            Call::Close => "    socket.close()\n".into(),
            Call::Send(text) => format!("    socket.send_bytes({text:?})\n"),
        });
    }
    source
}
fn oracle(calls: &[Call], events: &[E], expected: Result<&[String], &str>) {
    let checked = witchy::resolve_std_only_checked(&source(calls)).unwrap();
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("buffered fixture program admits");
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    for launch in [false, true] {
        let mut config = InterpreterConfig::default();
        config.fixture.net = Some(OracleNetFixture {
            grants: vec![OracleNetGrant { connect: true, listen: false, addresses: ["fixture:80".into()].into_iter().collect() }],
            transcript: events.to_vec(),
        });
        let values = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let result = if launch { interpret_entry_program(&validated, function, values, &[], config) }
            else { interpret_program(&validated, function, values, config) };
        match expected {
            Ok(output) => {
                let run = result.unwrap();
                assert_eq!(run.output, output);
                assert_eq!(run.net_trace, events);
                assert_eq!(run.leaked_objects, 0);
            }
            Err(message) => assert!(matches!(result, Err(InterpretError::NetFixture(
                witchy_wir::canonical_interpreter::OracleNetError::ReadFailed(ref actual))) if actual == message)),
        }
    }
}
fn conforms(calls: Vec<Call>, events: Vec<E>, expected: Vec<String>) -> Vec<(usize, usize)> {
    let events = tape(events);
    let stream = ScriptStream::new(events.clone());
    assert_eq!(native(stream.clone(), &calls).unwrap(), expected);
    let requests = stream.finish(&events);
    let stream = ScriptStream::new(events.clone());
    let mut source = NetReceiveFixture::new(Box::new(stream.clone()));
    assert_eq!(source.buffer_capacity(), B);
    let mut output = Vec::new();
    for call in &calls { if let Some(text) = source.call(call.clone()).unwrap() { output.push(text); } }
    assert_eq!(output, expected);
    stream.finish(&events);
    oracle(&calls, &events, Ok(&expected));
    requests
}

#[test]
fn buffered_timeouts_and_real_io_errors_preserve_existing_source_semantics() {
    for terminal in [R::Timeout, R::WouldBlock, R::Eof] {
        for call in [Call::All, Call::Line, Call::Bytes(10)] {
            conforms(vec![call], vec![arrival(b"ab"), read(R::Data(1)), read(R::Data(1)), read(terminal.clone())], vec!["ab".into()]);
        }
    }
    conforms(vec![Call::All], vec![arrival(b"ab"), read(R::Data(1)), read(R::Interrupted), read(R::Data(1)), read(R::Eof)], vec!["ab".into()]);
    let events = tape(vec![arrival(b"ab"), read(R::Data(1)), read(R::Error("reset".into()))]);
    let stream = ScriptStream::new(events.clone());
    assert_eq!(native(stream.clone(), &[Call::All]).unwrap_err().kind(), io::ErrorKind::ConnectionReset);
    stream.finish(&events);
    let stream = ScriptStream::new(events.clone());
    let mut source = NetReceiveFixture::new(Box::new(stream.clone()));
    assert!(source.call(Call::All).unwrap_err().message.contains("reset"));
    stream.finish(&events);
    oracle(&[Call::All], &events, Err("reset"));
}
