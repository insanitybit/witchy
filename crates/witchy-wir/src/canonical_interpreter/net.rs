//! Deterministic Net host semantics. Scripts describe wire events, not host
//! return values. Authority comes only from explicit launcher grants and
//! private tokens minted inside this execution; there is no OS network path.
use super::*;
use std::collections::VecDeque;
use std::sync::Arc;

const NET: u32 = crate::canonical_safewir::NET_CAPABILITY_KIND.get();
const LISTENER: u32 = crate::canonical_safewir::LISTENER_CAPABILITY_KIND.get();
const SOCKET: u32 = crate::canonical_safewir::SOCKET_CAPABILITY_KIND.get();
// Independent specification of the existing bounded receive semantics.
const RECEIVE_LIMIT: usize = 64 << 20;

#[path = "net_buffered.rs"]
mod buffered;
pub use buffered::{OracleNetRead, ORACLE_NET_BUFFER_CAPACITY};
#[path = "net_server_pool.rs"]
mod server_pool;
pub use server_pool::{OracleServerPoolFixture, OracleServerWorkerRun};
pub(super) use server_pool::ServerState;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OracleNetGrant {
    pub connect: bool,
    pub listen: bool,
    /// Exact script endpoints, not wildcard policies or DNS names to resolve.
    pub addresses: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OracleNetFixture {
    /// Ordered Net root grants, independent of interleaved non-Net parameters.
    pub grants: Vec<OracleNetGrant>,
    /// Ordered wire events, not one result per host call. A Receive may span
    /// calls; its original event is recorded exactly once when fully consumed
    /// or discarded by close. Close may discard only consecutive Receive
    /// events for its own socket immediately preceding its matching Close.
    /// No sends, other sockets, or terminal read events may be skipped.
    /// Wire events do not specify native buffered read-ahead residency:
    /// after local close, receives that would read explicitly refuse rather
    /// than manufacture EOF. Staged fills and zero-byte receives remain valid.
    pub transcript: Vec<OracleNetEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OracleNetEvent {
    Connect { grant: usize, address: String, socket: u64 },
    Listen { grant: usize, address: String, listener: u64 },
    Accept { listener: u64, socket: u64 },
    Send { socket: u64, bytes: Vec<u8> },
    Receive { socket: u64, bytes: Vec<u8> },
    Eof { socket: u64 },
    Timeout { socket: u64 },
    ReadError { socket: u64, message: String },
    Close { socket: u64 },
    /// Opt-in v1 profile, immediately after this socket's Connect/Accept.
    Buffered { socket: u64 },
    /// Byte availability, independent of transport read-completion boundaries.
    Arrive { socket: u64, bytes: Vec<u8> },
    StreamRead { socket: u64, outcome: OracleNetRead },
    /// Retain this prefix of unread transport bytes; preserve ALL read-ahead
    /// and completed receive staging. Discard only the remaining transport tail.
    BufferedClose { socket: u64, keep_transport_prefix: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OracleNetError {
    Unauthenticated,
    WrongKind,
    MissingGrant,
    RightsDenied,
    AddressDenied,
    TranscriptMismatch,
    IncompleteTranscript,
    DuplicateResource,
    PendingReceive,
    NoPendingReceive,
    OutputTooSmall,
    ReceiveLimit,
    ClosedSocket,
    ReadFailed(String),
}

fn refused(error: OracleNetError) -> InterpretError { InterpretError::NetFixture(error) }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resource { Grant(usize), Listener(u64), Socket(u64) }

#[derive(Debug, Clone)]
pub(super) struct Token { issuer: Arc<()>, resource: Resource }

impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.issuer, &other.issuer) && self.resource == other.resource
    }
}
impl Eq for Token {}

#[derive(Clone, Default)]
struct Socket {
    pending: Option<Vec<u8>>, eof: bool, closed: bool,
    buffered: Option<buffered::BufferedSocket>,
}

pub(super) struct NetState {
    issuer: Arc<()>,
    grants: Vec<OracleNetGrant>,
    events: VecDeque<OracleNetEvent>,
    observed: Vec<OracleNetEvent>,
    chunk_offset: usize,
    listeners: BTreeSet<u64>,
    listener_addresses: BTreeMap<u64, String>,
    shared_listener: Option<(u64, String)>,
    sockets: BTreeMap<u64, Socket>,
}

#[derive(Clone, Copy)]
enum Receive { Line, Bytes(usize), All }

impl NetState {
    pub(super) fn new(fixture: OracleNetFixture) -> Self {
        Self { issuer: Arc::new(()), grants: fixture.grants, events: fixture.transcript.into(),
            observed: Vec::new(), chunk_offset: 0, listeners: BTreeSet::new(), sockets: BTreeMap::new(),
            listener_addresses: BTreeMap::new(), shared_listener: None }
    }

    fn token(&self, resource: Resource) -> CapabilityToken {
        let kind = match resource { Resource::Grant(_) => NET, Resource::Listener(_) => LISTENER, Resource::Socket(_) => SOCKET };
        CapabilityToken { net: Some(Token { issuer: self.issuer.clone(), resource }), ..CapabilityToken::root(kind) }
    }

    fn authenticate(&self, token: &CapabilityToken, kind: u32) -> Result<Resource, InterpretError> {
        if token.kind != kind { return Err(refused(OracleNetError::WrongKind)); }
        let handle = token.net.as_ref().ok_or_else(|| refused(OracleNetError::Unauthenticated))?;
        if !Arc::ptr_eq(&handle.issuer, &self.issuer) { return Err(refused(OracleNetError::Unauthenticated)); }
        let valid = match handle.resource {
            Resource::Grant(index) => kind == NET && index < self.grants.len(),
            Resource::Listener(id) => kind == LISTENER && self.listeners.contains(&id),
            Resource::Socket(id) => kind == SOCKET && self.sockets.contains_key(&id),
        };
        if !valid { return Err(refused(OracleNetError::Unauthenticated)); }
        Ok(handle.resource)
    }

    fn pop(&mut self) {
        self.observed.push(self.events.pop_front().expect("matched transcript event"));
        self.chunk_offset = 0;
    }

    fn open(&mut self, token: &CapabilityToken, address: &str, listen: bool) -> Result<CapabilityToken, InterpretError> {
        let Resource::Grant(grant) = self.authenticate(token, NET)? else { return Err(refused(OracleNetError::WrongKind)); };
        let rights = &self.grants[grant];
        if !(if listen { rights.listen } else { rights.connect }) { return Err(refused(OracleNetError::RightsDenied)); }
        if !rights.addresses.contains(address) { return Err(refused(OracleNetError::AddressDenied)); }
        if listen && let Some((listener, endpoint)) = &self.shared_listener {
            // This bounded profile uses the exact numeric endpoint originally
            // bound by the primary, not DNS equivalence or caller token ids.
            if endpoint != address {
                return Err(InterpretError::UnsupportedOperation("server worker listener rebinding"));
            }
            let listener = *listener;
            self.listeners.insert(listener);
            self.listener_addresses.insert(listener, address.to_owned());
            return Ok(self.token(Resource::Listener(listener)));
        }
        let id = match self.events.front() {
            Some(OracleNetEvent::Listen { grant: expected, address: endpoint, listener })
                if listen && *expected == grant && endpoint == address => *listener,
            Some(OracleNetEvent::Connect { grant: expected, address: endpoint, socket })
                if !listen && *expected == grant && endpoint == address => *socket,
            _ => return Err(refused(OracleNetError::TranscriptMismatch)),
        };
        let resource = if listen {
            if !self.listeners.insert(id) { return Err(refused(OracleNetError::DuplicateResource)); }
            self.listener_addresses.insert(id, address.to_owned());
            Resource::Listener(id)
        } else {
            if self.sockets.contains_key(&id) { return Err(refused(OracleNetError::DuplicateResource)); }
            self.sockets.insert(id, Socket::default());
            Resource::Socket(id)
        };
        self.pop();
        if let Resource::Socket(socket) = resource { self.enable_buffered(socket); }
        Ok(self.token(resource))
    }

    fn accept(&mut self, token: &CapabilityToken) -> Result<CapabilityToken, InterpretError> {
        let Resource::Listener(listener) = self.authenticate(token, LISTENER)? else { return Err(refused(OracleNetError::WrongKind)); };
        let socket = match self.events.front() {
            Some(OracleNetEvent::Accept { listener: expected, socket }) if *expected == listener => *socket,
            _ => return Err(refused(OracleNetError::TranscriptMismatch)),
        };
        if self.sockets.contains_key(&socket) { return Err(refused(OracleNetError::DuplicateResource)); }
        self.sockets.insert(socket, Socket::default());
        self.pop();
        self.enable_buffered(socket);
        Ok(self.token(Resource::Socket(socket)))
    }

    fn socket(&self, token: &CapabilityToken) -> Result<u64, InterpretError> {
        match self.authenticate(token, SOCKET)? {
            Resource::Socket(id) => Ok(id),
            _ => Err(refused(OracleNetError::WrongKind)),
        }
    }

    fn send(&mut self, token: &CapabilityToken, bytes: &[u8]) -> Result<(), InterpretError> {
        let socket = self.socket(token)?;
        if self.sockets[&socket].closed && !bytes.is_empty() { return Err(refused(OracleNetError::ClosedSocket)); }
        if self.events.front() != Some(&OracleNetEvent::Send { socket, bytes: bytes.to_vec() }) {
            return Err(refused(OracleNetError::TranscriptMismatch));
        }
        self.pop();
        Ok(())
    }

    fn receive(&mut self, token: &CapabilityToken, mode: Receive, limit: usize) -> Result<u64, InterpretError> {
        let socket = self.socket(token)?;
        if self.sockets[&socket].pending.is_some() { return Err(refused(OracleNetError::PendingReceive)); }
        if self.sockets[&socket].buffered.is_some() {
            return self.receive_buffered(socket, mode, limit);
        }
        // Shutdown is not observed peer EOF: native's buffered reader may
        // still hold bytes. This wire-only fixture cannot infer that residency
        // from event boundaries. Refuse before touching the transcript or
        // staging; a zero-byte receive performs no read and remains defined.
        if self.sockets[&socket].closed && !matches!(mode, Receive::Bytes(0)) {
            return Err(InterpretError::UnsupportedOperation("Net fixture post-close receive"));
        }
        let mut raw = Vec::new();
        let wanted = match mode { Receive::Bytes(count) => count, _ => usize::MAX };
        while !self.sockets[&socket].eof && raw.len() < wanted {
            match self.events.front() {
                Some(OracleNetEvent::Receive { socket: expected, bytes }) if *expected == socket => {
                    let remaining = &bytes[self.chunk_offset..];
                    let room = match mode {
                        Receive::Line => limit.saturating_sub(raw.len()),
                        Receive::All => limit.saturating_add(1).saturating_sub(raw.len()),
                        Receive::Bytes(_) => wanted - raw.len(),
                    };
                    let amount = remaining.len().min(room);
                    let newline = matches!(mode, Receive::Line).then(|| remaining[..amount].iter().position(|byte| *byte == b'\n')).flatten();
                    let amount = newline.map_or(amount, |index| index + 1);
                    raw.extend_from_slice(&remaining[..amount]);
                    self.chunk_offset += amount;
                    if self.chunk_offset == bytes.len() { self.pop(); }
                    if newline.is_some() { break; }
                    if (matches!(mode, Receive::Line) && raw.len() >= limit)
                        || (matches!(mode, Receive::All) && raw.len() > limit) {
                        return Err(refused(OracleNetError::ReceiveLimit));
                    }
                }
                Some(OracleNetEvent::Eof { socket: expected }) if *expected == socket => {
                    self.sockets.get_mut(&socket).unwrap().eof = true;
                    self.pop();
                    break;
                }
                Some(OracleNetEvent::Timeout { socket: expected }) if *expected == socket => { self.pop(); break; }
                Some(OracleNetEvent::ReadError { socket: expected, message }) if *expected == socket => {
                    let error = refused(OracleNetError::ReadFailed(message.clone()));
                    self.pop();
                    return Err(error);
                }
                _ => return Err(refused(OracleNetError::TranscriptMismatch)),
            }
        }
        // Decode once per source receive, after its raw-byte bound is checked.
        let decoded = String::from_utf8_lossy(&raw);
        let bytes = if matches!(mode, Receive::Line) { decoded.trim_end_matches('\n').as_bytes().to_vec() }
            else { decoded.into_owned().into_bytes() };
        let length = u64::try_from(bytes.len()).map_err(|_| InterpretError::ArithmeticTrap)?;
        if length > i32::MAX as u64 { return Err(InterpretError::ArithmeticTrap); }
        self.sockets.get_mut(&socket).unwrap().pending = Some(bytes);
        Ok(length)
    }

    fn close(&mut self, token: &CapabilityToken) -> Result<(), InterpretError> {
        let socket = self.socket(token)?;
        if self.sockets[&socket].buffered.is_some() { return self.close_buffered(socket); }
        // Preflight without consuming even a partial chunk: a wrong/missing
        // Close or an intervening obligation must leave a retryable transcript.
        let unread = self.events.iter().take_while(|event|
            matches!(event, OracleNetEvent::Receive { socket: expected, .. } if *expected == socket)).count();
        if self.events.get(unread) != Some(&OracleNetEvent::Close { socket }) {
            return Err(refused(OracleNetError::TranscriptMismatch));
        }
        for _ in 0..unread { self.pop(); }
        let state = self.sockets.get_mut(&socket).unwrap();
        state.closed = true;
        // Staging is already the result of a completed receive, not unread
        // wire data. Like native shutdown, close does not invalidate its fill.
        self.pop();
        Ok(())
    }

    fn finish(&self) -> Result<Vec<OracleNetEvent>, InterpretError> {
        if !self.events.is_empty() || self.chunk_offset != 0 { return Err(refused(OracleNetError::IncompleteTranscript)); }
        Ok(self.observed.clone())
    }
}

pub(super) fn is_import(name: &str) -> bool {
    crate::canonical_safewir::NET_IMPORT_NAMES.contains(&name)
}

impl Engine<'_> {
    pub(super) fn bind_net_entry(&mut self, mut arguments: Vec<InterpreterValue>) -> Result<Vec<InterpreterValue>, InterpretError> {
        let Some(state) = self.net.as_ref() else { return Ok(arguments); };
        let mut ordinal = 0;
        for argument in &mut arguments {
            if let InterpreterValue::Capability(token) = argument && token.kind == NET {
                if token.net.is_some() || token.build_read.is_some() || !token.scope.is_empty() { return Err(refused(OracleNetError::Unauthenticated)); }
                if ordinal >= state.grants.len() { return Err(refused(OracleNetError::MissingGrant)); }
                *token = state.token(Resource::Grant(ordinal));
                ordinal += 1;
            }
        }
        Ok(arguments)
    }

    pub(super) fn finish_net(&self) -> Result<Vec<OracleNetEvent>, InterpretError> {
        self.server.finish()?;
        self.net.as_ref().map_or_else(|| Ok(Vec::new()), NetState::finish)
    }

    fn net_argument(&self, argument: &HostCallArgument, kind: u32, values: &BTreeMap<ValueId, InterpreterValue>) -> Result<CapabilityToken, InterpretError> {
        let HostCallArgument::Capability(value) = argument else { return Err(refused(OracleNetError::Unauthenticated)); };
        let InterpreterValue::Capability(token) = self.value(*value, values)? else { return Err(refused(OracleNetError::Unauthenticated)); };
        self.net.as_ref().ok_or(InterpretError::UnsupportedOperation("HostCall import"))?.authenticate(token, kind)?;
        Ok(token.clone())
    }

    pub(super) fn net_host_call(&mut self, name: &str, destinations: &[ValueId], arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError> {
        if self.net.is_none() { return Err(InterpretError::UnsupportedOperation("HostCall import")); }
        match (name, arguments, destinations) {
            ("net_connect_at" | "net_listen_at", [token, HostCallArgument::Buffer { place, .. }], [destination]) => {
                let token = self.net_argument(token, NET, values)?;
                let address = self.host_string(place, values)?;
                let result = self.net.as_mut().unwrap().open(&token, &address, name == "net_listen_at")?;
                values.insert(*destination, InterpreterValue::Capability(result));
            }
            ("net_accept", [token], [destination]) => {
                let token = self.net_argument(token, LISTENER, values)?;
                let result = self.net.as_mut().unwrap().accept(&token)?;
                values.insert(*destination, InterpreterValue::Capability(result));
            }
            ("net_send_slice", [token, HostCallArgument::Buffer { place, .. }], []) => {
                let token = self.net_argument(token, SOCKET, values)?;
                let text = self.host_string(place, values)?;
                self.net.as_mut().unwrap().send(&token, text.as_bytes())?;
            }
            ("net_recv_line_size" | "net_recv_all_size", [token], [destination]) => {
                let token = self.net_argument(token, SOCKET, values)?;
                let mode = if name == "net_recv_line_size" { Receive::Line } else { Receive::All };
                let length = self.net.as_mut().unwrap().receive(&token, mode, RECEIVE_LIMIT)?;
                values.insert(*destination, InterpreterValue::Size(length));
            }
            ("net_recv_bytes_size", [token, HostCallArgument::Value(count)], [destination]) => {
                let token = self.net_argument(token, SOCKET, values)?;
                let InterpreterValue::Int(count) = self.value(*count, values)? else { return Err(InterpretError::TypeMismatch("Net receive count")); };
                let count = usize::try_from((*count).max(0)).map_err(|_| InterpretError::ArithmeticTrap)?;
                let length = self.net.as_mut().unwrap().receive(&token, Receive::Bytes(count), RECEIVE_LIMIT)?;
                values.insert(*destination, InterpreterValue::Size(length));
            }
            ("net_take_received_into", [token, HostCallArgument::Buffer { place, .. }], [destination]) => {
                let token = self.net_argument(token, SOCKET, values)?;
                let state = self.net.as_ref().unwrap();
                let socket = state.socket(&token)?;
                let bytes = state.sockets[&socket].pending.as_ref().ok_or_else(|| refused(OracleNetError::NoPendingReceive))?.clone();
                let (_, _, capacity) = self.slice_place(place, values)?;
                if bytes.len() as u64 > capacity { return Err(refused(OracleNetError::OutputTooSmall)); }
                // Check the entire declared slice before changing cells or
                // consuming staging. Failed fills remain retryable.
                let length = self.fill_host_buffer(place, &bytes, values)?;
                self.net.as_mut().unwrap().sockets.get_mut(&socket).unwrap().pending = None;
                values.insert(*destination, InterpreterValue::Size(length));
            }
            ("net_close", [token], []) => {
                let token = self.net_argument(token, SOCKET, values)?;
                self.net.as_mut().unwrap().close(&token)?;
            }
            ("serve_pool", [token], []) => {
                let token = self.net_argument(token, LISTENER, values)?;
                self.serve_pool(&token)?;
            }
            _ => return Err(InterpretError::UnsupportedOperation("Net fixture host call")),
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "net_tests.rs"]
mod tests;
