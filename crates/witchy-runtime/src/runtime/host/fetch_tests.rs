use super::*;
use crate::runtime::{Capabilities, Runtime, Vm};
use std::{collections::VecDeque, io::{Read, Write}, sync::{Mutex, atomic::{AtomicBool, Ordering}}, time::Duration};

type Token = Option<Rooted<ExternRef>>;
type SendArgs = (Token, i32, i32, i32, i32, i32, i32, i32, i32);
const WAT: &str = r#"(module
  (import "witchy" "net_fetch_at" (func $derive (param externref i32 i32) (result externref)))
  (import "witchy" "fetch_only_at" (func $narrow (param externref i32 i32) (result externref)))
  (import "witchy" "fetch_send_at" (func $send (param externref i32 i32 i32 i32 i32 i32 i32 i32) (result externref)))
  (import "witchy" "fetch_response_size" (func $size (param externref) (result i32)))
  (import "witchy" "fetch_response_into" (func $copy (param externref i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "run"))
  (func (export "narrow") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $narrow)
  (func (export "derive") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $derive)
  (func (export "send") (param externref i32 i32 i32 i32 i32 i32 i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4
    local.get 5 local.get 6 local.get 7 local.get 8 call $send)
  (func (export "size") (param externref) (result i32) local.get 0 call $size)
  (func (export "copy") (param externref i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 call $copy))"#;

fn vm(caps: Capabilities, memory: bool) -> Vm {
    let wat = if memory { WAT.to_owned() } else { WAT.replace("(memory (export \"memory\") 1)", "") };
    Runtime::new().unwrap().spawn(&wat, caps, 1).unwrap()
}

fn grant(vm: &mut Vm, ordinal: usize) -> Token {
    let authority = mint(vm.store.data(), ordinal).unwrap();
    Some(ExternRef::new(&mut vm.store, authority).unwrap())
}

pub(in crate::runtime::host) fn empty_grant_fixture() -> (Vm, Token) {
    let mut vm = vm(Capabilities { fetch_grants: vec![vec![]], ..Capabilities::none() }, true);
    let root = grant(&mut vm, 0);
    (vm, root)
}

fn prepare(vm: &mut Vm, root: Token, method: &str, url: &str, headers: &str, body: &str) -> SendArgs {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    for (offset, text) in [(0, method), (256, url), (1024, headers), (4096, body)] {
        memory.write(&mut vm.store, offset, text.as_bytes()).unwrap();
    }
    (root, 0, method.len() as i32, 256, url.len() as i32,
        1024, headers.len() as i32, 4096, body.len() as i32)
}

fn send(vm: &mut Vm, root: Token, method: &str, url: &str, headers: &str, body: &str) -> Token {
    let args = prepare(vm, root, method, url, headers, body);
    vm.instance.get_typed_func::<SendArgs, Token>(&mut vm.store, "send").unwrap()
        .call(&mut vm.store, args).unwrap()
}

fn bytes(vm: &mut Vm, response: Token) -> Vec<u8> {
    assert!(response.is_some());
    let size = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap()
        .call(&mut vm.store, response).unwrap();
    let copied = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap()
        .call(&mut vm.store, (response, 8192, size)).unwrap();
    assert_eq!(copied, size);
    vm.instance.get_memory(&mut vm.store, "memory").unwrap()
        .data(&vm.store)[8192..8192 + size as usize].to_vec()
}

fn error_contains(error: Error, expected: &str) {
    let message = format!("{error:#}");
    assert!(message.contains(expected), "expected {expected:?}: {message}");
}

// Bounded, joined fixture: no detached peers or indefinite accepts, including
// assertion failures. Accepted sockets are explicitly blocking on Darwin.
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
        let seen = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut replies: VecDeque<_> = replies.into();
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                        stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
                        let mut request = Vec::new();
                        let mut buffer = [0; 512];
                        loop {
                            let n = stream.read(&mut buffer).unwrap();
                            if n == 0 { break; }
                            request.extend_from_slice(&buffer[..n]);
                            assert!(request.len() <= 65536);
                            if let Some(split) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                                let head = std::str::from_utf8(&request[..split]).unwrap();
                                let length = head.lines().filter_map(|line| line.split_once(':'))
                                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                    .map(|(_, n)| n.trim().parse::<usize>().unwrap()).unwrap_or(0);
                                if request.len() >= split + 4 + length { break; }
                            }
                        }
                        seen.lock().unwrap().push(request);
                        // An intentionally capped client may close before all bytes are sent.
                        let _ = stream.write_all(&replies.pop_front().unwrap_or_default());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopped.load(Ordering::Acquire) { break; }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("peer accept: {error}"),
                }
            }
        });
        Self { host, requests, stop, thread: Some(thread) }
    }
    fn finish(mut self) -> Vec<Vec<u8>> {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
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
fn fetch_host_preserves_request_and_once_decoded_provider_payload() {
    let peer = Peer::new(vec![
        b"HTTP/1.1 503 Busy\r\nTransfer-Encoding: chunked\r\nX-Test: yes\r\n\r\n3\r\n\xc3\xa9\xff\r\n0\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\ntwo".to_vec(),
    ]);
    let origin = format!("http://{}", peer.host);
    let mut vm = vm(Capabilities { fetch_grants: vec![vec![origin.clone()]], ..Capabilities::none() }, true);
    let root = grant(&mut vm, 0);
    let first = send(&mut vm, root, "POST", &format!("{origin}/echo?q=%2F#ignored"), "X-Request: yes\n", "caf\u{e9}");
    // Changing launch input tables cannot alter an already minted policy.
    vm.store.data_mut().fetch_grants.clear();
    vm.store.data_mut().caps.fetch_grants.clear();
    let second = send(&mut vm, root, "GET", &format!("{origin}/second"), "", "");
    let expected = "HTTP/1.1 503\r\nTransfer-Encoding: chunked\r\nX-Test: yes\r\n\r\n\u{e9}\u{fffd}";
    assert_eq!(bytes(&mut vm, first), expected.as_bytes());
    assert_eq!(bytes(&mut vm, second), b"HTTP/1.1 200\r\nContent-Length: 3\r\n\r\ntwo");
    assert_eq!(bytes(&mut vm, first), expected.as_bytes());
    assert!(vm.store.data().pending.is_none());
    assert!(vm.output().is_empty());
    let requests = peer.finish();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], format!("POST /echo?q=%2F HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nX-Request: yes\r\nContent-Length: 5\r\n\r\ncaf\u{e9}", peer_host(&origin)).as_bytes());
    assert!(requests[1].starts_with(b"GET /second HTTP/1.1\r\n"));
}

fn peer_host(origin: &str) -> &str { origin.strip_prefix("http://").unwrap() }

#[test]
fn fetch_host_errors_are_values_redirects_injection_and_size_remain_enforced() {
    let peer = Peer::new(vec![
        b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/no-follow\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\n\r\ntoo large".to_vec(),
    ]);
    let origin = format!("http://{}", peer.host);
    let mut vm = vm(Capabilities { fetch_grants: vec![vec![origin.clone()]], ..Capabilities::none() }, true);
    let root = grant(&mut vm, 0);
    let denied = send(&mut vm, root, "GET", "http://127.0.0.1:1/denied", "", "");
    assert!(bytes(&mut vm, denied).starts_with(b"WITCHY_FETCH_ERROR:denied:"));
    let injected = send(&mut vm, root, "GET\r\nInjected", &origin, "", "");
    assert!(bytes(&mut vm, injected).starts_with(b"WITCHY_FETCH_ERROR:invalid-request:"));
    let header = send(&mut vm, root, "GET", &origin, "Host: other", "");
    assert!(bytes(&mut vm, header).starts_with(b"WITCHY_FETCH_ERROR:invalid-request:"));
    let redirect = send(&mut vm, root, "GET", &origin, "", "");
    assert!(bytes(&mut vm, redirect).starts_with(b"WITCHY_FETCH_ERROR:redirect:"));
    let mut limited = mint(vm.store.data(), 0).unwrap();
    limited.policy = limited.policy.with_limits(Duration::from_secs(2), 8);
    let limited = Some(ExternRef::new(&mut vm.store, limited).unwrap());
    let oversized = send(&mut vm, limited, "GET", &origin, "", "");
    assert!(bytes(&mut vm, oversized).starts_with(b"WITCHY_FETCH_ERROR:response-too-large:"));
    assert_eq!(peer.finish().len(), 2, "denial/injection perform no I/O and redirects are not followed");
}

#[test]
fn fetch_host_preflights_every_complete_utf8_input_before_io() {
    let peer = Peer::new(vec![]);
    let origin = format!("http://{}", peer.host);
    let mut vm = vm(Capabilities { fetch_grants: vec![vec![origin.clone()]], ..Capabilities::none() }, true);
    let root = grant(&mut vm, 0);
    let call = vm.instance.get_typed_func::<SendArgs, Token>(&mut vm.store, "send").unwrap();
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    for lane in 0..4 {
        for (ptr, len) in [(-1, 0), (0, -1), (65535, 2), (65537, 0), (60000, 1)] {
            let valid = prepare(&mut vm, root, "POST", &origin, "X-Test: ok", "body");
            memory.write(&mut vm.store, 60000, &[0xff]).unwrap();
            let mut lanes = [(valid.1, valid.2), (valid.3, valid.4), (valid.5, valid.6), (valid.7, valid.8)];
            lanes[lane] = (ptr, len);
            assert!(call.call(&mut vm.store, (root, lanes[0].0, lanes[0].1, lanes[1].0, lanes[1].1,
                lanes[2].0, lanes[2].1, lanes[3].0, lanes[3].1)).is_err());
        }
    }
    assert!(peer.finish().is_empty());
    assert!(vm.store.data().pending.is_none());
}

#[test]
fn fetch_host_authenticates_private_kinds_and_issuer_before_memory() {
    let mut vm = vm(Capabilities { fetch_grants: vec![vec![]], net_allow: Some(vec![]), ..Capabilities::none() }, false);
    let root = grant(&mut vm, 0);
    let good = mint(vm.store.data(), 0).unwrap();
    let foreign = Some(ExternRef::new(&mut vm.store, Authority { issuer: Arc::new(()), ..good }).unwrap());
    let raw_policy = Some(ExternRef::new(&mut vm.store, FetchPolicy::allow(Vec::<String>::new()).unwrap()).unwrap());
    let response = Some(new_response(&mut vm.store, "not authority").unwrap());
    let built = Some(super::super::build_read::new_response(&mut vm.store, Some("not FetchResponse")).unwrap());
    let forged = Some(ExternRef::new(&mut vm.store, 17_i32).unwrap());
    let send = vm.instance.get_typed_func::<SendArgs, Token>(&mut vm.store, "send").unwrap();
    for (token, expected) in [(None, "null"), (foreign, "another instance"), (raw_policy, "wrong capability kind"),
        (response, "wrong capability kind"), (built, "wrong capability kind"), (forged, "wrong capability kind")] {
        error_contains(send.call(&mut vm.store, (token, -1, -1, -1, -1, -1, -1, -1, -1)).unwrap_err(), expected);
    }
    let derive = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "derive").unwrap();
    let net = vm.store.data().nets[0].clone();
    let local_net = Some(ExternRef::new(&mut vm.store, net.clone()).unwrap());
    let foreign_net = Some(ExternRef::new(&mut vm.store, NetAuthority { issuer: Arc::new(()), ..net }).unwrap());
    for (token, expected) in [(None, "null"), (root, "wrong capability kind"), (foreign_net, "another instance"), (local_net, "Connect right")] {
        error_contains(derive.call(&mut vm.store, (token, -1, -1)).unwrap_err(), expected);
    }
    let copy = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap();
    let foreign_result = Some(ExternRef::new(&mut vm.store, Response { issuer: Arc::new(()), bytes: Arc::from(&b"foreign"[..]) }).unwrap());
    for (token, expected) in [(None, "null"), (root, "wrong host kind"), (built, "wrong host kind"), (foreign_result, "another instance")] {
        error_contains(copy.call(&mut vm.store, (token, -1, -1)).unwrap_err(), expected);
    }
    assert!(vm.output().is_empty());
}

#[test]
fn fetch_host_response_copy_is_repeatable_atomic_and_releases_with_store() {
    let (mut vm, root) = empty_grant_fixture();
    let response = Some(new_response(&mut vm.store, "a\u{e9}z").unwrap());
    let weak_bytes = {
        let data = response.unwrap().data(&vm.store).unwrap().unwrap().downcast_ref::<Response>().unwrap();
        Arc::downgrade(&data.bytes)
    };
    let weak_issuer = Arc::downgrade(&vm.store.data().fetch_issuer);
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    let copy = vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "copy").unwrap();
    memory.write(&mut vm.store, 0, &[0x5a; 32]).unwrap();
    for cap in [0, 1, 3] { assert_eq!(copy.call(&mut vm.store, (response, 0, cap)).unwrap(), 4); }
    assert_eq!(&memory.data(&vm.store)[..32], &[0x5a; 32]);
    for (ptr, cap) in [(-1, 4), (0, -1), (65535, 2), (65537, 0), (32, i32::MAX)] {
        assert!(copy.call(&mut vm.store, (response, ptr, cap)).is_err());
        assert_eq!(&memory.data(&vm.store)[..32], &[0x5a; 32]);
    }
    for _ in 0..2 {
        assert_eq!(copy.call(&mut vm.store, (response, 0, 32)).unwrap(), 4);
        assert_eq!(&memory.data(&vm.store)[..4], "a\u{e9}z".as_bytes());
        assert_eq!(&memory.data(&vm.store)[4..32], &[0x5a; 28]);
    }
    let size = vm.instance.get_typed_func::<Token, i32>(&mut vm.store, "size").unwrap();
    assert!(size.call(&mut vm.store, root).is_err());
    let empty = Some(new_response(&mut vm.store, "").unwrap());
    assert_eq!(copy.call(&mut vm.store, (empty, 65536, 0)).unwrap(), 0);
    assert!(copy.call(&mut vm.store, (empty, 65536, 1)).is_err());
    assert!(checked_response_len(i32::MAX as usize + 1).is_err());
    drop(vm);
    assert!(weak_bytes.upgrade().is_none());
    assert!(weak_issuer.upgrade().is_none(), "Store teardown releases roots and all response owners");
}

#[test]
fn fetch_host_net_derivation_keeps_the_post_dns_floor() {
    let peer = Peer::new(vec![]);
    let port = peer.host.rsplit(':').next().unwrap();
    let origin = format!("http://localhost:{port}");
    let mut vm = vm(Capabilities { net_allow: Some(vec![
        format!("localhost:{port}"), format!("!127.0.0.1:{port}"), format!("![::1]:{port}"),
    ]), net_connect: true, ..Capabilities::none() }, true);
    let net = vm.store.data().nets[0].clone();
    let net = Some(ExternRef::new(&mut vm.store, net).unwrap());
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.write(&mut vm.store, 0, origin.as_bytes()).unwrap();
    let derive = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "derive").unwrap();
    let root = derive.call(&mut vm.store, (net, 0, origin.len() as i32)).unwrap();
    let response = send(&mut vm, root, "GET", &origin, "", "");
    assert!(bytes(&mut vm, response).starts_with(b"WITCHY_FETCH_ERROR:denied:"));
    assert!(peer.finish().is_empty(), "same Net floor must reject resolved loopback before dialing");
    memory.write(&mut vm.store, 0, b"http://127.0.0.1:1").unwrap();
    assert!(derive.call(&mut vm.store, (net, 0, 18)).is_err());
    for (ptr, len) in [(-1, 0), (0, -1), (65535, 2)] {
        assert!(derive.call(&mut vm.store, (net, ptr, len)).is_err());
    }
    memory.write(&mut vm.store, 0, &[0xff]).unwrap();
    assert!(derive.call(&mut vm.store, (net, 0, 1)).is_err());
}

#[test]
fn fetch_host_direct_grants_are_explicit_ordinal_and_empty_is_not_missing() {
    let mut vm = vm(Capabilities::none(), true);
    assert!(mint(vm.store.data(), 0).is_err());
    vm.store.data_mut().fetch_grants = vec![vec![], vec!["http://127.0.0.1:1".into()]];
    assert!(mint(vm.store.data(), 0).is_ok());
    assert!(mint(vm.store.data(), 1).is_ok());
    assert!(mint(vm.store.data(), 2).is_err());
    let empty = grant(&mut vm, 0);
    let denied = send(&mut vm, empty, "GET", "http://127.0.0.1:1/", "", "");
    assert!(bytes(&mut vm, denied).starts_with(b"WITCHY_FETCH_ERROR:denied:"));
    vm.store.data_mut().fetch_grants = vec![vec!["http://user@host".into()]];
    assert!(mint(vm.store.data(), 0).is_err());
}

#[test]
fn fetch_host_https_does_not_downgrade_to_plaintext() {
    // The unchanged rustls provider must see TLS, not an HTTP fallback. This
    // deliberately invalid TLS peer is bounded and joined, with no trust edits.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let thread = std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut first = [0; 5];
                    stream.read_exact(&mut first).unwrap();
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
                    return Some(first);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if stopped.load(Ordering::Acquire) { return None; }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => panic!("TLS peer accept: {e}"),
            }
        }
    });
    let origin = format!("https://{addr}");
    let mut vm = vm(Capabilities { fetch_grants: vec![vec![origin.clone()]], ..Capabilities::none() }, true);
    let root = grant(&mut vm, 0);
    let result = send(&mut vm, root, "GET", &origin, "", "");
    stop.store(true, Ordering::Release);
    let first = thread.join().unwrap().expect("HTTPS attempts TLS");
    assert_eq!(first[0], 0x16, "TLS handshake record, not plaintext GET");
    assert!(bytes(&mut vm, result).starts_with(b"WITCHY_FETCH_ERROR:network:"));
}

#[test]
fn fetch_host_legacy_mint_and_only_keep_authenticated_payload_and_attenuation() {
    let wat = WAT.replacen("(module", r#"(module
      (import "witchy" "mint_fetch" (func $mint (param i32) (result externref)))
      (import "witchy" "fetch_only" (func $only (param externref i32) (result externref)))
      (import "witchy" "mint_net" (func $net (param i32) (result externref)))
      (import "witchy" "net_restrict" (func $restrict (param externref i32) (result externref)))
      (import "witchy" "net_deny" (func $deny (param externref i32) (result externref)))"#, 1)
        .replace("(func (export \"run\"))", r#"
          (func (export "run"))
          (func (export "mint") (param i32) (result externref) local.get 0 call $mint)
          (func (export "only") (param externref i32) (result externref) local.get 0 local.get 1 call $only)
          (func (export "net") (param i32) (result externref) local.get 0 call $net)
          (func (export "restrict") (param externref i32) (result externref) local.get 0 local.get 1 call $restrict)
          (func (export "deny") (param externref i32) (result externref) local.get 0 local.get 1 call $deny)"#);
    let caps = Capabilities {
        fetch_grants: vec![vec!["http://127.0.0.1:1".into()]],
        net_allow: Some(vec!["127.0.0.1:1".into()]), net_connect: true,
        ..Capabilities::none()
    };
    let mut vm = Runtime::new().unwrap().spawn(&wat, caps, 1).unwrap();
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    let mint = vm.instance.get_typed_func::<i32, Token>(&mut vm.store, "mint").unwrap();
    assert!(mint.call(&mut vm.store, -1).is_err());
    assert!(mint.call(&mut vm.store, 1).is_err());
    let root = mint.call(&mut vm.store, 0).unwrap();
    let only = vm.instance.get_typed_func::<(Token, i32), Token>(&mut vm.store, "only").unwrap();
    for (origin, accepted) in [("http://127.0.0.1:1", true), ("http://127.0.0.1:2", false)] {
        memory.write(&mut vm.store, 0, &(origin.len() as u32).to_le_bytes()).unwrap();
        memory.write(&mut vm.store, 4, origin.as_bytes()).unwrap();
        let result = only.call(&mut vm.store, (root, 0));
        assert_eq!(result.is_ok(), accepted);
        if let Ok(Some(token)) = result {
            authenticate_capture(vm.store.data(), token.data(&vm.store).unwrap().unwrap()).unwrap();
        }
    }
    let net = vm.instance.get_typed_func::<i32, Token>(&mut vm.store, "net").unwrap()
        .call(&mut vm.store, 0).unwrap();
    let address = b"127.0.0.1:1";
    memory.write(&mut vm.store, 0, &(address.len() as u32).to_le_bytes()).unwrap();
    memory.write(&mut vm.store, 4, address).unwrap();
    let restricted = vm.instance.get_typed_func::<(Token, i32), Token>(&mut vm.store, "restrict").unwrap()
        .call(&mut vm.store, (net, 0)).unwrap();
    let denied = vm.instance.get_typed_func::<(Token, i32), Token>(&mut vm.store, "deny").unwrap()
        .call(&mut vm.store, (restricted, 0)).unwrap();
    let origin = "http://127.0.0.1:1";
    memory.write(&mut vm.store, 0, origin.as_bytes()).unwrap();
    let derive = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "derive").unwrap();
    assert!(derive.call(&mut vm.store, (restricted, 0, origin.len() as i32)).unwrap().is_some());
    assert!(derive.call(&mut vm.store, (denied, 0, origin.len() as i32)).is_err());
}

fn narrow(vm: &mut Vm, root: Token, origins: &str) -> Result<Token> {
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.write(&mut vm.store, 0, origins.as_bytes())?;
    vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "narrow")?
        .call(&mut vm.store, (root, 0, origins.len() as i32))
}

#[test]
fn fetch_host_canonical_only_preserves_subset_issuer_and_dns_floor() {
    let peer = Peer::new(vec![b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec()]);
    let origin = format!("http://{}", peer.host);
    let port = peer.host.rsplit(':').next().unwrap();
    let dns_origin = format!("http://localhost:{port}");
    let other = "http://127.0.0.1:1";
    let mut vm = vm(Capabilities {
        fetch_grants: vec![vec![origin.clone(), other.into()]],
        net_allow: Some(vec![format!("localhost:{port}"), format!("!127.0.0.1:{port}"), format!("![::1]:{port}")]),
        net_connect: true, ..Capabilities::none()
    }, true);
    let root = grant(&mut vm, 0);
    let selected = narrow(&mut vm, root, &format!("{origin}\n")).unwrap();
    let selected_again = narrow(&mut vm, selected, &origin).unwrap();
    for token in [selected, selected_again] {
        let data = token.unwrap().data(&vm.store).unwrap().unwrap();
        authenticate_capture(vm.store.data(), data).unwrap();
    }
    assert!(narrow(&mut vm, selected, other).is_err(), "cannot recover an origin removed from the parent");
    assert!(narrow(&mut vm, root, "http://not-granted.invalid").is_err());
    assert!(narrow(&mut vm, root, "http://user@host").is_err());
    assert!(narrow(&mut vm, root, other).unwrap().is_some(), "narrowing does not mutate the original root");
    let empty = narrow(&mut vm, selected, "").unwrap();
    let denied = send(&mut vm, empty, "GET", &origin, "", "");
    assert!(bytes(&mut vm, denied).starts_with(b"WITCHY_FETCH_ERROR:denied:"));
    let success = send(&mut vm, selected_again, "GET", &origin, "", "");
    assert_eq!(bytes(&mut vm, success), b"HTTP/1.1 200\r\nContent-Length: 2\r\n\r\nok");

    let net = vm.store.data().nets[0].clone();
    let net = Some(ExternRef::new(&mut vm.store, net).unwrap());
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.write(&mut vm.store, 0, dns_origin.as_bytes()).unwrap();
    let derived = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "derive").unwrap()
        .call(&mut vm.store, (net, 0, dns_origin.len() as i32)).unwrap();
    let narrowed = narrow(&mut vm, derived, &dns_origin).unwrap();
    let denied = send(&mut vm, narrowed, "GET", &dns_origin, "", "");
    assert!(bytes(&mut vm, denied).starts_with(b"WITCHY_FETCH_ERROR:denied:"));
    assert_eq!(peer.finish().len(), 1, "narrowing must retain post-DNS exclusions and perform no I/O itself");
    assert!(vm.store.data().pending.is_none());
    assert!(vm.output().is_empty());
}

#[test]
fn fetch_host_canonical_only_authenticates_before_memory_and_checks_full_slice() {
    let mut no_memory = vm(Capabilities { fetch_grants: vec![vec![]], ..Capabilities::none() }, false);
    let authority = mint(no_memory.store.data(), 0).unwrap();
    let foreign = Some(ExternRef::new(&mut no_memory.store, Authority { issuer: Arc::new(()), ..authority }).unwrap());
    let raw_policy = Some(ExternRef::new(&mut no_memory.store, FetchPolicy::allow(Vec::<String>::new()).unwrap()).unwrap());
    let response = Some(new_response(&mut no_memory.store, "not a root").unwrap());
    let forged = Some(ExternRef::new(&mut no_memory.store, 17_i32).unwrap());
    let call = no_memory.instance.get_typed_func::<(Token, i32, i32), Token>(&mut no_memory.store, "narrow").unwrap();
    for (token, expected) in [(None, "null"), (foreign, "another instance"),
        (raw_policy, "wrong capability kind"), (response, "wrong capability kind"), (forged, "wrong capability kind")] {
        error_contains(call.call(&mut no_memory.store, (token, -1, -1)).unwrap_err(), expected);
    }
    assert!(no_memory.output().is_empty());

    let (mut vm, root) = empty_grant_fixture();
    let memory = vm.instance.get_memory(&mut vm.store, "memory").unwrap();
    memory.write(&mut vm.store, 60000, &[0xff]).unwrap();
    let call = vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "narrow").unwrap();
    for (ptr, len) in [(-1, 0), (0, -1), (65535, 2), (65537, 0), (60000, 1)] {
        assert!(call.call(&mut vm.store, (root, ptr, len)).is_err());
    }
    let empty = call.call(&mut vm.store, (root, 65536, 0)).unwrap().unwrap();
    authenticate_capture(vm.store.data(), empty.data(&vm.store).unwrap().unwrap()).unwrap();
    let issuer = std::mem::replace(&mut vm.store.data_mut().fetch_issuer, Arc::new(()));
    error_contains(call.call(&mut vm.store, (Some(empty), -1, -1)).unwrap_err(), "another instance");
    vm.store.data_mut().fetch_issuer = issuer;
    assert!(call.call(&mut vm.store, (Some(empty), 65536, 0)).unwrap().is_some());
}
