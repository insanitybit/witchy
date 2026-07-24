//! Dependency-bottom vocabulary for Witchy's built-in capabilities.
//!
//! Syntax resolution, type checking, footprint analysis, runtime providers,
//! launch tooling, and host menus all consume this catalog. Keeping the model
//! free of AST and runtime dependencies prevents those consumers from growing
//! private capability-name and rights tables.

#![deny(unsafe_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityClass {
    Host,
    Derived,
    Build,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityRight {
    Read,
    Write,
    Connect,
    Listen,
    Tcp,
    Udp,
    Uds,
}

impl CapabilityRight {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::Write => "Write",
            Self::Connect => "Connect",
            Self::Listen => "Listen",
            Self::Tcp => "Tcp",
            Self::Udp => "Udp",
            Self::Uds => "Uds",
        }
    }
}

pub const READ_WRITE_RIGHTS: &[CapabilityRight] =
    &[CapabilityRight::Read, CapabilityRight::Write];
pub const NET_VERB_RIGHTS: &[CapabilityRight] =
    &[CapabilityRight::Connect, CapabilityRight::Listen];
pub const NET_TRANSPORT_RIGHTS: &[CapabilityRight] =
    &[CapabilityRight::Tcp, CapabilityRight::Udp, CapabilityRight::Uds];
pub const NET_RIGHTS: &[CapabilityRight] = &[
    CapabilityRight::Connect,
    CapabilityRight::Listen,
    CapabilityRight::Tcp,
    CapabilityRight::Udp,
    CapabilityRight::Uds,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityKind {
    Console,
    Clock,
    Rand,
    Env,
    Secret,
    SecretStore,
    Dir,
    File,
    Net,
    Fetch,
    Exec,
    Socket,
    Listener,
    BuildOut,
    BuildRead,
    BuildEnv,
    BuildNet,
    BuildExec,
}

impl CapabilityKind {
    pub const ALL: &'static [Self] = &[
        Self::Console,
        Self::Clock,
        Self::Rand,
        Self::Env,
        Self::Secret,
        Self::SecretStore,
        Self::Dir,
        Self::File,
        Self::Net,
        Self::Fetch,
        Self::Exec,
        Self::Socket,
        Self::Listener,
        Self::BuildOut,
        Self::BuildRead,
        Self::BuildEnv,
        Self::BuildNet,
        Self::BuildExec,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Console => "Console",
            Self::Clock => "Clock",
            Self::Rand => "Rand",
            Self::Env => "Env",
            Self::Secret => "Secret",
            Self::SecretStore => "SecretStore",
            Self::Dir => "Dir",
            Self::File => "File",
            Self::Net => "Net",
            Self::Fetch => "Fetch",
            Self::Exec => "Exec",
            Self::Socket => "Socket",
            Self::Listener => "Listener",
            Self::BuildOut => "BuildOut",
            Self::BuildRead => "BuildRead",
            Self::BuildEnv => "BuildEnv",
            Self::BuildNet => "BuildNet",
            Self::BuildExec => "BuildExec",
        }
    }

    pub const fn class(self) -> CapabilityClass {
        match self {
            Self::Socket | Self::Listener => CapabilityClass::Derived,
            Self::BuildOut
            | Self::BuildRead
            | Self::BuildEnv
            | Self::BuildNet
            | Self::BuildExec => CapabilityClass::Build,
            _ => CapabilityClass::Host,
        }
    }

    pub const fn rights(self) -> &'static [CapabilityRight] {
        match self {
            Self::Console | Self::Dir | Self::File => READ_WRITE_RIGHTS,
            Self::Net => NET_RIGHTS,
            _ => &[],
        }
    }

    pub fn right(self, name: &str) -> Option<CapabilityRight> {
        self.rights().iter().copied().find(|right| right.name() == name)
    }

    /// Ordinary capability values have arity zero. Rights-bearing capability
    /// syntax is checked by the dedicated marker validator rather than generic
    /// type arity checking.
    pub const fn builtin_arity(self) -> Option<usize> {
        if self.rights().is_empty() { Some(0) } else { None }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Console" => Some(Self::Console),
            "Clock" => Some(Self::Clock),
            "Rand" => Some(Self::Rand),
            "Env" => Some(Self::Env),
            "Secret" => Some(Self::Secret),
            "SecretStore" => Some(Self::SecretStore),
            "Dir" => Some(Self::Dir),
            "File" => Some(Self::File),
            "Net" => Some(Self::Net),
            "Fetch" => Some(Self::Fetch),
            "Exec" => Some(Self::Exec),
            "Socket" => Some(Self::Socket),
            "Listener" => Some(Self::Listener),
            "BuildOut" => Some(Self::BuildOut),
            "BuildRead" => Some(Self::BuildRead),
            "BuildEnv" => Some(Self::BuildEnv),
            "BuildNet" => Some(Self::BuildNet),
            "BuildExec" => Some(Self::BuildExec),
            _ => None,
        }
    }
}

pub fn is_capability_type_name(name: &str) -> bool {
    CapabilityKind::from_name(name).is_some()
}

pub fn is_host_capability(name: &str) -> bool {
    CapabilityKind::from_name(name)
        .is_some_and(|kind| kind.class() == CapabilityClass::Host)
}

pub fn is_build_capability(name: &str) -> bool {
    CapabilityKind::from_name(name)
        .is_some_and(|kind| kind.class() == CapabilityClass::Build)
}

pub const USE_ONLY_SECRET_REVEAL_ERROR: &str =
    "this secret is use-only and cannot be revealed; use it by handle (e.g. crypto.sign or server.serve_tls)";

const DIR_DENY_ALL: &str = "\u{0}";

/// Narrow a directory entry policy by intersecting each constrained dimension.
///
/// The empty policy is unrestricted. Recognized dimensions are represented as
/// newline-separated `dimension:value` rows. An invalid or disjoint refinement
/// fails closed to an unrepresentable sentinel accepted by [`dir_admits`].
pub fn dir_only(current: &str, refine: &str) -> String {
    if refine.is_empty() {
        return current.to_string();
    }
    use std::collections::{BTreeMap, BTreeSet};

    fn group(value: &str) -> BTreeMap<&str, BTreeSet<&str>> {
        let mut grouped: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for pattern in value.split('\n') {
            if let Some((dimension, _)) = pattern.split_once(':') {
                grouped.entry(dimension).or_default().insert(pattern);
            }
        }
        grouped
    }

    let refinement = group(refine);
    if refinement.is_empty() {
        return DIR_DENY_ALL.to_string();
    }
    if current.is_empty() {
        return refine.to_string();
    }
    let current = group(current);
    let mut narrowed = BTreeSet::new();
    for (dimension, patterns) in &current {
        if !refinement.contains_key(dimension) {
            narrowed.extend(patterns.iter().copied());
        }
    }
    for (dimension, patterns) in &refinement {
        match current.get(dimension) {
            Some(existing) => {
                let intersection: BTreeSet<&str> =
                    patterns.intersection(existing).copied().collect();
                if intersection.is_empty() {
                    return DIR_DENY_ALL.to_string();
                }
                narrowed.extend(intersection);
            }
            None => narrowed.extend(patterns.iter().copied()),
        }
    }
    narrowed.into_iter().collect::<Vec<_>>().join("\n")
}

/// Whether a normalized entry is admitted by a directory entry policy.
pub fn dir_admits(policy: &str, name: &str, is_dir: bool) -> bool {
    if policy.is_empty() {
        return true;
    }
    let (mut has_extension, mut extension_allowed) = (false, false);
    let (mut has_kind, mut kind_allowed) = (false, false);
    for pattern in policy.split('\n') {
        if pattern == DIR_DENY_ALL {
            return false;
        }
        if let Some(extension) = pattern.strip_prefix("ext:") {
            has_extension = true;
            if is_dir || name.ends_with(extension) {
                extension_allowed = true;
            }
        } else if let Some(kind) = pattern.strip_prefix("kind:") {
            has_kind = true;
            if (kind == "dir") == is_dir {
                kind_allowed = true;
            }
        }
    }
    (!has_extension || extension_allowed) && (!has_kind || kind_allowed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchScheme {
    Http,
    Https,
}

impl FetchScheme {
    pub fn parse(value: &str) -> Result<Self, FetchUrlError> {
        match value.to_ascii_lowercase().as_str() {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            _ => Err(FetchUrlError::new(
                "Fetch URLs and origins must use `http` or `https`",
            )),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    pub const fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOrigin {
    scheme: FetchScheme,
    host: String,
    port: u16,
}

impl FetchOrigin {
    pub fn parse(input: &str) -> Result<Self, FetchUrlError> {
        let parsed = ParsedFetchUrl::parse(input, true)?;
        if parsed.path_and_query != "/" {
            return Err(FetchUrlError::new(
                "an origin grant must not contain a path, query, or fragment",
            ));
        }
        Ok(parsed.origin)
    }

    pub const fn scheme(&self) -> FetchScheme {
        self.scheme
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub const fn port(&self) -> u16 {
        self.port
    }

    pub fn as_str(&self) -> String {
        format!(
            "{}://{}:{}",
            self.scheme.as_str(),
            display_fetch_host(&self.host),
            self.port
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFetchUrl {
    origin: FetchOrigin,
    path_and_query: String,
}

impl ParsedFetchUrl {
    pub fn parse(input: &str, origin_only: bool) -> Result<Self, FetchUrlError> {
        if input.bytes().any(|byte| byte.is_ascii_control() || byte == b' ') {
            return Err(FetchUrlError::new(
                "URL contains whitespace or a control character",
            ));
        }
        let fragment = input.find('#');
        if origin_only && fragment.is_some() {
            return Err(FetchUrlError::new(
                "an origin grant must not contain a path, query, or fragment",
            ));
        }
        let input = fragment.map_or(input, |index| &input[..index]);
        let (scheme, rest) = input
            .split_once("://")
            .ok_or_else(|| FetchUrlError::new("URL is missing `scheme://`"))?;
        let scheme = FetchScheme::parse(scheme)?;
        let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        if authority.is_empty() {
            return Err(FetchUrlError::new("URL has an empty host"));
        }
        if authority.contains('@') {
            return Err(FetchUrlError::new(
                "URL credentials are forbidden; pass explicit authorization headers",
            ));
        }
        let (host, port) = split_fetch_authority(authority, scheme.default_port())?;
        let path_and_query = match &rest[authority_end..] {
            "" => "/".to_string(),
            suffix if suffix.starts_with('?') => format!("/{suffix}"),
            suffix => suffix.to_string(),
        };
        if origin_only && path_and_query != "/" {
            return Err(FetchUrlError::new(
                "an origin grant must not contain a path or query",
            ));
        }
        Ok(Self {
            origin: FetchOrigin {
                scheme,
                host: host.to_ascii_lowercase(),
                port,
            },
            path_and_query,
        })
    }

    pub fn origin(&self) -> &FetchOrigin {
        &self.origin
    }

    pub fn path_and_query(&self) -> &str {
        &self.path_and_query
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchUrlError {
    message: String,
}

impl FetchUrlError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for FetchUrlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FetchUrlError {}

pub fn validate_fetch_method(method: &str) -> Result<(), FetchUrlError> {
    if is_http_token(method) {
        Ok(())
    } else {
        Err(FetchUrlError::new("method is not an HTTP token"))
    }
}

pub fn validate_fetch_header(name: &str, value: &str) -> Result<(), FetchUrlError> {
    validate_http_header_syntax(name, value)?;
    if name.eq_ignore_ascii_case("host")
        || name.eq_ignore_ascii_case("connection")
        || name.eq_ignore_ascii_case("content-length")
        || name.eq_ignore_ascii_case("transfer-encoding")
    {
        return Err(FetchUrlError::new(format!(
            "header `{name}` is controlled by the Fetch provider"
        )));
    }
    Ok(())
}

pub fn validate_http_header_syntax(name: &str, value: &str) -> Result<(), FetchUrlError> {
    if !is_http_token(name) {
        return Err(FetchUrlError::new(format!(
            "header name `{name}` is not an HTTP token"
        )));
    }
    if value
        .bytes()
        .any(|byte| matches!(byte, b'\r' | b'\n' | 0..=8 | 11..=31 | 127))
    {
        return Err(FetchUrlError::new(format!(
            "header `{name}` contains a forbidden control character"
        )));
    }
    Ok(())
}

fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn split_fetch_authority(
    authority: &str,
    default_port: u16,
) -> Result<(&str, u16), FetchUrlError> {
    if authority.starts_with('[') {
        let close = authority
            .find(']')
            .ok_or_else(|| FetchUrlError::new("unterminated IPv6 host"))?;
        let host = &authority[..=close];
        let suffix = &authority[close + 1..];
        let port = if suffix.is_empty() {
            default_port
        } else {
            suffix
                .strip_prefix(':')
                .ok_or_else(|| FetchUrlError::new("invalid IPv6 authority"))?
                .parse()
                .map_err(|_| FetchUrlError::new("invalid URL port"))?
        };
        return Ok((host, port));
    }
    if authority.matches(':').count() > 1 {
        return Err(FetchUrlError::new(
            "IPv6 URL hosts must be enclosed in brackets",
        ));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() => Ok((
            host,
            port.parse()
                .map_err(|_| FetchUrlError::new("invalid URL port"))?,
        )),
        Some(_) => Err(FetchUrlError::new("invalid URL authority")),
        None => Ok((authority, default_port)),
    }
}

fn display_fetch_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_names_are_unique_and_round_trip() {
        let mut names = std::collections::BTreeSet::new();
        for kind in CapabilityKind::ALL {
            assert!(names.insert(kind.name()), "duplicate capability {}", kind.name());
            assert_eq!(CapabilityKind::from_name(kind.name()), Some(*kind));
        }
    }

    #[test]
    fn rights_belong_only_to_their_declared_capability() {
        assert_eq!(CapabilityKind::Dir.right("Read"), Some(CapabilityRight::Read));
        assert_eq!(CapabilityKind::Net.right("Connect"), Some(CapabilityRight::Connect));
        assert_eq!(CapabilityKind::Net.right("Read"), None);
        assert_eq!(CapabilityKind::Console.right("Read"), Some(CapabilityRight::Read));
        assert_eq!(CapabilityKind::Console.right("Write"), Some(CapabilityRight::Write));
    }

    #[test]
    fn directory_policy_refinement_is_monotone_and_fail_closed() {
        assert_eq!(
            dir_only("kind:file\next:.txt", "ext:.txt"),
            "ext:.txt\nkind:file"
        );
        assert!(!dir_admits(
            &dir_only("ext:.txt", "ext:.md"),
            "note.txt",
            false
        ));
        assert!(!dir_admits(&dir_only("", "garbled"), "anything", true));
        assert!(dir_admits("ext:.txt", "nested", true));
        assert!(!dir_admits("kind:file", "nested", true));
    }

    #[test]
    fn fetch_origins_and_requests_share_canonical_security_checks() {
        assert_eq!(
            FetchOrigin::parse("HTTPS://Example.COM")
                .expect("origin")
                .as_str(),
            "https://example.com:443"
        );
        assert_eq!(
            ParsedFetchUrl::parse("https://example.com/a?q=1#ignored", false)
                .expect("request")
                .origin()
                .as_str(),
            "https://example.com:443"
        );
        assert!(FetchOrigin::parse("https://user@example.com").is_err());
        assert!(FetchOrigin::parse("https://example.com/path").is_err());
        assert!(validate_fetch_method("GE\rT").is_err());
        assert!(validate_fetch_header("Host", "example.com").is_err());
        assert!(validate_fetch_header("X-Test", "ok\r\nInjected: yes").is_err());
        assert!(validate_http_header_syntax("Content-Length", "12").is_ok());
    }
}
