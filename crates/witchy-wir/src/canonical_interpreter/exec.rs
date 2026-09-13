//! Independent, explicitly scripted process effects. Never launches a process.
use super::*;
use std::sync::Arc;
use crate::canonical_safewir::{EXEC_CAPABILITY_KIND, EXEC_RESPONSE_KIND, EXEC_IMPORT_NAMES};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleExecCall {
    pub directory: String,
    pub path: String,
    /// The primitive's exact NUL-separated argument string, after std validation.
    pub arguments: String,
    pub stdin: String,
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleExecFixture {
    pub allow: Option<BTreeSet<String>>,
    pub dir_read: bool,
    pub calls: Vec<OracleExecCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Resource { Grant, Response(Arc<[u8]>) }
#[derive(Debug, Clone)]
pub(super) struct Token { issuer: Arc<()>, resource: Resource }
impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.issuer, &other.issuer) && self.resource == other.resource }
}
impl Eq for Token {}
pub(super) struct State { issuer: Arc<()>, consumed: usize }
impl State {
    pub(super) fn new() -> Self { Self { issuer: Arc::new(()), consumed: 0 } }

    fn authenticate(&self, capability: &CapabilityToken, kind: u32) -> Result<Resource, InterpretError> {
        let token = capability.exec.as_ref().ok_or_else(|| refusal("Exec unauthenticated resource"))?;
        if capability.kind != kind || !Arc::ptr_eq(&token.issuer, &self.issuer) {
            return Err(refusal("Exec wrong kind or session"));
        }
        Ok(token.resource.clone())
    }
}
pub(super) fn is_import(name: &str) -> bool { EXEC_IMPORT_NAMES.contains(&name) }
fn refusal(message: &str) -> InterpretError { InterpretError::ExecFixture(message.into()) }

impl Engine<'_> {
    pub(super) fn bind_exec_entry(&self, mut arguments: Vec<InterpreterValue>) -> Result<Vec<InterpreterValue>, InterpretError> {
        for argument in &mut arguments {
            if let InterpreterValue::Capability(token) = argument
                && token.kind == EXEC_CAPABILITY_KIND.get()
            {
                if self.config.fixture.exec.is_none() || self.isolated { return Err(refusal("Exec entry was not granted")); }
                if token.exec.is_some() || token.build_read.is_some() || token.net.is_some() || token.service.is_some() || token.build_out.is_some() || !token.scope.is_empty() {
                    return Err(refusal("Exec entry is not a fresh root request"));
                }
                token.exec = Some(Token { issuer: self.exec.issuer.clone(), resource: Resource::Grant });
            }
        }
        Ok(arguments)
    }

    pub(super) fn finish_exec(&self) -> Result<(), InterpretError> {
        if self.config.fixture.exec.as_ref().is_some_and(|fixture| fixture.calls.len() != self.exec.consumed) {
            return Err(refusal("unused required Exec transcript calls"));
        }
        Ok(())
    }

    fn exec_resource(&self, argument: &HostCallArgument, kind: u32, values: &BTreeMap<ValueId, InterpreterValue>) -> Result<Resource, InterpretError> {
        let HostCallArgument::Capability(value) = argument else { return Err(refusal("Exec capability argument")) };
        let InterpreterValue::Capability(capability) = self.value(*value, values)? else { return Err(refusal("Exec capability value")) };
        if self.isolated { return Err(refusal("Exec unavailable in an isolated service")); }
        self.exec.authenticate(capability, kind)
    }

    pub(super) fn exec_host_call(&mut self, name: &str, destinations: &[ValueId], arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError> {
        let [destination] = destinations else { return Err(refusal("Exec result arity")) };
        match (name, arguments) {
            ("exec_run_at", [exec, dir, HostCallArgument::Buffer { place: path, .. }, HostCallArgument::Buffer { place: args, .. }, HostCallArgument::Buffer { place: stdin, .. }]) => {
                if self.exec_resource(exec, EXEC_CAPABILITY_KIND.get(), values)? != Resource::Grant { return Err(refusal("Exec response is not a grant")); }
                let scope = self.capability_scope(dir, DIR_KIND, values)?;
                let fixture = self.config.fixture.exec.as_ref().ok_or_else(|| refusal("Exec transport unsupported without a script"))?;
                if !fixture.dir_read { return Err(refusal("Exec Dir lacks read right")); }
                let path = self.host_string(path, values)?;
                let args = self.host_string(args, values)?;
                let stdin = self.host_string(stdin, values)?;
                if fixture.allow.as_ref().is_some_and(|allow| !allow.contains(&path)) { return Err(refusal("Exec path outside allow-list")); }
                if path.contains('\0') { return Err(refusal("Exec NUL path")); }
                // The script records the actual confined scope and spelling.
                // Parent/absolute traversal is refused before matching an effect.
                for component in std::path::Path::new(&path).components() {
                    if !matches!(component, std::path::Component::Normal(_) | std::path::Component::CurDir) { return Err(refusal("Exec escaping path")); }
                }
                let call = fixture.calls.get(self.exec.consumed).ok_or_else(|| refusal("unexpected Exec call"))?;
                if call.directory != scope || call.path != path || call.arguments != args || call.stdin != stdin {
                    return Err(refusal("Exec transcript mismatch"));
                }
                let payload = format!("{}\n{}{}", call.status, String::from_utf8_lossy(&call.stdout), String::from_utf8_lossy(&call.stderr)).into_bytes();
                if payload.len() > i32::MAX as usize { return Err(InterpretError::ArithmeticTrap); }
                self.exec.consumed += 1;
                let token = CapabilityToken { exec: Some(Token { issuer: self.exec.issuer.clone(), resource: Resource::Response(payload.into()) }), ..CapabilityToken::root(EXEC_RESPONSE_KIND.get()) };
                values.insert(*destination, InterpreterValue::Capability(token));
            }
            ("exec_response_size", [response]) => {
                let Resource::Response(bytes) = self.exec_resource(response, EXEC_RESPONSE_KIND.get(), values)? else { return Err(refusal("Exec response kind")) };
                values.insert(*destination, InterpreterValue::Size(bytes.len() as u64));
            }
            ("exec_response_into", [response, HostCallArgument::Buffer { place, .. }]) => {
                let Resource::Response(bytes) = self.exec_resource(response, EXEC_RESPONSE_KIND.get(), values)? else { return Err(refusal("Exec response kind")) };
                let (object, start, capacity) = self.slice_place(place, values)?;
                self.check_range(object, start, capacity, true)?;
                if capacity >= bytes.len() as u64 {
                    for (offset, byte) in bytes.iter().enumerate() {
                        self.object_mut(object)?.cells.insert(CellKey::Element(start + offset as u64), InterpreterValue::Size(u64::from(*byte)));
                    }
                }
                values.insert(*destination, InterpreterValue::Size(bytes.len() as u64));
            }
            _ => return Err(refusal("unsupported Exec host arguments")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_oracle_resources_require_private_kind_and_session() {
        let first = State::new();
        let second = State::new();
        let grant = CapabilityToken {
            exec: Some(Token { issuer: first.issuer.clone(), resource: Resource::Grant }),
            ..CapabilityToken::root(EXEC_CAPABILITY_KIND.get())
        };
        assert_eq!(first.authenticate(&grant, EXEC_CAPABILITY_KIND.get()).unwrap(), Resource::Grant);
        assert!(second.authenticate(&grant, EXEC_CAPABILITY_KIND.get()).is_err());
        assert!(first.authenticate(&grant, EXEC_RESPONSE_KIND.get()).is_err());
        assert!(first.authenticate(&CapabilityToken::root(EXEC_CAPABILITY_KIND.get()), EXEC_CAPABILITY_KIND.get()).is_err());
        let response = CapabilityToken {
            exec: Some(Token { issuer: first.issuer.clone(), resource: Resource::Response(Arc::from(&b"7\nout"[..])) }),
            ..CapabilityToken::root(EXEC_RESPONSE_KIND.get())
        };
        assert!(matches!(first.authenticate(&response, EXEC_RESPONSE_KIND.get()).unwrap(), Resource::Response(_)));
        assert!(first.authenticate(&response, EXEC_CAPABILITY_KIND.get()).is_err());
        assert!(second.authenticate(&response, EXEC_RESPONSE_KIND.get()).is_err());
    }
}
