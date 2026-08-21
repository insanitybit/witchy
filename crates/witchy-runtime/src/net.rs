//! Host-side networking shared by both backends: the `Stream` socket abstraction and
//! the dialer that completes a plain or TLS connection.
//!
//! TLS is the `tls:` address scheme (RFC-0009): `net.connect("tls:host:443")` performs
//! a TLS handshake terminated HERE, host-side, so the guest's `send`/`recv` see a
//! decrypted byte stream and the compiled (WASM) backend needs no new import — it
//! already routes `connect`/`send`/`recv` through these host ops. rustls is configured
//! with the aws_lc_rs provider, so ALL crypto stays on aws-lc (FIPS), never ring.

use std::io::{Read, Write};
use std::net::TcpStream;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "test-support")))]
use std::sync::{LazyLock, Mutex};

/// A connected byte stream behind a socket handle. Plain `TcpStream` and TLS
/// `rustls::StreamOwned` both satisfy it, so both backends keep plain and `tls:`
/// sockets in one table — `send`/`recv`/`close` operate on either without knowing which.
pub trait Stream: Read + Write + Send {
    /// Close both directions (idempotent). Plain → `TcpStream::shutdown`; TLS sends a
    /// close-notify first so the peer sees a clean shutdown rather than a truncation.
    fn shutdown(&mut self);
}

impl Stream for TcpStream {
    fn shutdown(&mut self) {
        let _ = TcpStream::shutdown(self, std::net::Shutdown::Both);
    }
}

// TLS (the `tls:` scheme) is native-only — rustls/aws-lc build C and have no
// wasm32 support, and the browser playground has no raw sockets anyway.
#[cfg(not(target_arch = "wasm32"))]
type TlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

#[cfg(not(target_arch = "wasm32"))]
impl Stream for TlsStream {
    fn shutdown(&mut self) {
        self.conn.send_close_notify();
        let _ = self.flush();
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
    }
}

// ---- Server-side TLS (RFC-0060) ----
//
// `server.serve_tls` terminates TLS HERE, host-side, exactly like the client
// `tls:` scheme above: the guest's `accept` yields an ordinary Socket handle over
// a decrypted stream, and — because the private key arrives as a `Secret`
// consumed BY HANDLE — the key bytes never enter guest memory. This module is
// the ONE implementation both backends call, so they cannot drift.

/// A listener's server-side TLS state: the rustls config built once at listen
/// time (unit on the wasm target, where TLS serving is unavailable).
#[cfg(not(target_arch = "wasm32"))]
pub type ServerTlsConfig = Arc<rustls::ServerConfig>;
/// The wasm build has no rustls; the constructor below always errors there.
#[cfg(target_arch = "wasm32")]
pub type ServerTlsConfig = ();

/// The server half of a TLS stream (mirrors the client `TlsStream` above).
#[cfg(not(target_arch = "wasm32"))]
type ServerTlsStream = rustls::StreamOwned<rustls::ServerConnection, TcpStream>;

#[cfg(not(target_arch = "wasm32"))]
impl Stream for ServerTlsStream {
    fn shutdown(&mut self) {
        self.conn.send_close_notify();
        let _ = self.flush();
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
    }
}

/// Build the rustls `ServerConfig` for `serve_tls` from a PEM certificate chain
/// and the private key's raw bytes (PEM-encoded PKCS#8, PKCS#1, or SEC1 — the
/// on-disk formats `openssl`/`rcgen` produce). Errors are LOUD and specific:
/// per RFC-0060's error policy a malformed cert, a malformed key, or a key that
/// does not match the certificate fails at listen time (startup), never at first
/// connection — `with_single_cert` verifies the key's SubjectPublicKeyInfo
/// against the end-entity certificate's.
#[cfg(not(target_arch = "wasm32"))]
pub fn server_tls_config(cert_pem: &str, key_bytes: &[u8]) -> Result<ServerTlsConfig, String> {
    let mut cert_reader = cert_pem.as_bytes();
    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("serve_tls: malformed certificate PEM: {e}"))?;
    if certs.is_empty() {
        return Err("serve_tls: no certificates found in the PEM chain".to_string());
    }
    let mut key_reader = key_bytes;
    let key = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|e| format!("serve_tls: malformed private-key PEM in the granted secret: {e}"))?
        .ok_or_else(|| {
            "serve_tls: the granted secret holds no private key (expected a PEM-encoded PKCS#8/PKCS#1/SEC1 key)"
                .to_string()
        })?;
    let config = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::aws_lc_rs::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .expect("aws-lc provider supports the default TLS protocol versions")
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .map_err(|e| format!("serve_tls: certificate/key rejected: {e}"))?;
    Ok(Arc::new(config))
}

/// TLS serving is native-only (rustls/aws-lc build C); the wasm build refuses at
/// listen time — the same loud startup-failure shape as a bad key.
#[cfg(target_arch = "wasm32")]
pub fn server_tls_config(_cert_pem: &str, _key_bytes: &[u8]) -> Result<ServerTlsConfig, String> {
    Err("serve_tls is unavailable in the wasm build".to_string())
}

/// Complete a server-side TLS handshake on an accepted connection, returning the
/// decrypted stream for the socket table. The handshake is driven EAGERLY here so
/// a failure (a plaintext client, a bad ClientHello, an unsupported version)
/// surfaces now — the accept loop drops that connection and keeps serving, per
/// RFC-0060's error policy: per-connection TLS failures are the network's
/// weather, not program errors.
#[cfg(not(target_arch = "wasm32"))]
pub fn accept_tls(config: ServerTlsConfig, tcp: TcpStream) -> std::io::Result<Box<dyn Stream>> {
    let conn = rustls::ServerConnection::new(config).map_err(std::io::Error::other)?;
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    while tls.conn.is_handshaking() {
        tls.conn.complete_io(&mut tls.sock)?;
    }
    Ok(Box::new(tls))
}

/// No TLS listener can exist on wasm (`server_tls_config` refused already).
#[cfg(target_arch = "wasm32")]
pub fn accept_tls(_config: ServerTlsConfig, _tcp: TcpStream) -> std::io::Result<Box<dyn Stream>> {
    Err(std::io::Error::other("serve_tls is unavailable in the wasm build"))
}

/// Split an optional `tls:` scheme off an address: `"tls:github.com:443"` →
/// `(true, "github.com:443")`; a bare `host:port` → `(false, "host:port")`. The scheme
/// is a connect-time choice; the capability allowlist governs the bare `host:port` (TLS
/// is strictly safer than plain, so permitting the endpoint and electing TLS is sound).
pub fn parse_scheme(addr: &str) -> (bool, &str) {
    match addr.strip_prefix("tls:") {
        Some(rest) => (true, rest),
        None => (false, addr),
    }
}

/// Resolve a hostname to its current IP literals (deduped, order-preserving). Backs
/// `net.resolve` (RFC-0020) so a program can inspect addresses before pinning one; it
/// adds no authority, since `connect_pinned` re-checks the chosen IP against the Net
/// allowlist. An empty result signals a resolution failure to the caller.
#[cfg(not(target_arch = "wasm32"))]
pub fn resolve_ips(host: &str) -> Vec<String> {
    use std::net::ToSocketAddrs;
    let bare = host.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(host);
    let mut ips: Vec<String> = Vec::new();
    if let Ok(addrs) = (bare, 0u16).to_socket_addrs() {
        for a in addrs {
            let ip = a.ip().to_string();
            if !ips.contains(&ip) {
                ips.push(ip);
            }
        }
    }
    ips
}

/// The browser shim has no DNS (deny-by-omission): resolution yields nothing.
#[cfg(target_arch = "wasm32")]
pub fn resolve_ips(_host: &str) -> Vec<String> {
    Vec::new()
}

/// Build a `host:port` authority, bracketing a bare IPv6 literal (`::1` -> `[::1]:80`) so
/// the last-colon `host:port` split stays unambiguous. Shared by `connect_pinned` on both
/// backends so a pinned IPv6 dial is byte-identical.
pub fn authority(host: &str, port: i64) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// The SNI / certificate name for a `host:port` (the host, without the port).
#[cfg(not(target_arch = "wasm32"))]
fn server_name(host_port: &str) -> &str {
    host_port.rsplit_once(':').map(|(h, _)| h).unwrap_or(host_port)
}

/// Dial `targets` (already resolved and allowlist-checked). When `tls`, complete a TLS
/// handshake that validates the server certificate against `host_port`'s name. Returns
/// the boxed stream for a backend's socket table.
pub fn dial(
    targets: &[std::net::SocketAddr],
    tls: bool,
    host_port: &str,
) -> std::io::Result<Box<dyn Stream>> {
    dial_with_timeout(targets, tls, host_port, None)
}

/// Dial an already-resolved target set with an optional per-address connect
/// timeout and matching read/write deadlines. Fetch uses this to make timeout a
/// provider contract rather than an unbounded host call; raw `Net` keeps the
/// historical blocking behavior through [`dial`].
pub fn dial_with_timeout(
    targets: &[std::net::SocketAddr],
    tls: bool,
    host_port: &str,
    timeout: Option<std::time::Duration>,
) -> std::io::Result<Box<dyn Stream>> {
    let tcp = match timeout {
        Some(timeout) => {
            let mut last = None;
            let mut connected = None;
            for target in targets {
                match TcpStream::connect_timeout(target, timeout) {
                    Ok(stream) => {
                        connected = Some(stream);
                        break;
                    }
                    Err(error) => last = Some(error),
                }
            }
            connected.ok_or_else(|| {
                last.unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "cannot dial an empty target set",
                    )
                })
            })?
        }
        None => TcpStream::connect(targets)?,
    };
    tcp.set_read_timeout(timeout)?;
    tcp.set_write_timeout(timeout)?;
    if !tls {
        return Ok(Box::new(tcp));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::io::{Error, ErrorKind};
        let sni = server_name(host_port).to_string();
        let name = rustls::pki_types::ServerName::try_from(sni)
            .map_err(|_| Error::new(ErrorKind::InvalidInput, format!("`{host_port}` is not a valid TLS server name")))?;
        let conn = rustls::ClientConnection::new(client_config(), name)
            .map_err(|e| Error::other(format!("TLS setup failed: {e}")))?;
        Ok(Box::new(rustls::StreamOwned::new(conn, tcp)))
    }
    // The browser has no raw TLS; `tls:` is a native-only scheme.
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (host_port, tcp);
        Err(std::io::Error::other("TLS (`tls:`) is unavailable in the wasm build"))
    }
}

#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "test-support")))]
static TEST_TLS_ROOTS: LazyLock<Mutex<std::collections::BTreeMap<u64, std::path::PathBuf>>> =
    LazyLock::new(|| Mutex::new(std::collections::BTreeMap::new()));
#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "test-support")))]
static NEXT_TEST_TLS_ROOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// RAII registration for a hermetic test CA. Unlike mutating
/// `WITCHY_TLS_EXTRA_ROOTS`, this is safe under parallel test execution: every
/// live fixture contributes its own root, and dropping the guard removes only
/// that registration.
#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "test-support")))]
#[doc(hidden)]
pub struct TestTlsRootGuard(u64);

#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "test-support")))]
#[doc(hidden)]
pub fn register_test_tls_root(path: impl Into<std::path::PathBuf>) -> TestTlsRootGuard {
    use std::sync::atomic::Ordering;
    let id = NEXT_TEST_TLS_ROOT.fetch_add(1, Ordering::Relaxed);
    assert_ne!(id, 0, "test TLS root registration id overflowed");
    let mut roots = TEST_TLS_ROOTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let old = roots.insert(id, path.into());
    debug_assert!(old.is_none(), "a fresh test TLS root id must be unique");
    TestTlsRootGuard(id)
}

#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "test-support")))]
impl Drop for TestTlsRootGuard {
    fn drop(&mut self) {
        let removed = TEST_TLS_ROOTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
        debug_assert!(removed.is_some(), "a live test TLS root guard must own one registration");
    }
}

/// A client config trusting the Mozilla CA roots plus any extra PEM root named
/// by `WITCHY_TLS_EXTRA_ROOTS` (a custom/corporate CA). Tests can also register
/// scoped roots when the non-default `test-support` feature is enabled. Built
/// per dial — correctness over the micro-cost; the handshake dominates.
#[cfg(not(target_arch = "wasm32"))]
fn client_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let extra_paths: Vec<std::path::PathBuf> = std::env::var_os("WITCHY_TLS_EXTRA_ROOTS")
        .into_iter()
        .map(std::path::PathBuf::from)
        .collect();
    #[cfg(any(test, feature = "test-support"))]
    let extra_paths = {
        let mut with_test_roots = extra_paths;
        with_test_roots.extend(
            TEST_TLS_ROOTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .values()
                .cloned(),
        );
        with_test_roots
    };
    for path in extra_paths {
        if let Ok(pem) = std::fs::read(&path) {
            let mut reader = &pem[..];
            for cert in rustls_pemfile::certs(&mut reader).flatten() {
                let _ = roots.add(cert);
            }
        }
    }
    let config = rustls::ClientConfig::builder_with_provider(
        rustls::crypto::aws_lc_rs::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .expect("aws-lc provider supports the default TLS protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    Arc::new(config)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn test_tls_root_guard_registers_and_removes_exactly_one_root() {
        let before = TEST_TLS_ROOTS.lock().unwrap().len();
        {
            let _guard = register_test_tls_root("fixture.pem");
            assert_eq!(TEST_TLS_ROOTS.lock().unwrap().len(), before + 1);
        }
        assert_eq!(TEST_TLS_ROOTS.lock().unwrap().len(), before);
    }

    #[test]
    fn read_timeout_is_classified_as_eof_signal() {
        use std::io::{Error, ErrorKind};
        assert!(is_read_timeout(&Error::from(ErrorKind::WouldBlock)));
        assert!(is_read_timeout(&Error::from(ErrorKind::TimedOut)));
        // Real read errors must still propagate, not be swallowed as EOF.
        assert!(!is_read_timeout(&Error::from(ErrorKind::ConnectionReset)));
        assert!(!is_read_timeout(&Error::from(ErrorKind::UnexpectedEof)));
    }

    #[test]
    fn server_read_timeout_default_override_and_disable() {
        let d = |secs| Some(std::time::Duration::from_secs(secs));
        // Unset (or absent) -> the conservative default.
        assert_eq!(server_read_timeout_from(None), d(DEFAULT_SERVER_READ_TIMEOUT_SECS));
        // An explicit value overrides.
        assert_eq!(server_read_timeout_from(Some("5")), d(5));
        assert_eq!(server_read_timeout_from(Some("  90 ")), d(90));
        // Zero disables the timeout (restores unbounded blocking).
        assert_eq!(server_read_timeout_from(Some("0")), None);
        // Garbage falls back to the default rather than silently disabling the guard.
        assert_eq!(server_read_timeout_from(Some("not-a-number")), d(DEFAULT_SERVER_READ_TIMEOUT_SECS));
    }

    // Slow-loris mitigation, end to end: a peer connects and sends nothing. With a read
    // timeout on the accepted socket, `read_line_capped` must return promptly with the
    // (empty) bytes-so-far as EOF — never hang, never error — so the worker is freed.
    #[test]
    fn accepted_socket_stall_reads_as_eof_within_the_timeout() {
        use std::io::BufReader;
        use std::time::{Duration, Instant};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // Client connects but never sends a byte, holding the connection open.
        let _client = TcpStream::connect(addr).unwrap();
        let (server, _peer) = listener.accept().unwrap();
        // Same call the accept loops make, but with a short timeout for the test.
        server.set_read_timeout(Some(Duration::from_millis(150))).unwrap();
        let mut reader = BufReader::new(server);
        let start = Instant::now();
        let line = read_line_capped(&mut reader).expect("a stalled read is EOF, not an error");
        assert!(line.is_empty(), "no bytes were sent, so the line is empty");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the read returned near the timeout, not after blocking indefinitely"
        );
        // The still-connected client keeps `_client` alive until here.
        drop(_client);
    }
}

/// (SEC-035) A capability-secure server must not let a peer OOM the host by streaming
/// without bound. Every "read to a delimiter / EOF" recv op caps its buffer at this many
/// bytes and fails closed past it (rather than silently truncating). 64 MiB is far above
/// any legitimate line/message; a hostile peer can't grow host memory beyond it. Lives
/// here (ungated, shared by both backends) so the interpreter and the compiled runtime
/// apply the SAME cap — keeping recv behavior in parity.
pub const MAX_RECV_BYTES: u64 = 64 << 20;

/// (Availability / edge-hardening) A slow or stalled client must not pin a server worker
/// forever. The deployed coven registry is a single small VM, so one wedged worker is a
/// real availability loss — the classic slow-loris: open a connection and send a partial
/// request (or nothing) and never finish, holding the worker in its request read. Every
/// ACCEPTED server connection gets this read timeout via [`prepare_accepted_socket`]; a
/// request read that stalls this long is treated as a clean end-of-stream (the recv
/// helpers below map a read timeout to EOF), so `read_request` sees a truncated request,
/// returns its 400/close, and the worker is freed — never crashing the accept loop, and
/// identically on both backends (this ONE module is what both call).
///
/// 30s is far longer than any healthy client needs to send a request over a slow link, yet
/// short enough that a stalled peer can't hold a worker for minutes. Override with the
/// `WITCHY_SERVER_READ_TIMEOUT_SECS` env var (an integer number of seconds; `0` disables
/// the timeout, restoring the historical unbounded blocking).
pub const DEFAULT_SERVER_READ_TIMEOUT_SECS: u64 = 30;

/// The configured server read timeout (see [`DEFAULT_SERVER_READ_TIMEOUT_SECS`]). `None`
/// when disabled via `WITCHY_SERVER_READ_TIMEOUT_SECS=0` (or an unparseable value falls
/// back to the default).
pub fn server_read_timeout() -> Option<std::time::Duration> {
    server_read_timeout_from(std::env::var("WITCHY_SERVER_READ_TIMEOUT_SECS").ok().as_deref())
}

/// Pure core of [`server_read_timeout`], factored out so the parse/default/disable policy
/// is unit-tested without mutating the process environment.
fn server_read_timeout_from(raw: Option<&str>) -> Option<std::time::Duration> {
    let secs = raw
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SERVER_READ_TIMEOUT_SECS);
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
}

/// Apply the server read timeout (if enabled) to a freshly ACCEPTED connection, BEFORE it
/// is wrapped (plain or TLS) — so a stall during the TLS handshake is bounded too. This is
/// the ONLY place a socket read timeout is set on a server socket; client (`net.connect`)
/// sockets set none, which keeps the timeout-as-EOF mapping in the recv helpers scoped to
/// the server path. Best-effort: a platform that rejects the option leaves the socket
/// blocking rather than failing the accept.
pub fn prepare_accepted_socket(stream: &TcpStream) {
    if let Some(timeout) = server_read_timeout() {
        let _ = stream.set_read_timeout(Some(timeout));
    }
}

/// True when an io error is a read timeout firing (`SO_RCVTIMEO`): `WouldBlock` on Unix,
/// `TimedOut` on Windows. The recv helpers treat this as a clean EOF. Only server-accepted
/// sockets carry a read timeout (see [`prepare_accepted_socket`]), so on any other socket
/// — client dials, which never set one — this is never reached, keeping the EOF mapping
/// scoped to the server path.
pub fn is_read_timeout(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
}

/// Read one line (through `\n`) from a buffered reader, but never buffer more than
/// [`MAX_RECV_BYTES`] — so a peer that never sends a newline can't grow the buffer
/// without bound. Returns the bytes read (including any trailing `\n`); errors if the cap
/// is reached with no newline (a hostile or malformed stream). A server read timeout
/// firing mid-read ([`is_read_timeout`]) ends the line as EOF — a stalled peer yields the
/// bytes so far, never an error.
pub fn read_line_capped<R: std::io::BufRead>(r: &mut R) -> std::io::Result<Vec<u8>> {
    use std::io::{Error, ErrorKind};
    let mut buf = Vec::new();
    loop {
        let room = MAX_RECV_BYTES as usize - buf.len();
        let chunk = match r.fill_buf() {
            Ok(chunk) => chunk,
            // A stalled server connection (read timeout) reads as a clean end-of-stream:
            // return the bytes gathered so far so the request parser sees a truncated
            // request and drops the connection, rather than erroring out the worker.
            Err(e) if is_read_timeout(&e) => break,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            break; // EOF before a newline
        }
        if let Some(i) = chunk.iter().take(room).position(|&b| b == b'\n') {
            buf.extend_from_slice(&chunk[..=i]);
            r.consume(i + 1);
            return Ok(buf);
        }
        let take = chunk.len().min(room);
        buf.extend_from_slice(&chunk[..take]);
        r.consume(take);
        if buf.len() as u64 >= MAX_RECV_BYTES {
            return Err(Error::new(ErrorKind::InvalidData, "recv_line exceeded the byte cap with no newline"));
        }
    }
    Ok(buf)
}
