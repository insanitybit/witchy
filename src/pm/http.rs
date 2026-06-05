//! A minimal synchronous HTTP/1.1 client — just enough for the coven registry
//! protocol (GET / POST JSON over plain http to a known host). Uses
//! `Connection: close` so the body is simply read to EOF; no chunked decoding,
//! no TLS, no dependencies.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use super::{err, PmResult};

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn is_ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub fn get(url: &str) -> PmResult<Response> {
    request("GET", url, None)
}

pub fn post_json(url: &str, body: &[u8]) -> PmResult<Response> {
    request("POST", url, Some(body))
}

fn request(method: &str, url: &str, body: Option<&[u8]>) -> PmResult<Response> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| super::PmError(format!("only http:// URLs are supported: {url}")))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().unwrap_or(80)),
        None => (authority, 80),
    };

    let mut stream = TcpStream::connect((host, port))
        .map_err(|e| super::PmError(format!("connect {authority}: {e}")))?;
    stream.set_read_timeout(Some(Duration::from_secs(15))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(15))).ok();

    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n"
    );
    if let Some(b) = body {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .map_err(|e| super::PmError(format!("write request: {e}")))?;
    if let Some(b) = body {
        stream
            .write_all(b)
            .map_err(|e| super::PmError(format!("write body: {e}")))?;
    }
    stream.flush().ok();

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| super::PmError(format!("read response: {e}")))?;

    let sep = find(&raw, b"\r\n\r\n")
        .ok_or_else(|| super::PmError("malformed HTTP response (no header terminator)".into()))?;
    let status = parse_status(&raw[..sep])
        .ok_or_else(|| super::PmError("malformed HTTP status line".into()))?;
    let body = raw[sep + 4..].to_vec();
    Ok(Response { status, body })
}

fn parse_status(head: &[u8]) -> Option<u16> {
    let line = head.split(|&b| b == b'\n').next()?;
    let text = std::str::from_utf8(line).ok()?;
    text.split_whitespace().nth(1)?.parse().ok()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// GET a URL and deserialize a JSON response, mapping a non-2xx status to the
/// server's `{ "error": ... }` payload.
pub fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> PmResult<T> {
    decode(get(url)?)
}

pub fn post<T: serde::de::DeserializeOwned, B: serde::Serialize>(url: &str, body: &B) -> PmResult<T> {
    let bytes = serde_json::to_vec(body).map_err(|e| super::PmError(e.to_string()))?;
    decode(post_json(url, &bytes)?)
}

fn decode<T: serde::de::DeserializeOwned>(resp: Response) -> PmResult<T> {
    if resp.is_ok() {
        serde_json::from_slice(&resp.body)
            .map_err(|e| super::PmError(format!("bad response body: {e}")))
    } else {
        // Try to surface the server's structured error.
        match serde_json::from_slice::<super::wire::ErrResp>(&resp.body) {
            Ok(e) => err(e.error),
            Err(_) => err(format!(
                "registry returned HTTP {}: {}",
                resp.status,
                String::from_utf8_lossy(&resp.body)
            )),
        }
    }
}
