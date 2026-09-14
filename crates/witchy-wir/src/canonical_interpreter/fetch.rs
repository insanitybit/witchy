//! Independent Fetch marshalling over explicit provider outcomes. This module
//! never opens a socket, resolves a name, or calls the native Fetch provider.
use super::*;
use std::sync::Arc;
use crate::canonical_safewir::{FETCH_CAPABILITY_KIND, FETCH_RESPONSE_KIND};

const LIMIT: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OracleFetchFixture {
    /// One exact origin list per direct Fetch root, in parameter order.
    pub grants: Vec<Vec<String>>,
    pub calls: Vec<OracleFetchCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OracleFetchSource { Direct(usize), Net(usize) }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OracleFetchOutcome {
    /// Entire raw HTTP response, before provider parsing/transfer decoding.
    Wire(Vec<u8>),
    Timeout,
    Network(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleFetchCall {
    pub source: OracleFetchSource,
    pub method: String,
    pub url: String,
    /// Exact source argument, before the provider splits and trims lines.
    pub headers: String,
    pub body: String,
    pub outcome: OracleFetchOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Grant {
    source: OracleFetchSource,
    origins: BTreeSet<String>,
    net_floor: Option<CapabilityToken>,
}

impl Grant {
    fn narrow(&self, spec: &str) -> Result<Self, InterpretError> {
        let mut origins = BTreeSet::new();
        for origin in spec.lines() {
            let (origin, _, _) = parse_url(origin, true).map_err(refusal)?;
            if !self.origins.contains(&origin) {
                return Err(refusal(format!("fetch.only: Fetch origin `{origin}` is not granted")));
            }
            origins.insert(origin);
        }
        // Keep the immutable source identity and authenticated Net token. A
        // narrowed grant cannot become a direct grant or lose its Net floor.
        Ok(Self { origins, source: self.source.clone(), net_floor: self.net_floor.clone() })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Resource { Grant(Arc<Grant>), Response(Arc<[u8]>) }

#[derive(Debug, Clone)]
pub(super) struct Token { issuer: Arc<()>, resource: Resource }
impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.issuer, &other.issuer) && self.resource == other.resource
    }
}
impl Eq for Token {}

pub(super) struct State { issuer: Arc<()>, consumed: usize }
fn refusal(message: impl Into<String>) -> InterpretError { InterpretError::FetchFixture(message.into()) }
fn invalid(message: impl AsRef<str>) -> String {
    format!("WITCHY_FETCH_ERROR:invalid-request:invalid Fetch request: {}", message.as_ref())
}
fn malformed(message: &str) -> String {
    format!("WITCHY_FETCH_ERROR:malformed-response:malformed Fetch response: {message}")
}
fn denied(origin: &str) -> String {
    format!("WITCHY_FETCH_ERROR:denied:Fetch origin `{origin}` is not granted")
}

impl State {
    pub(super) fn new() -> Self { Self { issuer: Arc::new(()), consumed: 0 } }

    fn mint(&self, resource: Resource) -> CapabilityToken {
        let kind = match &resource { Resource::Grant(_) => FETCH_CAPABILITY_KIND, Resource::Response(_) => FETCH_RESPONSE_KIND };
        CapabilityToken { fetch: Some(Token { issuer: self.issuer.clone(), resource }), ..CapabilityToken::root(kind.get()) }
    }

    fn authenticate(&self, token: &CapabilityToken, kind: u32) -> Result<Resource, InterpretError> {
        let handle = token.fetch.as_ref().ok_or_else(|| refusal("Fetch unauthenticated resource"))?;
        let resource_kind = match &handle.resource { Resource::Grant(_) => FETCH_CAPABILITY_KIND, Resource::Response(_) => FETCH_RESPONSE_KIND };
        if token.kind != kind || resource_kind.get() != kind || !Arc::ptr_eq(&handle.issuer, &self.issuer) {
            return Err(refusal("Fetch wrong kind or session"));
        }
        Ok(handle.resource.clone())
    }

    fn invoke(&mut self, fixture: &OracleFetchFixture, source: &OracleFetchSource, fields: &[String; 4]) -> Result<Arc<[u8]>, InterpretError> {
        let call = fixture.calls.get(self.consumed).ok_or_else(|| refusal("unexpected Fetch call after transcript end"))?;
        if &call.source != source || call.method != fields[0] || call.url != fields[1] || call.headers != fields[2] || call.body != fields[3] {
            return Err(refusal(format!("Fetch transcript mismatch at call {}", self.consumed)));
        }
        let payload = match &call.outcome {
            OracleFetchOutcome::Wire(wire) => wire_payload(wire).unwrap_or_else(|error| error),
            OracleFetchOutcome::Timeout => "WITCHY_FETCH_ERROR:timeout:Fetch request timed out".into(),
            OracleFetchOutcome::Network(message) => format!("WITCHY_FETCH_ERROR:network:Fetch network error: {message}"),
        };
        if payload.len() > i32::MAX as usize { return Err(InterpretError::ArithmeticTrap); }
        // Script mismatch is not a provider failure. Only exact calls consume
        // events; provider failures then return ordinary immutable String data.
        self.consumed += 1;
        Ok(Arc::from(payload.into_bytes()))
    }
}

impl Engine<'_> {
    pub(super) fn bind_fetch_entry(&self, mut arguments: Vec<InterpreterValue>) -> Result<Vec<InterpreterValue>, InterpretError> {
        let mut ordinal = 0;
        for argument in &mut arguments {
            if let InterpreterValue::Capability(token) = argument && token.kind == FETCH_CAPABILITY_KIND.get() {
                if self.isolated || *token != CapabilityToken::root(FETCH_CAPABILITY_KIND.get()) {
                    return Err(refusal("Fetch entry is not a fresh root request"));
                }
                let grant = self.config.fixture.fetch.as_ref().and_then(|fixture| fixture.grants.get(ordinal))
                    .ok_or_else(|| refusal("Fetch entry was not granted"))?;
                let origins = grant.iter().map(|origin| parse_url(origin, true).map(|origin| origin.0))
                    .collect::<Result<BTreeSet<_>, _>>().map_err(refusal)?;
                *token = self.fetch.mint(Resource::Grant(Arc::new(Grant {
                    source: OracleFetchSource::Direct(ordinal), origins, net_floor: None,
                })));
                ordinal += 1;
            }
        }
        Ok(arguments)
    }

    pub(super) fn finish_fetch(&self) -> Result<(), InterpretError> {
        let required = self.config.fixture.fetch.as_ref().map_or(0, |fixture| fixture.calls.len());
        if self.fetch.consumed != required {
            return Err(refusal(format!("Fetch unused transcript calls: consumed {} of {required}", self.fetch.consumed)));
        }
        Ok(())
    }

    fn fetch_resource(&self, value: ValueId, kind: u32, values: &BTreeMap<ValueId, InterpreterValue>) -> Result<Resource, InterpretError> {
        if self.isolated { return Err(refusal("Fetch unavailable in an isolated service")); }
        let InterpreterValue::Capability(token) = self.value(value, values)? else { return Err(refusal("Fetch capability value")); };
        self.fetch.authenticate(token, kind)
    }

    pub(super) fn fetch_host_call(&mut self, name: &str, destinations: &[ValueId], arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError> {
        let [destination] = destinations else { return Err(refusal("Fetch result arity")); };
        match (name, arguments) {
            ("net_fetch_at", [HostCallArgument::Capability(net), HostCallArgument::Buffer { place, .. }]) => {
                if self.isolated { return Err(refusal("Fetch unavailable in an isolated service")); }
                let InterpreterValue::Capability(net) = self.value(*net, values)?.clone() else { return Err(refusal("Fetch Net value")); };
                let state = self.net.as_ref().ok_or_else(|| refusal("Fetch derivation needs an explicit Net fixture"))?;
                let (ordinal, grant) = state.fetch_grant(&net)?;
                // Authenticate before reading the complete origin-spec slice.
                let spec = self.host_string(place, values)?;
                let mut origins = BTreeSet::new();
                for origin in spec.lines() {
                    let (origin, address, numeric) = parse_url(origin, true).map_err(refusal)?;
                    if !numeric { return Err(InterpretError::UnsupportedOperation("Fetch fixture Net floor requires numeric endpoints, not DNS")); }
                    if !grant.addresses.contains(&address) { return Err(refusal(format!("net.fetch: Fetch origin `{origin}` is not granted"))); }
                    origins.insert(origin);
                }
                let result = self.fetch.mint(Resource::Grant(Arc::new(Grant {
                    source: OracleFetchSource::Net(ordinal), origins, net_floor: Some(net),
                })));
                values.insert(*destination, InterpreterValue::Capability(result));
            }
            ("fetch_send_at", [HostCallArgument::Capability(fetch), HostCallArgument::Buffer { place: method, .. }, HostCallArgument::Buffer { place: url, .. }, HostCallArgument::Buffer { place: headers, .. }, HostCallArgument::Buffer { place: body, .. }]) => {
                let Resource::Grant(grant) = self.fetch_resource(*fetch, FETCH_CAPABILITY_KIND.get(), values)? else { return Err(refusal("Fetch grant kind")); };
                let fields = [self.host_string(method, values)?, self.host_string(url, values)?, self.host_string(headers, values)?, self.host_string(body, values)?];
                // Request/policy errors are values and perform no fixture IO.
                let admission = request_origin(&fields).and_then(|(origin, address, _)| {
                    if !grant.origins.contains(&origin) { return Err(denied(&origin)); }
                    Ok((origin, address))
                });
                let bytes = match admission {
                    Err(payload) => Arc::<[u8]>::from(payload.into_bytes()),
                    Ok((origin, address)) => {
                        let floor_allows = if let Some(net) = &grant.net_floor {
                            let state = self.net.as_ref().ok_or_else(|| refusal("Fetch lost Net floor"))?;
                            let (_, floor) = state.fetch_grant(net)?;
                            floor.addresses.contains(&address)
                        } else { true };
                        if !floor_allows { Arc::from(denied(&origin).into_bytes()) } else {
                            let fixture = self.config.fixture.fetch.as_ref().ok_or_else(|| refusal("Fetch provider fixture was not supplied"))?;
                            self.fetch.invoke(fixture, &grant.source, &fields)?
                        }
                    }
                };
                let response = self.fetch.mint(Resource::Response(bytes));
                values.insert(*destination, InterpreterValue::Capability(response));
            }
            ("fetch_only_at", [HostCallArgument::Capability(fetch), HostCallArgument::Buffer { place, .. }]) => {
                let Resource::Grant(grant) = self.fetch_resource(*fetch, FETCH_CAPABILITY_KIND.get(), values)? else { return Err(refusal("Fetch grant kind")); };
                let origins = self.host_string(place, values)?;
                let narrowed = grant.narrow(&origins)?;
                let token = self.fetch.mint(Resource::Grant(Arc::new(narrowed)));
                values.insert(*destination, InterpreterValue::Capability(token));
            }
            ("fetch_response_size", [HostCallArgument::Capability(response)]) => {
                let Resource::Response(bytes) = self.fetch_resource(*response, FETCH_RESPONSE_KIND.get(), values)? else { return Err(refusal("Fetch response kind")); };
                values.insert(*destination, InterpreterValue::Size(bytes.len() as u64));
            }
            ("fetch_response_into", [HostCallArgument::Capability(response), HostCallArgument::Buffer { place, .. }]) => {
                let Resource::Response(bytes) = self.fetch_resource(*response, FETCH_RESPONSE_KIND.get(), values)? else { return Err(refusal("Fetch response kind")); };
                let (object, start, capacity) = self.slice_place(place, values)?;
                self.check_range(object, start, capacity, true)?;
                if capacity >= bytes.len() as u64 {
                    for (offset, byte) in bytes.iter().enumerate() {
                        self.object_mut(object)?.cells.insert(CellKey::Element(start + offset as u64), InterpreterValue::Size(u64::from(*byte)));
                    }
                }
                values.insert(*destination, InterpreterValue::Size(bytes.len() as u64));
            }
            _ => return Err(refusal("unsupported Fetch host arguments")),
        }
        Ok(())
    }
}

// Lexical URL/request rules are independently specified here, without using
// the runtime provider, resolver, or shared source-checker implementation.
fn parse_url(input: &str, origin_only: bool) -> Result<(String, String, bool), String> {
    if input.bytes().any(|byte| byte.is_ascii_control() || byte == b' ') { return Err(invalid("URL contains whitespace or a control character")); }
    if origin_only && input.contains('#') { return Err(invalid("an origin grant must not contain a path, query, or fragment")); }
    let input = input.split('#').next().unwrap_or_default();
    let (scheme, rest) = input.split_once("://").ok_or_else(|| invalid("URL is missing `scheme://`"))?;
    let scheme = scheme.to_ascii_lowercase();
    let default = match scheme.as_str() { "http" => 80, "https" => 443, _ => return Err(invalid("Fetch URLs and origins must use `http` or `https`")) };
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.is_empty() { return Err(invalid("URL has an empty host")); }
    if authority.contains('@') { return Err(invalid("URL credentials are forbidden; pass explicit authorization headers")); }
    let (host, port): (&str, u16) = if authority.starts_with('[') {
        let close = authority.find(']').ok_or_else(|| invalid("unterminated IPv6 host"))?;
        let suffix = &authority[close + 1..];
        let port = if suffix.is_empty() { default } else {
            suffix.strip_prefix(':').ok_or_else(|| invalid("invalid IPv6 authority"))?.parse().map_err(|_| invalid("invalid URL port"))?
        };
        (&authority[..=close], port)
    } else {
        if authority.matches(':').count() > 1 { return Err(invalid("IPv6 URL hosts must be enclosed in brackets")); }
        match authority.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() && !port.is_empty() => (host, port.parse().map_err(|_| invalid("invalid URL port"))?),
            Some(_) => return Err(invalid("invalid URL authority")),
            None => (authority, default),
        }
    };
    let path = &rest[end..];
    if origin_only && !matches!(path, "" | "/") { return Err(invalid("an origin grant must not contain a path or query")); }
    let host = host.to_ascii_lowercase();
    let numeric = host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>().is_ok();
    let address = format!("{host}:{port}");
    Ok((format!("{scheme}://{address}"), address, numeric))
}

fn http_token(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn request_origin(fields: &[String; 4]) -> Result<(String, String, bool), String> {
    if !http_token(&fields[0]) { return Err(invalid("method is not an HTTP token")); }
    for (name, value) in fields[2].lines().filter_map(|line| line.split_once(':')) {
        let (name, value) = (name.trim(), value.trim());
        if !http_token(name) { return Err(invalid(format!("header name `{name}` is not an HTTP token"))); }
        if value.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0..=8 | 11..=31 | 127)) {
            return Err(invalid(format!("header `{name}` contains a forbidden control character")));
        }
        if ["host", "connection", "content-length", "transfer-encoding"].iter().any(|controlled| name.eq_ignore_ascii_case(controlled)) {
            return Err(invalid(format!("header `{name}` is controlled by the Fetch provider")));
        }
    }
    parse_url(&fields[1], false)
}

fn wire_payload(raw: &[u8]) -> Result<String, String> {
    if raw.len() > LIMIT { return Err(format!("WITCHY_FETCH_ERROR:response-too-large:Fetch response exceeds the {LIMIT}-byte host limit")); }
    let split = raw.windows(4).position(|part| part == b"\r\n\r\n").ok_or_else(|| malformed("missing header terminator"))?;
    let head = std::str::from_utf8(&raw[..split]).map_err(|_| malformed("response headers are not UTF-8"))?;
    let mut lines = head.split("\r\n");
    let mut status = lines.next().unwrap_or_default().split_whitespace();
    let version = status.next().unwrap_or_default();
    let status = status.next().and_then(|value| value.parse::<u16>().ok()).filter(|value| (100..=599).contains(value))
        .ok_or_else(|| malformed("invalid HTTP status line"))?;
    if !version.starts_with("HTTP/") { return Err(malformed("invalid HTTP version")); }
    if (300..400).contains(&status) { return Err(format!("WITCHY_FETCH_ERROR:redirect:Fetch redirects are disabled (HTTP status {status})")); }
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or_else(|| malformed("malformed response header"))?;
        if name.is_empty() || name.starts_with([' ', '\t']) { return Err(malformed("obsolete folded response header")); }
        headers.push((name, value.trim()));
    }
    let mut body = &raw[split + 4..];
    let chunked = headers.iter().any(|(name, value)| name.eq_ignore_ascii_case("transfer-encoding") && value.split(',').any(|encoding| encoding.trim().eq_ignore_ascii_case("chunked")));
    let decoded;
    if chunked {
        let mut output = Vec::new();
        loop {
            let end = body.windows(2).position(|part| part == b"\r\n").ok_or_else(|| malformed("truncated chunk-size line"))?;
            let digits = std::str::from_utf8(&body[..end]).ok().and_then(|line| line.split(';').next()).unwrap_or_default();
            let size = usize::from_str_radix(digits.trim(), 16).map_err(|_| malformed("invalid chunk size"))?;
            body = &body[end + 2..];
            if size == 0 { break; }
            let end = size.checked_add(2).filter(|end| *end <= body.len()).ok_or_else(|| malformed("truncated chunk body"))?;
            if &body[size..end] != b"\r\n" { return Err(malformed("truncated chunk body")); }
            output.extend_from_slice(&body[..size]);
            body = &body[end..];
        }
        decoded = output;
        body = &decoded;
    } else if let Some(length) = headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("content-length")).and_then(|(_, value)| value.parse::<usize>().ok()) {
        if body.len() < length { return Err(malformed("response body is shorter than Content-Length")); }
        body = &body[..length];
    }
    let mut payload = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in headers { payload.push_str(&format!("{name}: {value}\r\n")); }
    payload.push_str("\r\n");
    payload.push_str(&String::from_utf8_lossy(body));
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_only_oracle_preserves_source_floor_and_rejects_widening() {
        for source in [OracleFetchSource::Direct(2), OracleFetchSource::Net(3)] {
            let floor = matches!(&source, OracleFetchSource::Net(_)).then(|| CapabilityToken::root(4));
            let grant = Grant { source, net_floor: floor, origins: ["http://127.0.0.1:80".into(), "http://127.0.0.1:81".into()].into_iter().collect() };
            let narrowed = grant.narrow("HTTP://127.0.0.1/\nhttp://127.0.0.1:80").unwrap();
            assert_eq!(narrowed.origins, ["http://127.0.0.1:80".into()].into_iter().collect());
            assert_eq!(narrowed.source, grant.source);
            assert_eq!(narrowed.net_floor, grant.net_floor);
            assert!(narrowed.narrow("http://127.0.0.1:81").is_err());
            assert_eq!(grant.origins.len(), 2, "narrowing does not mutate the original grant");
            let empty = narrowed.narrow("").unwrap();
            assert!(empty.origins.is_empty());
            assert_eq!(empty.net_floor, grant.net_floor);
            assert!(empty.narrow("http://127.0.0.1").is_err());
            assert!(narrowed.narrow("http://127.0.0.1/path").is_err());
        }
    }

    #[test]
    fn fetch_oracle_authentication_is_kind_session_and_resource_specific() {
        let first = State::new();
        let second = State::new();
        let grant = first.mint(Resource::Grant(Arc::new(Grant {
            source: OracleFetchSource::Direct(0), origins: BTreeSet::new(), net_floor: None,
        })));
        assert!(first.authenticate(&grant, FETCH_CAPABILITY_KIND.get()).is_ok());
        assert!(second.authenticate(&grant, FETCH_CAPABILITY_KIND.get()).is_err());
        assert!(first.authenticate(&grant, FETCH_RESPONSE_KIND.get()).is_err());
        assert!(first.authenticate(&CapabilityToken::root(FETCH_CAPABILITY_KIND.get()), FETCH_CAPABILITY_KIND.get()).is_err());
        let response = first.mint(Resource::Response(Arc::from(&b"retained"[..])));
        assert!(first.authenticate(&response, FETCH_RESPONSE_KIND.get()).is_ok());
        assert!(second.authenticate(&response, FETCH_RESPONSE_KIND.get()).is_err());
        assert!(first.authenticate(&response, FETCH_CAPABILITY_KIND.get()).is_err());
        assert!(first.authenticate(&CapabilityToken::root(13), FETCH_RESPONSE_KIND.get()).is_err());
    }

    #[test]
    fn fetch_oracle_provider_wire_status_transfer_and_errors_are_values() {
        assert_eq!(wire_payload(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n1\r\n\xff\r\n0\r\n\r\n").unwrap(),
            "HTTP/1.1 200\r\nTransfer-Encoding: chunked\r\n\r\nhi\u{fffd}");
        assert_eq!(wire_payload(b"HTTP/1.0 404 No\r\nContent-Length: 1\r\n\r\nxy").unwrap(), "HTTP/1.1 404\r\nContent-Length: 1\r\n\r\nx");
        assert_eq!(wire_payload(b"HTTP/1.1 302 Redirect\r\nLocation: http://other/\r\n\r\n"),
            Err("WITCHY_FETCH_ERROR:redirect:Fetch redirects are disabled (HTTP status 302)".into()));
        assert_eq!(wire_payload(b"unframed"), Err(malformed("missing header terminator")));
        let overflow = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n", usize::MAX);
        assert_eq!(wire_payload(overflow.as_bytes()), Err(malformed("truncated chunk body")));
        assert_eq!(parse_url("HTTPS://Example.COM/", true).unwrap().0, "https://example.com:443");
        for origin in ["http://x/p", "http://x?q", "http://x#fragment", "http://a@b", "http://x:bad"] { assert!(parse_url(origin, true).is_err()); }
        assert_eq!(request_origin(&["bad method".into(), "http://x".into(), "".into(), "".into()]), Err(invalid("method is not an HTTP token")));
        assert!(request_origin(&["POST".into(), "http://x".into(), "Host: x".into(), "".into()]).unwrap_err().contains("controlled"));
    }

    #[test]
    fn fetch_oracle_mismatch_is_not_provider_error_and_responses_are_independent() {
        let source = OracleFetchSource::Direct(0);
        let fields: [String; 4] = ["GET".into(), "http://fixture/".into(), "".into(), "".into()];
        let fixture = OracleFetchFixture { grants: Vec::new(), calls: vec![
            OracleFetchCall { source: source.clone(), method: fields[0].clone(), url: fields[1].clone(), headers: "".into(), body: "".into(), outcome: OracleFetchOutcome::Wire(b"HTTP/1.1 200 OK\r\n\r\nfirst".to_vec()) },
            OracleFetchCall { source: source.clone(), method: fields[0].clone(), url: fields[1].clone(), headers: "".into(), body: "".into(), outcome: OracleFetchOutcome::Timeout },
        ] };
        let mut state = State::new();
        assert!(state.invoke(&fixture, &OracleFetchSource::Direct(1), &fields).is_err());
        assert_eq!(state.consumed, 0);
        let first = state.invoke(&fixture, &source, &fields).unwrap();
        let second = state.invoke(&fixture, &source, &fields).unwrap();
        assert_eq!(first.as_ref(), b"HTTP/1.1 200\r\n\r\nfirst");
        assert_eq!(second.as_ref(), b"WITCHY_FETCH_ERROR:timeout:Fetch request timed out");
        assert!(state.invoke(&fixture, &source, &fields).is_err());
        assert_eq!(state.consumed, 2);
    }
}
