//! Actual HTTP provider traffic is confined to the admitted Wasm leg. Both
//! canonical launch APIs run first against independent ordered wire fixtures.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Duration;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_entry_program, interpret_program,
    InterpreterConfig, InterpreterValue, OracleFetchCall, OracleFetchFixture,
    OracleFetchOutcome, OracleFetchSource, OracleNetFixture, OracleNetGrant};
use witchy_wir::canonical_safewir::FunctionId;

struct Peer {
    listener: Option<TcpListener>,
    address: String,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<Result<usize, String>>>,
}

impl Peer {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        Self { address: listener.local_addr().unwrap().to_string(), listener: Some(listener),
            stop: Arc::new(AtomicBool::new(false)), worker: None }
    }

    fn start(&mut self, rounds: usize) {
        let listener = self.listener.take().unwrap();
        let stop = self.stop.clone();
        let address = self.address.clone();
        self.worker = Some(std::thread::spawn(move || {
            let mut served = 0;
            for _ in 0..rounds {
                for (index, wire) in replies().into_iter().enumerate() {
                    let mut stream = loop {
                        if stop.load(Ordering::Acquire) { return Ok(served); }
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(2)),
                            Err(error) => return Err(error.to_string()),
                        }
                    };
                    stream.set_nonblocking(false).map_err(|error| error.to_string())?;
                    stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|error| error.to_string())?;
                    stream.set_write_timeout(Some(Duration::from_secs(5))).map_err(|error| error.to_string())?;
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        if request.len() >= 8192 { return Err("request header limit".into()); }
                        let mut byte = [0];
                        stream.read_exact(&mut byte).map_err(|error| error.to_string())?;
                        request.push(byte[0]);
                    }
                    let (method, path, body) = match index {
                        0 => ("GET", "/one", ""),
                        1 => ("POST", "/two", "posted"),
                        _ => ("POST", "/three", "posted"),
                    };
                    let length = if body.is_empty() { String::new() } else { format!("Content-Length: {}\r\n", body.len()) };
                    let expected = format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{length}\r\n");
                    if request != expected.as_bytes() { return Err(format!("request mismatch: {request:?}")); }
                    let mut received = vec![0; body.len()];
                    stream.read_exact(&mut received).map_err(|error| error.to_string())?;
                    if received != body.as_bytes() { return Err(format!("body mismatch: {received:?}")); }
                    stream.write_all(&wire).map_err(|error| error.to_string())?;
                    served += 1;
                }
            }
            Ok(served)
        }));
    }

    fn finish(&mut self, expected: usize) {
        self.stop.store(true, Ordering::Release);
        assert_eq!(self.worker.take().unwrap().join().unwrap().unwrap(), expected);
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

fn replies() -> Vec<Vec<u8>> {
    vec![b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nfirst\r\n0\r\n\r\n".to_vec(),
        b"HTTP/1.1 201 Created\r\nContent-Length: 5\r\n\r\ncaf\xc3\xa9".to_vec(),
        b"HTTP/1.1 202 Accepted\r\nContent-Length: 5\r\n\r\nthird".to_vec()]
}

fn fixture(origin: &str, derived: bool) -> InterpreterConfig {
    let calls = replies().into_iter().enumerate().map(|(index, wire)| OracleFetchCall {
        source: if derived { OracleFetchSource::Net(0) } else { OracleFetchSource::Direct(0) },
        method: if index == 0 { "GET" } else { "POST" }.into(),
        url: format!("{origin}/{}", ["one", "two", "three"][index]), headers: String::new(),
        body: if index == 0 { "" } else { "posted" }.into(), outcome: OracleFetchOutcome::Wire(wire),
    }).collect();
    let mut config = InterpreterConfig::default();
    config.fixture.fetch = Some(OracleFetchFixture { grants: if derived { Vec::new() } else { vec![vec![origin.into()]] }, calls });
    if derived {
        config.fixture.net = Some(OracleNetFixture { grants: vec![OracleNetGrant {
            connect: true, listen: false, addresses: [origin.strip_prefix("http://").unwrap().to_owned()].into_iter().collect(),
        }], transcript: Vec::new() });
    }
    config
}

fn three_legs(derived: bool) {
    let mut peer = Peer::new();
    let origin = format!("http://{}", peer.address);
    let (parameter, derive, requests) = if derived {
        ("net: Net[Connect, Tcp]", format!("let fetch = net.fetch(\"{origin}\")"), format!(
            "let first = library.registry_get(net, \"{origin}\", \"/one\")\n    let second = library.registry_post(net, \"{origin}\", \"/two\", \"posted\")"))
    } else {
        ("fetch: Fetch", String::new(), format!(
            "let first = http.get(client, \"{origin}/one\")\n    let second = http.post(client, \"{origin}/two\", \"posted\")"))
    };
    let source = format!(r#"import library
from http import Response
type Holder:
    client: Fetch
fn change(var url: String, console: Console) -> String:
    console.print("mutation")
    url = "changed"
    "posted"
fn main(console: Console, {parameter}):
    {derive}
    let holder = Holder(fetch)
    let captured = fn() -> Fetch: holder.client
    let client = captured()
    {requests}
    console.print("${{http.status(first)}}")
    console.print(http.body(first))
    console.print("${{http.status(second)}}")
    console.print(http.body(second))
    var url = "{origin}/three"
    let raw = client.send_raw("POST", url, "", change(url, console))
    console.print(raw)
    console.print(url)
    console.print(http.body(first))
"#);
    // Exact existing Grimoire wrapper bytes, not a rewritten registry shim.
    let library: &[u8] = if derived { include_bytes!("../../projects/grimoire/src/gm_core.witchy") } else { b"pub fn unused() -> Int:\n    1\n" };
    let checked = super::check_scoped_modules(source.as_bytes(), library);
    let expected = vec!["200".to_owned(), "first".into(), "201".into(), "caf\u{e9}".into(), "mutation".into(),
        "HTTP/1.1 202\r\nContent-Length: 5\r\n\r\nthird".into(), "changed".into(), "first".into()];
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(&checked, |validated, artifact| {
        let entry = artifact.entry().unwrap();
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        for with_argv in [false, true] {
            let args = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
            let config = fixture(&origin, derived);
            let run = if with_argv { interpret_entry_program(validated, function, args, &[], config) }
                else { interpret_program(validated, function, args, config) }.unwrap();
            assert_eq!(run.output, expected);
            assert_eq!(run.leaked_objects, 0);
            let args = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
            let mut incomplete = fixture(&origin, derived);
            let extra = incomplete.fixture.fetch.as_ref().unwrap().calls[0].clone();
            incomplete.fixture.fetch.as_mut().unwrap().calls.push(extra);
            let refused = if with_argv { interpret_entry_program(validated, function, args, &[], incomplete) }
                else { interpret_program(validated, function, args, incomplete) };
            assert!(refused.unwrap_err().to_string().contains("unused transcript"));
        }
    }).expect_lowered("Fetch and reachable HTTP/Grimoire operations admit");
    // The canonical oracle finished before the admitted Wasm leg accepts any
    // requests; production Wasm executes against the exact request script.
    peer.start(1);
    let net = if derived { vec![peer.address.clone()] } else { Vec::new() };
    let fetch = if derived { Vec::new() } else { vec![origin.clone()] };
    let mut runtime = Runtime::batch().unwrap();
    let caps = Capabilities { print: true, quiet: true, net_allow: derived.then_some(net), net_connect: derived,
        fetch_grants: if derived { Vec::new() } else { vec![fetch] }, ..Capabilities::none() };
    let mut vm = runtime.spawn_admitted(&artifact, caps, 256).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    peer.finish(3);
}

#[test]
fn fetch_derived_grimoire_registry_get_post_three_legs() { three_legs(true); }

#[test]
fn fetch_provider_denials_are_values_but_missing_roots_and_widening_refuse() {
    let source = "fn main(console: Console, fetch: Fetch):\n    let narrowed = fetch.only(\"http://127.0.0.1:1\")\n    console.print(narrowed.send_raw(\"GET\", \"http://127.0.0.1:2/\", \"\", \"\"))\n    console.print(narrowed.send_raw(\"bad method\", \"http://127.0.0.1:1/\", \"\", \"\"))\n";
    let checked = witchy::resolve_std_only_checked(source).unwrap();
    let expected = ["WITCHY_FETCH_ERROR:denied:Fetch origin `http://127.0.0.1:2` is not granted",
        "WITCHY_FETCH_ERROR:invalid-request:invalid Fetch request: method is not an HTTP token"];
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(&checked, |validated, artifact| {
        let entry = artifact.entry().unwrap();
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let mut config = InterpreterConfig::default();
        config.fixture.fetch = Some(OracleFetchFixture { grants: vec![vec!["http://127.0.0.1:1".into(), "http://127.0.0.1:2".into()]], calls: Vec::new() });
        let tokens = || entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let run = interpret_entry_program(validated, function, tokens(), &[], config).unwrap();
        assert_eq!(run.output, expected);
        assert_eq!(run.leaked_objects, 0);
        let error = interpret_entry_program(validated, function, tokens(), &[], InterpreterConfig::default()).unwrap_err();
        assert!(error.to_string().contains("not granted"));
        let wrong = vec![InterpreterValue::capability(0), InterpreterValue::capability(13)];
        assert!(interpret_entry_program(validated, function, wrong, &[], InterpreterConfig::default()).is_err());
    }).expect_lowered("denied Fetch requests are still admitted computations");
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, quiet: true,
        fetch_grants: vec![vec!["http://127.0.0.1:1".into(), "http://127.0.0.1:2".into()]], ..Capabilities::none() }, 256).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, ..Capabilities::none() }, 256).unwrap();
    assert!(vm.run().unwrap_err().to_string().contains("Fetch"));
    assert!(vm.output().is_empty());
    // Widening is an admitted computation with a real policy refusal, not a
    // missing-lowering diagnostic. No network fixture or listener is supplied.
    let only = witchy::resolve_std_only_checked("fn main(fetch: Fetch):\n    let wider = fetch.only(\"http://127.0.0.1:2\")\n    ()\n").unwrap();
    let (only_artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(&only, |validated, artifact| {
        let entry = artifact.entry().unwrap();
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        for with_argv in [false, true] {
            let args = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
            let mut config = InterpreterConfig::default();
            config.fixture.fetch = Some(OracleFetchFixture { grants: vec![vec!["http://127.0.0.1:1".into()]], calls: Vec::new() });
            let error = if with_argv { interpret_entry_program(validated, function, args, &[], config) }
                else { interpret_program(validated, function, args, config) }.unwrap_err();
            assert!(error.to_string().contains("not granted"), "{error}");
        }
    }).expect_lowered("Fetch.only must admit before its policy can refuse widening");
    let mut vm = runtime.spawn_admitted(&only_artifact, Capabilities {
        fetch_grants: vec![vec!["http://127.0.0.1:1".into()]], ..Capabilities::none()
    }, 256).unwrap();
    let error = format!("{:#}", vm.run().unwrap_err());
    assert!(error.contains("fetch.only") && error.contains("not granted"), "{error}");
    assert!(vm.output().is_empty());
    assert!(witchy::resolve_std_only_checked("fn main(net: Net[Listen, Tcp]):\n    let fetch = net.fetch(\"http://127.0.0.1:1\")\n    ()\n").is_err());
}
