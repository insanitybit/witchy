//! Independent BuildNet semantics over exact ordered raw-response fixtures.
//! No ambient network, HTTP client, redirect handling, or process state is used.
use super::*;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleBuildNetFixture {
    /// Exact host:port strings, not URLs, origins, or ordinary Net grants.
    pub hosts: BTreeSet<String>,
    pub expectations: Vec<OracleBuildNetCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleBuildNetCall {
    pub host: String,
    pub path: String,
    /// The entire wire response through EOF, including any HTTP headers.
    pub response: Vec<u8>,
}

pub(super) struct State { consumed: usize }

fn refusal(message: impl Into<String>) -> InterpretError { InterpretError::BuildNet(message.into()) }

impl State {
    pub(super) fn new() -> Self { Self { consumed: 0 } }

    fn invoke(&mut self, fixture: &OracleBuildNetFixture, host: &str, path: &str) -> Result<Arc<[u8]>, InterpretError> {
        if !fixture.hosts.contains(host) {
            return Err(refusal(format!("fetch_build: `{host}` is not in this BuildNet grant's allow-list")));
        }
        let expected = fixture.expectations.get(self.consumed)
            .ok_or_else(|| refusal("BuildNet unexpected request after transcript end"))?;
        if expected.host != host || expected.path != path {
            return Err(refusal(format!("BuildNet transcript mismatch at request {}", self.consumed)));
        }
        // A missing/mismatched fixture is not an IO outcome and consumes no
        // request. An exact raw response is consumed once, independent of its
        // status line or whether it has recognizable HTTP framing at all.
        self.consumed += 1;
        let text = String::from_utf8_lossy(&expected.response);
        let bytes = text.as_bytes();
        let start = bytes.windows(4).position(|window| window == b"\r\n\r\n")
            .map_or(0, |delimiter| delimiter + 4);
        let body = &bytes[start..];
        if body.len() > i32::MAX as usize { return Err(InterpretError::ArithmeticTrap); }
        Ok(Arc::from(body))
    }

    fn finish(&self, fixture: Option<&OracleBuildNetFixture>) -> Result<(), InterpretError> {
        let required = fixture.map_or(0, |fixture| fixture.expectations.len());
        if self.consumed != required {
            return Err(refusal(format!("BuildNet unused transcript expectations: consumed {} of {required}", self.consumed)));
        }
        Ok(())
    }
}

impl Engine<'_> {
    pub(super) fn finish_build_net(&self) -> Result<(), InterpretError> {
        self.build_net.finish(self.config.fixture.build_net.as_ref())
    }

    pub(super) fn build_net_host_call(&mut self, destinations: &[ValueId], arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError> {
        let ([destination], [HostCallArgument::Capability(grant), HostCallArgument::Buffer { place: host, .. }, HostCallArgument::Buffer { place: path, .. }]) = (destinations, arguments) else {
            return Err(refusal("BuildNet host arguments"));
        };
        let InterpreterValue::Capability(grant) = self.value(*grant, values)? else {
            return Err(refusal("BuildNet capability value"));
        };
        if self.isolated { return Err(refusal("BuildNet unavailable in an isolated service")); }
        self.build_read.authenticate_net(grant)?;
        // Authentication precedes memory; both complete strict UTF-8 slices
        // precede consuming a fixture event or producing a response resource.
        let host = self.host_string(host, values)?;
        let path = self.host_string(path, values)?;
        let fixture = self.config.fixture.build_net.as_ref()
            .ok_or_else(|| refusal("BuildNet network fixture was not supplied"))?;
        let bytes = self.build_net.invoke(fixture, &host, &path)?;
        let response = self.build_read.some_response(bytes);
        values.insert(*destination, InterpreterValue::Capability(response));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_net_oracle_preserves_raw_status_lossy_framing_and_exact_order() {
        let fixture = OracleBuildNetFixture {
            hosts: BTreeSet::from(["peer:80".into(), "other:80".into()]),
            expectations: vec![
                OracleBuildNetCall { host: "peer:80".into(), path: "/first".into(),
                    response: b"HTTP/1.1 404 Missing\r\nHeader: \xff\r\n\r\none\xff\r\n\r\nrest".to_vec() },
                OracleBuildNetCall { host: "other:80".into(), path: "/second".into(), response: b"unframed\xff".to_vec() },
                OracleBuildNetCall { host: "peer:80".into(), path: "/third".into(),
                    response: b"HTTP/1.1 302 Found\r\nLocation: http://denied:80/\r\n\r\n".to_vec() },
            ],
        };
        let mut state = State::new();
        assert!(state.finish(Some(&fixture)).is_err());
        assert!(state.invoke(&fixture, "peer:81", "/first").unwrap_err().to_string().contains("allow-list"));
        assert!(state.invoke(&fixture, "other:80", "/second").unwrap_err().to_string().contains("transcript mismatch"));
        assert!(state.invoke(&fixture, "peer:80", "/wrong").is_err());
        assert_eq!(state.consumed, 0);
        let first = state.invoke(&fixture, "peer:80", "/first").unwrap();
        assert_eq!(first.as_ref(), "one\u{fffd}\r\n\r\nrest".as_bytes());
        let second = state.invoke(&fixture, "other:80", "/second").unwrap();
        assert_eq!(second.as_ref(), "unframed\u{fffd}".as_bytes());
        assert!(state.finish(Some(&fixture)).is_err());
        assert!(state.invoke(&fixture, "peer:80", "/third").unwrap().is_empty());
        assert_eq!(state.consumed, 3);
        state.finish(Some(&fixture)).unwrap();
        assert_eq!(first.as_ref(), "one\u{fffd}\r\n\r\nrest".as_bytes(), "later responses do not mutate the first");
        assert!(state.invoke(&fixture, "peer:80", "/first").is_err());
        assert_eq!(state.consumed, 3);
    }
}
