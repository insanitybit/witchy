//! Frozen registry host semantics over explicit observations and secret data.
//! Only Ed25519 mathematics is shared with a crypto library. No native host
//! marshalling, provider, keychain, environment, or clock is consulted here.
use super::*;
use std::sync::Arc;
use std::cell::RefCell;
use crate::canonical_safewir::{AccessMode, CLOCK_CAPABILITY_KIND, SECRET_STORE_CAPABILITY_KIND, SECRET_CAPABILITY_KIND};

pub(super) const IMPORTS: [&str; 5] = [
    "clock_now", "dir_make_dir_at", "secretstore_lookup_at", "crypto_sign_into", "crypto_public_key_into",
];
const CLOCK: u32 = CLOCK_CAPABILITY_KIND.get();
const STORE: u32 = SECRET_STORE_CAPABILITY_KIND.get();
const SECRET: u32 = SECRET_CAPABILITY_KIND.get();

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OracleClockFixture {
    pub grants: usize,
    /// Exact call order across all Clock handles, in Unix milliseconds.
    pub observations: Vec<i64>,
}

/// Explicit secret fixtures retain their kind. There is no oracle reveal
/// operation; even diagnostic Debug output redacts all secret material.
#[derive(Clone, PartialEq, Eq)]
pub enum OracleSecretFixture {
    Plain(Vec<u8>),
    Sealed(Vec<u8>),
    Signing(Vec<u8>),
}

impl fmt::Debug for OracleSecretFixture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Plain(_) => "Plain([redacted])",
            Self::Sealed(_) => "Sealed([redacted])",
            Self::Signing(_) => "Signing([redacted])",
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OracleSecretStoreFixture {
    /// Names are matched exactly, first match wins, including duplicate names.
    pub secrets: Vec<(String, OracleSecretFixture)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Resource {
    Clock(usize),
    Dir(String),
    /// An opened parent directory and a leaf, not a snapshot of a file inode.
    File { provenance: String, parent: usize, leaf: String },
    Store(Arc<OracleSecretStoreFixture>),
    Secret(Arc<OracleSecretFixture>),
    DirBytes(Arc<[u8]>),
}

impl Resource {
    fn kind(&self) -> u32 {
        match self { Self::Clock(_) => CLOCK, Self::Dir(_) => DIR_KIND, Self::File { .. } => FILE_KIND,
            Self::Store(_) => STORE, Self::Secret(_) => SECRET,
            Self::DirBytes(_) => crate::canonical_safewir::DIR_BYTES_RESPONSE_KIND.get() }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Token { issuer: Arc<()>, resource: Resource, directory: Option<usize> }
impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.issuer, &other.issuer) && self.resource == other.resource && self.directory == other.directory
    }
}
impl Eq for Token {}

pub(super) struct State {
    issuer: Arc<()>, consumed_clock: usize,
    // IDs are private to this issuer. A rename changes a location, not a
    // capability's identity. None denotes an unlinked empty directory whose
    // retained handle must never address a replacement at its former name.
    directories: RefCell<Vec<Option<String>>>,
}
fn refusal(message: impl Into<String>) -> InterpretError { InterpretError::RegistryFixture(message.into()) }

impl State {
    pub(super) fn new() -> Self { Self { issuer: Arc::new(()), consumed_clock: 0, directories: RefCell::new(Vec::new()) } }

    fn directory_id(&self, path: &str) -> usize {
        let mut directories = self.directories.borrow_mut();
        if let Some(id) = directories.iter().position(|entry| entry.as_deref() == Some(path)) { return id; }
        let id = directories.len();
        directories.push(Some(path.to_owned()));
        id
    }

    fn mint(&self, resource: Resource) -> CapabilityToken {
        let scope = match &resource { Resource::Dir(scope) => scope.clone(), Resource::File { provenance, .. } => provenance.clone(), _ => String::new() };
        let directory = if matches!(&resource, Resource::Dir(_)) { Some(self.directory_id(&scope)) } else { None };
        CapabilityToken { scope, registry: Some(Token { issuer: self.issuer.clone(), resource: resource.clone(), directory }),
            ..CapabilityToken::root(resource.kind()) }
    }

    fn authenticate(&self, token: &CapabilityToken, kind: u32) -> Result<Resource, InterpretError> {
        let handle = token.registry.as_ref().ok_or_else(|| refusal("registry unauthenticated resource"))?;
        let scope = match &handle.resource { Resource::Dir(scope) => scope.as_str(), Resource::File { provenance, .. } => provenance.as_str(), _ => "" };
        if token.kind != kind || handle.resource.kind() != kind || token.scope != scope
            || !Arc::ptr_eq(&self.issuer, &handle.issuer)
        {
            return Err(refusal("registry wrong kind, scope, or session"));
        }
        Ok(handle.resource.clone())
    }

    pub(super) fn narrow_dir(&self, source: &CapabilityToken, scope: String) -> Result<CapabilityToken, InterpretError> {
        // Low-level legacy read fixtures may carry unbound Dir tokens. They
        // stay unbound, so narrowing them cannot mint mkdir authority.
        if source.registry.is_none() {
            return Ok(CapabilityToken { scope, ..CapabilityToken::root(DIR_KIND) });
        }
        let parent = self.storage_scope(source)?;
        if !parent.is_empty() && scope != parent && !scope.starts_with(&format!("{parent}/")) {
            return Err(refusal("directory narrowing escaped its grant"));
        }
        Ok(self.mint(Resource::Dir(scope)))
    }

    pub(super) fn storage_scope(&self, token: &CapabilityToken) -> Result<String, InterpretError> {
        self.directory_location(token)?.ok_or(InterpretError::ExplicitTrap)
    }

    pub(super) fn directory_location(&self, token: &CapabilityToken) -> Result<Option<String>, InterpretError> {
        self.authenticate(token, DIR_KIND)?;
        let id = token.registry.as_ref().and_then(|handle| handle.directory)
            .ok_or_else(|| refusal("registry missing directory identity"))?;
        self.directories.borrow().get(id).cloned().ok_or_else(|| refusal("registry unknown directory identity"))
    }

    pub(super) fn relocate_directories(&self, from: &str, to: &str) {
        if from == to { return; }
        for location in self.directories.borrow_mut().iter_mut() {
            let Some(path) = location.as_deref() else { continue; };
            if path == from || path.strip_prefix(from).is_some_and(|suffix| suffix.starts_with('/')) {
                *location = Some(format!("{to}{}", &path[from.len()..]));
            } else if path == to || path.strip_prefix(to).is_some_and(|suffix| suffix.starts_with('/')) {
                *location = None;
            }
        }
    }

    pub(super) fn mint_file(&self, source: &CapabilityToken, path: String) -> Result<CapabilityToken, InterpretError> {
        let scope = self.storage_scope(source)?;
        if !scope.is_empty() && !path.starts_with(&format!("{scope}/")) {
            return Err(refusal("file escaped its parent directory grant"));
        }
        let (parent, leaf) = path.rsplit_once('/').unwrap_or(("", &path));
        let parent = self.directory_id(parent);
        let leaf = leaf.to_owned();
        Ok(self.mint(Resource::File { provenance: path, parent, leaf }))
    }

    pub(super) fn file_scope(&self, token: &CapabilityToken) -> Result<String, InterpretError> {
        let Resource::File { parent, leaf, .. } = self.authenticate(token, FILE_KIND)? else { unreachable!() };
        let directories = self.directories.borrow();
        let parent = directories.get(parent).and_then(Option::as_deref).ok_or(InterpretError::ExplicitTrap)?;
        Ok(if parent.is_empty() { leaf } else { format!("{parent}/{leaf}") })
    }

    pub(super) fn mint_dir_bytes(&self, bytes: Vec<u8>) -> CapabilityToken {
        self.mint(Resource::DirBytes(Arc::from(bytes)))
    }

    pub(super) fn dir_bytes(&self, token: &CapabilityToken) -> Result<Arc<[u8]>, InterpretError> {
        let Resource::DirBytes(bytes) = self.authenticate(token, crate::canonical_safewir::DIR_BYTES_RESPONSE_KIND.get())? else { unreachable!() };
        Ok(bytes)
    }
}

impl Engine<'_> {
    pub(super) fn bind_registry_entry(&self, mut arguments: Vec<InterpreterValue>) -> Result<Vec<InterpreterValue>, InterpretError> {
        let mut clocks = 0;
        let mut directories = 0;
        for argument in &mut arguments {
            if matches!(argument,
                InterpreterValue::Capability(token) | InterpreterValue::NullableCapability(Some(token))
                    if token.kind == crate::canonical_safewir::DIR_BYTES_RESPONSE_KIND.get())
            {
                return Err(refusal("directory byte responses are not entry grants"));
            }
            if let InterpreterValue::NullableCapability(Some(token)) = argument
                && matches!(token.kind, CLOCK | STORE | SECRET | DIR_KIND)
            {
                return Err(refusal("registry entry requires a fresh non-nullable root request"));
            }
            let InterpreterValue::Capability(token) = argument else { continue; };
            if !matches!(token.kind, CLOCK | STORE | SECRET | DIR_KIND) { continue; }
            if self.isolated || *token != CapabilityToken::root(token.kind) {
                return Err(refusal("registry entry is not a fresh root request"));
            }
            // Binding is not a host effect. Existing server profiles can
            // carry/read a Dir without invoking any registry import. Apply
            // worker effect restrictions at dispatch, using the actual import.
            let resource = match token.kind {
                CLOCK => {
                    let fixture = self.config.fixture.clock.as_ref().ok_or_else(|| refusal("Clock entry was not granted"))?;
                    if clocks >= fixture.grants { return Err(refusal("Clock entry was not granted")); }
                    let resource = Resource::Clock(clocks);
                    clocks += 1;
                    resource
                }
                DIR_KIND => {
                    if self.config.fixture.dir_grants.is_some_and(|grants| directories >= grants) {
                        return Err(refusal("Dir entry was not granted"));
                    }
                    directories += 1;
                    Resource::Dir(String::new())
                }
                STORE => Resource::Store(Arc::new(self.config.fixture.secret_store.clone()
                    .ok_or_else(|| refusal("SecretStore entry was not granted"))?)),
                SECRET => {
                    let secret = self.config.fixture.signing_secret.as_ref()
                        .filter(|secret| matches!(secret, OracleSecretFixture::Signing(_)))
                        .ok_or_else(|| refusal("Secret entry requires an explicit signing grant"))?;
                    Resource::Secret(Arc::new(secret.clone()))
                }
                _ => unreachable!(),
            };
            *token = self.registry.mint(resource);
        }
        Ok(arguments)
    }

    pub(super) fn finish_registry(&self) -> Result<(), InterpretError> {
        let required = self.config.fixture.clock.as_ref().map_or(0, |fixture| fixture.observations.len());
        if self.registry.consumed_clock != required {
            return Err(refusal(format!("Clock unused observations: consumed {} of {required}", self.registry.consumed_clock)));
        }
        Ok(())
    }

    fn registry_resource(&self, id: ValueId, kind: u32, values: &BTreeMap<ValueId, InterpreterValue>) -> Result<Resource, InterpretError> {
        let InterpreterValue::Capability(token) = self.value(id, values)? else { return Err(refusal("registry capability value")); };
        self.registry.authenticate(token, kind)
    }

    pub(super) fn registry_host_call(&mut self, name: &str, destinations: &[ValueId],
        arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError>
    {
        if self.isolated { return Err(InterpretError::UnsupportedHostImport); }
        self.server.before_host_call(name)?;
        if self.detached_dir_host_call(name, destinations, arguments, values)? { return Ok(()); }
        match (name, arguments, destinations) {
            ("clock_now", [HostCallArgument::Capability(clock)], [destination]) => {
                self.registry_resource(*clock, CLOCK, values)?;
                let observation = self.config.fixture.clock.as_ref()
                    .and_then(|fixture| fixture.observations.get(self.registry.consumed_clock))
                    .copied().ok_or_else(|| refusal("unexpected Clock call after observations end"))?;
                self.registry.consumed_clock += 1;
                values.insert(*destination, InterpreterValue::Int(observation));
            }
            ("dir_make_dir_at", [HostCallArgument::Capability(dir), HostCallArgument::Buffer { place, .. }], []) => {
                // Legacy read-only fixture tokens may have a scope string but
                // no issuer. Authenticate before resolving the retained handle
                // and before reading any guest input or mutating the namespace.
                self.registry_resource(*dir, DIR_KIND, values)?;
                let scope = self.capability_scope(&HostCallArgument::Capability(*dir), DIR_KIND, values)?;
                let path = self.host_string(place, values)?;
                let path = dir_create::normalize_path(&path)?;
                let path = self.host_path(&scope, &path)?;
                self.config.fixture.make_directory(&path)?;
            }
            ("secretstore_lookup_at", [HostCallArgument::Capability(store), HostCallArgument::Buffer { place, .. }], [destination]) => {
                let Resource::Store(store) = self.registry_resource(*store, STORE, values)? else { unreachable!() };
                let name = self.host_string(place, values)?;
                let secret = store.secrets.iter().find(|(candidate, _)| candidate == &name)
                    .map(|(_, secret)| self.registry.mint(Resource::Secret(Arc::new(secret.clone()))));
                values.insert(*destination, InterpreterValue::NullableCapability(secret));
            }
            ("crypto_sign_into", [HostCallArgument::Capability(secret), HostCallArgument::Buffer { place: message, .. }, HostCallArgument::Buffer { place: output, .. }], [destination]) => {
                let Resource::Secret(secret) = self.registry_resource(*secret, SECRET, values)? else { unreachable!() };
                // Snapshot the entire UTF-8 message before any output write,
                // including when the input and output slices alias.
                let message = self.host_string(message, values)?;
                let range = self.registry_output(output, values)?;
                let encoded = secret_bytes(&secret, Some(message.as_bytes()))?;
                self.registry_write(range, &encoded)?;
                values.insert(*destination, InterpreterValue::Size(128));
            }
            ("crypto_public_key_into", [HostCallArgument::Capability(secret), HostCallArgument::Buffer { place: output, .. }], [destination]) => {
                let Resource::Secret(secret) = self.registry_resource(*secret, SECRET, values)? else { unreachable!() };
                let range = self.registry_output(output, values)?;
                let encoded = secret_bytes(&secret, None)?;
                self.registry_write(range, &encoded)?;
                values.insert(*destination, InterpreterValue::Size(64));
            }
            _ => return Err(refusal("unsupported registry host arguments")),
        }
        Ok(())
    }

    pub(super) fn registry_output(&self, place: &Place, values: &BTreeMap<ValueId, InterpreterValue>) -> Result<(u64, u64, u64), InterpretError> {
        if place.access() != AccessMode::Write { return Err(refusal("registry output requires a write slice")); }
        let range @ (object, start, capacity) = self.slice_place(place, values)?;
        self.check_range(object, start, capacity, true)?;
        let object_state = self.object(object)?;
        if object_state.immutable || object_state.storage == RuntimeStorage::StaticData {
            return Err(InterpretError::ImmutableObject(object));
        }
        Ok(range)
    }

    pub(super) fn registry_write(&mut self, (object, start, capacity): (u64, u64, u64), bytes: &[u8]) -> Result<(), InterpretError> {
        if capacity >= bytes.len() as u64 {
            let cells = &mut self.object_mut(object)?.cells;
            for (offset, byte) in bytes.iter().enumerate() {
                cells.insert(CellKey::Element(start + offset as u64), InterpreterValue::Size(u64::from(*byte)));
            }
        }
        Ok(())
    }
}

fn seed(secret: &OracleSecretFixture) -> Result<[u8; 32], InterpretError> {
    let bytes = match secret { OracleSecretFixture::Plain(bytes) | OracleSecretFixture::Sealed(bytes) | OracleSecretFixture::Signing(bytes) => bytes };
    if let Ok(seed) = bytes.as_slice().try_into() { return Ok(seed); }
    if bytes.len() != 64 || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return Err(refusal("Ed25519 secret must be a 32-byte seed or 64 ASCII hex digits"));
    }
    let mut seed = [0; 32];
    let nibble = |byte: u8| if byte.is_ascii_digit() { byte - b'0' } else { byte.to_ascii_lowercase() - b'a' + 10 };
    for (index, pair) in bytes.chunks_exact(2).enumerate() { seed[index] = nibble(pair[0]) * 16 + nibble(pair[1]); }
    Ok(seed)
}

fn secret_bytes(secret: &OracleSecretFixture, message: Option<&[u8]>) -> Result<Vec<u8>, InterpretError> {
    let seed = seed(secret)?;
    let bytes = ed25519_bytes(&seed, message)?;
    let digits = b"0123456789abcdef";
    Ok(bytes.iter().flat_map(|byte| [digits[(byte >> 4) as usize], digits[(byte & 15) as usize]]).collect())
}

#[cfg(all(not(target_arch = "wasm32"), feature = "native"))]
fn ed25519_bytes(seed: &[u8; 32], message: Option<&[u8]>) -> Result<Vec<u8>, InterpretError> {
    use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
    let key = Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| refusal("Ed25519 seed refused"))?;
    Ok(match message { Some(message) => key.sign(message).as_ref().to_vec(), None => key.public_key().as_ref().to_vec() })
}

#[cfg(any(target_arch = "wasm32", not(feature = "native")))]
fn ed25519_bytes(seed: &[u8; 32], message: Option<&[u8]>) -> Result<Vec<u8>, InterpretError> {
    use ed25519_dalek::{Signer, SigningKey};
    let key = SigningKey::from_bytes(seed);
    Ok(match message { Some(message) => key.sign(message).to_bytes().to_vec(), None => key.verifying_key().to_bytes().to_vec() })
}

#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
#[path = "registry_host_tests.rs"]
pub mod self_checks;
