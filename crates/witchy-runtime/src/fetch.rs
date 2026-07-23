//! Shared host-side provider for RFC-0102 `Fetch`.
//!
//! The language-facing ABI is built above this module. This layer owns the
//! security contract common to the interpreter and compiled-Wasm hosts:
//! canonical origins, allowlist checks before DNS/I/O, monotone narrowing,
//! bounded buffered responses, uniform timeouts, and redirect rejection.

use std::fmt;
use std::io::{Read, Write};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    scheme: Scheme,
    host: String,
    port: u16,
}

impl Origin {
    pub fn parse(input: &str) -> Result<Self, FetchError> {
        let parsed = ParsedUrl::parse(input, true)?;
        if parsed.path_and_query != "/" {
            return Err(FetchError::invalid(
                "an origin grant must not contain a path, query, or fragment",
            ));
        }
        Ok(parsed.origin)
    }

    pub fn as_str(&self) -> String {
        format!(
            "{}://{}:{}",
            self.scheme.as_str(),
            display_host(&self.host),
            self.port
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scheme {
    Http,
    Https,
}

impl Scheme {
    fn parse(value: &str) -> Result<Self, FetchError> {
        match value.to_ascii_lowercase().as_str() {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            _ => Err(FetchError::invalid(
                "Fetch URLs and origins must use `http` or `https`",
            )),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    const fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchPolicy {
    allowed: AllowedOrigins,
    /// A derived Fetch permanently retains the source Net policy. Direct Fetch
    /// grants have no Net floor.
    network_floor: Option<Vec<String>>,
    timeout: Duration,
    max_response_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AllowedOrigins {
    Any,
    Exact(Vec<Origin>),
}

impl FetchPolicy {
    pub fn allow(origins: impl IntoIterator<Item = String>) -> Result<Self, FetchError> {
        let mut allowed = Vec::new();
        for origin in origins {
            let origin = Origin::parse(&origin)?;
            if !allowed.contains(&origin) {
                allowed.push(origin);
            }
        }
        Ok(Self {
            allowed: AllowedOrigins::Exact(allowed),
            network_floor: None,
            timeout: Duration::from_secs(30),
            max_response_bytes: 16 * 1024 * 1024,
        })
    }

    pub fn system() -> Self {
        Self {
            allowed: AllowedOrigins::Any,
            network_floor: None,
            timeout: Duration::from_secs(30),
            max_response_bytes: 16 * 1024 * 1024,
        }
    }

    /// Derive exact origin-scoped Fetch authority from a Net capability.
    ///
    /// Every requested origin must be admitted by the source Net policy before
    /// the Fetch exists. The complete source policy is retained and re-applied
    /// after DNS resolution on every send, so a hostname allow plus an IP/CIDR
    /// deny cannot be bypassed by derivation.
    pub fn from_net(
        net_allow: &[String],
        origins: impl IntoIterator<Item = String>,
    ) -> Result<Self, FetchError> {
        let mut allowed = Vec::new();
        for origin in origins {
            let origin = Origin::parse(&origin)?;
            let authority = format!("{}:{}", display_host(&origin.host), origin.port);
            if !witchy_caps::capabilities::net_allows(net_allow, &authority) {
                return Err(FetchError::Denied {
                    origin: origin.as_str(),
                });
            }
            if !allowed.contains(&origin) {
                allowed.push(origin);
            }
        }
        Ok(Self {
            allowed: AllowedOrigins::Exact(allowed),
            network_floor: Some(net_allow.to_vec()),
            timeout: Duration::from_secs(30),
            max_response_bytes: 16 * 1024 * 1024,
        })
    }

    pub fn with_limits(mut self, timeout: Duration, max_response_bytes: usize) -> Self {
        self.timeout = timeout;
        self.max_response_bytes = max_response_bytes;
        self
    }

    pub fn only(&self, origins: impl IntoIterator<Item = String>) -> Result<Self, FetchError> {
        let mut narrowed = Vec::new();
        for origin in origins {
            let origin = Origin::parse(&origin)?;
            if !self.admits(&origin) {
                return Err(FetchError::Denied {
                    origin: origin.as_str(),
                });
            }
            if !narrowed.contains(&origin) {
                narrowed.push(origin);
            }
        }
        Ok(Self {
            allowed: AllowedOrigins::Exact(narrowed),
            network_floor: self.network_floor.clone(),
            timeout: self.timeout,
            max_response_bytes: self.max_response_bytes,
        })
    }

    pub fn admits(&self, origin: &Origin) -> bool {
        match &self.allowed {
            AllowedOrigins::Any => true,
            AllowedOrigins::Exact(origins) => origins.contains(origin),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    Denied { origin: String },
    InvalidRequest { message: String },
    Timeout,
    Redirect { status: u16 },
    Network { message: String },
    MalformedResponse { message: String },
    ResponseTooLarge { limit: usize },
}

impl FetchError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidRequest {
            message: message.into(),
        }
    }

    pub const fn code(&self) -> &'static str {
        match self {
            Self::Denied { .. } => "denied",
            Self::InvalidRequest { .. } => "invalid-request",
            Self::Timeout => "timeout",
            Self::Redirect { .. } => "redirect",
            Self::Network { .. } => "network",
            Self::MalformedResponse { .. } => "malformed-response",
            Self::ResponseTooLarge { .. } => "response-too-large",
        }
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied { origin } => write!(f, "Fetch origin `{origin}` is not granted"),
            Self::InvalidRequest { message } => write!(f, "invalid Fetch request: {message}"),
            Self::Timeout => write!(f, "Fetch request timed out"),
            Self::Redirect { status } => {
                write!(f, "Fetch redirects are disabled (HTTP status {status})")
            }
            Self::Network { message } => write!(f, "Fetch network error: {message}"),
            Self::MalformedResponse { message } => {
                write!(f, "malformed Fetch response: {message}")
            }
            Self::ResponseTooLarge { limit } => {
                write!(f, "Fetch response exceeds the {limit}-byte host limit")
            }
        }
    }
}

impl std::error::Error for FetchError {}

pub fn send(policy: &FetchPolicy, request: &FetchRequest) -> Result<FetchResponse, FetchError> {
    validate_method(&request.method)?;
    validate_headers(&request.headers)?;
    let parsed = ParsedUrl::parse(&request.url, false)?;
    if !policy.admits(&parsed.origin) {
        return Err(FetchError::Denied {
            origin: parsed.origin.as_str(),
        });
    }

    let host_port = format!(
        "{}:{}",
        display_host(&parsed.origin.host),
        parsed.origin.port
    );
    let targets = if let Some(net_allow) = &policy.network_floor {
        witchy_caps::capabilities::resolve_admitted(net_allow, &host_port).map_err(|_| {
            FetchError::Denied {
                origin: parsed.origin.as_str(),
            }
        })?
    } else {
        resolve(&parsed.origin.host, parsed.origin.port)?
    };
    let mut stream = crate::net::dial_with_timeout(
        &targets,
        parsed.origin.scheme == Scheme::Https,
        &host_port,
        Some(policy.timeout),
    )
    .map_err(io_error)?;

    let host_header = if parsed.origin.port == parsed.origin.scheme.default_port() {
        display_host(&parsed.origin.host)
    } else {
        host_port
    };
    let mut wire = Vec::new();
    write!(
        wire,
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        request.method, parsed.path_and_query, host_header
    )
    .map_err(io_error)?;
    for (name, value) in &request.headers {
        write!(wire, "{name}: {value}\r\n").map_err(io_error)?;
    }
    if !request.body.is_empty() {
        write!(wire, "Content-Length: {}\r\n", request.body.len()).map_err(io_error)?;
    }
    wire.extend_from_slice(b"\r\n");
    wire.extend_from_slice(&request.body);
    stream.write_all(&wire).map_err(io_error)?;
    stream.flush().map_err(io_error)?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                if raw.len().saturating_add(read) > policy.max_response_bytes {
                    return Err(FetchError::ResponseTooLarge {
                        limit: policy.max_response_bytes,
                    });
                }
                raw.extend_from_slice(&chunk[..read]);
            }
            Err(error) => return Err(io_error(error)),
        }
    }
    parse_response(&raw)
}

fn validate_method(method: &str) -> Result<(), FetchError> {
    if method.is_empty()
        || !method
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
    {
        return Err(FetchError::invalid("method is not an HTTP token"));
    }
    Ok(())
}

fn validate_headers(headers: &[(String, String)]) -> Result<(), FetchError> {
    for (name, value) in headers {
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        {
            return Err(FetchError::invalid(format!(
                "header name `{name}` is not an HTTP token"
            )));
        }
        if value.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0..=8 | 11..=31 | 127)) {
            return Err(FetchError::invalid(format!(
                "header `{name}` contains a forbidden control character"
            )));
        }
        if name.eq_ignore_ascii_case("host")
            || name.eq_ignore_ascii_case("connection")
            || name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("transfer-encoding")
        {
            return Err(FetchError::invalid(format!(
                "header `{name}` is controlled by the Fetch provider"
            )));
        }
    }
    Ok(())
}

fn resolve(host: &str, port: u16) -> Result<Vec<std::net::SocketAddr>, FetchError> {
    use std::net::ToSocketAddrs;
    let bare = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    let addresses: Vec<_> = (bare, port)
        .to_socket_addrs()
        .map_err(io_error)?
        .collect();
    if addresses.is_empty() {
        return Err(FetchError::Network {
            message: format!("origin `{}` resolved to no addresses", display_host(host)),
        });
    }
    Ok(addresses)
}

fn io_error(error: std::io::Error) -> FetchError {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        FetchError::Timeout
    } else {
        FetchError::Network {
            message: error.to_string(),
        }
    }
}

fn parse_response(raw: &[u8]) -> Result<FetchResponse, FetchError> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| FetchError::MalformedResponse {
            message: "missing header terminator".to_string(),
        })?;
    let head = std::str::from_utf8(&raw[..split]).map_err(|_| {
        FetchError::MalformedResponse {
            message: "response headers are not UTF-8".to_string(),
        }
    })?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut status_parts = status_line.split_whitespace();
    let version = status_parts.next().unwrap_or_default();
    let status = status_parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|status| (100..=599).contains(status))
        .ok_or_else(|| FetchError::MalformedResponse {
            message: "invalid HTTP status line".to_string(),
        })?;
    if !version.starts_with("HTTP/") {
        return Err(FetchError::MalformedResponse {
            message: "invalid HTTP version".to_string(),
        });
    }
    if (300..400).contains(&status) {
        return Err(FetchError::Redirect { status });
    }

    let mut headers = Vec::new();
    for line in lines {
        let (name, value) =
            line.split_once(':')
                .ok_or_else(|| FetchError::MalformedResponse {
                    message: "malformed response header".to_string(),
                })?;
        if name.is_empty() || name.as_bytes().first().is_some_and(|byte| matches!(byte, b' ' | b'\t')) {
            return Err(FetchError::MalformedResponse {
                message: "obsolete folded response header".to_string(),
            });
        }
        headers.push((name.to_string(), value.trim().to_string()));
    }
    let body = &raw[split + 4..];
    let body = if headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding")
            && value
                .split(',')
                .any(|encoding| encoding.trim().eq_ignore_ascii_case("chunked"))
    }) {
        decode_chunked(body)?
    } else if let Some(length) = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
    {
        if body.len() < length {
            return Err(FetchError::MalformedResponse {
                message: "response body is shorter than Content-Length".to_string(),
            });
        }
        body[..length].to_vec()
    } else {
        body.to_vec()
    };
    Ok(FetchResponse {
        status,
        headers,
        body,
    })
}

fn decode_chunked(mut input: &[u8]) -> Result<Vec<u8>, FetchError> {
    let mut output = Vec::new();
    loop {
        let line_end = input
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| FetchError::MalformedResponse {
                message: "truncated chunk-size line".to_string(),
            })?;
        let size_text = std::str::from_utf8(&input[..line_end])
            .ok()
            .and_then(|line| line.split(';').next())
            .unwrap_or_default();
        let size =
            usize::from_str_radix(size_text.trim(), 16).map_err(|_| {
                FetchError::MalformedResponse {
                    message: "invalid chunk size".to_string(),
                }
            })?;
        input = &input[line_end + 2..];
        if size == 0 {
            return Ok(output);
        }
        if input.len() < size + 2 || &input[size..size + 2] != b"\r\n" {
            return Err(FetchError::MalformedResponse {
                message: "truncated chunk body".to_string(),
            });
        }
        output.extend_from_slice(&input[..size]);
        input = &input[size + 2..];
    }
}

#[derive(Debug)]
struct ParsedUrl {
    origin: Origin,
    path_and_query: String,
}

impl ParsedUrl {
    fn parse(input: &str, origin_only: bool) -> Result<Self, FetchError> {
        if input.bytes().any(|byte| byte.is_ascii_control() || byte == b' ') {
            return Err(FetchError::invalid(
                "URL contains whitespace or a control character",
            ));
        }
        if input.contains('#') {
            return Err(FetchError::invalid("URL fragments are not sent by Fetch"));
        }
        let (scheme, rest) = input
            .split_once("://")
            .ok_or_else(|| FetchError::invalid("URL is missing `scheme://`"))?;
        let scheme = Scheme::parse(scheme)?;
        let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        if authority.is_empty() {
            return Err(FetchError::invalid("URL has an empty host"));
        }
        if authority.contains('@') {
            return Err(FetchError::invalid(
                "URL credentials are forbidden; pass explicit authorization headers",
            ));
        }
        let (host, port) = split_authority(authority, scheme.default_port())?;
        let path_and_query = match &rest[authority_end..] {
            "" => "/".to_string(),
            suffix if suffix.starts_with('?') => format!("/{suffix}"),
            suffix => suffix.to_string(),
        };
        if origin_only && path_and_query != "/" {
            return Err(FetchError::invalid(
                "an origin grant must not contain a path or query",
            ));
        }
        Ok(Self {
            origin: Origin {
                scheme,
                host: host.to_ascii_lowercase(),
                port,
            },
            path_and_query,
        })
    }
}

fn split_authority(authority: &str, default_port: u16) -> Result<(&str, u16), FetchError> {
    if authority.starts_with('[') {
        let close = authority
            .find(']')
            .ok_or_else(|| FetchError::invalid("unterminated IPv6 host"))?;
        let host = &authority[..=close];
        let suffix = &authority[close + 1..];
        let port = if suffix.is_empty() {
            default_port
        } else {
            suffix
                .strip_prefix(':')
                .ok_or_else(|| FetchError::invalid("invalid IPv6 authority"))?
                .parse()
                .map_err(|_| FetchError::invalid("invalid URL port"))?
        };
        return Ok((host, port));
    }
    if authority.matches(':').count() > 1 {
        return Err(FetchError::invalid(
            "IPv6 URL hosts must be enclosed in brackets",
        ));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() => Ok((
            host,
            port.parse()
                .map_err(|_| FetchError::invalid("invalid URL port"))?,
        )),
        Some(_) => Err(FetchError::invalid("invalid URL authority")),
        None => Ok((authority, default_port)),
    }
}

fn display_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    fn serve(response: &'static [u8]) -> (String, mpsc::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sent, received) = mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = stream.read(&mut chunk).unwrap();
                request.extend_from_slice(&chunk[..read]);
                if read == 0
                    || request.windows(4).any(|window| window == b"\r\n\r\n")
                {
                    break;
                }
            }
            let _ = sent.send(request);
            stream.write_all(response).unwrap();
        });
        (format!("http://{address}"), received)
    }

    fn get(url: String) -> FetchRequest {
        FetchRequest {
            method: "GET".to_string(),
            url,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    #[test]
    fn origins_are_canonical_and_credentials_are_rejected() {
        assert_eq!(
            Origin::parse("HTTPS://Example.COM").unwrap().as_str(),
            "https://example.com:443"
        );
        assert_eq!(
            Origin::parse("http://example.com:8080").unwrap().as_str(),
            "http://example.com:8080"
        );
        assert!(matches!(
            Origin::parse("https://user@example.com"),
            Err(FetchError::InvalidRequest { .. })
        ));
    }

    #[test]
    fn narrowing_is_intersective_and_denial_precedes_io() {
        let policy = FetchPolicy::allow([
            "https://example.com".to_string(),
            "http://127.0.0.1:9".to_string(),
        ])
        .unwrap();
        let narrowed = policy
            .only(["https://example.com:443".to_string()])
            .unwrap();
        assert!(matches!(
            narrowed.only(["http://127.0.0.1:9".to_string()]),
            Err(FetchError::Denied { .. })
        ));
        assert!(matches!(
            send(&narrowed, &get("http://127.0.0.1:9/never-dial".to_string())),
            Err(FetchError::Denied { .. })
        ));
    }

    #[test]
    fn net_derivation_is_bounded_and_preserves_the_network_floor() {
        let allow = vec![
            "127.0.0.1:8080".to_string(),
            "!127.0.0.1:8081".to_string(),
        ];
        let fetch = FetchPolicy::from_net(
            &allow,
            ["http://127.0.0.1:8080".to_string()],
        )
        .expect("admitted origin derives");
        assert!(fetch
            .only(["http://127.0.0.1:8080".to_string()])
            .is_ok());
        assert!(matches!(
            FetchPolicy::from_net(
                &allow,
                ["http://127.0.0.1:8081".to_string()]
            ),
            Err(FetchError::Denied { .. })
        ));
        assert_eq!(fetch.network_floor, Some(allow));
    }

    #[test]
    fn sends_one_bounded_request_and_parses_the_response() {
        let (origin, received) =
            serve(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Test: yes\r\n\r\nok");
        let policy = FetchPolicy::allow([origin.clone()]).unwrap();
        let response = send(&policy, &get(format!("{origin}/hello?x=1"))).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"ok");
        let request = String::from_utf8(received.recv().unwrap()).unwrap();
        assert!(request.starts_with("GET /hello?x=1 HTTP/1.1\r\n"));
        assert!(request.contains("\r\nConnection: close\r\n"));
    }

    #[test]
    fn redirects_are_uniform_errors_and_never_followed() {
        let (origin, _) =
            serve(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/\r\nContent-Length: 0\r\n\r\n");
        let policy = FetchPolicy::allow([origin.clone()]).unwrap();
        assert_eq!(
            send(&policy, &get(format!("{origin}/redirect"))),
            Err(FetchError::Redirect { status: 302 })
        );
    }

    #[test]
    fn request_injection_and_oversized_responses_fail_closed() {
        let policy = FetchPolicy::system();
        let mut request = get("http://127.0.0.1:9/".to_string());
        request.headers.push(("X-Test".to_string(), "ok\r\nInjected: yes".to_string()));
        assert!(matches!(
            send(&policy, &request),
            Err(FetchError::InvalidRequest { .. })
        ));

        let (origin, _) = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nbody");
        let policy = FetchPolicy::allow([origin.clone()])
            .unwrap()
            .with_limits(Duration::from_secs(1), 8);
        assert_eq!(
            send(&policy, &get(format!("{origin}/large"))),
            Err(FetchError::ResponseTooLarge { limit: 8 })
        );
    }

    #[test]
    fn chunked_bodies_are_decoded() {
        let response =
            parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n")
                .unwrap();
        assert_eq!(response.body, b"ok");
    }
}
