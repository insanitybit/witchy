//! Canonical Net adapters. The existing host owns policy, sockets and TLS.
//!
//! Every fallible operation answers with a Net response (RFC-0126 rule 3).
//! An address the grant does not admit is `Denied`, decided before any socket
//! activity. Unless the grant holds a CIDR or IP pattern, it is decided before
//! any DNS query too; a name such a pattern might admit is resolved to judge
//! it, but every denial answers the same whether or not the name resolves. Any other failure is `Io` with a fixed reason. A message names
//! the address only as the guest spelled it, never a resolved address or
//! operating-system error text.
use crate::runtime::GuestAddress;
use super::*;
use super::policy::{fail, respond, slice_text, Minted, ALREADY_EXISTS, DENIED, IO};
use witchy_caps::capabilities::AdmitFailure;

pub(super) fn link_connect<P: GuestAddress>(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "net_connect_at", connect_at::<P>)?;
    linker.func_wrap("witchy", "net_resolve_at", resolve_at::<P>)?;
    Ok(())
}

/// The canonical adapters are registered unconditionally, because Wasmtime
/// resolves every import before the admitted entry starts - including one
/// reachable only from a helper `main` never calls. Registration therefore
/// cannot carry the Connect/Listen split the legacy families get from their
/// conditional `link_*`, so each verb checks its own right here, where the
/// token is authenticated (SEC-097).
fn require_net_right(granted: bool, verb: &str) -> Result<()> {
    if granted {
        return Ok(());
    }
    bail!("Net grant does not carry the {verb} right")
}

pub(super) fn link_listen<P: GuestAddress>(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "net_listen_at", listen_at::<P>)?;
    linker.func_wrap("witchy", "net_listen_tls_at", listen_tls_at::<P>)?;
    linker.func_wrap("witchy", "net_accept_at", accept_at)?;
    Ok(())
}

pub(super) fn link_socket_io<P: GuestAddress>(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "net_send_at", send_at::<P>)?;
    linker.func_wrap("witchy", "net_send_line_at", send_line_at::<P>)?;
    linker.func_wrap("witchy", "net_recv_line_at", recv_line_at)?;
    linker.func_wrap("witchy", "net_recv_bytes_at", recv_bytes_at)?;
    linker.func_wrap("witchy", "net_recv_all_at", recv_all_at)?;
    Ok(())
}

/// The fixed, guest-visible reason for a network failure.
fn reason(error: &std::io::Error) -> &'static str {
    use std::io::ErrorKind::*;
    match error.kind() {
        ConnectionRefused => "connection refused",
        ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof => "connection reset",
        TimedOut | WouldBlock => "timed out",
        HostUnreachable | NetworkUnreachable | NetworkDown => "unreachable",
        AddrInUse => "address in use",
        AddrNotAvailable => "address not available",
        InvalidData => "invalid data",
        _ => "network failure",
    }
}

/// The admitted targets for `address` (`host:port`), or the failure status
/// and message. `spelled` is the address as the guest wrote it.
fn admit(net: &NetAuthority, address: &str, spelled: &str) -> Result<Vec<std::net::SocketAddr>, (u8, String)> {
    let denied = || (DENIED, format!("`{spelled}` is not permitted by this Net capability"));
    // Only a CIDR or IP pattern can admit an address the grant does not name,
    // so without one the denial needs no DNS query.
    if !witchy_caps::capabilities::net_allows(&net.allow, address) && !admits_by_resolved_address(&net.allow) {
        return Err(denied());
    }
    witchy_caps::capabilities::resolve_admitted_typed(&net.allow, address).map_err(|failure| match failure {
        AdmitFailure::Denied(_) => denied(),
        AdmitFailure::Unresolved(_) => (IO, format!("`{spelled}`: name resolution failed")),
    })
}

/// Whether some allow entry is a CIDR or IP pattern, matched against an
/// address the name resolves to.
fn admits_by_resolved_address(allow: &[String]) -> bool {
    allow.iter().filter(|entry| !entry.starts_with('!')).any(|entry| {
        let host = entry.rsplit_once(':').map_or(entry.as_str(), |(host, _)| host);
        let host = host.trim_start_matches('[').trim_end_matches(']');
        host.contains('/') || host.parse::<std::net::IpAddr>().is_ok()
    })
}

fn socket(stream: Box<dyn Stream>) -> Minted {
    Minted::Socket(SocketResource { stream: Arc::new(Mutex::new(std::io::BufReader::new(stream))) })
}

fn connect_at<P: GuestAddress>(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, ptr: P, len: i32) -> Result<Option<Rooted<ExternRef>>> {
    let net = net_authority_ref(&caller, token)?;
    require_net_right(caller.data().caps.net_connect, "Connect")?;
    let address = slice_text(&mut caller, ptr, len)?;
    let (tls, host_port) = crate::net::parse_scheme(&address);
    let targets = match admit(&net, host_port, &address) {
        Ok(targets) => targets,
        Err((status, message)) => return fail(&mut caller, status, message),
    };
    match crate::net::dial(&targets, tls, host_port) {
        Ok(stream) => respond(&mut caller, 0, Vec::new(), Some(socket(stream))),
        Err(error) => fail(&mut caller, IO, format!("`{address}`: {}", reason(&error))),
    }
}

/// `net_resolve_at(net, host) -> response`: the host's addresses, NUL-joined.
/// A host the grant does not name is `Denied` before any DNS query.
fn resolve_at<P: GuestAddress>(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, ptr: P, len: i32) -> Result<Option<Rooted<ExternRef>>> {
    let net = net_authority_ref(&caller, token)?;
    require_net_right(caller.data().caps.net_connect, "Connect")?;
    let host = slice_text(&mut caller, ptr, len)?;
    match witchy_caps::capabilities::resolve_host_admitted_typed(&net.allow, &host) {
        Ok(ips) => {
            let joined = ips.iter().map(|ip| ip.to_string()).collect::<Vec<_>>().join("\u{0}");
            respond(&mut caller, 0, joined.into_bytes(), None)
        }
        Err(AdmitFailure::Denied(_)) => fail(&mut caller, DENIED, format!("`{host}` is not permitted by this Net capability")),
        Err(AdmitFailure::Unresolved(_)) => fail(&mut caller, IO, format!("`{host}`: name resolution failed")),
    }
}

/// `net_listen_tls_at(net, addr, cert_pem, key) -> response`: `listen_at`
/// serving TLS. The key is an opaque `Secret`, sealed ones included, so its
/// bytes never enter guest memory. An address the grant does not admit is
/// `Denied` before the certificate is parsed or a socket opened; an unusable
/// certificate or key is `Io` with a fixed reason.
fn listen_tls_at<P: GuestAddress>(
    mut caller: Caller<'_, VmState>,
    token: Option<Rooted<ExternRef>>,
    address_ptr: P, address_len: i32,
    cert_ptr: P, cert_len: i32,
    key: Option<Rooted<ExternRef>>,
) -> Result<Option<Rooted<ExternRef>>> {
    let net = net_authority_ref(&caller, token)?;
    require_net_right(caller.data().caps.net_listen, "Listen")?;
    let key = secret_material_ref(&caller, key)?.bytes;
    let address = slice_text(&mut caller, address_ptr, address_len)?;
    let certificate = slice_text(&mut caller, cert_ptr, cert_len)?;
    if let Some(entry) = caller.data().worker_listener.clone()
        && worker_listener_matches(&entry, &net.allow, &address, true)
    {
        let (listener, tls) = entry;
        return respond(&mut caller, 0, Vec::new(), Some(Minted::Listener(ListenerResource { listener, tls })));
    }
    let targets = match admit(&net, &address, &address) {
        Ok(targets) => targets,
        Err((status, message)) => return fail(&mut caller, status, message),
    };
    let Ok(config) = crate::net::server_tls_config(&certificate, &key) else {
        return fail(&mut caller, IO, format!("`{address}`: invalid TLS certificate or key"));
    };
    match std::net::TcpListener::bind(targets.as_slice()) {
        Ok(listener) => {
            let listener = ListenerResource { listener: Arc::new(listener), tls: Some(config) };
            respond(&mut caller, 0, Vec::new(), Some(Minted::Listener(listener)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            fail(&mut caller, ALREADY_EXISTS, format!("`{address}` is already in use"))
        }
        Err(error) => fail(&mut caller, IO, format!("`{address}`: {}", reason(&error))),
    }
}

/// `net_listen_at(net, addr) -> response`. An address already bound is
/// `AlreadyExists`.
fn listen_at<P: GuestAddress>(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, ptr: P, len: i32) -> Result<Option<Rooted<ExternRef>>> {
    let net = net_authority_ref(&caller, token)?;
    require_net_right(caller.data().caps.net_listen, "Listen")?;
    let address = slice_text(&mut caller, ptr, len)?;
    if let Some(entry) = caller.data().worker_listener.clone()
        && worker_listener_matches(&entry, &net.allow, &address, false)
    {
        let (listener, tls) = entry;
        return respond(&mut caller, 0, Vec::new(), Some(Minted::Listener(ListenerResource { listener, tls })));
    }
    let targets = match admit(&net, &address, &address) {
        Ok(targets) => targets,
        Err((status, message)) => return fail(&mut caller, status, message),
    };
    match std::net::TcpListener::bind(targets.as_slice()) {
        Ok(listener) => {
            let listener = ListenerResource { listener: Arc::new(listener), tls: None };
            respond(&mut caller, 0, Vec::new(), Some(Minted::Listener(listener)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            fail(&mut caller, ALREADY_EXISTS, format!("`{address}` is already in use"))
        }
        Err(error) => fail(&mut caller, IO, format!("`{address}`: {}", reason(&error))),
    }
}

/// `net_accept_at(listener) -> response`: block for a client. A TLS
/// listener handshakes host-side first; a failed handshake drops that client
/// and keeps waiting.
fn accept_at(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<Option<Rooted<ExternRef>>> {
    let ListenerResource { listener, tls } = listener_resource_ref(&caller, token)?;
    loop {
        let stream = match listener.accept() {
            Ok((stream, _peer)) => stream,
            Err(error) => return fail(&mut caller, IO, format!("accept failed: {}", reason(&error))),
        };
        // Bound how long a stalled peer can pin this worker, before the TLS
        // handshake so a slow handshake is bounded too.
        crate::net::prepare_accepted_socket(&stream);
        let stream: Box<dyn Stream> = match &tls {
            None => Box::new(stream),
            Some(config) => match crate::net::accept_tls(config.clone(), stream) {
                Ok(stream) => stream,
                Err(_) => continue,
            },
        };
        return respond(&mut caller, 0, Vec::new(), Some(socket(stream)));
    }
}

fn send<P: GuestAddress>(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, ptr: P, len: i32, line: bool) -> Result<Option<Rooted<ExternRef>>> {
    use std::io::Write;
    let socket = socket_resource_ref(&caller, token)?;
    let text = slice_text(&mut caller, ptr, len)?;
    let sent = {
        let mut stream = lock_socket(&socket)?;
        let stream = stream.get_mut();
        stream.write_all(text.as_bytes()).and_then(|()| if line { stream.write_all(b"\n") } else { Ok(()) })
    };
    match sent {
        Ok(()) => respond(&mut caller, 0, Vec::new(), None),
        Err(error) => fail(&mut caller, IO, format!("send failed: {}", reason(&error))),
    }
}

fn send_at<P: GuestAddress>(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, ptr: P, len: i32) -> Result<Option<Rooted<ExternRef>>> {
    send(caller, token, ptr, len, false)
}

fn send_line_at<P: GuestAddress>(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, ptr: P, len: i32) -> Result<Option<Rooted<ExternRef>>> {
    send(caller, token, ptr, len, true)
}

/// A receive's answer: the (lossily decoded) bytes, or `Io`.
fn received(caller: &mut Caller<'_, VmState>, bytes: std::io::Result<Vec<u8>>) -> Result<Option<Rooted<ExternRef>>> {
    match bytes {
        Ok(bytes) => respond(caller, 0, String::from_utf8_lossy(&bytes).into_owned().into_bytes(), None),
        Err(error) => fail(caller, IO, format!("receive failed: {}", reason(&error))),
    }
}

fn recv_line_at(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<Option<Rooted<ExternRef>>> {
    let socket = socket_resource_ref(&caller, token)?;
    let line = crate::net::read_line_capped(&mut *lock_socket(&socket)?).map(|mut bytes| {
        if bytes.last() == Some(&b'\n') { bytes.pop(); }
        bytes
    });
    received(&mut caller, line)
}

fn recv_bytes_at(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, count: i64) -> Result<Option<Rooted<ExternRef>>> {
    use std::io::Read;
    let socket = socket_resource_ref(&caller, token)?;
    // `count` is guest-supplied: read in bounded chunks so memory tracks the
    // bytes actually received, never the claimed count, and stop at the same
    // host cap as `recv_all` -- a peer streaming into `recv_bytes(huge)` must
    // not grow host memory past the guest's own limits. Like EOF or a read
    // timeout, the cap ends the read short. The response holding the bytes is
    // an externref the checked caller consumes at once and never stores, so
    // responses cannot accumulate.
    let want = usize::try_from(count.max(0)).unwrap_or(usize::MAX)
        .min(crate::net::MAX_RECV_BYTES as usize);
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    let outcome = {
        let mut stream = lock_socket(&socket)?;
        loop {
            if bytes.len() >= want { break Ok(()); }
            let amount = (want - bytes.len()).min(chunk.len());
            match stream.read(&mut chunk[..amount]) {
                Ok(0) => break Ok(()),
                Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                Err(error) if crate::net::is_read_timeout(&error) => break Ok(()),
                Err(error) => break Err(error),
            }
        }
    };
    received(&mut caller, outcome.map(|()| bytes))
}

fn recv_all_at(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<Option<Rooted<ExternRef>>> {
    let socket = socket_resource_ref(&caller, token)?;
    let bytes = receive_all(&mut *lock_socket(&socket)?, crate::net::MAX_RECV_BYTES);
    received(&mut caller, bytes)
}

// Decode once after EOF/timeout: chunked decoding would corrupt UTF-8 split
// across chunks and count decoded bytes against the raw-byte cap.
fn receive_all(reader: &mut impl std::io::Read, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    match reader.take(limit + 1).read_to_end(&mut bytes) {
        Ok(_) => {}
        Err(error) if crate::net::is_read_timeout(&error) => {}
        Err(error) => return Err(error),
    }
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "receive exceeded the byte cap"));
    }
    Ok(bytes)
}
