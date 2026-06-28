//! Host-side networking shared by both backends: the `Stream` socket abstraction and
//! the dialer that completes a plain or TLS connection.
//!
//! TLS is the `tls:` address scheme (RFC-0009): `connect(net, "tls:host:443")` performs
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
/// `WITCHY_TLS_EXTRA_ROOTS` (a custom/corporate CA, or the hermetic test's self-signed
/// cert). Built per dial — correctness over the micro-cost; the handshake dominates.
#[cfg(not(target_arch = "wasm32"))]
fn client_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = std::env::var_os("WITCHY_TLS_EXTRA_ROOTS") {
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
