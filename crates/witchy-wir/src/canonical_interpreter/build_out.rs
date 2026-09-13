//! Independent BuildOut host semantics for a fresh virtual output directory.
//! No runtime filesystem helper or encoder implementation participates here.
use super::*;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleBuildWrite { pub path: String, pub contents: String }

#[derive(Debug, Clone)]
pub(super) struct Token(Arc<()>);
impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) }
}
impl Eq for Token {}

pub(super) struct State {
    issuer: Arc<()>,
    pub(super) writes: Vec<OracleBuildWrite>,
    files: BTreeSet<String>,
    directories: BTreeSet<String>,
}
impl State {
    pub(super) fn new() -> Self {
        Self { issuer: Arc::new(()), writes: Vec::new(), files: BTreeSet::new(), directories: BTreeSet::new() }
    }
}

impl Engine<'_> {
    pub(super) fn bind_build_entry(&self, mut arguments: Vec<InterpreterValue>) -> Result<Vec<InterpreterValue>, InterpretError> {
        for argument in &mut arguments {
            if let InterpreterValue::Capability(token) = argument
                && token.kind == crate::canonical_safewir::BUILD_OUT_CAPABILITY_KIND.get()
            {
                if !self.config.fixture.build_out || self.isolated {
                    return Err(InterpretError::BuildOut("write_out: no BuildOut grant".into()));
                }
                // A public root is a launch request, never a reusable host token.
                if token.build_out.is_some() || token.build_read.is_some() || token.net.is_some() || token.service.is_some() || !token.scope.is_empty() {
                    return Err(InterpretError::BuildOut("write_out: invalid root request".into()));
                }
                token.build_out = Some(Token(self.build_out.issuer.clone()));
            }
        }
        Ok(arguments)
    }

    pub(super) fn build_out_host_call(&mut self, destinations: &[ValueId], arguments: &[HostCallArgument], values: &BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError> {
        let ([HostCallArgument::Capability(token), HostCallArgument::Buffer { place: name, .. }, HostCallArgument::Buffer { place: contents, .. }], []) = (arguments, destinations) else {
            return Err(InterpretError::TypeMismatch("BuildOut arguments"));
        };
        let InterpreterValue::Capability(token) = self.value(*token, values)? else {
            return Err(InterpretError::TypeMismatch("BuildOut token"));
        };
        if token.kind != crate::canonical_safewir::BUILD_OUT_CAPABILITY_KIND.get()
            || !token.build_out.as_ref().is_some_and(|token| Arc::ptr_eq(&token.0, &self.build_out.issuer))
            || !self.config.fixture.build_out || self.isolated
        { return Err(InterpretError::BuildOut("write_out: unauthenticated write capability".into())); }
        let name = self.host_string(name, values)?;
        let contents = self.host_string(contents, values)?;
        // Unlike the older Dir oracle helper, the actual confined BuildOut
        // transport allows harmless '.' components. Parent/absolute/NUL refuse.
        if name.contains('\0') { return Err(InterpretError::BuildOut("write_out: NUL in path".into())); }
        let mut pieces = Vec::new();
        for component in std::path::Path::new(&name).components() {
            match component {
                std::path::Component::Normal(value) => pieces.push(value.to_str().ok_or(InterpretError::TypeMismatch("BuildOut path UTF-8"))?),
                std::path::Component::CurDir => {},
                std::path::Component::ParentDir => return Err(InterpretError::BuildOut("`..` escapes the Dir capability".into())),
                _ => return Err(InterpretError::BuildOut("absolute paths are not allowed (a Dir capability is a subtree)".into())),
            }
        }
        let path = pieces.join("/");
        if path.is_empty() || self.build_out.directories.contains(&path) {
            return Err(InterpretError::BuildOut("write_out: target is a directory".into()));
        }
        for index in 1..pieces.len() {
            let parent = pieces[..index].join("/");
            if self.build_out.files.contains(&parent) { return Err(InterpretError::BuildOut("write_out: parent is a file".into())); }
            self.build_out.directories.insert(parent);
        }
        self.build_out.files.insert(path);
        self.build_out.writes.push(OracleBuildWrite { path: name, contents });
        Ok(())
    }
}
