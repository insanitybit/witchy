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

/// The browser shim has no DNS (pure-compute, deny-by-omission): resolution yields nothing.
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
    let tcp = TcpStream::connect(targets)?;
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

/// A client config trusting the Mozilla CA roots plus any extra PEM roots named by
/// `WITCHY_TLS_EXTRA_ROOTS` (a custom/corporate CA) or `WITCHY_TLS_TEST_ROOTS`
/// (RFC-0060: the TEST-ONLY hook the `serve_tls` differential/e2e suites use to
/// trust their self-signed fixture; consumed only when set, never for production
/// trust). Built per dial — correctness over the micro-cost; the handshake dominates.
#[cfg(not(target_arch = "wasm32"))]
fn client_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for var in ["WITCHY_TLS_EXTRA_ROOTS", "WITCHY_TLS_TEST_ROOTS"] {
        if let Some(path) = std::env::var_os(var) {
            if let Ok(pem) = std::fs::read(&path) {
                let mut reader = &pem[..];
                for cert in rustls_pemfile::certs(&mut reader).flatten() {
                    let _ = roots.add(cert);
                }
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

/// (SEC-035) A capability-secure server must not let a peer OOM the host by streaming
/// without bound. Every "read to a delimiter / EOF" recv op caps its buffer at this many
/// bytes and fails closed past it (rather than silently truncating). 64 MiB is far above
/// any legitimate line/message; a hostile peer can't grow host memory beyond it. Lives
/// here (ungated, shared by both backends) so the interpreter and the compiled runtime
/// apply the SAME cap — keeping recv behavior in parity.
pub const MAX_RECV_BYTES: u64 = 64 << 20;

/// Read one line (through `\n`) from a buffered reader, but never buffer more than
/// [`MAX_RECV_BYTES`] — so a peer that never sends a newline can't grow the buffer
/// without bound. Returns the bytes read (including any trailing `\n`); errors if the cap
/// is reached with no newline (a hostile or malformed stream).
pub fn read_line_capped<R: std::io::BufRead>(r: &mut R) -> std::io::Result<Vec<u8>> {
    use std::io::{Error, ErrorKind};
    let mut buf = Vec::new();
    loop {
        let room = MAX_RECV_BYTES as usize - buf.len();
        let chunk = r.fill_buf()?;
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
