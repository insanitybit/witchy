use super::*;
use crate::runtime::{Capabilities, Runtime, Vm};
use std::{collections::VecDeque, io::{Read, Write}, sync::{Mutex, atomic::{AtomicBool, Ordering}}, time::Duration};
use witchy_wir::canonical_safewir::BUILD_NET_CAPABILITY_KIND;

type Token = Option<Rooted<ExternRef>>;
type Arguments = (Token, i32, i32, i32, i32);

const WAT: &str = r#"(module
  (import "witchy" "build_net_fetch" (func $fetch (param externref i32 i32 i32 i32) (result externref)))
  (import "witchy" "build_response_is_some" (func $some (param externref) (result i32)))
  (import "witchy" "build_read_response_size" (func $size (param externref) (result i32)))
  (import "witchy" "build_read_response_into" (func $copy (param externref i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "run"))
  (func (export "fetch") (param externref i32 i32 i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $fetch)
  (func (export "some") (param externref) (result i32) local.get 0 call $some)
  (func (export "size") (param externref) (result i32) local.get 0 call $size)
  (func (export "copy") (param externref i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 call $copy))"#;

fn vm(caps: Capabilities, memory: bool) -> Vm {
    let wat = if memory { WAT.to_owned() } else {
        WAT.replace("(memory (export \"memory\") 1)", "")
    };
    Runtime::new().unwrap().spawn(&wat, caps, 1).unwrap()
}

fn grant(vm: &mut Vm) -> Token {
    let authority = mint(vm.store.data()).unwrap();
    Some(ExternRef::new(&mut vm.store, authority).unwrap())
}

pub(in crate::runtime::host) fn empty_grant_fixture() -> (Vm, Token) {
    let mut vm = vm(Capabilities { build_net_allow: Some(Vec::new()), ..Capabilities::none() }, true);
    let grant = grant(&mut vm);
    (vm, grant)
}

fn call(vm: &mut Vm, grant: Token, host: &str, path: &str) -> Result<Token> {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.write(&mut vm.store, 0, host.as_bytes())?;
    memory.write(&mut vm.store, 1024, path.as_bytes())?;
    vm.instance.get_typed_func::<Arguments, Token>(&mut vm.store, "fetch")?
        .call(&mut vm.store, (grant, 0, host.len() as i32, 1024, path.len() as i32))
}

fn response_bytes(vm: &mut Vm, token: Token) -> Vec<u8> {
    assert!(token.is_some(), "a successful fetch returns a non-null Some response");
    let some = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "some").unwrap();
    let size = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap();
    let copy = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap();
    assert_eq!(some.call(&mut vm.store, token).unwrap(), 1);
    let size = size.call(&mut vm.store, token).unwrap();
    assert_eq!(copy.call(&mut vm.store, (token, 4096, size)).unwrap(), size);
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.data(&vm.store)[4096..4096 + size as usize].to_vec()
}

// Test-only bounded peer. Even an unexpected connection receives EOF, so a
// failed preflight assertion cannot leave the production read waiting forever.
// finish drains queued accepts before returning its complete request census.
struct Peer {
    host: String,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    fn new(replies: Vec<Vec<u8>>) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let host = listener.local_addr().unwrap().to_string();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let seen = requests.clone();
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut replies: VecDeque<_> = replies.into();
            loop {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket.set_nonblocking(false).unwrap();
                        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                        socket.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
                        let mut request = Vec::new();
                        let mut buffer = [0; 512];
                        loop {
                            let n = socket.read(&mut buffer).unwrap();
                            if n == 0 { break; }
                            request.extend_from_slice(&buffer[..n]);
                            assert!(request.len() <= 65536, "unexpected oversized test request");
                            if request.ends_with(b"\r\n\r\n") { break; }
                        }
                        seen.lock().unwrap().push(request);
                        socket.write_all(&replies.pop_front().unwrap_or_default()).unwrap();
                        // Drop closes the connection regardless of response framing.
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopped.load(Ordering::Acquire) { break; }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("test peer accept: {error}"),
                }
            }
        });
        Self { host, requests, stop, thread: Some(thread) }
    }

    fn finish(mut self) -> Vec<Vec<u8>> {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().expect("test peer must finish");
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

#[test]
fn build_net_preserves_literal_request_status_lossy_decode_and_eof_framing() {
    let peer = Peer::new(vec![
        b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nbad\r\n0\r\n\r\n".to_vec(),
        b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/elsewhere\r\nX-Raw: \xff\r\n\r\ncaf\xc3\xa9\xff".to_vec(),
        b"no headers\xff\0end".to_vec(),
        b"HTTP/1.1 200 OK\r\n\r\nfirst\r\n\r\nsecond".to_vec(),
        Vec::new(),
    ]);
    let mut vm = vm(Capabilities { build_net_allow: Some(vec![peer.host.clone()]), ..Capabilities::none() }, true);
    let grant = grant(&mut vm);
    let cases = [
        ("/schema?x=%2F", "3\r\nbad\r\n0\r\n\r\n"),
        ("/raw caf\u{e9}\r\nX-Requested: literal", "caf\u{e9}\u{fffd}"),
        ("relative path", "no headers\u{fffd}\0end"),
        ("/first-separator", "first\r\n\r\nsecond"),
        ("/empty", ""),
    ];
    for (path, expected) in cases {
        let response = call(&mut vm, grant, &peer.host, path).unwrap();
        assert_eq!(response_bytes(&mut vm, response), expected.as_bytes());
    }
    let requests = peer.finish();
    assert_eq!(requests, cases.iter().map(|(path, _)| {
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").into_bytes()
    }).collect::<Vec<_>>());
    assert!(vm.output().is_empty());
    assert!(vm.store.data().pending.is_none());
}

#[test]
fn build_net_retains_independent_responses_without_refetch_and_releases_host_roots() {
    let peer = Peer::new(vec![b"HTTP/1.1 200 OK\r\n\r\nfirst".to_vec(), b"second".to_vec()]);
    let mut vm = vm(Capabilities { build_net_allow: Some(vec![peer.host.clone()]), ..Capabilities::none() }, true);
    let grant = grant(&mut vm);
    let weak_hosts = {
        let authority = grant.unwrap().data(&vm.store).unwrap().unwrap().downcast_ref::<Authority>().unwrap();
        Arc::downgrade(&authority.hosts)
    };
    let weak_response_issuer = Arc::downgrade(&vm.store.data().build_read_issuer);
    let first = call(&mut vm, grant, &peer.host, "/first").unwrap();
    // A granted root owns its immutable snapshot, not a live view of caps.
    vm.store.data_mut().caps.build_net_allow = None;
    let second = call(&mut vm, grant, &peer.host, "/second").unwrap();
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    let size = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap();
    let copy = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap();
    for (token, expected) in [(first, "first"), (second, "second"), (first, "first")] {
        let length = expected.len() as i32;
        assert_eq!(size.call(&mut vm.store, token).unwrap(), length);
        memory.write(&mut vm.store, 4096, &[0xa5; 32]).unwrap();
        let before = memory.data(&vm.store).to_vec();
        assert_eq!(copy.call(&mut vm.store, (token, 4096, length - 1)).unwrap(), length);
        assert_eq!(memory.data(&vm.store), before.as_slice());
        for (ptr, capacity) in [(-1, 1), (0, -1), (65535, 2), (65537, 0), (4096, i32::MAX)] {
            assert!(copy.call(&mut vm.store, (token, ptr, capacity)).is_err());
            assert_eq!(memory.data(&vm.store), before.as_slice());
        }
        assert_eq!(copy.call(&mut vm.store, (token, 65536, 0)).unwrap(), length);
        assert_eq!(memory.data(&vm.store), before.as_slice());
        assert_eq!(copy.call(&mut vm.store, (token, 4096, 32)).unwrap(), length);
        assert_eq!(&memory.data(&vm.store)[4096..4096 + expected.len()], expected.as_bytes());
        assert!(memory.data(&vm.store)[4096 + expected.len()..4128].iter().all(|byte| *byte == 0xa5));
    }
    assert_eq!(peer.finish().len(), 2, "size/copy must never issue another request");
    assert!(vm.output().is_empty());
    assert!(vm.store.data().pending.is_none());
    // This raw host fixture has no canonical guest heap. Audit its host roots,
    // not a fabricated zero-guest-leak result. Responses share this issuer.
    drop(vm);
    assert!(weak_hosts.upgrade().is_none(), "Store teardown releases the exact host grant");
    assert!(weak_response_issuer.upgrade().is_none(), "no response payload retains its issuer after Store teardown");
}

#[test]
fn build_net_authenticates_kind_current_issuer_and_read_before_memory() {
    let mut vm = vm(Capabilities { build_net_allow: Some(Vec::new()), ..Capabilities::none() }, false);
    let authority = mint(vm.store.data()).unwrap();
    let forged = Some(ExternRef::new(&mut vm.store, BUILD_NET_CAPABILITY_KIND.get()).unwrap());
    let ordinary = Some(ExternRef::new(&mut vm.store, crate::runtime::NetAuthority { allow: vec!["127.0.0.1:1".into()], issuer: Arc::new(()) }).unwrap());
    let foreign = Some(ExternRef::new(&mut vm.store, Authority { issuer: Arc::new(()), ..authority.clone() }).unwrap());
    let denied = Some(ExternRef::new(&mut vm.store, Authority { read: false, ..authority }).unwrap());
    let response = Some(super::super::build_read::new_response(&mut vm.store, Some("not authority")).unwrap());
    let fetch = vm.instance.get_typed_func::<Arguments, Token>(&mut vm.store, "fetch").unwrap();
    for (token, expected) in [
        (None, "fetch_build: null capability"),
        (forged, "fetch_build: wrong capability kind"),
        (ordinary, "fetch_build: wrong capability kind"),
        (response, "fetch_build: wrong capability kind"),
        (foreign, "fetch_build: capability belongs to another instance"),
        (denied, "fetch_build: capability lacks read right"),
    ] {
        assert_eq!(fetch.call(&mut vm.store, (token, -1, -1, -1, -1)).unwrap_err().root_cause().to_string(), expected);
    }
    assert!(vm.output().is_empty());
    assert!(vm.store.data().pending.is_none());
}

#[test]
fn build_net_requires_explicit_grants_and_denies_unlisted_hosts_before_network() {
    let peer = Peer::new(Vec::new());
    let absent = vm(Capabilities { net_connect: true, net_allow: Some(vec![peer.host.clone()]), ..Capabilities::none() }, false);
    assert_eq!(mint(absent.store.data()).unwrap_err().to_string(), "fetch_build: no BuildNet grant");
    let (mut empty, empty_grant) = empty_grant_fixture();
    assert!(mint(empty.store.data()).unwrap().hosts.is_empty());
    let expected = format!("fetch_build: `{}` is not in this BuildNet grant's allow-list", peer.host);
    assert_eq!(call(&mut empty, empty_grant, &peer.host, "/").unwrap_err().root_cause().to_string(), expected);
    let mut other = vm(Capabilities { build_net_allow: Some(vec!["different.example:80".into()]), ..Capabilities::none() }, true);
    let other_grant = grant(&mut other);
    assert_eq!(call(&mut other, other_grant, &peer.host, "/").unwrap_err().root_cause().to_string(), expected);
    let mut exact = vm(Capabilities { build_net_allow: Some(vec![peer.host.clone()]), ..Capabilities::none() }, true);
    let exact_grant = grant(&mut exact);
    for host in [format!("http://{}", peer.host), format!("tcp:{}", peer.host), format!("{} ", peer.host)] {
        assert_eq!(call(&mut exact, exact_grant, &host, "/").unwrap_err().root_cause().to_string(),
            format!("fetch_build: `{host}` is not in this BuildNet grant's allow-list"));
    }
    assert!(peer.finish().is_empty(), "denied grants must not even connect");
}

#[test]
fn build_net_preflights_both_complete_utf8_slices_without_network_or_writes() {
    let peer = Peer::new(Vec::new());
    let mut vm = vm(Capabilities { build_net_allow: Some(vec![peer.host.clone()]), ..Capabilities::none() }, true);
    let grant = grant(&mut vm);
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.write(&mut vm.store, 0, peer.host.as_bytes()).unwrap();
    let fetch = vm.instance.get_typed_func::<Arguments, Token>(&mut vm.store, "fetch").unwrap();
    for (ptr, len) in [(-1, 1), (0, -1), (65535, 2), (65537, 0), (i32::MAX, i32::MAX)] {
        let before = memory.data(&vm.store).to_vec();
        assert!(fetch.call(&mut vm.store, (grant, ptr, len, 1024, 0)).is_err());
        assert!(fetch.call(&mut vm.store, (grant, 0, peer.host.len() as i32, ptr, len)).is_err());
        assert_eq!(memory.data(&vm.store), before.as_slice());
    }
    memory.write(&mut vm.store, 1024, &[0xff]).unwrap();
    assert!(fetch.call(&mut vm.store, (grant, 0, peer.host.len() as i32, 1024, 1))
        .unwrap_err().root_cause().to_string().contains("not valid UTF-8"));
    memory.write(&mut vm.store, 0, &[0xff]).unwrap();
    assert!(fetch.call(&mut vm.store, (grant, 0, 1, 2048, 0))
        .unwrap_err().root_cause().to_string().contains("not valid UTF-8"));
    assert!(peer.finish().is_empty(), "malformed input must not connect before validation");
    assert!(vm.output().is_empty());
    assert!(vm.store.data().pending.is_none());
}

#[test]
fn build_net_responses_are_not_roots_and_reauthenticate_the_response_issuer() {
    let (mut vm, grant) = empty_grant_fixture();
    let response = Some(super::super::build_read::new_response(&mut vm.store, Some("response")).unwrap());
    assert_eq!(call(&mut vm, response, "127.0.0.1:1", "/").unwrap_err().root_cause().to_string(), "fetch_build: wrong capability kind");
    let size = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap();
    let copy = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap();
    assert_eq!(size.call(&mut vm.store, grant).unwrap_err().root_cause().to_string(), "BuildRead response has wrong host kind");
    vm.store.data_mut().build_net_issuer = Arc::new(());
    assert_eq!(call(&mut vm, grant, "127.0.0.1:1", "/").unwrap_err().root_cause().to_string(), "fetch_build: capability belongs to another instance");
    assert_eq!(size.call(&mut vm.store, response).unwrap(), 8, "changing root authority does not invalidate immutable data");
    vm.store.data_mut().build_read_issuer = Arc::new(());
    assert_eq!(size.call(&mut vm.store, response).unwrap_err().root_cause().to_string(), "BuildRead response belongs to another instance");
    assert_eq!(copy.call(&mut vm.store, (response, -1, -1)).unwrap_err().root_cause().to_string(), "BuildRead response belongs to another instance");
}

#[test]
fn build_net_preserves_connection_error_without_staging_a_response() {
    let host = "127.0.0.1:not-a-port";
    let mut vm = vm(Capabilities { build_net_allow: Some(vec![host.into()]), ..Capabilities::none() }, true);
    let grant = grant(&mut vm);
    let error = call(&mut vm, grant, host, "/").unwrap_err().root_cause().to_string();
    assert!(error.starts_with(&format!("fetch_build: cannot connect to `{host}`:")), "{error}");
    assert!(vm.store.data().pending.is_none());
    assert!(vm.output().is_empty());
}

#[test]
fn build_net_legacy_staging_and_old_signature_do_not_link() {
    for name in ["build_fetch_len", "build_net_fetch"] {
        let wat = format!("(module (import \"witchy\" \"{name}\" (func (param i32 i32) (result i32))) (func (export \"run\")))");
        let result = Runtime::new().unwrap().spawn(&wat,
            Capabilities { build_net_allow: Some(Vec::new()), ..Capabilities::none() }, 1);
        assert!(result.is_err(), "legacy import/signature must not instantiate: {name}");
    }
}
