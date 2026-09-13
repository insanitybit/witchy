//! Independent confined-build semantics over explicit virtual read roots.
//! No interpreter fallback consults the host filesystem or process environment.
use super::*;
use std::sync::Arc;
use crate::canonical_safewir::{BUILD_READ_CAPABILITY_KIND, BUILD_READ_RESPONSE_KIND, BUILD_READ_IMPORT_NAMES};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleBuildReadFixture {
    /// Each map is one granted root, in resolution order. Entries are regular
    /// files; absent, unreadable and non-UTF-8 files do not satisfy a read.
    pub roots: Vec<BTreeMap<String, Vec<u8>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Resource { Grant, Response(Arc<[u8]>) }
#[derive(Debug, Clone)]
pub(super) struct Token { issuer: Arc<()>, resource: Resource }
impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.issuer, &other.issuer) && self.resource == other.resource
    }
}
impl Eq for Token {}

pub(super) struct State { issuer: Arc<()> }
impl State {
    pub(super) fn new() -> Self { Self { issuer: Arc::new(()) } }

    fn mint(&self, resource: Resource) -> CapabilityToken {
        let kind = match &resource {
            Resource::Grant => BUILD_READ_CAPABILITY_KIND.get(),
            Resource::Response(_) => BUILD_READ_RESPONSE_KIND.get(),
        };
        CapabilityToken {
            build_read: Some(Token { issuer: self.issuer.clone(), resource }),
            ..CapabilityToken::root(kind)
        }
    }

    fn authenticate(&self, capability: &CapabilityToken, kind: u32) -> Result<Resource, InterpretError> {
        let token = capability.build_read.as_ref().ok_or_else(|| refusal("BuildRead unauthenticated resource"))?;
        let actual_kind = match &token.resource {
            Resource::Grant => BUILD_READ_CAPABILITY_KIND.get(),
            Resource::Response(_) => BUILD_READ_RESPONSE_KIND.get(),
        };
        if capability.kind != kind || actual_kind != kind || !Arc::ptr_eq(&token.issuer, &self.issuer) {
            return Err(refusal("BuildRead wrong kind or session"));
        }
        Ok(token.resource.clone())
    }
}

fn refusal(message: &str) -> InterpretError { InterpretError::BuildRead(message.into()) }
pub(super) fn is_import(name: &str) -> bool { BUILD_READ_IMPORT_NAMES.contains(&name) }

fn read_response(fixture: &OracleBuildReadFixture, path: &str) -> Result<Arc<[u8]>, InterpretError> {
    if path.contains('\0') { return Err(refusal("BuildRead NUL path")); }
    let mut parts = Vec::new();
    for component in std::path::Path::new(path).components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_str().ok_or_else(|| refusal("BuildRead non-UTF-8 path"))?),
            std::path::Component::CurDir => {},
            _ => return Err(refusal("BuildRead path escapes granted root")),
        }
    }
    if parts.is_empty() { return Err(refusal("BuildRead path is not a file")); }
    let path = parts.join("/");
    for root in &fixture.roots {
        let Some(bytes) = root.get(&path) else { continue; };
        if std::str::from_utf8(bytes).is_err() { continue; }
        if bytes.len() > i32::MAX as usize { return Err(InterpretError::ArithmeticTrap); }
        return Ok(Arc::from(bytes.as_slice()));
    }
    Err(refusal("BuildRead file unavailable in granted roots"))
}

impl Engine<'_> {
    pub(super) fn bind_build_read_entry(&self, mut arguments: Vec<InterpreterValue>) -> Result<Vec<InterpreterValue>, InterpretError> {
        for argument in &mut arguments {
            if let InterpreterValue::Capability(token) = argument
                && token.kind == BUILD_READ_CAPABILITY_KIND.get()
            {
                if self.isolated || self.config.fixture.build_read.as_ref().is_none_or(|fixture| fixture.roots.is_empty()) {
                    return Err(refusal("BuildRead entry was not granted"));
                }
                if token.build_read.is_some() || token.build_out.is_some() || token.exec.is_some()
                    || token.net.is_some() || token.service.is_some() || !token.scope.is_empty()
                {
                    return Err(refusal("BuildRead entry is not a fresh root request"));
                }
                *token = self.build_read.mint(Resource::Grant);
            }
        }
        Ok(arguments)
    }

    fn build_read_resource(&self, argument: &HostCallArgument, kind: u32, values: &BTreeMap<ValueId, InterpreterValue>) -> Result<Resource, InterpretError> {
        let HostCallArgument::Capability(value) = argument else { return Err(refusal("BuildRead capability argument")) };
        let InterpreterValue::Capability(capability) = self.value(*value, values)? else { return Err(refusal("BuildRead capability value")) };
        if self.isolated { return Err(refusal("BuildRead unavailable in an isolated service")); }
        self.build_read.authenticate(capability, kind)
    }

    pub(super) fn build_read_host_call(&mut self, name: &str, destinations: &[ValueId], arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError> {
        let [destination] = destinations else { return Err(refusal("BuildRead result arity")) };
        match (name, arguments) {
            ("build_read_at", [grant, HostCallArgument::Buffer { place, .. }]) => {
                if self.build_read_resource(grant, BUILD_READ_CAPABILITY_KIND.get(), values)? != Resource::Grant {
                    return Err(refusal("BuildRead response is not a read grant"));
                }
                let path = self.host_string(place, values)?;
                let fixture = self.config.fixture.build_read.as_ref().ok_or_else(|| refusal("BuildRead roots not supplied"))?;
                let response = self.build_read.mint(Resource::Response(read_response(fixture, &path)?));
                values.insert(*destination, InterpreterValue::Capability(response));
            }
            ("build_read_response_size", [response]) => {
                let Resource::Response(bytes) = self.build_read_resource(response, BUILD_READ_RESPONSE_KIND.get(), values)? else { return Err(refusal("BuildRead response kind")) };
                values.insert(*destination, InterpreterValue::Size(bytes.len() as u64));
            }
            ("build_read_response_into", [response, HostCallArgument::Buffer { place, .. }]) => {
                let Resource::Response(bytes) = self.build_read_resource(response, BUILD_READ_RESPONSE_KIND.get(), values)? else { return Err(refusal("BuildRead response kind")) };
                let (object, start, capacity) = self.slice_place(place, values)?;
                self.check_range(object, start, capacity, true)?;
                if capacity >= bytes.len() as u64 {
                    for (offset, byte) in bytes.iter().enumerate() {
                        self.object_mut(object)?.cells.insert(CellKey::Element(start + offset as u64), InterpreterValue::Size(u64::from(*byte)));
                    }
                }
                values.insert(*destination, InterpreterValue::Size(bytes.len() as u64));
            }
            _ => return Err(refusal("unsupported BuildRead host arguments")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_read_oracle_tokens_require_private_kind_and_session() {
        let first = State::new();
        let second = State::new();
        let grant = first.mint(Resource::Grant);
        let response = first.mint(Resource::Response(Arc::from(&b"snapshot"[..])));
        assert_eq!(first.authenticate(&grant, BUILD_READ_CAPABILITY_KIND.get()).unwrap(), Resource::Grant);
        assert!(first.authenticate(&response, BUILD_READ_RESPONSE_KIND.get()).is_ok());
        for (token, wrong_kind) in [(&grant, BUILD_READ_RESPONSE_KIND.get()), (&response, BUILD_READ_CAPABILITY_KIND.get())] {
            assert!(first.authenticate(token, wrong_kind).is_err());
            assert!(second.authenticate(token, token.kind).is_err());
            assert!(first.authenticate(&CapabilityToken::root(token.kind), token.kind).is_err());
        }
        let mut mislabeled = grant.clone();
        mislabeled.kind = BUILD_READ_RESPONSE_KIND.get();
        assert!(first.authenticate(&mislabeled, BUILD_READ_RESPONSE_KIND.get()).is_err());
    }

    #[test]
    fn build_read_oracle_roots_are_ordered_strict_utf8_and_snapshotted() {
        let mut fixture = OracleBuildReadFixture { roots: vec![
            BTreeMap::from([("file".into(), vec![0xff])]),
            BTreeMap::from([("file".into(), b"second".to_vec())]),
            BTreeMap::from([("file".into(), b"third".to_vec())]),
        ] };
        let snapshot = read_response(&fixture, "./file/").unwrap();
        assert_eq!(snapshot.as_ref(), b"second");
        fixture.roots[0].insert("file".into(), b"first".to_vec());
        fixture.roots[1].clear();
        assert_eq!(read_response(&fixture, "file").unwrap().as_ref(), b"first");
        assert_eq!(snapshot.as_ref(), b"second");
        for path in ["../file", "/file", "file\0", ".", "absent"] {
            assert!(read_response(&fixture, path).is_err(), "must reject {path:?}");
        }
        assert!(read_response(&OracleBuildReadFixture { roots: vec![] }, "file").is_err());
        assert!(read_response(&OracleBuildReadFixture { roots: vec![BTreeMap::from([("file".into(), vec![0xff])])] }, "file").is_err());
    }
}
