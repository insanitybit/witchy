use super::{checked, Folder};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Duration;
use witchy_wir::canonical_interpreter::{
    self as oracle, InterpreterConfig, InterpreterValue, OracleBuildNetCall,
    OracleBuildNetFixture, OracleFixture,
};
use witchy_wir::CanonicalEntryCapability::{BuildNet, BuildOut};

// A bounded request reader, with cancellation even when compilation/execution
// fails before connecting. The peer checks real request bytes, not just output.
struct Peer {
    host: String,
    expected: usize,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<Result<usize, String>>>,
}

impl Peer {
    fn new(replies: Vec<(String, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let expected = replies.len();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut served = 0;
            for (path, response) in replies {
                let mut socket = loop {
                    if cancelled.load(Ordering::Acquire) { return Ok(served); }
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => return Err(error.to_string()),
                    }
                };
                socket.set_nonblocking(false).map_err(|error| error.to_string())?;
                socket.set_read_timeout(Some(Duration::from_secs(5))).map_err(|error| error.to_string())?;
                socket.set_write_timeout(Some(Duration::from_secs(5))).map_err(|error| error.to_string())?;
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    if request.len() >= 8192 { return Err("fixture request exceeds its bound".into()); }
                    let mut byte = [0];
                    socket.read_exact(&mut byte).map_err(|error| error.to_string())?;
                    request.push(byte[0]);
                }
                let expected = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
                if request != expected.as_bytes() {
                    return Err(format!("unexpected BuildNet request: {request:?}"));
                }
                socket.write_all(&response).map_err(|error| error.to_string())?;
                served += 1;
                // Dropping this connection supplies the real EOF boundary.
            }
            Ok(served)
        });
        Self { host, expected, stop, worker: Some(worker) }
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::Release);
        let count = self.worker.take().unwrap().join().expect("peer thread").expect("exact request transcript");
        assert_eq!(count, self.expected, "every source/native request must execute exactly once");
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

fn transcript(host: &str, replies: &[(String, Vec<u8>)]) -> OracleBuildNetFixture {
    OracleBuildNetFixture {
        hosts: BTreeSet::from([host.to_owned()]),
        expectations: replies.iter().map(|(path, response)| OracleBuildNetCall {
            host: host.into(), path: path.clone(), response: response.clone(),
        }).collect(),
    }
}

#[test]
fn build_net_recipe_three_legs_capture_snapshot_and_raw_response_semantics() {
    witchy_interp::compiler_natives::install();
    let folder = Folder::new();
    let second_path = "/caf\u{e9}";
    let first = "first\u{fffd}\r\n\r\nsuffix".to_owned();
    let second = format!("unframed{}\u{fffd}", "caf\u{e9}".repeat(80));
    let mut unframed = format!("unframed{}", "caf\u{e9}".repeat(80)).into_bytes();
    unframed.push(0xff);
    let replies = vec![
        ("/first".into(), b"HTTP/1.1 404 Missing\r\nHeader: \xff\r\n\r\nfirst\xff\r\n\r\nsuffix".to_vec()),
        (second_path.into(), unframed),
        ("/third".into(), b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/never\r\n\r\n".to_vec()),
    ];
    let peer = Peer::new(replies.iter().chain(&replies).cloned().collect());
    let host = &peer.host;
    let recipe = checked(&folder, &format!(
        "fn mutate(var host: String) -> String:\n    host = host + \".changed\"\n    \"/first\"\nfn build(net: BuildNet, out: BuildOut):\n    let fetch = fn(path: String): net.fetch_build({host:?}, path)\n    var host = {host:?}\n    let first = net.fetch_build(host, mutate(host))\n    let second = fetch({second_path:?})\n    let third = fetch(\"/third\")\n    out.write_out(\"first.txt\", first)\n    out.write_out(\"second.txt\", second)\n    out.write_out(\"third.txt\", third)\n    out.write_out(\"host.txt\", host)\n"
    ));
    assert_eq!(recipe.identity().parameter_kinds(), &[16, 9]);
    assert!(recipe.canonical_source().launch_contract_payload().ends_with(&[2, 0, 0, 0, 16, 1, 9, 1]));
    let interpreted = folder.0.join("interpreted");
    crate::interpreter::run_build_step(recipe.original_module().module().clone(), crate::interpreter::BuildGrants {
        out_dir: interpreted.clone(), net_hosts: vec![host.clone()], ..Default::default()
    }).expect("source BuildNet requests");
    let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("BuildNet admission");
    assert_eq!(artifact.entry().unwrap().capabilities(), &[BuildNet, BuildOut]);
    assert!(!artifact.owner_bridges().is_empty(), "captured BuildNet must use an authenticated owner bridge");
    let compiled = folder.0.join("compiled");
    std::fs::create_dir_all(&compiled).unwrap();
    let mut runtime = crate::runtime::Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, crate::runtime::Capabilities {
        build_out: Some(compiled.clone()), build_net_allow: Some(vec![host.clone()]),
        ..crate::runtime::Capabilities::none()
    }, crate::RUN_MEMORY_PAGES).unwrap();
    vm.run().expect("admitted BuildNet requests");
    assert!(vm.output().is_empty());
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    let expected = [
        ("first.txt", first), ("second.txt", second), ("third.txt", String::new()),
        ("host.txt", format!("{host}.changed")),
    ];
    for directory in [&interpreted, &compiled] {
        for (name, contents) in &expected {
            assert_eq!(std::fs::read_to_string(directory.join(name)).unwrap(), contents.as_str());
        }
    }
    let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
    for entry_api in [false, true] {
        let config = InterpreterConfig { fixture: OracleFixture {
            build_out: true, build_net: Some(transcript(host, &replies)), ..Default::default()
        }, ..Default::default() };
        let parameters = vec![InterpreterValue::entry_capability(BuildNet), InterpreterValue::entry_capability(BuildOut)];
        let run = if entry_api {
            oracle::interpret_entry_program(&validated, function, parameters, &[], config)
        } else { oracle::interpret_program(&validated, function, parameters, config) }.unwrap();
        assert_eq!(run.leaked_objects, 0);
        assert!(run.output.is_empty());
        assert_eq!(run.build_trace, expected.iter().map(|(path, contents)| oracle::OracleBuildWrite {
            path: (*path).into(), contents: contents.clone(),
        }).collect::<Vec<_>>());
    }
    peer.finish();
}

#[test]
fn build_net_recipe_denies_hosts_and_missing_grants_without_forged_output() {
    witchy_interp::compiler_natives::install();
    let folder = Folder::new();
    let recipe = checked(&folder, "fn build(out: BuildOut, net: BuildNet):\n    out.write_out(\"before.txt\", \"before\")\n    out.write_out(\"never.txt\", net.fetch_build(\"127.0.0.1:2\", \"/secret\"))\n");
    let interpreted = folder.0.join("interpreted");
    let error = crate::interpreter::run_build_step(recipe.original_module().module().clone(), crate::interpreter::BuildGrants {
        out_dir: interpreted.clone(), net_hosts: vec!["127.0.0.1:1".into()], ..Default::default()
    }).unwrap_err();
    let denied = "fetch_build: `127.0.0.1:2` is not in this BuildNet grant's allow-list";
    assert!(error.message.contains(denied), "{error}");
    assert_eq!(std::fs::read_to_string(interpreted.join("before.txt")).unwrap(), "before");
    assert!(!interpreted.join("never.txt").exists());
    let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("denial is a host outcome, not missing lowering");
    let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
    for missing in [false, true] {
        let compiled = folder.0.join(format!("compiled-{missing}"));
        std::fs::create_dir_all(&compiled).unwrap();
        let mut runtime = crate::runtime::Runtime::batch().unwrap();
        let result = runtime.spawn_admitted(&artifact, crate::runtime::Capabilities {
            build_out: Some(compiled.clone()),
            build_net_allow: (!missing).then(|| vec!["127.0.0.1:1".into()]),
            ..crate::runtime::Capabilities::none()
        }, crate::RUN_MEMORY_PAGES).and_then(|mut vm| vm.run());
        let error = result.unwrap_err();
        assert!(format!("{error:#}").contains(if missing { "BuildNet" } else { denied }), "{error:#}");
        assert!(!compiled.join("never.txt").exists());
        if missing {
            assert!(!compiled.join("before.txt").exists(), "a missing root must refuse before entry execution");
        } else {
            assert_eq!(std::fs::read_to_string(compiled.join("before.txt")).unwrap(), "before");
        }
        for entry_api in [false, true] {
            let config = InterpreterConfig { fixture: OracleFixture {
                build_out: true,
                build_net: (!missing).then(|| transcript("127.0.0.1:1", &[])),
                ..Default::default()
            }, ..Default::default() };
            let parameters = vec![InterpreterValue::entry_capability(BuildOut), InterpreterValue::entry_capability(BuildNet)];
            let result = if entry_api {
                oracle::interpret_entry_program(&validated, function, parameters, &[], config)
            } else { oracle::interpret_program(&validated, function, parameters, config) };
            let error = result.unwrap_err();
            assert!(error.to_string().contains(if missing { "not granted" } else { denied }), "{error}");
            // Neither backend's aborted execution establishes zero leaks.
        }
    }
}

#[test]
fn build_net_recipe_oracle_requires_exact_and_complete_wire_transcripts() {
    witchy_interp::compiler_natives::install();
    let folder = Folder::new();
    let recipe = checked(&folder, "fn build(net: BuildNet, out: BuildOut):\n    out.write_out(\"x\", net.fetch_build(\"fixture:80\", \"/expected\"))\n");
    let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("fixture recipe admits without ambient network");
    let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
    for (replies, message) in [
        (vec![], "unexpected request"),
        (vec![("/different".into(), b"must not be returned".to_vec())], "transcript mismatch"),
        (vec![("/expected".into(), b"first".to_vec()), ("/unused".into(), b"unused".to_vec())], "unused transcript expectations"),
    ] {
        for entry_api in [false, true] {
            let config = InterpreterConfig { fixture: OracleFixture {
                build_out: true, build_net: Some(transcript("fixture:80", &replies)), ..Default::default()
            }, ..Default::default() };
            let parameters = vec![InterpreterValue::entry_capability(BuildNet), InterpreterValue::entry_capability(BuildOut)];
            let result = if entry_api {
                oracle::interpret_entry_program(&validated, function, parameters, &[], config)
            } else { oracle::interpret_program(&validated, function, parameters, config) };
            let error = result.unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
        }
    }
}

#[test]
fn build_net_recipe_binds_original_source_order_and_refuses_ordinary_roots() {
    witchy_interp::compiler_natives::install();
    let folder = Folder::new();
    let first = checked(&folder, "fn build(net: BuildNet, out: BuildOut):\n    out.write_out(\"x\", net.fetch_build(\"fixture:80\", \"/x\"))\n");
    let second = checked(&folder, "fn build(out: BuildOut, net: BuildNet):\n    out.write_out(\"x\", net.fetch_build(\"fixture:80\", \"/x\"))\n");
    assert_eq!(first.identity().parameter_kinds(), &[16, 9]);
    assert_eq!(second.identity().parameter_kinds(), &[9, 16]);
    assert_ne!(first.identity().original_source_digest(), second.identity().original_source_digest());
    assert_ne!(first.identity().derived_source_digest(), second.identity().derived_source_digest());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&first).unwrap();
    assert!(witchy_wir::admit_canonical(validated, second.canonical_source()).is_err());
    let main = folder.source("fn main(net: BuildNet) -> ():\n    ()\n");
    assert!(crate::link_file_checked(main.to_str().unwrap()).is_err(), "BuildNet cannot become an ordinary main root");
    for source in [
        "fn build(own net: BuildNet, out: BuildOut) -> ():\n    ()\n",
        "fn build(var net: BuildNet, out: BuildOut) -> ():\n    ()\n",
    ] {
        assert!(crate::source::link_build_file(folder.source(source).to_str().unwrap()).is_err());
    }
}
