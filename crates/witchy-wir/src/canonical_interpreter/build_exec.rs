//! Independent BuildExec transcript semantics. Fixture results are explicit;
//! neither subprocess execution nor ambient executable resolution is available.
use super::*;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleBuildExecFixture {
    pub tools: BTreeSet<String>,
    pub expectations: Vec<OracleBuildExecCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleBuildExecCall {
    pub tool: String,
    pub stdin: String,
    /// Raw tool stdout: success applies the source contract's lossy UTF-8 decode.
    /// Stderr is not a source return value.
    pub stdout: Vec<u8>,
    pub exit_code: i32,
}

pub(super) struct State { consumed: usize }

fn refusal(message: impl Into<String>) -> InterpretError { InterpretError::BuildExec(message.into()) }

impl State {
    pub(super) fn new() -> Self { Self { consumed: 0 } }

    fn invoke(&mut self, fixture: &OracleBuildExecFixture, tool: &str, stdin: &str) -> Result<Arc<[u8]>, InterpretError> {
        if !fixture.tools.contains(tool) {
            return Err(refusal(format!("run_tool: `{tool}` is not in this BuildExec grant's allow-list")));
        }
        let expected = fixture.expectations.get(self.consumed)
            .ok_or_else(|| refusal("BuildExec unexpected invocation after transcript end"))?;
        if expected.tool != tool || expected.stdin != stdin {
            return Err(refusal(format!("BuildExec transcript mismatch at invocation {}", self.consumed)));
        }
        if !(0..=255).contains(&expected.exit_code) {
            return Err(refusal("BuildExec fixture supports ordinary exit codes only"));
        }
        // Only an exact invocation consumes an event. A real nonzero process
        // outcome consumes it too, then aborts; it is not a transport mismatch.
        self.consumed += 1;
        if expected.exit_code != 0 {
            return Err(refusal(format!("run_tool: `{tool}` exited with exit status: {}", expected.exit_code)));
        }
        let stdout = String::from_utf8_lossy(&expected.stdout);
        if stdout.len() > i32::MAX as usize { return Err(InterpretError::ArithmeticTrap); }
        Ok(Arc::from(stdout.as_bytes()))
    }

    fn finish(&self, fixture: Option<&OracleBuildExecFixture>) -> Result<(), InterpretError> {
        let required = fixture.map_or(0, |fixture| fixture.expectations.len());
        if self.consumed != required {
            return Err(refusal(format!("BuildExec unused transcript expectations: consumed {} of {required}", self.consumed)));
        }
        Ok(())
    }
}

impl Engine<'_> {
    pub(super) fn finish_build_exec(&self) -> Result<(), InterpretError> {
        self.build_exec.finish(self.config.fixture.build_exec.as_ref())
    }

    pub(super) fn build_exec_host_call(&mut self, destinations: &[ValueId], arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError> {
        let ([destination], [HostCallArgument::Capability(grant), HostCallArgument::Buffer { place: tool, .. }, HostCallArgument::Buffer { place: stdin, .. }]) = (destinations, arguments) else {
            return Err(refusal("BuildExec host arguments"));
        };
        let InterpreterValue::Capability(grant) = self.value(*grant, values)? else {
            return Err(refusal("BuildExec capability value"));
        };
        if self.isolated { return Err(refusal("BuildExec unavailable in an isolated service")); }
        self.build_read.authenticate_exec(grant)?;
        // Authenticate before either buffer, and validate both full slices
        // before consuming a process expectation or producing any response.
        let tool = self.host_string(tool, values)?;
        let stdin = self.host_string(stdin, values)?;
        let fixture = self.config.fixture.build_exec.as_ref()
            .ok_or_else(|| refusal("BuildExec tool fixture was not supplied"))?;
        let bytes = self.build_exec.invoke(fixture, &tool, &stdin)?;
        let response = self.build_read.some_response(bytes);
        values.insert(*destination, InterpreterValue::Capability(response));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_exec_oracle_transcript_is_exact_ordered_and_terminal() {
        let fixture = OracleBuildExecFixture {
            tools: BTreeSet::from(["tool".into(), "other".into()]),
            expectations: vec![
                OracleBuildExecCall { tool: "tool".into(), stdin: "first".into(), stdout: vec![0xff, b'x'], exit_code: 0 },
                OracleBuildExecCall { tool: "other".into(), stdin: "second".into(), stdout: b"not returned".to_vec(), exit_code: 7 },
            ],
        };
        let mut state = State::new();
        assert!(state.finish(Some(&fixture)).is_err());
        assert!(state.invoke(&fixture, "denied", "first").unwrap_err().to_string().contains("allow-list"));
        assert!(state.invoke(&fixture, "other", "second").unwrap_err().to_string().contains("transcript mismatch"));
        assert!(state.invoke(&fixture, "tool", "wrong").is_err());
        assert_eq!(state.consumed, 0, "refused calls must not skip expected events");
        let first = state.invoke(&fixture, "tool", "first").unwrap();
        assert_eq!(first.as_ref(), "\u{fffd}x".as_bytes());
        assert_eq!(state.consumed, 1);
        assert!(state.finish(Some(&fixture)).is_err());
        assert!(state.invoke(&fixture, "other", "second").unwrap_err().to_string().contains("exit status: 7"));
        assert_eq!(state.consumed, 2, "a nonzero real outcome consumes its invocation");
        assert_eq!(first.as_ref(), "\u{fffd}x".as_bytes(), "later calls cannot alter an earlier response");
        assert!(state.invoke(&fixture, "tool", "first").is_err());
        assert_eq!(state.consumed, 2);
    }
}
