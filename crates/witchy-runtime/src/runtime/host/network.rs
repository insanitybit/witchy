use std::sync::{Arc, Mutex};

use wasmtime::{bail, Caller, Error, ExternRef, Linker, Result, Rooted};

use super::super::{
    memory_of, read_wstr, ListenerResource, NetAuthority, SocketResource, VmState,
};
use super::secret::secret_material_ref;
use super::vm_worker::listener_resource_ref;
use crate::net::Stream;

/// Register the root `Net` grant mint (RFC-0005 stage 3).
pub(in crate::runtime) fn link_mint(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "mint_net", host_mint_net)?;
    Ok(())
}

/// Register the Connect-right family, including resolve-then-pin (RFC-0020):
/// inspect a name's IPs, then dial an exact IP with the hostname carried only
/// for SNI/Host. Pinned dialing is gated with connect because it adds no
/// authority — `connect_pinned` re-checks the pinned IP against the same
/// allowlist.
pub(in crate::runtime) fn link_connect(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "net_restrict", host_net_restrict)?;
    linker.func_wrap("witchy", "net_deny", host_net_deny)?;
    linker.func_wrap("witchy", "net_connect", host_net_connect)?;
    linker.func_wrap("witchy", "net_try_connect", host_net_try_connect)?;
    linker.func_wrap("witchy", "net_resolve_size", host_net_resolve_size)?;
    linker.func_wrap("witchy", "net_connect_pinned", host_net_connect_pinned)?;
    linker.func_wrap("witchy", "net_try_connect_pinned", host_net_try_connect_pinned)?;
    Ok(())
}

/// Register the Listen-right family. HTTPS listen (RFC-0060) uses the same
/// Listen right plus a Secret externref resolved host-side; null or
/// wrong-host-data refs fail loudly at use. (`serve_pool` shares this gate but
/// is worker-pool machinery and registers in the kernel, after these.)
pub(in crate::runtime) fn link_listen(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "net_listen", host_net_listen)?;
    linker.func_wrap("witchy", "net_listen_tls", host_net_listen_tls)?;
    linker.func_wrap("witchy", "net_accept", host_net_accept)?;
    Ok(())
}

/// Register socket I/O. It carries no authority of its own (a socket is only
/// obtainable through a granted connect or accept), so the caller links it
/// under either verb.
pub(in crate::runtime) fn link_socket_io(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "net_send_line", host_net_send_line)?;
    linker.func_wrap("witchy", "net_send_bytes", host_net_send_bytes)?;
    linker.func_wrap("witchy", "net_recv_line_len", host_net_recv_line_len)?;
    linker.func_wrap("witchy", "net_recv_all_len", host_net_recv_all_len)?;
    linker.func_wrap("witchy", "net_recv_bytes_len", host_net_recv_bytes_len)?;
    linker.func_wrap("witchy", "net_close", host_net_close)?;
    Ok(())
}

// --- the Net capability family ---
//
// A guest `Net`, `Socket`, or `Listener` is an opaque externref carrying a
// host-side authority object. There is no guest-visible integer handle to forge,
// widen, or swap through linear-memory corruption. The address allowlist check is
// the SAME rule the interpreter applies, so the backends still agree on reachability.

fn net_grant_by_ordinal(caller: &Caller<'_, VmState>, h: i32) -> Result<NetAuthority> {
    caller
        .data()
        .nets
        .get(h as usize)
        .cloned()
        .ok_or_else(|| Error::msg(format!("invalid Net grant index {h}")))
}

fn net_authority_ref(caller: &Caller<'_, VmState>, n: Option<Rooted<ExternRef>>) -> Result<NetAuthority> {
    let n = n.ok_or_else(|| Error::msg("Net externref is null"))?;
    n.data(caller)?
        .ok_or_else(|| Error::msg("Net externref has no host data"))?
        .downcast_ref::<NetAuthority>()
        .cloned()
        .ok_or_else(|| Error::msg("Net externref has wrong host data"))
}

fn socket_resource_ref(
    caller: &Caller<'_, VmState>,
    s: Option<Rooted<ExternRef>>,
) -> Result<SocketResource> {
    let s = s.ok_or_else(|| Error::msg("Socket externref is null"))?;
    s.data(caller)?
        .ok_or_else(|| Error::msg("Socket externref has no host data"))?
        .downcast_ref::<SocketResource>()
        .cloned()
        .ok_or_else(|| Error::msg("Socket externref has wrong host data"))
}

fn host_mint_net(mut caller: Caller<'_, VmState>, i: i32) -> Result<Option<Rooted<ExternRef>>> {
    let net = net_grant_by_ordinal(&caller, i)?;
    ExternRef::new(&mut caller, net).map(Some)
}

fn socket_ref(
    caller: &mut Caller<'_, VmState>,
    stream: Box<dyn Stream>,
) -> Result<Option<Rooted<ExternRef>>> {
    ExternRef::new(caller, SocketResource {
        stream: Arc::new(Mutex::new(std::io::BufReader::new(stream))),
    }).map(Some)
}

fn listener_ref(
    caller: &mut Caller<'_, VmState>,
    listener: std::sync::Arc<std::net::TcpListener>,
    tls: Option<crate::net::ServerTlsConfig>,
) -> Result<Option<Rooted<ExternRef>>> {
    ExternRef::new(caller, ListenerResource { listener, tls }).map(Some)
}

/// `net_restrict(net, addr) -> Net`: attenuate to the allowlisted address set.
/// `addr` may carry several patterns newline-joined (a `confine.union`); each
/// must already be admitted.
fn host_net_restrict(
    mut caller: Caller<'_, VmState>,
    net_ref: Option<Rooted<ExternRef>>,
    addr_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let net = net_authority_ref(&caller, net_ref)?;
    let narrowed = witchy_caps::capabilities::net_only(&net.allow, &addr)
        .map_err(|p| Error::msg(format!("restrict: `{p}` is not in this Net capability")))?;
    ExternRef::new(&mut caller, NetAuthority { allow: narrowed }).map(Some)
}

/// `net_deny(net, addr) -> Net`: subtract an address pattern (RFC-0011
/// `net.deny`), or several newline-joined (a `confine.union`). A monotone
/// exclusion recorded as `!`-prefixed entries the shared `net_allows` honours.
fn host_net_deny(
    mut caller: Caller<'_, VmState>,
    net_ref: Option<Rooted<ExternRef>>,
    addr_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let mut allow = net_authority_ref(&caller, net_ref)?.allow;
    for p in addr.split('\n') {
        allow.push(format!("!{p}"));
    }
    ExternRef::new(&mut caller, NetAuthority { allow }).map(Some)
}

/// `net_connect(net, addr) -> Socket`: dial an allowlisted address.
fn host_net_connect(
    mut caller: Caller<'_, VmState>,
    net_ref: Option<Rooted<ExternRef>>,
    addr_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let net = net_authority_ref(&caller, net_ref)?;
    let (tls, host_port) = crate::net::parse_scheme(&addr);
    let targets = witchy_caps::capabilities::resolve_admitted(&net.allow, host_port)
        .map_err(|e| Error::msg(format!("connect: {e}")))?;
    let stream = crate::net::dial(&targets, tls, host_port)
        .map_err(|e| Error::msg(format!("connect to `{addr}` failed: {e}")))?;
    socket_ref(&mut caller, stream)
}

/// `net_try_connect(net, addr) -> Option(Socket)`: failed connection yields null
/// (Witchy's nullable-externref `None`) instead of trapping. A capability
/// violation still traps: that is a policy breach, not transient network weather.
fn host_net_try_connect(
    mut caller: Caller<'_, VmState>,
    net_ref: Option<Rooted<ExternRef>>,
    addr_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let net = net_authority_ref(&caller, net_ref)?;
    let (tls, host_port) = crate::net::parse_scheme(&addr);
    let targets = witchy_caps::capabilities::resolve_admitted(&net.allow, host_port)
        .map_err(|e| Error::msg(format!("try_connect: {e}")))?;
    match crate::net::dial(&targets, tls, host_port) {
        Ok(stream) => socket_ref(&mut caller, stream),
        Err(_) => Ok(None),
    }
}

/// `net_resolve_size(net, host_ptr) -> bytes` (RFC-0020): resolve `host` to its
/// current IP literals NOW, stage them, and report the `List(String)` byte size
/// the guest reserves. Resolve itself adds no authority, but the guest must still
/// pass a real `Net` externref so null/forged values fail closed.
fn host_net_resolve_size(
    mut caller: Caller<'_, VmState>,
    net_ref: Option<Rooted<ExternRef>>,
    host_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let host = read_wstr(mem.data(&caller), host_ptr)?;
    let _net = net_authority_ref(&caller, net_ref)?;
    let ips = crate::net::resolve_ips(&host);
    let size = 4 + 8 * ips.len() + ips.iter().map(|s| 4 + s.len()).sum::<usize>();
    caller.data_mut().pending_list = Some(ips);
    Ok(size as i32)
}

/// `net_connect_pinned(net, ip, host, port, secure) -> Socket` (RFC-0020): dial
/// the EXACT `ip:port` with `host` carried only for TLS SNI / `Host`.
fn host_net_connect_pinned(
    mut caller: Caller<'_, VmState>,
    net_ref: Option<Rooted<ExternRef>>,
    ip_ptr: i32,
    host_ptr: i32,
    port: i64,
    secure: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let ip = read_wstr(mem.data(&caller), ip_ptr)?;
    let host = read_wstr(mem.data(&caller), host_ptr)?;
    let net = net_authority_ref(&caller, net_ref)?;
    let ip_port = crate::net::authority(&ip, port);
    let targets = witchy_caps::capabilities::resolve_admitted(&net.allow, &ip_port)
        .map_err(|e| Error::msg(format!("connect_pinned: {e}")))?;
    let host_port = crate::net::authority(&host, port);
    let stream = crate::net::dial(&targets, secure != 0, &host_port)
        .map_err(|e| Error::msg(format!("connect_pinned to `{ip_port}` failed: {e}")))?;
    socket_ref(&mut caller, stream)
}

/// `net_try_connect_pinned(...) -> Option(Socket)`: like `connect_pinned`, but a
/// failed connection yields nullable-externref `None`. Capability violations trap.
fn host_net_try_connect_pinned(
    mut caller: Caller<'_, VmState>,
    net_ref: Option<Rooted<ExternRef>>,
    ip_ptr: i32,
    host_ptr: i32,
    port: i64,
    secure: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let ip = read_wstr(mem.data(&caller), ip_ptr)?;
    let host = read_wstr(mem.data(&caller), host_ptr)?;
    let net = net_authority_ref(&caller, net_ref)?;
    let ip_port = crate::net::authority(&ip, port);
    let targets = witchy_caps::capabilities::resolve_admitted(&net.allow, &ip_port)
        .map_err(|e| Error::msg(format!("try_connect_pinned: {e}")))?;
    let host_port = crate::net::authority(&host, port);
    match crate::net::dial(&targets, secure != 0, &host_port) {
        Ok(stream) => socket_ref(&mut caller, stream),
        Err(_) => Ok(None),
    }
}

/// `net_listen(net, addr) -> Listener`: bind an allowlisted address.
fn host_net_listen(
    mut caller: Caller<'_, VmState>,
    net_ref: Option<Rooted<ExternRef>>,
    addr_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let net = net_authority_ref(&caller, net_ref)?;
    if !witchy_caps::capabilities::net_allows(&net.allow, &addr) {
        bail!("listen: `{addr}` is not permitted by this Net capability");
    }
    let shared = caller.data().worker_listener.clone();
    let (listener, tls) = match shared {
        Some(entry) => entry,
        None => (
            std::sync::Arc::new(
                std::net::TcpListener::bind(&addr)
                    .map_err(|e| Error::msg(format!("listen on `{addr}` failed: {e}")))?,
            ),
            None,
        ),
    };
    listener_ref(&mut caller, listener, tls)
}

/// (RFC-0060) `net_listen_tls(net, addr, cert_pem, key) -> Listener`: bind an
/// allowlisted address for HTTPS. The private key is resolved from an opaque
/// Secret externref, so key bytes never enter guest memory.
fn host_net_listen_tls(
    mut caller: Caller<'_, VmState>,
    net_ref: Option<Rooted<ExternRef>>,
    addr_ptr: i32,
    cert_ptr: i32,
    key_ref: Option<Rooted<ExternRef>>,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let cert_pem = read_wstr(mem.data(&caller), cert_ptr)?;
    let net = net_authority_ref(&caller, net_ref)?;
    if !witchy_caps::capabilities::net_allows(&net.allow, &addr) {
        bail!("listen: `{addr}` is not permitted by this Net capability");
    }
    let key_bytes = secret_material_ref(&caller, key_ref)?.bytes;
    let shared = caller.data().worker_listener.clone();
    let (listener, tls) = match shared {
        Some(entry) => entry,
        None => {
            let config = crate::net::server_tls_config(&cert_pem, &key_bytes).map_err(Error::msg)?;
            (
                std::sync::Arc::new(
                    std::net::TcpListener::bind(&addr)
                        .map_err(|e| Error::msg(format!("listen on `{addr}` failed: {e}")))?,
                ),
                Some(config),
            )
        }
    };
    listener_ref(&mut caller, listener, tls)
}

/// `net_accept(listener) -> Socket`: block for a client connection. TLS listeners
/// handshake host-side before minting the returned socket.
fn host_net_accept(
    mut caller: Caller<'_, VmState>,
    listener_ref_: Option<Rooted<ExternRef>>,
) -> Result<Option<Rooted<ExternRef>>> {
    let ListenerResource { listener, tls } = listener_resource_ref(&caller, listener_ref_)?;
    loop {
        let (stream, _peer) = listener
            .accept()
            .map_err(|e| Error::msg(format!("accept failed: {e}")))?;
        let stream: Box<dyn Stream> = match &tls {
            None => Box::new(stream),
            Some(config) => match crate::net::accept_tls(config.clone(), stream) {
                Ok(tls_stream) => tls_stream,
                Err(_) => continue,
            },
        };
        return socket_ref(&mut caller, stream);
    }
}

fn lock_socket(
    socket: &SocketResource,
) -> Result<std::sync::MutexGuard<'_, std::io::BufReader<Box<dyn Stream>>>> {
    socket
        .stream
        .lock()
        .map_err(|_| Error::msg("socket lock poisoned"))
}

/// `net_send_line(sock, s)`: write the string and a trailing newline.
fn host_net_send_line(
    mut caller: Caller<'_, VmState>,
    socket_ref_: Option<Rooted<ExternRef>>,
    line_ptr: i32,
) -> Result<()> {
    use std::io::Write;
    let mem = memory_of(&mut caller)?;
    let line = read_wstr(mem.data(&caller), line_ptr)?;
    let socket = socket_resource_ref(&caller, socket_ref_)?;
    let mut sock = lock_socket(&socket)?;
    sock.get_mut()
        .write_all(line.as_bytes())
        .and_then(|_| sock.get_mut().write_all(b"\n"))
        .map_err(|e| Error::msg(format!("send failed: {e}")))
}

/// `net_send_bytes(sock, s)`: write the exact bytes, no framing added.
fn host_net_send_bytes(
    mut caller: Caller<'_, VmState>,
    socket_ref_: Option<Rooted<ExternRef>>,
    ptr: i32,
) -> Result<()> {
    use std::io::Write;
    let mem = memory_of(&mut caller)?;
    let s = read_wstr(mem.data(&caller), ptr)?;
    let socket = socket_resource_ref(&caller, socket_ref_)?;
    let mut sock = lock_socket(&socket)?;
    sock.get_mut()
        .write_all(s.as_bytes())
        .map_err(|e| Error::msg(format!("send failed: {e}")))
}

/// `net_recv_line_len(sock) -> len`: read one line NOW (newline trimmed, like
/// the interpreter), stage it, and report its length for `fill_pending`.
fn host_net_recv_line_len(
    mut caller: Caller<'_, VmState>,
    socket_ref_: Option<Rooted<ExternRef>>,
) -> Result<i32> {
    let socket = socket_resource_ref(&caller, socket_ref_)?;
    let raw = {
        let mut sock = lock_socket(&socket)?;
        crate::net::read_line_capped(&mut *sock).map_err(|e| Error::msg(format!("recv failed: {e}")))?
    };
    let line = String::from_utf8_lossy(&raw);
    let trimmed = line.trim_end_matches('\n').to_string();
    let len = trimmed.len() as i32;
    caller.data_mut().pending = Some(trimmed.into_bytes());
    Ok(len)
}

/// `net_recv_all_len(sock) -> len`: read to EOF NOW (lossy UTF-8, like the
/// interpreter), stage it, and report its length.
fn host_net_recv_all_len(
    mut caller: Caller<'_, VmState>,
    socket_ref_: Option<Rooted<ExternRef>>,
) -> Result<i32> {
    use std::io::Read;
    let mut buf = Vec::new();
    let socket = socket_resource_ref(&caller, socket_ref_)?;
    {
        let mut sock = lock_socket(&socket)?;
        // (SEC-035) Cap the read so a peer streaming without EOF can't OOM the host. Read one
        // byte past the cap to detect overflow, then fail closed.
        sock.by_ref()
            .take(crate::net::MAX_RECV_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(|e| Error::msg(format!("recv failed: {e}")))?;
    }
    if buf.len() as u64 > crate::net::MAX_RECV_BYTES {
        bail!("recv_all exceeded the {}-byte cap", crate::net::MAX_RECV_BYTES);
    }
    let s = String::from_utf8_lossy(&buf).into_owned();
    let len = s.len() as i32;
    caller.data_mut().pending = Some(s.into_bytes());
    Ok(len)
}

/// `net_recv_bytes_len(sock, n) -> len`: read exactly `n` bytes (fewer only on
/// early EOF, matching the interpreter), stage them lossily, report the length.
fn host_net_recv_bytes_len(
    mut caller: Caller<'_, VmState>,
    socket_ref_: Option<Rooted<ExternRef>>,
    n: i64,
) -> Result<i32> {
    use std::io::Read;
    let socket = socket_resource_ref(&caller, socket_ref_)?;
    let want = n.max(0) as usize;
    // `want` is guest-supplied (up to i64::MAX); do NOT pre-allocate it — that would let a
    // module request `sock.recv_bytes(huge)` and ABORT the host with a multi-GB/EB Vec even
    // when the socket holds almost nothing. Read in bounded chunks so memory tracks the bytes
    // actually received, not the claimed count.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    {
        let mut sock = lock_socket(&socket)?;
        while buf.len() < want {
            let to_read = (want - buf.len()).min(chunk.len());
            match sock.read(&mut chunk[..to_read]) {
                Ok(0) => break,
                Ok(k) => buf.extend_from_slice(&chunk[..k]),
                Err(e) => bail!("recv failed: {e}"),
            }
        }
    }
    let s = String::from_utf8_lossy(&buf).into_owned();
    let len = s.len() as i32;
    caller.data_mut().pending = Some(s.into_bytes());
    Ok(len)
}

/// `net_close(sock)`: shut the connection down (idempotent).
fn host_net_close(
    caller: Caller<'_, VmState>,
    socket_ref_: Option<Rooted<ExternRef>>,
) -> Result<()> {
    let socket = socket_resource_ref(&caller, socket_ref_)?;
    lock_socket(&socket)?.get_mut().shutdown();
    Ok(())
}
