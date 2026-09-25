//! Executable reference semantics for validated canonical SafeWIR.
//!
//! This interpreter intentionally operates on semantic operations rather than
//! decoding or sharing logic with the Wasm backend. Unsupported semantics fail
//! closed so differential execution cannot accidentally bless backend behavior
//! that the oracle does not model.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::canonical_safewir::{
    AllocationClass, ArithmeticOperator, BasicBlock, BlockId, CallArgument, CanonicalModule,
    ComparisonOperator, Extent, Function, FunctionId, HeaderField, HostCallArgument, LoadTransfer,
    Operation, InitialValid, OverflowBehavior, Place, Projection, StoreTransfer, Terminator,
    ValidatedSafeWir, ValueId, ValueType, FunctionSignature, FunctionSignatureId,
};
use crate::canonical_safewir::closure_owner_contract::{
    CaptureStorage, CaptureTransfer, ConstructorId, OwnerFamily, OwnerFamilySource, OwnerOperation,
    SharedPlaceBounds, SharedReferenceConstructor,
};
use crate::layout::{HeaderLayout, LayoutBundle, LayoutId, LayoutInterner, LayoutKind, ScalarKind};

const SIZE_MAX: u64 = u32::MAX as u64;

fn rooted_int_slice_reference(source_type: &witchy_syntax::ast::Type) -> bool {
    matches!(source_type,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
            if matches!(inner.as_ref(), witchy_syntax::ast::Type::Slice(element)
                if matches!(element.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
                    if name == "Int" && arguments.is_empty())))
}

mod exec;
pub use exec::{OracleExecFixture, OracleExecCall};
mod fetch;
pub use fetch::{OracleFetchFixture, OracleFetchCall, OracleFetchSource, OracleFetchOutcome};
pub(crate) mod registry_host;
pub use registry_host::{OracleClockFixture, OracleSecretFixture, OracleSecretStoreFixture};
mod dir_create;
mod dir_storage;
mod dir_handles;
pub use dir_storage::OracleDirMutations;
pub(crate) mod crypto;
mod crypto_hash;
mod crypto_extensions;
mod dir_metadata;
mod build_out;
pub use build_out::OracleBuildWrite;
pub(crate) mod build_read;
pub use build_read::{OracleBuildReadFixture, OracleBuildEnvFixture};
mod build_exec;
pub use build_exec::{OracleBuildExecFixture, OracleBuildExecCall};
mod build_net;
pub use build_net::{OracleBuildNetFixture, OracleBuildNetCall};
mod launch;
mod nullable;
mod regex;
mod net;
mod vm_service;
pub use net::{OracleNetEvent, OracleNetFixture, OracleNetGrant, OracleNetError, OracleNetRead, ORACLE_NET_BUFFER_CAPACITY};
pub use net::{OracleServerPoolFixture, OracleServerWorkerRun};
pub use launch::interpret_entry_program;

/// The canonical capability operations the oracle serves, by import name.
const CAPABILITY_IMPORTS: [&str; 10] = [
    "dir_read_into",
    "dir_list_into",
    "dir_exists_at",
    "dir_is_dir_at",
    "dir_write_at",
    "dir_subtree_at",
    "dir_read_file",
    "file_read_into",
    "env_has_var",
    "env_get_into",
];

/// The fixed ABI kinds of the tokens those operations take.
const DIR_KIND: u32 = crate::canonical_safewir::DIR_CAPABILITY_KIND.get();
const ENV_KIND: u32 = crate::canonical_safewir::ENV_CAPABILITY_KIND.get();
const FILE_KIND: u32 = crate::canonical_safewir::FILE_CAPABILITY_KIND.get();

/// A value observable at a SafeWIR function boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpreterValue {
    Int(i64),
    FloatBits(u64),
    Bool(bool),
    Size(u64),
    Unit,
    V128([u8; 16]),
    InlineAggregate { layout: LayoutId, fields: Vec<InterpreterValue> },
    ObjectRef(u64),
    Function(FunctionId),
    /// An opaque host token for a capability parameter. The program never
    /// reads it; only the oracle's host calls do, and only for the authority
    /// the token's kind and scope carry.
    Capability(CapabilityToken),
    NullableCapability(Option<CapabilityToken>),
    /// An opaque owner token. The numeric identity is private oracle state and
    /// is never a guest value or an authority selector.
    OpaqueOwner(OpaqueOwnerToken),
    /// An opaque typed list of owner tokens.
    OpaqueOwnerList(OpaqueOwnerListToken),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct OpaqueOwnerToken(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct OpaqueOwnerListToken(u64);

/// A host-minted capability token. Kind 1 (`Dir`) carries the subtree it is
/// confined to — the empty string for a root grant — and kind 2 (`Env`)
/// carries none. A token is only ever minted here: by the launcher for an
/// entry parameter, or by the host narrowing one it was handed.
/// Net resource operations additionally require a private execution-bound
/// handle. `root(NET_KIND)` is only a launch request; `root(SOCKET_KIND)`
/// cannot authenticate socket I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityToken {
    kind: u32,
    scope: String,
    net: Option<net::Token>,
    service: Option<vm_service::Token>,
    build_out: Option<build_out::Token>,
    build_read: Option<build_read::Token>,
    exec: Option<exec::Token>,
    fetch: Option<fetch::Token>,
    registry: Option<registry_host::Token>,
}

impl CapabilityToken {
    /// The root token a launcher mints for one entry capability parameter.
    pub fn root(kind: u32) -> Self {
        Self { kind, scope: String::new(), net: None, service: None, build_out: None, build_read: None, exec: None, fetch: None, registry: None }
    }

    pub fn kind(&self) -> u32 {
        self.kind
    }

    /// The subtree prefix this token is confined to, `""` at the root.
    pub fn scope(&self) -> &str {
        &self.scope
    }
}

impl InterpreterValue {
    /// The launcher's entry token for a capability parameter of `kind`.
    pub fn capability(kind: u32) -> Self {
        Self::Capability(CapabilityToken::root(kind))
    }

    /// The token a launcher mints for one entry capability parameter: a
    /// `Dir` parameter receives its granted root, scoped to the whole tree.
    pub fn entry_capability(capability: crate::CanonicalEntryCapability) -> Self {
        Self::capability(match capability {
            crate::CanonicalEntryCapability::Console => 0,
            crate::CanonicalEntryCapability::Exec => crate::canonical_safewir::EXEC_CAPABILITY_KIND.get(),
            crate::CanonicalEntryCapability::Fetch => crate::canonical_safewir::FETCH_CAPABILITY_KIND.get(),
            crate::CanonicalEntryCapability::Clock => crate::canonical_safewir::CLOCK_CAPABILITY_KIND.get(),
            crate::CanonicalEntryCapability::SecretStore => crate::canonical_safewir::SECRET_STORE_CAPABILITY_KIND.get(),
            crate::CanonicalEntryCapability::Secret => crate::canonical_safewir::SECRET_CAPABILITY_KIND.get(),
            crate::CanonicalEntryCapability::BuildOut => crate::canonical_safewir::BUILD_OUT_CAPABILITY_KIND.get(),
            crate::CanonicalEntryCapability::BuildRead => crate::canonical_safewir::BUILD_READ_CAPABILITY_KIND.get(),
            crate::CanonicalEntryCapability::BuildEnv => crate::canonical_safewir::BUILD_ENV_CAPABILITY_KIND.get(),
            crate::CanonicalEntryCapability::BuildExec => crate::canonical_safewir::BUILD_EXEC_CAPABILITY_KIND.get(),
            crate::CanonicalEntryCapability::BuildNet => crate::canonical_safewir::BUILD_NET_CAPABILITY_KIND.get(),
            crate::CanonicalEntryCapability::Dir => DIR_KIND,
            crate::CanonicalEntryCapability::Env => ENV_KIND,
            crate::CanonicalEntryCapability::Net => crate::canonical_safewir::NET_CAPABILITY_KIND.get(),
        })
    }
}

/// The confined world the oracle serves `Dir` and `Env` host calls from.
/// Paths are relative and "/"-separated, without a leading "/", exactly as a
/// `Dir` grant addresses its root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OracleFixture {
    pub exec: Option<OracleExecFixture>,
    /// Explicit root grants and ordered provider wire outcomes. No network IO.
    pub fetch: Option<OracleFetchFixture>,
    /// Explicit grants and ordered Unix-millisecond observations. No ambient clock.
    pub clock: Option<OracleClockFixture>,
    /// Presence grants lookup, including an explicitly empty store.
    pub secret_store: Option<OracleSecretStoreFixture>,
    /// Direct Secret entry authority must be an explicit Signing fixture.
    pub signing_secret: Option<OracleSecretFixture>,
    /// Explicit write grant for a fresh virtual build output directory.
    pub build_out: bool,
    pub build_read: Option<OracleBuildReadFixture>,
    /// Only this explicit snapshot grants BuildEnv; never consult ambient Env.
    pub build_env: Option<OracleBuildEnvFixture>,
    /// Exact ordered tool invocations; no subprocess or PATH lookup is available.
    pub build_exec: Option<OracleBuildExecFixture>,
    /// Exact ordered raw HTTP responses; no ambient network is available.
    pub build_net: Option<OracleBuildNetFixture>,
    pub files: BTreeMap<String, Vec<u8>>,
    /// Explicit empty directories as well as parents created by virtual mkdir.
    /// Like `files`, these are normalized relative paths under the granted root.
    pub directories: BTreeSet<String>,
    /// Virtual installer changes. Deleted backing entries never reappear;
    /// these observations do not grant permission to mutate the fallback root.
    pub dir_mutations: OracleDirMutations,
    pub environment: BTreeMap<String, String>,
    /// A directory the oracle reads when a path is not in `files` (the
    /// parity third leg runs against the same root the backend was granted);
    /// writes stay in `files`, so a program sees its own writes first.
    pub root: Option<std::path::PathBuf>,
    /// Consult the process environment for a name not in `environment`.
    pub process_environment: bool,
    /// How many root `Dir` grants the launcher holds (`None`: unlimited, for
    /// fixtures that model no launcher); an entry with more `Dir` parameters
    /// is refused, as the runtime refuses to mint an ungranted one.
    pub dir_grants: Option<usize>,
    /// Launch-only Net grants when no deterministic Net fixture is supplied.
    /// This count alone never authorizes socket I/O.
    pub net_grants: usize,
    /// Explicit rights, endpoints and an ordered deterministic socket script.
    /// No host networking, DNS, or native receive implementation is consulted.
    pub net: Option<OracleNetFixture>,
    /// Bounded, independently executed server-worker scripts. No OS networking.
    pub server_pool: Option<OracleServerPoolFixture>,
}

impl OracleFixture {
    fn net_grant_count(&self) -> usize {
        self.net.as_ref().map_or(self.net_grants, |fixture| fixture.grants.len())
    }
    /// Resolve `path` under the fallback root without following a symbolic
    /// link anywhere along it: the runtime's confined directory refuses a
    /// link that leads out of the root, and the oracle must be no weaker.
    fn root_entry(&self, path: &str) -> Option<std::path::PathBuf> {
        if self.backing_hidden(path) { return None; }
        let root = self.root.as_ref()?;
        let mut resolved = root.clone();
        let mut prefix = String::new();
        for component in path.split('/').filter(|component| !component.is_empty()) {
            if !prefix.is_empty() { prefix.push('/'); }
            prefix.push_str(component);
            if self.dir_mutations.links.contains_key(&prefix) { return None; }
            resolved.push(component);
            let metadata = std::fs::symlink_metadata(&resolved).ok()?;
            if metadata.file_type().is_symlink() {
                return None;
            }
        }
        Some(resolved)
    }

    fn file(&self, path: &str) -> Option<Vec<u8>> {
        if let Some(contents) = self.files.get(path) {
            return Some(contents.clone());
        }
        if self.directories.contains(path) { return None; }
        let entry = self.root_entry(path)?;
        if !std::fs::symlink_metadata(&entry).ok()?.is_file() {
            return None;
        }
        std::fs::read(entry).ok()
    }

    fn exists(&self, path: &str) -> bool {
        self.metadata_exists("", path)
    }

    /// The entry names directly under `scope` (the fixture map's, then the
    /// fallback root's, links listed by name but never followed), sorted.
    fn entry_names(&self, scope: &str) -> Vec<String> {
        let prefix = if scope.is_empty() { String::new() } else { format!("{scope}/") };
        let mut names = std::collections::BTreeSet::new();
        for key in self.files.keys().chain(self.directories.iter()).chain(self.dir_mutations.links.keys()) {
            if let Some(rest) = key.strip_prefix(&prefix)
                && let Some(first) = rest.split('/').next()
                && !first.is_empty()
            {
                names.insert(first.to_owned());
            }
        }
        let root = if scope.is_empty() { self.root.clone() } else { self.root_entry(scope) };
        if let Some(root) = root
            && let Ok(entries) = std::fs::read_dir(root)
        {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if !self.backing_hidden(&format!("{prefix}{name}")) { names.insert(name); }
                }
            }
        }
        names.into_iter().collect()
    }

    fn is_directory(&self, path: &str) -> bool {
        let prefix = format!("{path}/");
        !self.files.contains_key(path) && !self.dir_mutations.links.contains_key(path) && (path.is_empty()
            || self.directories.contains(path)
            || self.directories.iter().any(|name| name.starts_with(&prefix))
            || self.files.keys().any(|name| name.starts_with(&prefix))
            || self.dir_mutations.links.keys().any(|name| name.starts_with(&prefix))
            || self
                .root_entry(path)
                .and_then(|entry| std::fs::symlink_metadata(entry).ok())
                .is_some_and(|metadata| metadata.is_dir()))
    }

    fn variable(&self, name: &str) -> Option<String> {
        if let Some(value) = self.environment.get(name) {
            return Some(value.clone());
        }
        if self.process_environment {
            return std::env::var(name).ok();
        }
        None
    }
}

/// The observable outcome of running a program entry to completion.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgramRun {
    /// Successful BuildOut writes in execution order, including overwrites.
    pub build_trace: Vec<OracleBuildWrite>,
    pub results: Vec<InterpreterValue>,
    /// Lines the program printed through the console host call, in order.
    pub output: Vec<String>,
    /// Reference-counted objects still live when the entry returned. Under
    /// callee-drops every token is released by exit, so any survivor is a
    /// reclamation defect in the program's lowering.
    pub leaked_objects: usize,
    /// Matched socket transcript, empty when no Net fixture was configured.
    pub net_trace: Vec<OracleNetEvent>,
    pub server_workers: Vec<OracleServerWorkerRun>,
}

/// A precise failure of the executable SafeWIR semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterpretError {
    FunctionNotFound(FunctionId),
    BlockNotFound(BlockId),
    ArityMismatch {
        expected: usize,
        actual: usize,
    },
    MissingValue(ValueId),
    TypeMismatch(&'static str),
    ArithmeticTrap,
    GuardTrap,
    ExplicitTrap,
    /// Terminal diagnostic plus the already-observed output prefix. This is
    /// not a successful return or a claim that abandoned trap roots were freed.
    DiagnosticAbort { message: String, output: Vec<String> },
    UnreachableExecuted,
    StepLimitExceeded,
    ObjectNotFound(u64),
    ObjectNotReferenceCounted(u64),
    ReferenceCountOverflow(u64),
    ReferenceCountUnderflow(u64),
    ReclaimWithAliases {
        object: u64,
        references: u64,
    },
    ImmutableObject(u64),
    IndexOutOfBounds {
        index: u64,
        length: u64,
    },
    CountExceedsCapacity {
        count: u64,
        capacity: u64,
    },
    UninitializedPlace,
    UnsupportedHostImport,
    BuildOut(String),
    BuildRead(String),
    BuildEnv(String),
    BuildExec(String),
    BuildNet(String),
    ExecFixture(String),
    FetchFixture(String),
    RegistryFixture(String),
    NetFixture(OracleNetError),
    UnsupportedOperation(&'static str),
}

impl fmt::Display for InterpretError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Self::DiagnosticAbort { message, .. } = self { return formatter.write_str(message); }
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for InterpretError {}

/// Bounds execution so malformed or accidentally nonterminating fixtures do
/// not hang a differential test, and carries the host world the capability
/// imports observe.
#[derive(Debug, Clone)]
pub struct InterpreterConfig {
    /// Observation only; this sink grants no capability and survives a trap.
    pub console_trace: Option<std::sync::Arc<std::sync::Mutex<Vec<OracleConsoleEvent>>>>,
    pub max_steps: u64,
    pub fixture: OracleFixture,
    pub compiler_services: Option<fn(CompilerServiceKind, &[String]) -> Result<String, String>>,
}

impl Default for InterpreterConfig {
    fn default() -> Self {
        Self { max_steps: 1_000_000, fixture: OracleFixture::default(), compiler_services: None, console_trace: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilerServiceKind { Footprint, Diff, Doc, DocResultJson }

// The physical i32 result carries an unsigned wasm32 byte count. Match the
// native compiler service without allocating a payload to test its boundary.
fn compiler_result_size(byte_len: usize) -> Result<u64, InterpretError> {
    u32::try_from(byte_len).map(u64::from).map_err(|_| InterpretError::ArithmeticTrap)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleConsoleEvent {
    pub text: String,
    pub function: String,
    pub line: u32,
}


/// Execute one function from an independently validated SafeWIR module.
pub fn interpret(
    module: &ValidatedSafeWir,
    function: FunctionId,
    arguments: Vec<InterpreterValue>,
) -> Result<Vec<InterpreterValue>, InterpretError> {
    interpret_with_config(module, function, arguments, InterpreterConfig::default())
}

pub fn interpret_with_config(
    module: &ValidatedSafeWir,
    function: FunctionId,
    arguments: Vec<InterpreterValue>,
    config: InterpreterConfig,
) -> Result<Vec<InterpreterValue>, InterpretError> {
    let mut engine = Engine::new(module.module(), config);
    let results = engine.execute(function, arguments)?;
    engine.finish_net()?;
    engine.finish_exec()?;
    engine.finish_build_exec()?;
    engine.finish_build_net()?;
    engine.finish_fetch()?;
    engine.finish_registry()?;
    Ok(results)
}

fn program_entry_signature(
    module: &ValidatedSafeWir,
    function: FunctionId,
) -> Result<&FunctionSignature, InterpretError> {
    let program = module.module();
    let body = program.functions().iter().find(|body| body.id == function)
        .ok_or(InterpretError::FunctionNotFound(function))?;
    program.signatures().iter().find(|signature| signature.id == body.signature)
        .ok_or(InterpretError::TypeMismatch("entry signature"))
}

fn validate_entry_parameters(
    parameters: &[ValueType],
    arguments: &[InterpreterValue],
    net_grants: usize,
) -> Result<(), InterpretError> {
    // Authority requirements come from the independently validated callee,
    // never from caller-selected token kinds (including unused parameters).
    let net_roots = parameters.iter().filter(|parameter| matches!(parameter,
        ValueType::Capability(kind) if *kind == crate::canonical_safewir::NET_CAPABILITY_KIND)).count();
    if net_roots > net_grants { return Err(InterpretError::ExplicitTrap); }
    if parameters.len() != arguments.len() {
        return Err(InterpretError::ArityMismatch {
            expected: parameters.len(),
            actual: arguments.len(),
        });
    }
    for (parameter, argument) in parameters.iter().zip(arguments) {
        match (parameter, argument) {
            (ValueType::Capability(kind), InterpreterValue::Capability(token))
                if token.kind == kind.get() => {}
            (ValueType::NullableCapability(kind), InterpreterValue::NullableCapability(Some(token)))
                if token.kind == kind.get() => {}
            (ValueType::NullableCapability(_), InterpreterValue::NullableCapability(None)) => {}
            (ValueType::Capability(_) | ValueType::NullableCapability(_), _)
            | (_, InterpreterValue::Capability(_) | InterpreterValue::NullableCapability(_)) => {
                return Err(InterpretError::TypeMismatch("capability parameter kind"));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Run a program entry as the launcher would: capability tokens in, console
/// output captured, and the heap audited for objects the program never
/// released. This is the execution oracle for admitted programs: its
/// semantics are the checker's, independent of the Wasm backend.
pub fn interpret_program(
    module: &ValidatedSafeWir,
    function: FunctionId,
    arguments: Vec<InterpreterValue>,
    config: InterpreterConfig,
) -> Result<ProgramRun, InterpretError> {
    let signature = program_entry_signature(module, function)?;
    validate_entry_parameters(&signature.parameters, &arguments, config.fixture.net_grant_count())?;
    // The launcher mints one root `Dir` token per grant; an entry asking for
    // more than the fixture grants is refused before it runs.
    if let Some(grants) = config.fixture.dir_grants {
        let roots = arguments
            .iter()
            .filter(|argument| matches!(argument, InterpreterValue::Capability(token) if token.kind == DIR_KIND && token.scope.is_empty()))
            .count();
        if roots > grants {
            return Err(InterpretError::ExplicitTrap);
        }
    }
    let mut engine = Engine::new(module.module(), config);
    let arguments = engine.bind_net_entry(arguments)?;
    let arguments = engine.bind_build_entry(arguments)?;
    let arguments = engine.bind_build_read_entry(arguments)?;
    let arguments = engine.bind_exec_entry(arguments)?;
    let arguments = engine.bind_fetch_entry(arguments)?;
    let arguments = engine.bind_registry_entry(arguments)?;
    let results = engine.execute(function, arguments)?;
    let net_trace = engine.finish_net()?;
    engine.finish_exec()?;
    engine.finish_build_exec()?;
    engine.finish_build_net()?;
    engine.finish_fetch()?;
    engine.finish_registry()?;
    let leaked_objects = engine
        .objects
        .values()
        .filter(|object| matches!(object.storage, RuntimeStorage::Allocation(_)))
        .count()
        + engine.owners.len()
        + engine.owner_lists.len();
    Ok(ProgramRun { results, output: engine.output, leaked_objects, net_trace, build_trace: engine.build_out.writes, server_workers: engine.server.completed })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CellKey {
    Field(u32),
    Element(u64),
}

#[derive(Debug, PartialEq, Eq)]
enum RuntimeExtent {
    FixedBytes(u64),
    HeaderPrefix(u32),
    Counted {
        header_bytes: u32,
        count: u64,
        capacity: u64,
        stride_bytes: u32,
    },
}

#[derive(Debug)]
struct Object {
    layout: LayoutId,
    storage: RuntimeStorage,
    extent: RuntimeExtent,
    references: u64,
    immutable: bool,
    cells: BTreeMap<CellKey, InterpreterValue>,
    // Initialized absence is storage state, never an SSA reference value.
    absent_owned_slots: BTreeSet<CellKey>,
}

#[derive(Debug)]
struct OwnerState {
    family: FunctionSignatureId,
    constructor: ConstructorId,
    environment: u64,
    owner_fields: BTreeMap<u32, InterpreterValue>,
    references: u64,
}

#[derive(Debug)]
struct OwnerListState {
    family: FunctionSignatureId,
    elements: Vec<OpaqueOwnerToken>,
    references: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeTransfer {
    Copy,
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeStorage {
    Allocation(AllocationClass),
    StaticData,
}

struct Engine<'a> {
    server: net::ServerState,
    exec: exec::State,
    fetch: fetch::State,
    registry: registry_host::State,
    build_out: build_out::State,
    build_read: build_read::State,
    build_exec: build_exec::State,
    build_net: build_net::State,
    services: vm_service::ServiceState<'a>,
    isolated: bool,
    module: &'a CanonicalModule<InitialValid>,
    /// The module's layout catalog, decoded once: projections, static data,
    /// and owner environments consult it on every step.
    layouts: Option<std::sync::Arc<LayoutInterner>>,
    config: InterpreterConfig,
    steps: u64,
    next_object: u64,
    next_owner: u64,
    next_owner_list: u64,
    objects: BTreeMap<u64, Object>,
    owners: BTreeMap<OpaqueOwnerToken, OwnerState>,
    owner_lists: BTreeMap<OpaqueOwnerListToken, OwnerListState>,
    static_objects: BTreeMap<crate::canonical_safewir::StaticDataId, u64>,
    output: Vec<String>,
    net: Option<net::NetState>,
}

enum FunctionExit {
    Return(Vec<InterpreterValue>),
    TailCall(FunctionId, Vec<InterpreterValue>),
}

impl<'a> Engine<'a> {
    fn new(module: &'a CanonicalModule<InitialValid>, mut config: InterpreterConfig) -> Self {
        let net = config.fixture.net.take().map(net::NetState::new);
        let layouts = LayoutBundle::decode_canonical(&module.layouts().canonical_bytes())
            .ok()
            .map(|(_, layouts)| std::sync::Arc::new(layouts));
        let server = net::ServerState::new(config.fixture.server_pool.take());
        Self {
            server,
            exec: exec::State::new(),
            fetch: fetch::State::new(),
            registry: registry_host::State::new(),
            services: vm_service::ServiceState::new(),
            build_out: build_out::State::new(),
            build_read: build_read::State::new(),
            build_exec: build_exec::State::new(),
            build_net: build_net::State::new(),
            isolated: false,
            module,
            layouts,
            config,
            steps: 0,
            next_object: 0,
            next_owner: 0,
            next_owner_list: 0,
            objects: BTreeMap::new(),
            owners: BTreeMap::new(),
            owner_lists: BTreeMap::new(),
            static_objects: BTreeMap::new(),
            output: Vec::new(),
            net,
        }
    }

    fn execute(
        &mut self,
        mut function: FunctionId,
        mut arguments: Vec<InterpreterValue>,
    ) -> Result<Vec<InterpreterValue>, InterpretError> {
        loop {
            match self.execute_frame(function, arguments)? {
                FunctionExit::Return(values) => return Ok(values),
                FunctionExit::TailCall(next, next_arguments) => {
                    function = next;
                    arguments = next_arguments;
                }
            }
        }
    }

    fn execute_frame(
        &mut self,
        function_id: FunctionId,
        arguments: Vec<InterpreterValue>,
    ) -> Result<FunctionExit, InterpretError> {
        // The module outlives the engine's mutable state (`&'a`), so the
        // function and its blocks are borrowed from it directly. This used to
        // clone the whole function per frame and every block per entry.
        let module = self.module;
        let function = module.functions().iter()
            .find(|function| function.id == function_id)
            .ok_or(InterpretError::FunctionNotFound(function_id))?;
        let entry = self.block(function, function.entry)?;
        let mut values = BTreeMap::new();
        self.bind_parameters(entry, arguments, &mut values)?;
        let mut block_id = function.entry;

        loop {
            let block = self.block(function, block_id)?;
            for operation in &block.operations {
                self.bump_step()?;
                self.execute_operation(operation, &mut values)?;
            }

            self.bump_step()?;
            match &block.terminator {
                Terminator::Branch { target, arguments } => {
                    let next = self.read_values(arguments, &values)?;
                    self.bind_parameters(self.block(function, *target)?, next, &mut values)?;
                    block_id = *target;
                }
                Terminator::BranchIf {
                    condition,
                    then_target,
                    then_arguments,
                    else_target,
                    else_arguments,
                } => {
                    let condition = self.bool_value(*condition, &values)?;
                    let (target, arguments) = if condition {
                        (*then_target, then_arguments)
                    } else {
                        (*else_target, else_arguments)
                    };
                    let next = self.read_values(arguments, &values)?;
                    self.bind_parameters(self.block(function, target)?, next, &mut values)?;
                    block_id = target;
                }
                Terminator::Return { values: returned } => {
                    return Ok(FunctionExit::Return(self.read_values(returned, &values)?));
                }
                Terminator::TailCall {
                    callee, arguments, ..
                } => {
                    let arguments = self.take_call_arguments(arguments, &mut values)?;
                    return Ok(FunctionExit::TailCall(*callee, arguments));
                }
                Terminator::VarTailCall {
                    callee, arguments, ..
                } => {
                    let callee = self.function_value(*callee, &values)?;
                    let arguments = self.take_call_arguments(arguments, &mut values)?;
                    return Ok(FunctionExit::TailCall(callee, arguments));
                }
                Terminator::Trap => return Err(InterpretError::ExplicitTrap),
                Terminator::Unreachable => return Err(InterpretError::UnreachableExecuted),
            }
        }
    }

    fn execute_operation(
        &mut self,
        operation: &Operation,
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        match operation {
            Operation::ClosureOwner(operation) => self.execute_owner_operation(operation, values)?,
            Operation::ConstUnit { destination } => {
                values.insert(*destination, InterpreterValue::Unit);
            }
            Operation::ConstInt { destination, value } => {
                values.insert(*destination, InterpreterValue::Int(*value));
            }
            Operation::ConstFloat { destination, bits } => {
                values.insert(*destination, InterpreterValue::FloatBits(*bits));
            }
            Operation::ConstBool { destination, value } => {
                values.insert(*destination, InterpreterValue::Bool(*value));
            }
            Operation::ConstSize { destination, value } => {
                values.insert(*destination, InterpreterValue::Size(self.size_from_u64(*value)?));
            }
            Operation::CapabilityNone { destination, .. } => {
                values.insert(*destination, InterpreterValue::NullableCapability(None));
            }
            Operation::CapabilitySome { destination, source } => {
                let InterpreterValue::Capability(token) = self.value(*source, values)? else { return Err(InterpretError::TypeMismatch("capability token")); };
                values.insert(*destination, InterpreterValue::NullableCapability(Some(token.clone())));
            }
            Operation::CapabilityIsPresent { destination, source } => {
                let InterpreterValue::NullableCapability(value) = self.value(*source, values)? else { return Err(InterpretError::TypeMismatch("nullable capability")); };
                values.insert(*destination, InterpreterValue::Bool(value.is_some()));
            }
            Operation::CapabilityUnwrap { destination, source } => {
                let InterpreterValue::NullableCapability(value) = self.value(*source, values)? else { return Err(InterpretError::TypeMismatch("nullable capability")); };
                values.insert(*destination, InterpreterValue::Capability(value.clone().ok_or(InterpretError::ExplicitTrap)?));
            }
            Operation::MakeInlineAggregate { destination, layout, fields } => {
                values.insert(*destination, InterpreterValue::InlineAggregate {
                    layout: *layout,
                    fields: self.read_values(fields, values)?,
                });
            }
            Operation::ExtractInlineField { destination, aggregate, index } => {
                let InterpreterValue::InlineAggregate { fields, .. } = self.value(*aggregate, values)? else {
                    return Err(InterpretError::TypeMismatch("inline aggregate"));
                };
                let field = fields.get(*index as usize).cloned().ok_or(InterpretError::TypeMismatch("inline aggregate index"))?;
                values.insert(*destination, field);
            }
            Operation::Arithmetic {
                destination,
                operator,
                left,
                right,
                overflow,
            } => {
                let result = self.arithmetic(
                    *operator,
                    self.value(*left, values)?,
                    self.value(*right, values)?,
                    *overflow,
                )?;
                values.insert(*destination, result);
            }
            Operation::Compare {
                destination,
                operator,
                left,
                right,
            } => {
                let result = self.compare(
                    *operator,
                    self.value(*left, values)?,
                    self.value(*right, values)?,
                )?;
                values.insert(*destination, InterpreterValue::Bool(result));
            }
            Operation::NarrowSize { destination, value } => {
                let value = self.int_value(*value, values)?;
                let narrowed = u32::try_from(value).map_err(|_| InterpretError::ArithmeticTrap)?;
                values.insert(*destination, InterpreterValue::Size(u64::from(narrowed)));
            }
            Operation::WidenSize { destination, value } => {
                let value = self.size_value(*value, values)?;
                let widened = i64::try_from(value).map_err(|_| InterpretError::ArithmeticTrap)?;
                values.insert(*destination, InterpreterValue::Int(widened));
            }
            Operation::FloatUnary { destination, operator, value } => {
                let result = match operator {
                    crate::canonical_safewir::FloatUnaryOperator::FromInt => {
                        let operand = self.int_value(*value, values)?;
                        InterpreterValue::FloatBits((operand as f64).to_bits())
                    }
                    operator => {
                        let InterpreterValue::FloatBits(bits) = self.value(*value, values)? else {
                            return Err(InterpretError::TypeMismatch("FloatUnary operand"));
                        };
                        let operand = f64::from_bits(*bits);
                        match operator {
                            crate::canonical_safewir::FloatUnaryOperator::Sqrt => InterpreterValue::FloatBits(operand.sqrt().to_bits()),
                            // `as` truncates toward zero, saturates, and maps
                            // NaN to zero: the contract of `i64.trunc_sat_f64_s`.
                            crate::canonical_safewir::FloatUnaryOperator::TruncateToInt => InterpreterValue::Int(operand as i64),
                            crate::canonical_safewir::FloatUnaryOperator::FromInt => unreachable!("handled above"),
                        }
                    }
                };
                values.insert(*destination, result);
            }
            Operation::Guard { condition, .. } => {
                if !self.bool_value(*condition, values)? {
                    return Err(InterpretError::GuardTrap);
                }
            }
            Operation::Alloc {
                destination,
                layout,
                extent,
                class,
            } => {
                let extent = self.runtime_extent(extent, values)?;
                let object = self.allocate(*layout, *class, extent, false)?;
                values.insert(*destination, InterpreterValue::ObjectRef(object));
            }
            Operation::StaticData {
                destination,
                layout,
                extent,
                data,
            } => {
                let extent = self.runtime_extent(extent, values)?;
                let layout = self
                    .layouts
                    .as_deref()
                    .ok_or(InterpretError::UnsupportedOperation("StaticData layouts"))?
                    .resolve_nominal(*layout)
                    .map_err(|_| InterpretError::UnsupportedOperation("StaticData nominal layout"))?;
                let bytes = self
                    .module
                    .static_data_segments()
                    .iter()
                    .find(|segment| segment.id() == *data)
                    .map(|segment| segment.bytes())
                    .ok_or(InterpretError::UnsupportedOperation("StaticData missing segment"))?;
                let object = if let Some(object) = self.static_objects.get(data) {
                    let existing = self.object(*object)?;
                    if existing.layout != layout || existing.extent != extent {
                        return Err(InterpretError::UnsupportedOperation(
                            "StaticData inconsistent segment reuse",
                        ));
                    }
                    *object
                } else {
                    let object = self.allocate_static(layout, extent, bytes)?;
                    self.static_objects.insert(*data, object);
                    object
                };
                values.insert(*destination, InterpreterValue::ObjectRef(object));
            }
            Operation::Load {
                destination,
                place,
                transfer,
            } => {
                let value = self.load(place, *transfer, values)?;
                values.insert(*destination, value);
            }
            Operation::Store {
                place,
                value,
                transfer,
            } => self.store(place, *value, *transfer, values)?,
            Operation::InitNullableOwned { place } => {
                self.init_nullable_owned(place, values)?;
            }
            Operation::CopyBytes { destination, source, length } => {
                self.copy_bytes(destination, source, *length, values)?;
            }
            Operation::MoveRange { destination, source, count } => {
                self.copy_range(destination, source, *count, RangeTransfer::Move, values)?;
            }
            Operation::CloneRange { destination, source, count } => {
                self.copy_range(destination, source, *count, RangeTransfer::Copy, values)?;
            }
            Operation::ReauthenticateRawHeaderExtent { .. } => {}
            Operation::UpdateCount { object, count } => {
                let object = self.object_value(*object, values)?;
                let count = self.size_value(*count, values)?;
                self.update_count(object, count)?;
            }
            Operation::MoveValue { destination, source } => {
                // A reference move transfers the token; a scalar move is a
                // copy, as the checker and the backend both treat it.
                let value = self.value(*source, values)?.clone();
                if matches!(value, InterpreterValue::ObjectRef(_)) {
                    values.remove(source);
                }
                values.insert(*destination, value);
            }
            Operation::CloneRef { destination, source } => {
                let object = self.object_value(*source, values)?;
                self.retain(object)?;
                values.insert(*destination, InterpreterValue::ObjectRef(object));
            }
            Operation::Retain { object } => {
                let object = self.object_value(*object, values)?;
                self.retain(object)?;
            }
            Operation::Release { object } | Operation::DropRef { object } => {
                let object = self.object_value(*object, values)?;
                self.release(object)?;
            }
            Operation::Reclaim { object } => {
                let object = self.object_value(*object, values)?;
                self.reclaim(object)?;
            }
            Operation::FunctionValue {
                destination,
                function,
            } => {
                values.insert(*destination, InterpreterValue::Function(*function));
            }
            Operation::Call {
                destinations,
                callee,
                arguments,
            } => {
                let arguments = self.take_call_arguments(arguments, values)?;
                let returned = self.execute(*callee, arguments)?;
                self.bind_results(destinations, returned, values)?;
            }
            Operation::VarCall {
                destinations,
                callee,
                arguments,
                ..
            } => {
                let callee = self.function_value(*callee, values)?;
                let arguments = self.take_call_arguments(arguments, values)?;
                let returned = self.execute(callee, arguments)?;
                self.bind_results(destinations, returned, values)?;
            }
            // The ordinary interpreter has no Wasmtime artifact loader.  Do
            // not emulate an external import or turn it into a host call: the
            // separate runtime binds this only after exact artifact identity
            // verification.
            Operation::ExternalCall { .. } => {
                return Err(InterpretError::UnsupportedOperation(
                    "authenticated external library runtime required",
                ));
            }
            Operation::HostCall { destinations, import, arguments } => {
                self.host_call(*import, destinations, arguments, values)?;
            }
        }
        Ok(())
    }

    fn owner_family(&self, signature: FunctionSignatureId) -> Result<&OwnerFamily, InterpretError> {
        let mut found = None;
        for family in self.module.owner_families() {
            if family.signature == signature {
                if found.is_some() {
                    return Err(InterpretError::UnsupportedOperation("ambiguous owner family"));
                }
                found = Some(family);
            }
        }
        found.ok_or(InterpretError::UnsupportedOperation("unregistered owner family"))
    }

    fn owner_constructor(&self, id: ConstructorId) -> Result<(&OwnerFamily, &crate::canonical_safewir::closure_owner_contract::OwnerConstructor), InterpretError> {
        let mut found = None;
        for family in self.module.owner_families() {
            for constructor in &family.constructors {
                if constructor.id == id {
                    if found.is_some() {
                        return Err(InterpretError::UnsupportedOperation("ambiguous owner constructor"));
                    }
                    found = Some((family, constructor));
                }
            }
        }
        found.ok_or(InterpretError::UnsupportedOperation("unregistered owner constructor"))
    }

    fn owner_value(&self, id: ValueId, values: &BTreeMap<ValueId, InterpreterValue>) -> Result<OpaqueOwnerToken, InterpretError> {
        match self.value(id, values)? {
            InterpreterValue::OpaqueOwner(owner) => Ok(*owner),
            _ => Err(InterpretError::TypeMismatch("opaque owner")),
        }
    }

    fn owner_list_value(&self, id: ValueId, values: &BTreeMap<ValueId, InterpreterValue>) -> Result<OpaqueOwnerListToken, InterpretError> {
        match self.value(id, values)? {
            InterpreterValue::OpaqueOwnerList(list) => Ok(*list),
            _ => Err(InterpretError::TypeMismatch("opaque owner list")),
        }
    }

    fn clone_owner_capture(&mut self, value: InterpreterValue, transfer: CaptureTransfer) -> Result<InterpreterValue, InterpretError> {
        match (transfer, value) {
            (CaptureTransfer::CopyScalar, value @ (InterpreterValue::Int(_)
                | InterpreterValue::FloatBits(_)
                | InterpreterValue::Bool(_)
                | InterpreterValue::Size(_)
                | InterpreterValue::Unit
                | InterpreterValue::V128(_))) => Ok(value),
            (CaptureTransfer::CopyCapability(kind), InterpreterValue::Capability(token)) if token.kind == kind.get() => {
                Ok(InterpreterValue::Capability(token))
            }
            (CaptureTransfer::CloneManaged, InterpreterValue::ObjectRef(object)) => {
                self.retain(object)?;
                Ok(InterpreterValue::ObjectRef(object))
            }
            (CaptureTransfer::CloneManaged, InterpreterValue::OpaqueOwner(owner)) => {
                self.retain_owner(owner)?;
                Ok(InterpreterValue::OpaqueOwner(owner))
            }
            (CaptureTransfer::CloneManaged, InterpreterValue::OpaqueOwnerList(list)) => {
                self.retain_owner_list(list)?;
                Ok(InterpreterValue::OpaqueOwnerList(list))
            }
            _ => Err(InterpretError::TypeMismatch("owner capture transfer")),
        }
    }

    fn execute_owner_operation(
        &mut self,
        operation: &OwnerOperation,
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        match operation {
            OwnerOperation::Construct { destination, constructor, captures } => {
                let (family_signature, environment_layout, constructor_id, environment_tag, capture_info) = {
                    let (family, constructor_info) = self.owner_constructor(*constructor)?;
                    (
                        family.signature,
                        family.environment,
                        constructor_info.id,
                        constructor_info.environment_tag,
                        constructor_info.captures.clone(),
                    )
                };
                if captures.len() != capture_info.len() {
                    return Err(InterpretError::ArityMismatch {
                        expected: capture_info.len(),
                        actual: captures.len(),
                    });
                }
                let mut seen_fields = BTreeSet::new();
                for capture in &capture_info {
                    let key = match capture.storage {
                        CaptureStorage::EnvironmentField(field) => (0_u8, field),
                        CaptureStorage::OwnerField(field) => (1_u8, field),
                    };
                    if !seen_fields.insert(key) {
                        return Err(InterpretError::UnsupportedOperation("duplicate owner capture field"));
                    }
                }
                let owner_number = self.next_owner.checked_add(1).ok_or(InterpretError::ArithmeticTrap)?;
                let layouts = self.layouts.clone().ok_or(InterpretError::TypeMismatch("owner environment layouts"))?;
                let physical = layouts.resolve_nominal(environment_layout)
                    .map_err(|_| InterpretError::TypeMismatch("owner environment layout"))?;
                let descriptor = layouts.get(physical)
                    .ok_or(InterpretError::TypeMismatch("owner environment layout"))?;
                let crate::layout::LayoutSize::Fixed(bytes) = descriptor.size() else {
                    return Err(InterpretError::TypeMismatch("owner environment extent"));
                };
                let environment = self.allocate(
                    environment_layout,
                    AllocationClass::ReferenceCounted,
                    RuntimeExtent::FixedBytes(u64::from(bytes)),
                    false,
                )?;
                let mut staged = Vec::with_capacity(captures.len());
                for (source, capture) in captures.iter().zip(&capture_info) {
                    let source_value = match self.value(*source, values) {
                        Ok(value) => value.clone(),
                        Err(error) => {
                            self.release(environment)?;
                            return Err(error);
                        }
                    };
                    match self.clone_owner_capture(source_value, capture.transfer) {
                        Ok(value) => staged.push((capture.storage, value)),
                        Err(error) => {
                            for (_, value) in staged.drain(..) {
                                self.release_interpreter_value(value)?;
                            }
                            self.release(environment)?;
                            return Err(error);
                        }
                    }
                }
                let mut owner_fields = BTreeMap::new();
                self.object_mut(environment)?.cells.insert(
                    CellKey::Field(0),
                    InterpreterValue::Int(environment_tag),
                );
                for (storage, value) in staged {
                    match storage {
                        CaptureStorage::EnvironmentField(field) => {
                            self.object_mut(environment)?.cells.insert(CellKey::Field(field), value);
                        }
                        CaptureStorage::OwnerField(field) => {
                            owner_fields.insert(field, value);
                        }
                    }
                }
                let token = OpaqueOwnerToken(self.next_owner);
                self.next_owner = owner_number;
                self.owners.insert(token, OwnerState {
                    family: family_signature,
                    constructor: constructor_id,
                    environment,
                    owner_fields,
                    references: 1,
                });
                values.insert(*destination, InterpreterValue::OpaqueOwner(token));
            }
            OwnerOperation::ReadCapability { destination, owner, constructor, capture } => {
                let (family, constructor_info) = self.owner_constructor(*constructor)?;
                let owner = self.owner_value(*owner, values)?;
                let state = self.owners.get(&owner).ok_or(InterpretError::UnsupportedOperation("released owner"))?;
                if state.family != family.signature || state.constructor != constructor_info.id {
                    return Err(InterpretError::TypeMismatch("owner constructor"));
                }
                let capture_info = constructor_info
                    .captures
                    .get(capture.0 as usize)
                    .ok_or(InterpretError::UnsupportedOperation("owner capture ordinal"))?;
                let value = self.owner_capture_value(state, capture_info.storage)?;
                let CaptureTransfer::CopyCapability(kind) = capture_info.transfer else {
                    return Err(InterpretError::TypeMismatch("owner capability capture"));
                };
                let InterpreterValue::Capability(token) = value else {
                    return Err(InterpretError::TypeMismatch("owner capability value"));
                };
                if token.kind != kind.get() {
                    return Err(InterpretError::TypeMismatch("owner capability kind"));
                }
                values.insert(*destination, InterpreterValue::Capability(token));
            }
            OwnerOperation::ReadOwner { destination, owner, constructor, capture } => {
                let (family, constructor_info) = self.owner_constructor(*constructor)?;
                let owner = self.owner_value(*owner, values)?;
                let capture_info = {
                    let state = self.owners.get(&owner).ok_or(InterpretError::UnsupportedOperation("released owner"))?;
                    if state.family != family.signature || state.constructor != constructor_info.id {
                        return Err(InterpretError::TypeMismatch("owner constructor"));
                    }
                    let capture_info = constructor_info.captures.get(capture.0 as usize)
                        .ok_or(InterpretError::UnsupportedOperation("owner capture ordinal"))?;
                    if !matches!((capture_info.value_type, capture_info.storage, capture_info.transfer),
                        (ValueType::Gc(_), CaptureStorage::OwnerField(_), CaptureTransfer::CloneManaged))
                    {
                        return Err(InterpretError::TypeMismatch("nested owner capture"));
                    }
                    (capture_info.clone(), self.owner_capture_value(state, capture_info.storage)?)
                };
                let retained = self.clone_owner_capture(capture_info.1, capture_info.0.transfer)?;
                let exact_type = match capture_info.0.value_type {
                    ValueType::Gc(id) => id,
                    _ => unreachable!("checked above"),
                };
                let exact = match retained {
                    InterpreterValue::OpaqueOwner(token) => self.owners.get(&token).and_then(|state| {
                        self.module.owner_families().iter().find(|candidate| candidate.signature == state.family)
                    }).is_some_and(|candidate| candidate.owner_type == exact_type),
                    InterpreterValue::OpaqueOwnerList(token) => self.owner_lists.get(&token).and_then(|state| {
                        self.module.owner_families().iter().find(|candidate| candidate.signature == state.family)
                    }).is_some_and(|candidate| candidate.list_type == exact_type),
                    _ => false,
                };
                if !exact {
                    self.release_interpreter_value(retained)?;
                    return Err(InterpretError::TypeMismatch("nested owner family"));
                }
                values.insert(*destination, retained);
            }
            OwnerOperation::ReadSharedPlace { destination, owner, family } => {
                let family_info = self.owner_family(*family)?;
                if !matches!(family_info.source, OwnerFamilySource::SharedReference(_)) {
                    return Err(InterpretError::UnsupportedOperation("non-reference shared-place family"));
                }
                let owner = self.owner_value(*owner, values)?;
                let (environment, constructor) = {
                    let state = self.owners.get(&owner)
                        .ok_or(InterpretError::UnsupportedOperation("released owner"))?;
                    if state.family != *family {
                        return Err(InterpretError::TypeMismatch("owner family"));
                    }
                    let (constructor_family, constructor) = self.owner_constructor(state.constructor)?;
                    if constructor_family.signature != *family {
                        return Err(InterpretError::TypeMismatch("owner constructor family"));
                    }
                    (state.environment, constructor.clone())
                };
                let projection = constructor.shared_reference_constructor()
                    .ok_or(InterpretError::UnsupportedOperation("shared-place constructor schema"))?;
                self.retain(environment)?;
                let read = (|| {
                    let capture = |ordinal: crate::canonical_safewir::closure_owner_contract::CaptureOrdinal|
                        -> Result<InterpreterValue, InterpretError> {
                        let capture = constructor.captures.get(ordinal.0 as usize)
                            .ok_or(InterpretError::UnsupportedOperation("shared-place capture ordinal"))?;
                        let CaptureStorage::EnvironmentField(field) = capture.storage else {
                            return Err(InterpretError::TypeMismatch("shared-place capture storage"));
                        };
                        self.object(environment)?.cells.get(&CellKey::Field(field)).cloned()
                            .ok_or(InterpretError::UninitializedPlace)
                    };
                    let (root, index) = match projection {
                        SharedReferenceConstructor::WholeIntCell { root } => (capture(root)?, None),
                        SharedReferenceConstructor::ListIntElement { root, index, bounds: SharedPlaceBounds::CurrentLogicalLength } => {
                            let InterpreterValue::Size(index) = capture(index)? else {
                                return Err(InterpretError::TypeMismatch("shared-place index"));
                            };
                            (capture(root)?, Some(index))
                        }
                        SharedReferenceConstructor::ListIntElementFromSlice { slice, index, bounds: SharedPlaceBounds::CurrentLogicalLength } => {
                            let InterpreterValue::Size(local_index) = capture(index)? else {
                                return Err(InterpretError::TypeMismatch("shared-slice element index"));
                            };
                            let slice_capture = constructor.captures.get(slice.0 as usize)
                                .ok_or(InterpretError::UnsupportedOperation("shared-slice element capture ordinal"))?;
                            if !matches!((slice_capture.value_type, slice_capture.storage, slice_capture.transfer),
                                (ValueType::Gc(_), CaptureStorage::OwnerField(_), CaptureTransfer::CloneManaged))
                            {
                                return Err(InterpretError::TypeMismatch("shared-slice element capture storage"));
                            }
                            let child_state = self.owners.get(&owner)
                                .ok_or(InterpretError::UnsupportedOperation("released shared-place owner"))?;
                            let InterpreterValue::OpaqueOwner(slice) = self.owner_capture_value(child_state, slice_capture.storage)? else {
                                return Err(InterpretError::TypeMismatch("shared-slice element carrier"));
                            };
                            // `owner` remains live for this operation, hence
                            // its nested OwnerField retains this slice and the
                            // slice's environment retains the backing root.
                            // Keep that chain intact until the common root
                            // read below retains/releases the backing object.
                            let state = self.owners.get(&slice)
                                .ok_or(InterpretError::UnsupportedOperation("released shared-slice carrier"))?;
                            let slice_family = self.owner_family(state.family)?;
                            if !matches!(&slice_family.source, OwnerFamilySource::SharedReference(source)
                                if rooted_int_slice_reference(source))
                            {
                                return Err(InterpretError::TypeMismatch("shared-slice element family"));
                            }
                            let (constructor_family, slice_constructor) = self.owner_constructor(state.constructor)?;
                            if constructor_family.signature != state.family {
                                return Err(InterpretError::TypeMismatch("shared-slice element constructor family"));
                            }
                            let SharedReferenceConstructor::ListSlice { root, start, length } =
                                slice_constructor.shared_reference_constructor()
                                    .ok_or(InterpretError::UnsupportedOperation("shared-slice element constructor schema"))?
                            else { return Err(InterpretError::TypeMismatch("shared-slice element constructor")) };
                            let read_slice_capture = |ordinal: crate::canonical_safewir::closure_owner_contract::CaptureOrdinal| -> Result<InterpreterValue, InterpretError> {
                                let capture = slice_constructor.captures.get(ordinal.0 as usize)
                                    .ok_or(InterpretError::UnsupportedOperation("shared-slice element capture ordinal"))?;
                                let CaptureStorage::EnvironmentField(field) = capture.storage else {
                                    return Err(InterpretError::TypeMismatch("shared-slice element capture storage"));
                                };
                                self.object(state.environment)?.cells.get(&CellKey::Field(field)).cloned()
                                    .ok_or(InterpretError::UninitializedPlace)
                            };
                            let InterpreterValue::ObjectRef(root) = read_slice_capture(root)? else {
                                return Err(InterpretError::TypeMismatch("shared-slice element root"));
                            };
                            let InterpreterValue::Size(start) = read_slice_capture(start)? else {
                                return Err(InterpretError::TypeMismatch("shared-slice element start"));
                            };
                            let InterpreterValue::Size(length) = read_slice_capture(length)? else {
                                return Err(InterpretError::TypeMismatch("shared-slice element length"));
                            };
                            if local_index >= length { return Err(InterpretError::IndexOutOfBounds { index: local_index, length }); }
                            let absolute = start.checked_add(local_index).ok_or(InterpretError::ArithmeticTrap)?;
                            (InterpreterValue::ObjectRef(root), Some(absolute))
                        }
                        SharedReferenceConstructor::ListSlice { .. } => {
                            return Err(InterpretError::UnsupportedOperation("shared-place slice carrier"));
                        }
                    };
                    let InterpreterValue::ObjectRef(root) = root else {
                        return Err(InterpretError::TypeMismatch("shared-place root"));
                    };
                    self.retain(root)?;
                    let value = match index {
                        None => self.object(root)?.cells.get(&CellKey::Field(0)).cloned()
                            .ok_or(InterpretError::UninitializedPlace),
                        Some(index) => {
                            self.check_element(root, index)?;
                            self.object(root)?.cells.get(&CellKey::Element(index)).cloned()
                                .ok_or(InterpretError::UninitializedPlace)
                        }
                    };
                    let release = self.release(root);
                    let value = value?;
                    release?;
                    if !matches!(value, InterpreterValue::Int(_)) {
                        return Err(InterpretError::TypeMismatch("shared-place Int"));
                    }
                    Ok(value)
                })();
                let release = self.release(environment);
                let value = read?;
                release?;
                values.insert(*destination, value);
            }
            OwnerOperation::ReadSharedSlice { destination, owner, family, field } => {
                let family_info = self.owner_family(*family)?;
                if !matches!(&family_info.source, OwnerFamilySource::SharedReference(_))
                    || family_info.slice_carrier.is_none()
                {
                    return Err(InterpretError::UnsupportedOperation("non-slice shared-slice family"));
                }
                let owner = self.owner_value(*owner, values)?;
                let (environment, constructor) = {
                    let state = self.owners.get(&owner)
                        .ok_or(InterpretError::UnsupportedOperation("released owner"))?;
                    if state.family != *family {
                        return Err(InterpretError::TypeMismatch("owner family"));
                    }
                    let (constructor_family, constructor) = self.owner_constructor(state.constructor)?;
                    if constructor_family.signature != *family {
                        return Err(InterpretError::TypeMismatch("owner constructor family"));
                    }
                    (state.environment, constructor.clone())
                };
                let SharedReferenceConstructor::ListSlice { root, start, length } = constructor.shared_reference_constructor()
                    .ok_or(InterpretError::UnsupportedOperation("shared-slice constructor schema"))?
                else {
                    return Err(InterpretError::UnsupportedOperation("non-slice shared carrier"));
                };
                use crate::canonical_safewir::closure_owner_contract::SharedSliceField;
                let ordinal = match field {
                    SharedSliceField::Root => root,
                    SharedSliceField::Start => start,
                    SharedSliceField::Length => length,
                };
                let capture = constructor.captures.get(ordinal.0 as usize)
                    .ok_or(InterpretError::UnsupportedOperation("shared-slice capture ordinal"))?;
                let CaptureStorage::EnvironmentField(cell) = capture.storage else {
                    return Err(InterpretError::TypeMismatch("shared-slice capture storage"));
                };
                self.retain(environment)?;
                let value = self.object(environment)?.cells.get(&CellKey::Field(cell)).cloned()
                    .ok_or(InterpretError::UninitializedPlace);
                let value = match (field, value) {
                    (SharedSliceField::Start | SharedSliceField::Length, Ok(InterpreterValue::Size(size))) => {
                        Ok(InterpreterValue::Size(size))
                    }
                    // The retained root is one more strong token of the
                    // backing list, released by its owner like any shared
                    // reference; the environment's own token is untouched.
                    (SharedSliceField::Root, Ok(InterpreterValue::ObjectRef(root))) => {
                        self.retain(root).map(|()| InterpreterValue::ObjectRef(root))
                    }
                    (_, Ok(_)) => Err(InterpretError::TypeMismatch("shared-slice field")),
                    (_, Err(error)) => Err(error),
                };
                let release = self.release(environment);
                let value = value?;
                release?;
                values.insert(*destination, value);
            }
            OwnerOperation::ReadTag { destination, owner, family } => {
                self.owner_family(*family)?;
                let owner = self.owner_value(*owner, values)?;
                let state = self.owners.get(&owner).ok_or(InterpretError::UnsupportedOperation("released owner"))?;
                if state.family != *family {
                    return Err(InterpretError::TypeMismatch("owner family"));
                }
                let (constructor_family, constructor) = self.owner_constructor(state.constructor)?;
                if constructor_family.signature != *family {
                    return Err(InterpretError::TypeMismatch("owner constructor family"));
                }
                values.insert(*destination, InterpreterValue::Int(constructor.environment_tag));
            }
            OwnerOperation::ReadCallable { destination, owner, family } => {
                let owner_family = self.owner_family(*family)?;
                if !matches!(owner_family.source, crate::canonical_safewir::closure_owner_contract::OwnerFamilySource::Closure) {
                    return Err(InterpretError::UnsupportedOperation("aggregate owner is not callable"));
                }
                let target_signature = owner_family.signature;
                let owner = self.owner_value(*owner, values)?;
                let state = self.owners.get(&owner).ok_or(InterpretError::UnsupportedOperation("released owner"))?;
                if state.family != target_signature {
                    return Err(InterpretError::TypeMismatch("owner family"));
                }
                let (constructor_family, constructor_info) = self.owner_constructor(state.constructor)?;
                if constructor_family.signature != target_signature {
                    return Err(InterpretError::TypeMismatch("owner constructor family"));
                }
                values.insert(*destination, InterpreterValue::Function(constructor_info.function));
            }
            OwnerOperation::Clone { destination, owner, family } => {
                self.owner_family(*family)?;
                let owner = self.owner_value(*owner, values)?;
                if self.owners.get(&owner).is_some_and(|state| state.family != *family) {
                    return Err(InterpretError::TypeMismatch("owner family"));
                }
                self.retain_owner(owner)?;
                values.insert(*destination, InterpreterValue::OpaqueOwner(owner));
            }
            OwnerOperation::Drop { owner, family } => {
                self.owner_family(*family)?;
                let owner = self.owner_value(*owner, values)?;
                if self.owners.get(&owner).is_some_and(|state| state.family != *family) {
                    return Err(InterpretError::TypeMismatch("owner family"));
                }
                self.release_owner(owner)?;
            }
            OwnerOperation::CloneEnvironment { destination, owner, constructor } => {
                let (family, constructor_info) = self.owner_constructor(*constructor)?;
                let owner = self.owner_value(*owner, values)?;
                let environment = {
                    let state = self.owners.get(&owner).ok_or(InterpretError::UnsupportedOperation("released owner"))?;
                    if state.family != family.signature || state.constructor != constructor_info.id {
                        return Err(InterpretError::TypeMismatch("owner constructor"));
                    }
                    state.environment
                };
                self.retain(environment)?;
                values.insert(*destination, InterpreterValue::ObjectRef(environment));
            }
            OwnerOperation::NewList { destination, family, elements } => {
                self.owner_family(*family)?;
                let list_number = self.next_owner_list.checked_add(1).ok_or(InterpretError::ArithmeticTrap)?;
                let mut retained = Vec::with_capacity(elements.len());
                for element in elements {
                    let owner = self.owner_value(*element, values)?;
                    if self.owners.get(&owner).is_none_or(|state| state.family != *family) {
                        for retained_owner in retained { self.release_owner(retained_owner)?; }
                        return Err(InterpretError::TypeMismatch("owner list family"));
                    }
                    if let Err(error) = self.retain_owner(owner) {
                        for retained_owner in retained { self.release_owner(retained_owner)?; }
                        return Err(error);
                    }
                    retained.push(owner);
                }
                let token = OpaqueOwnerListToken(self.next_owner_list);
                self.next_owner_list = list_number;
                self.owner_lists.insert(token, OwnerListState { family: *family, elements: retained, references: 1 });
                values.insert(*destination, InterpreterValue::OpaqueOwnerList(token));
            }
            OwnerOperation::ListGet { destination, list, index, family } => {
                self.owner_family(*family)?;
                let list = self.owner_list_value(*list, values)?;
                let index = self.size_value(*index, values)?;
                let owner = {
                    let state = self.owner_lists.get(&list).ok_or(InterpretError::UnsupportedOperation("released owner list"))?;
                    if state.family != *family { return Err(InterpretError::TypeMismatch("owner list family")); }
                    *state.elements.get(usize::try_from(index).map_err(|_| InterpretError::ArithmeticTrap)?).ok_or(InterpretError::IndexOutOfBounds { index, length: state.elements.len() as u64 })?
                };
                self.retain_owner(owner)?;
                values.insert(*destination, InterpreterValue::OpaqueOwner(owner));
            }
            OwnerOperation::ListLength { destination, list, family } => {
                self.owner_family(*family)?;
                let list = self.owner_list_value(*list, values)?;
                let length = self.owner_lists.get(&list).ok_or(InterpretError::UnsupportedOperation("released owner list"))?;
                if length.family != *family { return Err(InterpretError::TypeMismatch("owner list family")); }
                values.insert(*destination, InterpreterValue::Size(length.elements.len() as u64));
            }
            OwnerOperation::CloneList { destination, list, family } => {
                self.owner_family(*family)?;
                let list = self.owner_list_value(*list, values)?;
                let state = self.owner_lists.get(&list).ok_or(InterpretError::UnsupportedOperation("released owner list"))?;
                if state.family != *family { return Err(InterpretError::TypeMismatch("owner list family")); }
                self.retain_owner_list(list)?;
                values.insert(*destination, InterpreterValue::OpaqueOwnerList(list));
            }
            OwnerOperation::ListPush { destination, list, value, family } => {
                self.owner_family(*family)?;
                let list_token = self.owner_list_value(*list, values)?;
                let owner = self.owner_value(*value, values)?;
                let mut elements = {
                    let state = self.owner_lists.get(&list_token).ok_or(InterpretError::UnsupportedOperation("released owner list"))?;
                    if state.family != *family { return Err(InterpretError::TypeMismatch("owner list family")); }
                    if self.owners.get(&owner).is_none_or(|state| state.family != *family) {
                        return Err(InterpretError::TypeMismatch("owner list value family"));
                    }
                    state.elements.clone()
                };
                let mut retained = Vec::with_capacity(elements.len());
                for element in &elements {
                    if let Err(error) = self.retain_owner(*element) {
                        for retained_owner in retained { self.release_owner(retained_owner)?; }
                        return Err(error);
                    }
                    retained.push(*element);
                }
                elements.push(owner);
                let list_number = self.next_owner_list.checked_add(1).ok_or(InterpretError::ArithmeticTrap)?;
                let new_list = OpaqueOwnerListToken(self.next_owner_list);
                self.next_owner_list = list_number;
                if let Err(error) = self.release_owner_list(list_token) {
                    for retained_owner in retained { self.release_owner(retained_owner)?; }
                    return Err(error);
                }
                self.owner_lists.insert(new_list, OwnerListState { family: *family, elements, references: 1 });
                values.remove(list);
                values.remove(value);
                values.insert(*destination, InterpreterValue::OpaqueOwnerList(new_list));
            }
            OwnerOperation::ListSet { destination, list, index, value, family } => {
                self.owner_family(*family)?;
                let list_token = self.owner_list_value(*list, values)?;
                let owner = self.owner_value(*value, values)?;
                let index_value = self.size_value(*index, values)?;
                let index = usize::try_from(index_value).map_err(|_| InterpretError::ArithmeticTrap)?;
                let mut elements = {
                    let state = self.owner_lists.get(&list_token).ok_or(InterpretError::UnsupportedOperation("released owner list"))?;
                    if state.family != *family { return Err(InterpretError::TypeMismatch("owner list family")); }
                    if self.owners.get(&owner).is_none_or(|state| state.family != *family) {
                        return Err(InterpretError::TypeMismatch("owner list value family"));
                    }
                    if index >= state.elements.len() {
                        return Err(InterpretError::IndexOutOfBounds { index: index_value, length: state.elements.len() as u64 });
                    }
                    state.elements.clone()
                };
                let mut retained = Vec::with_capacity(elements.len().saturating_sub(1));
                for (position, element) in elements.iter().copied().enumerate() {
                    if position == index { continue; }
                    if let Err(error) = self.retain_owner(element) {
                        for retained_owner in retained { self.release_owner(retained_owner)?; }
                        return Err(error);
                    }
                    retained.push(element);
                }
                elements[index] = owner;
                let list_number = self.next_owner_list.checked_add(1).ok_or(InterpretError::ArithmeticTrap)?;
                let new_list = OpaqueOwnerListToken(self.next_owner_list);
                self.next_owner_list = list_number;
                if let Err(error) = self.release_owner_list(list_token) {
                    for retained_owner in retained { self.release_owner(retained_owner)?; }
                    return Err(error);
                }
                self.owner_lists.insert(new_list, OwnerListState { family: *family, elements, references: 1 });
                values.remove(list);
                values.remove(value);
                values.insert(*destination, InterpreterValue::OpaqueOwnerList(new_list));
            }
            OwnerOperation::DropList { list, family } => {
                self.owner_family(*family)?;
                let list = self.owner_list_value(*list, values)?;
                let state = self.owner_lists.get(&list).ok_or(InterpretError::UnsupportedOperation("released owner list"))?;
                if state.family != *family { return Err(InterpretError::TypeMismatch("owner list family")); }
                self.release_owner_list(list)?;
            }
        }
        Ok(())
    }

    fn owner_capture_value(
        &self,
        state: &OwnerState,
        storage: CaptureStorage,
    ) -> Result<InterpreterValue, InterpretError> {
        match storage {
            CaptureStorage::EnvironmentField(field) => self
                .object(state.environment)?
                .cells
                .get(&CellKey::Field(field))
                .cloned()
                .ok_or(InterpretError::UninitializedPlace),
            CaptureStorage::OwnerField(field) => state
                .owner_fields
                .get(&field)
                .cloned()
                .ok_or(InterpretError::UninitializedPlace),
        }
    }

    fn retain_owner(&mut self, owner: OpaqueOwnerToken) -> Result<(), InterpretError> {
        let state = self.owners.get_mut(&owner).ok_or(InterpretError::UnsupportedOperation("released owner"))?;
        state.references = state.references.checked_add(1).ok_or(InterpretError::ReferenceCountOverflow(owner.0))?;
        Ok(())
    }

    fn release_owner(&mut self, owner: OpaqueOwnerToken) -> Result<(), InterpretError> {
        let final_release = {
            let state = self.owners.get_mut(&owner).ok_or(InterpretError::UnsupportedOperation("released owner"))?;
            state.references = state.references.checked_sub(1).ok_or(InterpretError::ReferenceCountUnderflow(owner.0))?;
            state.references == 0
        };
        if !final_release { return Ok(()); }
        let state = self.owners.remove(&owner).ok_or(InterpretError::UnsupportedOperation("released owner"))?;
        for value in state.owner_fields.into_values() {
            self.release_interpreter_value(value)?;
        }
        self.release(state.environment)
    }

    fn retain_owner_list(&mut self, list: OpaqueOwnerListToken) -> Result<(), InterpretError> {
        let state = self.owner_lists.get_mut(&list).ok_or(InterpretError::UnsupportedOperation("released owner list"))?;
        state.references = state.references.checked_add(1).ok_or(InterpretError::ReferenceCountOverflow(list.0))?;
        Ok(())
    }

    fn release_owner_list(&mut self, list: OpaqueOwnerListToken) -> Result<(), InterpretError> {
        let final_release = {
            let state = self.owner_lists.get_mut(&list).ok_or(InterpretError::UnsupportedOperation("released owner list"))?;
            state.references = state.references.checked_sub(1).ok_or(InterpretError::ReferenceCountUnderflow(list.0))?;
            state.references == 0
        };
        if !final_release { return Ok(()); }
        let state = self.owner_lists.remove(&list).ok_or(InterpretError::UnsupportedOperation("released owner list"))?;
        for owner in state.elements { self.release_owner(owner)?; }
        Ok(())
    }

    fn release_interpreter_value(&mut self, value: InterpreterValue) -> Result<(), InterpretError> {
        match value {
            InterpreterValue::ObjectRef(object) => self.release(object),
            InterpreterValue::OpaqueOwner(owner) => self.release_owner(owner),
            InterpreterValue::OpaqueOwnerList(list) => self.release_owner_list(list),
            _ => Ok(()),
        }
    }

    /// The two prelude imports are the host calls the oracle models: `print`
    /// reads the buffer's bytes and records a line; `float_to_str` writes a
    /// `Float`'s shortest round-trip decimal (the shared `render_float`
    /// rule) into the buffer and returns the byte length. Every other import
    /// is an effect the oracle does not model and fails closed.
    fn host_call(
        &mut self,
        import: crate::canonical_safewir::ImportId,
        destinations: &[ValueId],
        arguments: &[HostCallArgument],
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        let contract = self
            .module
            .imports()
            .iter()
            .find(|contract| contract.id == import)
            .ok_or(InterpretError::UnsupportedOperation("HostCall unknown import"))?;
        if contract.module != "witchy" {
            return Err(InterpretError::UnsupportedOperation("HostCall import"));
        }
        self.server.before_host_call(&contract.name)?;
        if matches!(contract.name.as_str(), "__witchy_abort_at" | "__witchy_list_bounds_at") {
            let read = |place| -> Result<String, InterpretError> {
                let (object, start, length) = self.slice_place(place, values)?;
                self.check_range(object, start, length, false)?;
                let cells = &self.object(object)?.cells;
                let mut bytes = Vec::new();
                for index in start..start.checked_add(length).ok_or(InterpretError::ArithmeticTrap)? {
                    match cells.get(&CellKey::Element(index)) {
                        Some(InterpreterValue::Size(byte)) => bytes.push(u8::try_from(*byte).map_err(|_| InterpretError::TypeMismatch("abort byte"))?),
                        _ => return Err(InterpretError::UninitializedPlace),
                    }
                }
                String::from_utf8(bytes).map_err(|_| InterpretError::TypeMismatch("abort UTF-8"))
            };
            let (core, function, line) = match (contract.name.as_str(), arguments, destinations) {
                ("__witchy_abort_at", [HostCallArgument::Buffer { place: message, .. }, HostCallArgument::Buffer { place: function, .. }, HostCallArgument::Value(line)], []) =>
                    (witchy_syntax::diag::DiagTemplate::Fail.render(0, 0, &read(message)?), function, line),
                ("__witchy_list_bounds_at", [HostCallArgument::Value(index), HostCallArgument::Value(length), HostCallArgument::Buffer { place: function, .. }, HostCallArgument::Value(line)], []) => {
                    let (InterpreterValue::Int(index), InterpreterValue::Size(length)) = (self.value(*index, values)?, self.value(*length, values)?) else {
                        return Err(InterpretError::TypeMismatch("bounds diagnostic values"));
                    };
                    let length = u32::try_from(*length).map_err(|_| InterpretError::ArithmeticTrap)?;
                    (witchy_syntax::diag::DiagTemplate::ListIndexOob.render(*index, i64::from(length), ""), function, line)
                }
                _ => return Err(InterpretError::TypeMismatch("abort arguments")),
            };
            let function = read(function)?;
            let InterpreterValue::Size(line) = self.value(*line, values)? else { return Err(InterpretError::TypeMismatch("abort line")); };
            let line = u32::try_from(*line).map_err(|_| InterpretError::ArithmeticTrap)?;
            return Err(InterpretError::DiagnosticAbort {
                message: witchy_syntax::diag::runtime_error(&function, line, &core),
                output: self.output.clone(),
            });
        }
        if contract.name == "float_to_str" {
            let ([HostCallArgument::Value(value), HostCallArgument::Buffer { place, .. }], [destination]) = (arguments, destinations) else {
                return Err(InterpretError::UnsupportedOperation("HostCall float_to_str arguments"));
            };
            let InterpreterValue::FloatBits(bits) = self.value(*value, values)? else {
                return Err(InterpretError::TypeMismatch("float_to_str operand"));
            };
            let rendered = witchy_syntax::fmt::render_float(f64::from_bits(*bits)).into_bytes();
            let (object, start, length) = self.slice_place(place, values)?;
            self.check_range(object, start, length, true)?;
            let written = u64::try_from(rendered.len()).map_err(|_| InterpretError::ArithmeticTrap)?;
            if written > length {
                return Err(InterpretError::IndexOutOfBounds { index: written, length });
            }
            let cells = &mut self.object_mut(object)?.cells;
            for (offset, byte) in rendered.iter().enumerate() {
                cells.insert(CellKey::Element(start + offset as u64), InterpreterValue::Size(u64::from(*byte)));
            }
            values.insert(*destination, InterpreterValue::Size(written));
            return Ok(());
        }
        if matches!(contract.name.as_str(), "compiler_footprint_into" | "compiler_diff_into" | "compiler_doc_into" | "compiler_doc_result_json_into") {
            return self.compiler_host_call(&contract.name, destinations, arguments, values);
        }
        if contract.name == "regex_match_spans_into" {
            return self.regex_spans_host_call(destinations, arguments, values);
        }
        if self.isolated && !vm_service::isolated_import_allowed(contract) {
            return Err(InterpretError::UnsupportedHostImport);
        }
        if contract.name == crate::canonical_safewir::CRYPTO_ED25519_VERIFY_IMPORT {
            return self.ed25519_verify_host_call(destinations, arguments, values);
        }
        if contract.name == crate::canonical_safewir::CRYPTO_RSA_PKCS1_SHA256_VERIFY_IMPORT {
            return self.rsa_verify_host_call(destinations, arguments, values);
        }
        if contract.name == crate::canonical_safewir::CRYPTO_SHA256_IMPORT
            || contract.name == crate::canonical_safewir::CRYPTO_RUNE_HASH_IMPORT
        {
            return self.crypto_hash_host_call(&contract.name, destinations, arguments, values);
        }
        if crate::canonical_safewir::CRYPTO_EXTENSION_IMPORT_NAMES.contains(&contract.name.as_str()) {
            return self.crypto_extension_host_call(&contract.name, destinations, arguments, values);
        }
        if exec::is_import(&contract.name) {
            return self.exec_host_call(&contract.name, destinations, arguments, values);
        }
        if crate::canonical_safewir::FETCH_IMPORT_NAMES.contains(&contract.name.as_str()) {
            return self.fetch_host_call(&contract.name, destinations, arguments, values);
        }
        if registry_host::IMPORTS.contains(&contract.name.as_str()) {
            return self.registry_host_call(&contract.name, destinations, arguments, values);
        }
        if dir_storage::is_import(&contract.name) {
            return self.dir_storage_host_call(&contract.name, destinations, arguments, values);
        }
        if build_read::is_import(&contract.name) {
            return self.build_read_host_call(&contract.name, destinations, arguments, values);
        }
        if contract.name == "build_exec_run" {
            return self.build_exec_host_call(destinations, arguments, values);
        }
        if contract.name == "build_net_fetch" {
            return self.build_net_host_call(destinations, arguments, values);
        }
        if contract.name == "build_out_write_at" {
            return self.build_out_host_call(destinations, arguments, values);
        }
        if vm_service::is_import(&contract.name) {
            return self.vm_service_host_call(&contract.name, destinations, arguments, values);
        }
        if net::is_import(&contract.name) {
            return self.net_host_call(&contract.name, destinations, arguments, values);
        }
        if let Some(name) = CAPABILITY_IMPORTS.iter().find(|name| **name == contract.name) {
            return self.capability_host_call(name, destinations, arguments, values);
        }
        if !matches!(contract.name.as_str(), "print" | "console_print" | "console_print_at") || !destinations.is_empty() {
            return Err(InterpretError::UnsupportedOperation("HostCall import"));
        }
        let site = if contract.name == "console_print_at" {
            let [token, HostCallArgument::Buffer { .. }, HostCallArgument::Buffer { place, .. }, HostCallArgument::Value(line)] = arguments else {
                return Err(InterpretError::UnsupportedOperation("Console source arguments"));
            };
            self.capability_scope(token, 0, values)?;
            let function = self.host_string(place, values)?;
            let line = match values.get(line) {
                Some(InterpreterValue::Size(line)) => u32::try_from(*line).map_err(|_| InterpretError::ArithmeticTrap)?,
                _ => return Err(InterpretError::TypeMismatch("Console source line")),
            };
            Some((function, line))
        } else { None };
        let place = match (contract.name.as_str(), arguments) {
            ("console_print_at", [_, HostCallArgument::Buffer { place, .. }, _, _]) => place,

            ("console_print", [token, HostCallArgument::Buffer { place, .. }]) => {
                self.capability_scope(token, 0, values)?;
                place
            }
            ("print", [HostCallArgument::Buffer { place, .. }]) => place,
            _ => return Err(InterpretError::UnsupportedOperation("HostCall print arguments")),
        };
        let (object, start, length) = self.slice_place(place, values)?;
        self.check_range(object, start, length, false)?;
        let cells = &self.object(object)?.cells;
        let mut bytes = Vec::with_capacity(length as usize);
        for index in start..start.checked_add(length).ok_or(InterpretError::ArithmeticTrap)? {
            match cells.get(&CellKey::Element(index)) {
                Some(InterpreterValue::Size(byte)) => bytes.push(u8::try_from(*byte).map_err(|_| InterpretError::TypeMismatch("byte cell"))?),
                Some(_) => return Err(InterpretError::TypeMismatch("byte cell")),
                None => return Err(InterpretError::UninitializedPlace),
            }
        }
        // The console host records a line with its trailing newlines removed.
        let text = String::from_utf8_lossy(&bytes).trim_end_matches('\n').to_owned();
        if let Some((function, line)) = site
            && let Some(trace) = &self.config.console_trace
        {
            trace.lock().map_err(|_| InterpretError::UnsupportedOperation("Console trace lock"))?
                .push(OracleConsoleEvent { text: text.clone(), function, line });
        }
        self.output.push(text);

        Ok(())
    }

    fn compiler_host_call(
        &mut self,
        name: &str,
        destinations: &[ValueId],
        arguments: &[HostCallArgument],
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        let Some(service) = self.config.compiler_services else {
            return Err(InterpretError::UnsupportedHostImport);
        };
        let (kind, inputs, output) = match (name, arguments) {
            ("compiler_footprint_into", [HostCallArgument::Buffer { place: source, .. }, HostCallArgument::Buffer { place: output, .. }]) =>
                (CompilerServiceKind::Footprint, vec![self.host_string(source, values)?], output),
            ("compiler_diff_into", [HostCallArgument::Buffer { place: old, .. }, HostCallArgument::Buffer { place: new, .. }, HostCallArgument::Buffer { place: output, .. }]) =>
                (CompilerServiceKind::Diff, vec![self.host_string(old, values)?, self.host_string(new, values)?], output),
            ("compiler_doc_into", [HostCallArgument::Buffer { place: name, .. }, HostCallArgument::Buffer { place: source, .. }, HostCallArgument::Buffer { place: output, .. }]) =>
                (CompilerServiceKind::Doc, vec![self.host_string(name, values)?, self.host_string(source, values)?], output),
            ("compiler_doc_result_json_into", [HostCallArgument::Buffer { place: name, .. }, HostCallArgument::Buffer { place: source, .. }, HostCallArgument::Buffer { place: output, .. }]) =>
                (CompilerServiceKind::DocResultJson, vec![self.host_string(name, values)?, self.host_string(source, values)?], output),
            _ => return Err(InterpretError::UnsupportedOperation("compiler host arguments")),
        };
        let [destination] = destinations else { return Err(InterpretError::UnsupportedOperation("compiler host result")); };
        let (object, start, capacity) = self.slice_place(output, values)?;
        self.check_range(object, start, capacity, true)?;
        // Even a size probe must authenticate its complete output extent
        // before invoking the injected service, as the native host does.
        let payload = service(kind, &inputs).map_err(|_| InterpretError::ExplicitTrap)?.into_bytes();
        let required = compiler_result_size(payload.len())?;
        if required <= capacity {
            let cells = &mut self.object_mut(object)?.cells;
            for (offset, byte) in payload.iter().enumerate() {
                cells.insert(CellKey::Element(start + offset as u64), InterpreterValue::Size(u64::from(*byte)));
            }
        }
        values.insert(*destination, InterpreterValue::Size(required));
        Ok(())
    }

/// The canonical `Dir`/`File`/`Env` operations, served from the fixture. Every
    /// refusal the runtime host would trap on — a missing file, an unset
    /// variable, a path that escapes the token's subtree — is an abort here,
    /// never a different answer.
    fn capability_host_call(
        &mut self,
        name: &str,
        destinations: &[ValueId],
        arguments: &[HostCallArgument],
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        if self.detached_dir_host_call(name, destinations, arguments, values)? { return Ok(()); }
        match (name, arguments, destinations) {
            (
                "dir_read_into",
                [token, HostCallArgument::Buffer { place: path, .. }, HostCallArgument::Buffer { place: out, .. }],
                [destination],
            ) => {
                let scope = self.capability_scope(token, DIR_KIND, values)?;
                let path = self.host_path(&scope, &self.host_string(path, values)?)?;
                // A `dir.read` is text: the runtime refuses a file that is
                // not UTF-8, so the oracle does too.
                let contents = self.config.fixture.file(&path).ok_or(InterpretError::ExplicitTrap)?;
                if std::str::from_utf8(&contents).is_err() {
                    return Err(InterpretError::ExplicitTrap);
                }
                let length = self.fill_host_buffer(out, &contents, values)?;
                values.insert(*destination, InterpreterValue::Size(length));
                Ok(())
            }
            ("dir_list_into", [token, HostCallArgument::Buffer { place: out, .. }], [destination]) => {
                let scope = self.capability_scope(token, DIR_KIND, values)?;
                let payload = self.config.fixture.entry_names(&scope).join("\0");
                let length = self.fill_host_buffer(out, payload.as_bytes(), values)?;
                values.insert(*destination, InterpreterValue::Size(length));
                Ok(())
            }
            ("dir_exists_at", [token, HostCallArgument::Buffer { place: path, .. }], [destination]) => {
                let scope = self.capability_scope(token, DIR_KIND, values)?;
                let path = self.host_string(path, values)?;
                self.host_path(&scope, &path)?;
                let exists = self.config.fixture.metadata_exists(&scope, &path);
                values.insert(*destination, InterpreterValue::Size(u64::from(exists)));
                Ok(())
            }
            ("dir_is_dir_at", [token, HostCallArgument::Buffer { place: path, .. }], [destination]) => {
                // Authenticate before any guest slice read. Path denial is a
                // false predicate; malformed transport still traps.
                let scope = self.capability_scope(token, DIR_KIND, values)?;
                let path = self.host_string(path, values)?;
                let result = self.config.fixture.metadata_is_directory(&scope, &path);
                values.insert(*destination, InterpreterValue::Bool(result));
                Ok(())
            }
            (
                "dir_write_at",
                [token, HostCallArgument::Buffer { place: path, .. }, HostCallArgument::Buffer { place: data, .. }],
                [],
            ) => {
                let scope = self.capability_scope(token, DIR_KIND, values)?;
                let path = self.host_path(&scope, &self.host_string(path, values)?)?;
                let data = self.host_bytes(data, values)?;
                // A directory cannot also become a fixture file. Include the
                // root and empty explicit directories, which have no file keys
                // from which to infer their existence. Refuse before mutation.
                if path.is_empty() || self.config.fixture.directories.contains(&path)
                    || self.config.fixture.is_directory(&path)
                {
                    return Err(InterpretError::ExplicitTrap);
                }
                self.config.fixture.files.insert(path, data);
                Ok(())
            }
            ("dir_subtree_at", [token, HostCallArgument::Buffer { place: path, .. }], [destination]) => {
                let scope = self.capability_scope(token, DIR_KIND, values)?;
                let scope = self.host_path(&scope, &self.host_string(path, values)?)?;
                // Narrowing opens the directory: a missing one traps, as the
                // runtime's confined open does.
                if !self.config.fixture.is_directory(&scope) {
                    return Err(InterpretError::ExplicitTrap);
                }
                let HostCallArgument::Capability(source) = token else { unreachable!() };
                let InterpreterValue::Capability(source) = self.value(*source, values)? else { unreachable!() };
                let narrowed = self.registry.narrow_dir(source, scope)?;
                values.insert(*destination, InterpreterValue::Capability(narrowed));
                Ok(())
            }
            ("dir_read_file", [token, HostCallArgument::Buffer { place: path, .. }], [destination]) => {
                let scope = self.capability_scope(token, DIR_KIND, values)?;
                let path = self.host_path(&scope, &self.host_string(path, values)?)?;
                if self.config.fixture.file(&path).is_none() {
                    return Err(InterpretError::ExplicitTrap);
                }
                let HostCallArgument::Capability(source) = token else { unreachable!() };
                let InterpreterValue::Capability(source) = self.value(*source, values)? else { unreachable!() };
                let file = if source.registry.is_some() { self.registry.mint_file(source, path)? }
                    else { CapabilityToken { scope: path, ..CapabilityToken::root(FILE_KIND) } };
                values.insert(*destination, InterpreterValue::Capability(file));
                Ok(())
            }
            ("file_read_into", [token, HostCallArgument::Buffer { place: out, .. }], [destination]) => {
                let path = self.capability_scope(token, FILE_KIND, values)?;
                let contents = self.config.fixture.file(&path).ok_or(InterpretError::ExplicitTrap)?;
                if std::str::from_utf8(&contents).is_err() {
                    return Err(InterpretError::ExplicitTrap);
                }
                let length = self.fill_host_buffer(out, &contents, values)?;
                values.insert(*destination, InterpreterValue::Size(length));
                Ok(())
            }
            ("env_has_var", [token, HostCallArgument::Buffer { place: name, .. }], [destination]) => {
                let _ = self.capability_scope(token, ENV_KIND, values)?;
                let name = self.host_string(name, values)?;
                let present = self.config.fixture.variable(&name).is_some();
                values.insert(*destination, InterpreterValue::Size(u64::from(present)));
                Ok(())
            }
            (
                "env_get_into",
                [token, HostCallArgument::Buffer { place: name, .. }, HostCallArgument::Buffer { place: out, .. }],
                [destination],
            ) => {
                let _ = self.capability_scope(token, ENV_KIND, values)?;
                let name = self.host_string(name, values)?;
                let value = self.config.fixture.variable(&name).ok_or(InterpretError::ExplicitTrap)?;
                let length = self.fill_host_buffer(out, value.as_bytes(), values)?;
                values.insert(*destination, InterpreterValue::Size(length));
                Ok(())
            }
            _ => Err(InterpretError::UnsupportedOperation("HostCall capability arguments")),
        }
    }

    /// The subtree a capability argument of the expected kind is confined to.
    fn capability_scope(
        &self,
        argument: &HostCallArgument,
        kind: u32,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<String, InterpretError> {
        let HostCallArgument::Capability(value) = argument else {
            return Err(InterpretError::UnsupportedOperation("HostCall capability argument"));
        };
        let InterpreterValue::Capability(token) = self.value(*value, values)? else {
            return Err(InterpretError::TypeMismatch("capability token"));
        };
        if token.kind != kind {
            return Err(InterpretError::TypeMismatch("capability kind"));
        }
        if token.registry.is_some() {
            if kind == DIR_KIND { return self.registry.storage_scope(token); }
            if kind == FILE_KIND { return self.registry.file_scope(token); }
        }
        Ok(token.scope.clone())
    }

    /// A checked read slice's bytes.
    fn host_bytes(
        &self,
        place: &Place,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<Vec<u8>, InterpretError> {
        let (object, start, length) = self.slice_place(place, values)?;
        self.check_range(object, start, length, false)?;
        let cells = &self.object(object)?.cells;
        let mut bytes = Vec::with_capacity(length as usize);
        for index in start..start.checked_add(length).ok_or(InterpretError::ArithmeticTrap)? {
            match cells.get(&CellKey::Element(index)) {
                Some(InterpreterValue::Size(byte)) => bytes.push(u8::try_from(*byte).map_err(|_| InterpretError::TypeMismatch("byte cell"))?),
                Some(_) => return Err(InterpretError::TypeMismatch("byte cell")),
                None => return Err(InterpretError::UninitializedPlace),
            }
        }
        Ok(bytes)
    }

    /// A path or variable name arrives as a string slice, so it is UTF-8 or
    /// the host refuses it.
    fn host_string(
        &self,
        place: &Place,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<String, InterpretError> {
        String::from_utf8(self.host_bytes(place, values)?)
            .map_err(|_| InterpretError::TypeMismatch("host string is not UTF-8"))
    }

    /// Confine one relative path to a token's subtree, refusing exactly what
    /// the runtime's `dir_guard` refuses: an absolute path, a `..` or `.`
    /// component, and an empty component.
    fn host_path(&self, scope: &str, path: &str) -> Result<String, InterpretError> {
        if path.starts_with('/') {
            return Err(InterpretError::ExplicitTrap);
        }
        // An empty component (`a//b`) is tolerated like the runtime's
        // confined path; `.` and `..` are refused.
        let components = path.split('/').filter(|component| !component.is_empty()).collect::<Vec<_>>();
        if components.iter().any(|component| *component == "." || *component == "..") {
            return Err(InterpretError::ExplicitTrap);
        }
        let path = components.join("/");
        Ok(if scope.is_empty() { path } else if path.is_empty() { scope.to_owned() } else { format!("{scope}/{path}") })
    }

    /// Write the leading bytes that fit into a checked write slice and answer
    /// the payload's FULL length: the guest publishes `min(length, capacity)`
    /// itself, under its own guard.
    fn fill_host_buffer(
        &mut self,
        place: &Place,
        payload: &[u8],
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<u64, InterpretError> {
        let (object, start, capacity) = self.slice_place(place, values)?;
        self.check_range(object, start, capacity, true)?;
        let full = u64::try_from(payload.len()).map_err(|_| InterpretError::ArithmeticTrap)?;
        if full > i32::MAX as u64 {
            return Err(InterpretError::ArithmeticTrap);
        }
        let written = full.min(capacity);
        let cells = &mut self.object_mut(object)?.cells;
        for offset in 0..written {
            let byte = payload[offset as usize];
            cells.insert(CellKey::Element(start + offset), InterpreterValue::Size(u64::from(byte)));
        }
        Ok(full)
    }

    fn slice_place(
        &self,
        place: &Place,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(u64, u64, u64), InterpretError> {
        let object = self.object_value(place.object(), values)?;
        let Projection::Slice { start, length } = place.projection() else {
            return Err(InterpretError::UnsupportedOperation("range place"));
        };
        Ok((object, self.size_value(*start, values)?, self.size_value(*length, values)?))
    }

    /// A range must lie inside the logical count for reads and inside the
    /// capacity for writes, exactly as the checker's range rule bounds it.
    fn check_range(&self, object: u64, start: u64, length: u64, write: bool) -> Result<(), InterpretError> {
        let RuntimeExtent::Counted { count, capacity, .. } = &self.object(object)?.extent else {
            return Err(InterpretError::TypeMismatch("range on uncounted object"));
        };
        let bound = if write { *capacity } else { *count };
        let end = start.checked_add(length).ok_or(InterpretError::ArithmeticTrap)?;
        if end > bound {
            return Err(InterpretError::IndexOutOfBounds { index: end, length: bound });
        }
        Ok(())
    }

    /// Copies raw bytes with `memmove` semantics. Unlike typed range transfer,
    /// byte copies may overlap because the complete source is snapshotted
    /// before any destination cell is written.
    fn copy_bytes(
        &mut self,
        destination: &Place,
        source: &Place,
        length: ValueId,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        let length = self.size_value(length, values)?;
        let (source_object, source_start, source_length) = self.slice_place(source, values)?;
        let (destination_object, destination_start, destination_length) = self.slice_place(destination, values)?;
        if source_length != length || destination_length != length {
            return Err(InterpretError::TypeMismatch("range length"));
        }
        self.check_range(source_object, source_start, length, false)?;
        self.check_range(destination_object, destination_start, length, true)?;
        if self.object(destination_object)?.immutable {
            return Err(InterpretError::ImmutableObject(destination_object));
        }

        let mut bytes = Vec::with_capacity(length as usize);
        for offset in 0..length {
            let key = CellKey::Element(source_start.checked_add(offset).ok_or(InterpretError::ArithmeticTrap)?);
            let byte = self
                .object(source_object)?
                .cells
                .get(&key)
                .cloned()
                .ok_or(InterpretError::UninitializedPlace)?;
            if matches!(byte, InterpreterValue::ObjectRef(_)) {
                return Err(InterpretError::TypeMismatch("byte cell"));
            }
            bytes.push(byte);
        }

        let cells = &mut self.object_mut(destination_object)?.cells;
        for (offset, byte) in bytes.into_iter().enumerate() {
            let key = CellKey::Element(destination_start.checked_add(offset as u64).ok_or(InterpretError::ArithmeticTrap)?);
            cells.insert(key, byte);
        }
        Ok(())
    }

    /// `CloneRange` copies typed cells; `MoveRange` moves them, which carries
    /// each child token across without a retain. Same-object ranges are refused
    /// even when disjoint because their ownership accounting is not byte copy.
    fn copy_range(
        &mut self,
        destination: &Place,
        source: &Place,
        length: ValueId,
        transfer: RangeTransfer,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        let length = self.size_value(length, values)?;
        let (source_object, source_start, source_length) = self.slice_place(source, values)?;
        let (destination_object, destination_start, destination_length) = self.slice_place(destination, values)?;
        if source_length != length || destination_length != length {
            return Err(InterpretError::TypeMismatch("range length"));
        }
        if source_object == destination_object {
            return Err(InterpretError::UnsupportedOperation("overlapping range"));
        }
        self.check_range(source_object, source_start, length, false)?;
        self.check_range(destination_object, destination_start, length, true)?;
        if self.object(destination_object)?.immutable {
            return Err(InterpretError::ImmutableObject(destination_object));
        }
        // Element keys of one object are contiguous in `CellKey` order, so
        // the source range is one `range` scan (copy) or one `split_off`
        // detachment (move) rather than a lookup or removal per element: a
        // long list copied per append was a B-tree search per cell.
        let source_end = source_start.checked_add(length).ok_or(InterpretError::ArithmeticTrap)?;
        let span = CellKey::Element(source_start)..CellKey::Element(source_end);
        let moved: Vec<InterpreterValue> = match transfer {
            RangeTransfer::Copy => {
                let cells = &self.object(source_object)?.cells;
                let run = cells.range(span).map(|(_, cell)| cell.clone()).collect::<Vec<_>>();
                if run.len() as u64 != length {
                    return Err(InterpretError::UninitializedPlace);
                }
                run
            }
            RangeTransfer::Move => {
                let cells = &mut self.object_mut(source_object)?.cells;
                if cells.range(span).count() as u64 != length {
                    return Err(InterpretError::UninitializedPlace);
                }
                let mut detached = cells.split_off(&CellKey::Element(source_start));
                let mut after = detached.split_off(&CellKey::Element(source_end));
                cells.append(&mut after);
                detached.into_values().collect()
            }
        };
        // A copied reference cell is a second owner of its object.
        if matches!(transfer, RangeTransfer::Copy) {
            for cell in &moved {
                if let InterpreterValue::ObjectRef(object) = cell {
                    self.retain(*object)?;
                }
            }
        }
        let destination_end = destination_start.checked_add(length).ok_or(InterpretError::ArithmeticTrap)?;
        let cells = &mut self.object_mut(destination_object)?.cells;
        // The checker requires the destination range to be empty; the oracle
        // states the same rule so an overwritten reference cell can never be
        // silently dropped.
        if cells.range(CellKey::Element(destination_start)..CellKey::Element(destination_end))
            .any(|(_, cell)| matches!(cell, InterpreterValue::ObjectRef(_)))
        {
            return Err(InterpretError::UnsupportedOperation("range copy over a stored reference"));
        }
        // Sorted input builds the run in bulk; `append` merges it, replacing
        // any scalar cell already in the range exactly as `insert` did.
        let mut run = moved.into_iter().zip(destination_start..)
            .map(|(cell, index)| (CellKey::Element(index), cell))
            .collect::<BTreeMap<_, _>>();
        cells.append(&mut run);
        Ok(())
    }

    fn arithmetic(
        &self,
        operator: ArithmeticOperator,
        left: &InterpreterValue,
        right: &InterpreterValue,
        overflow: OverflowBehavior,
    ) -> Result<InterpreterValue, InterpretError> {
        match (left, right) {
            (InterpreterValue::Int(left), InterpreterValue::Int(right)) => {
                self.int_arithmetic(operator, *left, *right, overflow)
                    .map(InterpreterValue::Int)
            }
            (InterpreterValue::Size(left), InterpreterValue::Size(right)) => {
                self.size_arithmetic(
                    operator,
                    self.size_from_u64(*left)? as u32,
                    self.size_from_u64(*right)? as u32,
                    overflow,
                )
                .map(|value| InterpreterValue::Size(u64::from(value)))
            }
            (InterpreterValue::FloatBits(left), InterpreterValue::FloatBits(right)) => {
                self.float_arithmetic(operator, *left, *right)
                    .map(InterpreterValue::FloatBits)
            }
            _ => Err(InterpretError::TypeMismatch("arithmetic operands")),
        }
    }

    fn int_arithmetic(
        &self,
        operator: ArithmeticOperator,
        left: i64,
        right: i64,
        overflow: OverflowBehavior,
    ) -> Result<i64, InterpretError> {
        let checked = |value: Option<i64>| value.ok_or(InterpretError::ArithmeticTrap);
        match operator {
            ArithmeticOperator::Add => match overflow {
                OverflowBehavior::Wrap => Ok(left.wrapping_add(right)),
                OverflowBehavior::Trap => checked(left.checked_add(right)),
            },
            ArithmeticOperator::Subtract => match overflow {
                OverflowBehavior::Wrap => Ok(left.wrapping_sub(right)),
                OverflowBehavior::Trap => checked(left.checked_sub(right)),
            },
            ArithmeticOperator::Multiply => match overflow {
                OverflowBehavior::Wrap => Ok(left.wrapping_mul(right)),
                OverflowBehavior::Trap => checked(left.checked_mul(right)),
            },
            ArithmeticOperator::DivideSigned => checked(left.checked_div(right)),
            ArithmeticOperator::DivideUnsigned => {
                let right = right as u64;
                if right == 0 {
                    Err(InterpretError::ArithmeticTrap)
                } else {
                    Ok(((left as u64) / right) as i64)
                }
            }
            ArithmeticOperator::RemainderSigned => {
                if right == 0 {
                    Err(InterpretError::ArithmeticTrap)
                } else if left == i64::MIN && right == -1 {
                    Ok(0)
                } else {
                    checked(left.checked_rem(right))
                }
            }
            ArithmeticOperator::RemainderUnsigned => {
                let right = right as u64;
                if right == 0 {
                    Err(InterpretError::ArithmeticTrap)
                } else {
                    Ok(((left as u64) % right) as i64)
                }
            }
            ArithmeticOperator::BitAnd => Ok(left & right),
            ArithmeticOperator::BitOr => Ok(left | right),
            ArithmeticOperator::BitXor => Ok(left ^ right),
            ArithmeticOperator::ShiftLeft => Ok(left.wrapping_shl((right as u32) & 63)),
            ArithmeticOperator::ShiftRightSigned => Ok(left.wrapping_shr((right as u32) & 63)),
            ArithmeticOperator::ShiftRightUnsigned => {
                Ok(((left as u64) >> ((right as u32) & 63)) as i64)
            }
        }
    }

    fn size_arithmetic(
        &self,
        operator: ArithmeticOperator,
        left: u32,
        right: u32,
        overflow: OverflowBehavior,
    ) -> Result<u32, InterpretError> {
        let checked = |value: Option<u32>| value.ok_or(InterpretError::ArithmeticTrap);
        let signed_checked = |value: Option<i32>| {
            value
                .map(|value| value as u32)
                .ok_or(InterpretError::ArithmeticTrap)
        };
        match operator {
            ArithmeticOperator::Add => match overflow {
                OverflowBehavior::Wrap => Ok(left.wrapping_add(right)),
                OverflowBehavior::Trap => checked(left.checked_add(right)),
            },
            ArithmeticOperator::Subtract => match overflow {
                OverflowBehavior::Wrap => Ok(left.wrapping_sub(right)),
                OverflowBehavior::Trap => checked(left.checked_sub(right)),
            },
            ArithmeticOperator::Multiply => match overflow {
                OverflowBehavior::Wrap => Ok(left.wrapping_mul(right)),
                OverflowBehavior::Trap => checked(left.checked_mul(right)),
            },
            ArithmeticOperator::DivideSigned => {
                signed_checked((left as i32).checked_div(right as i32))
            }
            ArithmeticOperator::DivideUnsigned => checked(left.checked_div(right)),
            ArithmeticOperator::RemainderSigned => {
                let left = left as i32;
                let right = right as i32;
                if right == 0 {
                    Err(InterpretError::ArithmeticTrap)
                } else if left == i32::MIN && right == -1 {
                    Ok(0)
                } else {
                    signed_checked(left.checked_rem(right))
                }
            }
            ArithmeticOperator::RemainderUnsigned => checked(left.checked_rem(right)),
            ArithmeticOperator::BitAnd => Ok(left & right),
            ArithmeticOperator::BitOr => Ok(left | right),
            ArithmeticOperator::BitXor => Ok(left ^ right),
            ArithmeticOperator::ShiftLeft => Ok(left.wrapping_shl(right & 31)),
            ArithmeticOperator::ShiftRightSigned | ArithmeticOperator::ShiftRightUnsigned => {
                if operator == ArithmeticOperator::ShiftRightSigned {
                    Ok((left as i32).wrapping_shr(right & 31) as u32)
                } else {
                    Ok(left.wrapping_shr(right & 31))
                }
            }
        }
    }

    fn float_arithmetic(
        &self,
        operator: ArithmeticOperator,
        left: u64,
        right: u64,
    ) -> Result<u64, InterpretError> {
        let left = f64::from_bits(left);
        let right = f64::from_bits(right);
        let value = match operator {
            ArithmeticOperator::Add => left + right,
            ArithmeticOperator::Subtract => left - right,
            ArithmeticOperator::Multiply => left * right,
            ArithmeticOperator::DivideSigned | ArithmeticOperator::DivideUnsigned => left / right,
            ArithmeticOperator::RemainderSigned | ArithmeticOperator::RemainderUnsigned => {
                left % right
            }
            _ => return Err(InterpretError::TypeMismatch("float arithmetic operator")),
        };
        Ok(value.to_bits())
    }

    fn compare(
        &self,
        operator: ComparisonOperator,
        left: &InterpreterValue,
        right: &InterpreterValue,
    ) -> Result<bool, InterpretError> {
        match (left, right) {
            (InterpreterValue::Int(left), InterpreterValue::Int(right)) => Ok(match operator {
                ComparisonOperator::Equal => left == right,
                ComparisonOperator::NotEqual => left != right,
                ComparisonOperator::LessSigned => left < right,
                ComparisonOperator::LessUnsigned => (*left as u64) < (*right as u64),
                ComparisonOperator::LessEqualSigned => left <= right,
                ComparisonOperator::LessEqualUnsigned => (*left as u64) <= (*right as u64),
                ComparisonOperator::GreaterSigned => left > right,
                ComparisonOperator::GreaterUnsigned => (*left as u64) > (*right as u64),
                ComparisonOperator::GreaterEqualSigned => left >= right,
                ComparisonOperator::GreaterEqualUnsigned => (*left as u64) >= (*right as u64),
            }),
            (InterpreterValue::Size(left), InterpreterValue::Size(right)) => {
                let left = self.size_from_u64(*left)? as u32;
                let right = self.size_from_u64(*right)? as u32;
                Ok(match operator {
                    ComparisonOperator::Equal => left == right,
                    ComparisonOperator::NotEqual => left != right,
                    ComparisonOperator::LessSigned => (left as i32) < (right as i32),
                    ComparisonOperator::LessUnsigned => left < right,
                    ComparisonOperator::LessEqualSigned => (left as i32) <= (right as i32),
                    ComparisonOperator::LessEqualUnsigned => left <= right,
                    ComparisonOperator::GreaterSigned => (left as i32) > (right as i32),
                    ComparisonOperator::GreaterUnsigned => left > right,
                    ComparisonOperator::GreaterEqualSigned => (left as i32) >= (right as i32),
                    ComparisonOperator::GreaterEqualUnsigned => left >= right,
                })
            }
            (InterpreterValue::Bool(left), InterpreterValue::Bool(right)) => match operator {
                ComparisonOperator::Equal => Ok(left == right),
                ComparisonOperator::NotEqual => Ok(left != right),
                _ => Err(InterpretError::TypeMismatch("ordered boolean comparison")),
            },
            (InterpreterValue::FloatBits(left), InterpreterValue::FloatBits(right)) => {
                let left = f64::from_bits(*left);
                let right = f64::from_bits(*right);
                Ok(match operator {
                    ComparisonOperator::Equal => left == right,
                    ComparisonOperator::NotEqual => left != right,
                    ComparisonOperator::LessSigned | ComparisonOperator::LessUnsigned => left < right,
                    ComparisonOperator::LessEqualSigned
                    | ComparisonOperator::LessEqualUnsigned => left <= right,
                    ComparisonOperator::GreaterSigned | ComparisonOperator::GreaterUnsigned => {
                        left > right
                    }
                    ComparisonOperator::GreaterEqualSigned
                    | ComparisonOperator::GreaterEqualUnsigned => left >= right,
                })
            }
            _ => Err(InterpretError::TypeMismatch("comparison operands")),
        }
    }

    fn runtime_extent(
        &self,
        extent: &Extent,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<RuntimeExtent, InterpretError> {
        Ok(match extent {
            Extent::FixedBytes(bytes) => RuntimeExtent::FixedBytes(*bytes),
            Extent::HeaderPrefix { minimum_bytes } => RuntimeExtent::HeaderPrefix(*minimum_bytes),
            Extent::Counted {
                header_bytes,
                count,
                capacity,
                stride_bytes,
            } => {
                let count = self.size_value(*count, values)?;
                let capacity = self.size_value(*capacity, values)?;
                if count > capacity {
                    return Err(InterpretError::CountExceedsCapacity { count, capacity });
                }
                RuntimeExtent::Counted {
                    header_bytes: *header_bytes,
                    count,
                    capacity,
                    stride_bytes: *stride_bytes,
                }
            }
        })
    }

    fn allocate(
        &mut self,
        layout: LayoutId,
        class: AllocationClass,
        extent: RuntimeExtent,
        immutable: bool,
    ) -> Result<u64, InterpretError> {
        let object = self.next_object;
        self.next_object = self
            .next_object
            .checked_add(1)
            .ok_or(InterpretError::ArithmeticTrap)?;
        self.objects.insert(
            object,
            Object {
                layout,
                storage: RuntimeStorage::Allocation(class),
                extent,
                references: 1,
                immutable,
                cells: BTreeMap::new(),
                absent_owned_slots: BTreeSet::new(),
            },
        );
        Ok(object)
    }

    fn allocate_static(
        &mut self,
        layout: LayoutId,
        extent: RuntimeExtent,
        bytes: &[u8],
    ) -> Result<u64, InterpretError> {
        let cells = self.materialize_static_cells(layout, &extent, bytes)?;
        let object = self.next_object;
        self.next_object = self
            .next_object
            .checked_add(1)
            .ok_or(InterpretError::ArithmeticTrap)?;
        self.objects.insert(
            object,
            Object {
                layout,
                storage: RuntimeStorage::StaticData,
                extent,
                references: 0,
                immutable: true,
                cells,
                absent_owned_slots: BTreeSet::new(),
            },
        );
        Ok(object)
    }

    fn materialize_static_cells(
        &self,
        layout: LayoutId,
        extent: &RuntimeExtent,
        bytes: &[u8],
    ) -> Result<BTreeMap<CellKey, InterpreterValue>, InterpretError> {
        let layouts = self.layouts.as_deref().ok_or(InterpretError::UnsupportedOperation("StaticData layouts"))?;
        let layout = layouts
            .resolve_nominal(layout)
            .map_err(|_| InterpretError::UnsupportedOperation("StaticData nominal layout"))?;
        let descriptor = layouts
            .get(layout)
            .ok_or(InterpretError::UnsupportedOperation("StaticData unknown layout"))?;
        if let RuntimeExtent::FixedBytes(expected) = extent {
            let crate::layout::LayoutSize::Fixed(actual) = descriptor.size() else {
                return Err(InterpretError::UnsupportedOperation("StaticData fixed dynamic layout"));
            };
            if *expected != u64::from(actual) || bytes.len() != actual as usize {
                return Err(InterpretError::UnsupportedOperation("StaticData fixed extent"));
            }
            return materialize_fixed_static_cells(layouts, layout, bytes);
        }
        let (element, count, data_offset, stride) = match (descriptor.kind(), extent) {
            (
                LayoutKind::RuntimeBuffer { element },
                RuntimeExtent::Counted {
                    header_bytes,
                    count,
                    stride_bytes,
                    ..
                },
            ) => (element, *count, u64::from(*header_bytes), *stride_bytes),
            (
                LayoutKind::PackedList { element, .. },
                RuntimeExtent::Counted { count, stride_bytes, .. },
            ) => {
                let HeaderLayout::PackedList { data_offset, .. } = descriptor.header() else {
                    return Err(InterpretError::UnsupportedOperation("StaticData packed-list header"));
                };
                (element, *count, u64::from(data_offset), *stride_bytes)
            }
            _ => return Err(InterpretError::UnsupportedOperation("StaticData layout")),
        };
        let element = layouts
            .resolve_nominal(*element)
            .map_err(|_| InterpretError::UnsupportedOperation("StaticData nominal element layout"))?;
        let element_descriptor = layouts
            .get(element)
            .ok_or(InterpretError::UnsupportedOperation("StaticData element layout"))?;
        let LayoutKind::Scalar(kind) = element_descriptor.kind() else {
            return Err(InterpretError::UnsupportedOperation("StaticData non-scalar element"));
        };
        if u64::from(kind.size()) != u64::from(stride) {
            return Err(InterpretError::UnsupportedOperation("StaticData element stride"));
        }
        let mut cells = BTreeMap::new();
        for index in 0_u64..count {
            let offset = data_offset
                .checked_add(index.checked_mul(u64::from(stride)).ok_or(InterpretError::ArithmeticTrap)?)
                .ok_or(InterpretError::ArithmeticTrap)?;
            cells.insert(CellKey::Element(index), static_scalar(bytes, offset, *kind)?);
        }
        Ok(cells)
    }

    fn load(
        &mut self,
        place: &Place,
        transfer: LoadTransfer,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<InterpreterValue, InterpretError> {
        let object = self.object_value(place.object(), values)?;
        if transfer == LoadTransfer::TakeLast {
            let Projection::Element { index } = place.projection() else { return Err(InterpretError::UnsupportedOperation("TakeLast non-element")); };
            let index = self.size_value(*index, values)?;
            let root = self.object(object)?;
            if root.immutable { return Err(InterpretError::ImmutableObject(object)); }
            if root.references != 1 { return Err(InterpretError::ReclaimWithAliases { object, references: root.references }); }
            let RuntimeExtent::Counted { count, .. } = root.extent else { return Err(InterpretError::TypeMismatch("TakeLast uncounted")); };
            if index.checked_add(1) != Some(count) { return Err(InterpretError::GuardTrap); }
            self.check_element(object, index)?;
            let root = self.object_mut(object)?;
            let key = CellKey::Element(index);
            let value = if matches!(place.value_type(), ValueType::Ref(_)) {
                root.cells.remove(&key).ok_or(InterpretError::UninitializedPlace)?
            } else {
                // Alloc uses fresh zero-filled linear storage. Scalar bytes
                // remain resident after shrinking count, unlike owning edges.
                // Unit still needs explicit semantic initialization.
                root.cells.get(&key).cloned().or_else(|| match place.value_type() {
                    ValueType::Int => Some(InterpreterValue::Int(0)),
                    ValueType::Float => Some(InterpreterValue::FloatBits(0)),
                    ValueType::Bool => Some(InterpreterValue::Bool(false)),
                    ValueType::Size => Some(InterpreterValue::Size(0)),
                    _ => None,
                }).ok_or(InterpretError::UninitializedPlace)?
            };
            if let RuntimeExtent::Counted { count, .. } = &mut root.extent { *count = index; }
            return Ok(value);
        }
        if place.value_type() == ValueType::Unit {
            if transfer != LoadTransfer::CopyScalar { return Err(InterpretError::TypeMismatch("Unit load")); }
            let key = self.unit_cell(object, place, values, false)?;
            return match self.object(object)?.cells.get(&key) {
                Some(InterpreterValue::Unit) => Ok(InterpreterValue::Unit),
                Some(_) => Err(InterpretError::TypeMismatch("Unit storage")),
                None => Err(InterpretError::UninitializedPlace),
            };
        }
        if transfer != LoadTransfer::CopyScalar {
            // A reference cell: a shared clone retains the child, a unique
            // move takes the cell (its token leaves the container).
            let key = match place.projection() {
                Projection::Field { index } => CellKey::Field(*index),
                Projection::Element { index } => {
                    let index = self.size_value(*index, values)?;
                    self.check_element(object, index)?;
                    CellKey::Element(index)
                }
                _ => return Err(InterpretError::UnsupportedOperation("reference-bearing Load place")),
            };
            return match transfer {
                LoadTransfer::ClonePresent => {
                    let root = self.object(object)?;
                    if root.absent_owned_slots.contains(&key) {
                        return Err(InterpretError::GuardTrap);
                    }
                    let value = root.cells.get(&key).cloned()
                        .ok_or(InterpretError::UninitializedPlace)?;
                    let InterpreterValue::ObjectRef(child) = value else {
                        return Err(InterpretError::TypeMismatch("ClonePresent reference cell"));
                    };
                    self.retain(child)?;
                    Ok(InterpreterValue::ObjectRef(child))
                }
                LoadTransfer::CloneShared => {
                    let value = self.object(object)?.cells.get(&key).cloned().ok_or(InterpretError::UninitializedPlace)?;
                    if let InterpreterValue::ObjectRef(child) = value {
                        self.retain(child)?;
                    }
                    Ok(value)
                }
                LoadTransfer::MoveUnique => {
                    if self.object(object)?.immutable {
                        return Err(InterpretError::ImmutableObject(object));
                    }
                    self.object_mut(object)?.cells.remove(&key).ok_or(InterpretError::UninitializedPlace)
                }
                LoadTransfer::CopyScalar | LoadTransfer::TakeLast => unreachable!("handled below"),
            };
        }
        match place.projection() {
            Projection::Header(field) => {
                let object = self.object(object)?;
                let RuntimeExtent::Counted {
                    count, capacity, ..
                } = &object.extent
                else {
                    return Err(InterpretError::TypeMismatch("header on uncounted object"));
                };
                Ok(InterpreterValue::Size(match field {
                    HeaderField::Length => *count,
                    HeaderField::Capacity => *capacity,
                }))
            }
            Projection::Field { index } => self
                .object(object)?
                .cells
                .get(&CellKey::Field(*index))
                .cloned()
                .ok_or(InterpretError::UninitializedPlace),
            Projection::Element { index } => {
                let index = self.size_value(*index, values)?;
                self.check_element(object, index)?;
                self.object(object)?
                    .cells
                    .get(&CellKey::Element(index))
                    .cloned()
                    .ok_or(InterpretError::UninitializedPlace)
            }
            Projection::Slice { .. } => Err(InterpretError::UnsupportedOperation("Slice load")),
        }
    }

    fn store(
        &mut self,
        place: &Place,
        source: ValueId,
        transfer: StoreTransfer,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        let object = self.object_value(place.object(), values)?;
        let value = self.value(source, values)?.clone();
        if self.object(object)?.immutable {
            return Err(InterpretError::ImmutableObject(object));
        }
        if place.value_type() == ValueType::Unit {
            if value != InterpreterValue::Unit || transfer != StoreTransfer::CopyScalar {
                return Err(InterpretError::TypeMismatch("Unit store"));
            }
            let key = self.unit_cell(object, place, values, true)?;
            self.object_mut(object)?.cells.insert(key, InterpreterValue::Unit);
            return Ok(());
        }
        // A unique move stores the token itself; a shared store adds one.
        if let (StoreTransfer::CloneShared, InterpreterValue::ObjectRef(child)) = (transfer, &value) {
            self.retain(*child)?;
        }
        let key = match place.projection() {
            Projection::Header(_) => {
                return Err(InterpretError::UnsupportedOperation("direct header Store"));
            }
            Projection::Field { index } => CellKey::Field(*index),
            Projection::Element { index } => {
                // A store may initialize a slot past the logical count, up to
                // the capacity; the count follows through `UpdateCount`.
                let index = self.size_value(*index, values)?;
                self.check_range(object, index, 1, true)?;
                CellKey::Element(index)
            }
            Projection::Slice { .. } => {
                return Err(InterpretError::UnsupportedOperation("Slice store"));
            }
        };
        let root = self.object_mut(object)?;
        root.absent_owned_slots.remove(&key);
        root.cells.insert(key, value);
        Ok(())
    }

    /// Oracle-owned projection checks; no checker or backend safety helpers.
    fn unit_cell(&self, object: u64, place: &Place, values: &BTreeMap<ValueId, InterpreterValue>, write: bool) -> Result<CellKey, InterpretError> {
        let root = self.object(object)?;
        let layouts = self.layouts.as_deref().ok_or(InterpretError::TypeMismatch("Unit layouts"))?;
        let physical = layouts.resolve_nominal(root.layout).map_err(|_| InterpretError::TypeMismatch("Unit layout"))?;
        let layout = layouts.get(physical).ok_or(InterpretError::TypeMismatch("Unit layout"))?;
        match place.projection() {
            Projection::Field { index } => {
                let field = layout.fields().get(*index as usize).ok_or(InterpretError::TypeMismatch("Unit field bounds"))?;
                let RuntimeExtent::FixedBytes(bytes) = root.extent else { return Err(InterpretError::TypeMismatch("Unit field extent")) };
                if field.kind() != crate::layout::FieldKind::Scalar(ScalarKind::Unit) || u64::from(field.offset()) + 4 > bytes {
                    return Err(InterpretError::TypeMismatch("Unit field layout"));
                }
                Ok(CellKey::Field(*index))
            }
            Projection::Element { index } => {
                let element = layout.kind().packed_element_layout().and_then(|id| layouts.get(id));
                if !element.is_some_and(|d| d.kind() == &LayoutKind::Scalar(ScalarKind::Unit)) {
                    return Err(InterpretError::TypeMismatch("Unit element layout"));
                }
                let index = self.size_value(*index, values)?;
                if write { self.check_range(object, index, 1, true)?; } else { self.check_element(object, index)?; }
                Ok(CellKey::Element(index))
            }
            _ => Err(InterpretError::TypeMismatch("Unit projection")),
        }
    }

    fn update_count(&mut self, object: u64, count: u64)
 -> Result<(), InterpretError> {
        if self.object(object)?.immutable {
            return Err(InterpretError::ImmutableObject(object));
        }
        let RuntimeExtent::Counted {
            count: current,
            capacity,
            ..
        } = &mut self.object_mut(object)?.extent
        else {
            return Err(InterpretError::TypeMismatch("UpdateCount on uncounted object"));
        };
        if count > *capacity {
            return Err(InterpretError::CountExceedsCapacity {
                count,
                capacity: *capacity,
            });
        }
        *current = count;
        Ok(())
    }

    fn check_element(&self, object: u64, index: u64) -> Result<(), InterpretError> {
        let RuntimeExtent::Counted { count, .. } = &self.object(object)?.extent else {
            return Err(InterpretError::TypeMismatch("element on uncounted object"));
        };
        if index >= *count {
            return Err(InterpretError::IndexOutOfBounds {
                index,
                length: *count,
            });
        }
        Ok(())
    }

    fn retain(&mut self, object: u64) -> Result<(), InterpretError> {
        let object_state = self.object_mut(object)?;
        match object_state.storage {
            RuntimeStorage::StaticData => return Ok(()),
            RuntimeStorage::Allocation(AllocationClass::ReferenceCounted) => {}
            RuntimeStorage::Allocation(_) => {
                return Err(InterpretError::ObjectNotReferenceCounted(object));
            }
        }
        object_state.references = u32::try_from(object_state.references)
            .ok()
            .and_then(|references| references.checked_add(1))
            .map(u64::from)
            .ok_or(InterpretError::ReferenceCountOverflow(object))?;
        Ok(())
    }

    fn release(&mut self, object: u64) -> Result<(), InterpretError> {
        let mut pending = vec![object];
        while let Some(object) = pending.pop() {
            let remaining = {
                let object_state = self.object_mut(object)?;
                match object_state.storage {
                    RuntimeStorage::StaticData => continue,
                    RuntimeStorage::Allocation(AllocationClass::ReferenceCounted) => {}
                    RuntimeStorage::Allocation(_) => {
                        return Err(InterpretError::ObjectNotReferenceCounted(object));
                    }
                }
                object_state.references = u32::try_from(object_state.references)
                    .ok()
                    .and_then(|references| references.checked_sub(1))
                    .map(u64::from)
                    .ok_or(InterpretError::ReferenceCountUnderflow(object))?;
                object_state.references
            };
            if remaining == 0 {
                let cells = self.objects.remove(&object).map(|object| object.cells).unwrap_or_default();
                // Reverse insertion preserves the previous depth-first cell
                // order without using one Rust stack frame per nested value.
                // Each edge transfers one owned token, including repeated
                // edges to a shared child; do not deduplicate the worklist.
                for cell in cells.into_values().rev() {
                    match cell {
                        InterpreterValue::ObjectRef(child) => pending.push(child),
                        InterpreterValue::OpaqueOwner(owner) => self.release_owner(owner)?,
                        InterpreterValue::OpaqueOwnerList(list) => self.release_owner_list(list)?,
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    fn reclaim(&mut self, object: u64) -> Result<(), InterpretError> {
        let object_state = self.object(object)?;
        match object_state.storage {
            RuntimeStorage::StaticData => return Err(InterpretError::ImmutableObject(object)),
            RuntimeStorage::Allocation(AllocationClass::ReferenceCounted) => {}
            RuntimeStorage::Allocation(_) => {
                return Err(InterpretError::ObjectNotReferenceCounted(object));
            }
        }
        if object_state.references != 1 {
            return Err(InterpretError::ReclaimWithAliases {
                object,
                references: object_state.references,
            });
        }
        self.objects.remove(&object);
        Ok(())
    }

    fn take_call_arguments(
        &mut self,
        arguments: &[CallArgument],
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<Vec<InterpreterValue>, InterpretError> {
        arguments
            .iter()
            .map(|argument| match argument {
                CallArgument::Value(value) => Ok(self.value(*value, values)?.clone()),
                CallArgument::Move(value) => values
                    .remove(value)
                    .ok_or(InterpretError::MissingValue(*value)),
            })
            .collect()
    }

    fn bind_results(
        &self,
        destinations: &[ValueId],
        returned: Vec<InterpreterValue>,
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        if destinations.len() != returned.len() {
            return Err(InterpretError::ArityMismatch {
                expected: destinations.len(),
                actual: returned.len(),
            });
        }
        for (destination, value) in destinations.iter().zip(returned) {
            values.insert(*destination, value);
        }
        Ok(())
    }

    fn bind_parameters(
        &self,
        block: &BasicBlock,
        arguments: Vec<InterpreterValue>,
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        if block.parameters.len() != arguments.len() {
            return Err(InterpretError::ArityMismatch {
                expected: block.parameters.len(),
                actual: arguments.len(),
            });
        }
        for (parameter, argument) in block.parameters.iter().zip(arguments) {
            if parameter.value_type == crate::canonical_safewir::ValueType::Size {
                match argument {
                    InterpreterValue::Size(value) => {
                        self.size_from_u64(value)?;
                    }
                    _ => return Err(InterpretError::TypeMismatch("Size")),
                }
            }
            values.insert(parameter.value, argument);
        }
        Ok(())
    }

    fn read_values(
        &self,
        ids: &[ValueId],
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<Vec<InterpreterValue>, InterpretError> {
        ids.iter().map(|id| Ok(self.value(*id, values)?.clone())).collect()
    }

    fn function(&self, id: FunctionId) -> Result<&Function, InterpretError> {
        self.module
            .functions()
            .iter()
            .find(|function| function.id == id)
            .ok_or(InterpretError::FunctionNotFound(id))
    }

    fn block<'b>(
        &self,
        function: &'b Function,
        id: BlockId,
    ) -> Result<&'b BasicBlock, InterpretError> {
        // Blocks are normally stored at their id; confirm before trusting it,
        // and fall back to the search for any other order.
        usize::try_from(id.get()).ok()
            .and_then(|index| function.blocks.get(index))
            .filter(|block| block.id == id)
            .or_else(|| function.blocks.iter().find(|block| block.id == id))
            .ok_or(InterpretError::BlockNotFound(id))
    }

    fn value<'b>(
        &self,
        id: ValueId,
        values: &'b BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<&'b InterpreterValue, InterpretError> {
        values.get(&id).ok_or(InterpretError::MissingValue(id))
    }

    fn bool_value(
        &self,
        id: ValueId,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<bool, InterpretError> {
        match self.value(id, values)? {
            InterpreterValue::Bool(value) => Ok(*value),
            _ => Err(InterpretError::TypeMismatch("Bool")),
        }
    }

    fn int_value(
        &self,
        id: ValueId,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<i64, InterpretError> {
        match self.value(id, values)? {
            InterpreterValue::Int(value) => Ok(*value),
            _ => Err(InterpretError::TypeMismatch("Int")),
        }
    }

    fn size_value(
        &self,
        id: ValueId,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<u64, InterpretError> {
        match self.value(id, values)? {
            InterpreterValue::Size(value) => self.size_from_u64(*value),
            _ => Err(InterpretError::TypeMismatch("Size")),
        }
    }

    fn size_from_u64(&self, value: u64) -> Result<u64, InterpretError> {
        if value > SIZE_MAX {
            return Err(InterpretError::ArithmeticTrap);
        }
        Ok(value)
    }

    fn object_value(
        &self,
        id: ValueId,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<u64, InterpretError> {
        match self.value(id, values)? {
            InterpreterValue::ObjectRef(value) => Ok(*value),
            _ => Err(InterpretError::TypeMismatch("Ref")),
        }
    }

    fn function_value(
        &self,
        id: ValueId,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<FunctionId, InterpretError> {
        match self.value(id, values)? {
            InterpreterValue::Function(value) => Ok(*value),
            _ => Err(InterpretError::TypeMismatch("Callable")),
        }
    }

    fn object(&self, id: u64) -> Result<&Object, InterpretError> {
        self.objects.get(&id).ok_or(InterpretError::ObjectNotFound(id))
    }

    fn object_mut(&mut self, id: u64) -> Result<&mut Object, InterpretError> {
        self.objects
            .get_mut(&id)
            .ok_or(InterpretError::ObjectNotFound(id))
    }

    fn bump_step(&mut self) -> Result<(), InterpretError> {
        self.steps = self
            .steps
            .checked_add(1)
            .ok_or(InterpretError::StepLimitExceeded)?;
        if self.steps > self.config.max_steps {
            return Err(InterpretError::StepLimitExceeded);
        }
        Ok(())
    }
}

fn materialize_fixed_static_cells(
    layouts: &crate::layout::LayoutInterner,
    layout: LayoutId,
    bytes: &[u8],
) -> Result<BTreeMap<CellKey, InterpreterValue>, InterpretError> {
    let layout = layouts
        .resolve_nominal(layout)
        .map_err(|_| InterpretError::UnsupportedOperation("StaticData nominal fixed layout"))?;
    let descriptor = layouts
        .get(layout)
        .ok_or(InterpretError::UnsupportedOperation("StaticData fixed layout"))?;
    let crate::layout::LayoutSize::Fixed(size) = descriptor.size() else {
        return Err(InterpretError::UnsupportedOperation("StaticData fixed layout size"));
    };
    if bytes.len() != size as usize {
        return Err(InterpretError::UnsupportedOperation("StaticData fixed payload width"));
    }
    match descriptor.kind() {
        LayoutKind::Scalar(kind) => Ok(BTreeMap::from([(
            CellKey::Field(0),
            static_scalar(bytes, 0, *kind)?,
        )])),
        LayoutKind::Tuple { .. } | LayoutKind::PackedRecord { .. } => descriptor
            .fields()
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let start = field.offset() as usize;
                let value = match field.kind() {
                    crate::layout::FieldKind::Scalar(kind) => {
                        static_scalar(bytes, start as u64, kind)?
                    }
                    crate::layout::FieldKind::Inline(child) => {
                        let child = layouts.resolve_nominal(child).map_err(|_| {
                            InterpretError::UnsupportedOperation("StaticData nominal inline layout")
                        })?;
                        let child_descriptor = layouts.get(child).ok_or(
                            InterpretError::UnsupportedOperation("StaticData inline layout"),
                        )?;
                        let crate::layout::LayoutSize::Fixed(child_size) = child_descriptor.size() else {
                            return Err(InterpretError::UnsupportedOperation("StaticData inline size"));
                        };
                        let end = start
                            .checked_add(child_size as usize)
                            .ok_or(InterpretError::ArithmeticTrap)?;
                        static_fixed_value(
                            layouts,
                            child,
                            bytes.get(start..end).ok_or(InterpretError::UnsupportedOperation(
                                "StaticData inline bounds",
                            ))?,
                        )?
                    }
                    crate::layout::FieldKind::Pointer { .. } => {
                        return Err(InterpretError::UnsupportedOperation(
                            "StaticData pointer field",
                        ));
                    }
                };
                Ok((CellKey::Field(index as u32), value))
            })
            .collect(),
        _ => Err(InterpretError::UnsupportedOperation("StaticData fixed layout kind")),
    }
}

fn static_fixed_value(
    layouts: &crate::layout::LayoutInterner,
    layout: LayoutId,
    bytes: &[u8],
) -> Result<InterpreterValue, InterpretError> {
    let layout = layouts
        .resolve_nominal(layout)
        .map_err(|_| InterpretError::UnsupportedOperation("StaticData nominal inline value"))?;
    let descriptor = layouts
        .get(layout)
        .ok_or(InterpretError::UnsupportedOperation("StaticData inline value layout"))?;
    match descriptor.kind() {
        LayoutKind::Scalar(kind) => static_scalar(bytes, 0, *kind),
        LayoutKind::Tuple { .. } | LayoutKind::PackedRecord { .. } => {
            let cells = materialize_fixed_static_cells(layouts, layout, bytes)?;
            let fields = (0..descriptor.fields().len())
                .map(|index| {
                    cells
                        .get(&CellKey::Field(index as u32))
                        .cloned()
                        .ok_or(InterpretError::UninitializedPlace)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(InterpreterValue::InlineAggregate { layout, fields })
        }
        _ => Err(InterpretError::UnsupportedOperation("StaticData inline value kind")),
    }
}

fn static_scalar(
    bytes: &[u8],
    offset: u64,
    kind: ScalarKind,
) -> Result<InterpreterValue, InterpretError> {
    let offset = usize::try_from(offset).map_err(|_| InterpretError::ArithmeticTrap)?;
    let end = offset
        .checked_add(kind.size() as usize)
        .ok_or(InterpretError::ArithmeticTrap)?;
    let bytes = bytes
        .get(offset..end)
        .ok_or(InterpretError::UnsupportedOperation("StaticData scalar bounds"))?;
    let size = |width: usize| bytes.get(..width).ok_or(InterpretError::UnsupportedOperation("StaticData scalar width"));
    match kind {
        ScalarKind::Unit => Ok(InterpreterValue::Unit),
        ScalarKind::Bool => match bytes {
            [0] => Ok(InterpreterValue::Bool(false)),
            [1] => Ok(InterpreterValue::Bool(true)),
            _ => Err(InterpretError::UnsupportedOperation("StaticData bool encoding")),
        },
        ScalarKind::Int | ScalarKind::Duration => Ok(InterpreterValue::Int(i64::from_le_bytes(
            size(8)?.try_into().expect("validated scalar width"),
        ))),
        ScalarKind::Float => Ok(InterpreterValue::FloatBits(u64::from_le_bytes(
            size(8)?.try_into().expect("validated scalar width"),
        ))),
        ScalarKind::U32 | ScalarKind::Tag32 => Ok(InterpreterValue::Size(u64::from(u32::from_le_bytes(
            size(4)?.try_into().expect("validated scalar width"),
        )))),
        ScalarKind::Tag16 => Ok(InterpreterValue::Size(u64::from(u16::from_le_bytes(
            size(2)?.try_into().expect("validated scalar width"),
        )))),
        ScalarKind::Tag8 | ScalarKind::RawByte => Ok(InterpreterValue::Size(u64::from(bytes[0]))),
        ScalarKind::BoolValueSlot => match u64::from_le_bytes(size(8)?.try_into().expect("validated scalar width")) {
            0 => Ok(InterpreterValue::Bool(false)),
            1 => Ok(InterpreterValue::Bool(true)),
            _ => Err(InterpretError::UnsupportedOperation("StaticData bool slot encoding")),
        },
        ScalarKind::V128 => Ok(InterpreterValue::V128(
            size(16)?.try_into().expect("validated scalar width"),
        )),
        // There are no static closures: a callable lane is only ever written
        // by a `FunctionValue` at run time.
        ScalarKind::Callable => Err(InterpretError::UnsupportedOperation("StaticData callable lane")),
    }
}

#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
pub mod self_checks {
    use super::*;
    use crate::canonical_safewir::{
        check_initial_with_test_witness, validate_final_for_test, BasicBlock,
        AccessMode, BlockParameter, CallableContract, CheckedSourceIdentity, FunctionSignature,
        FunctionSignatureId, LoweredModuleParts, Operation,
        ParameterContract, ParameterOwnership, Place, Projection, ResultContract,
        Revision, ValueType,
    };
    use crate::layout::{LayoutBundle, LayoutInterner, RcHeader, ScalarKind};

    const FUNCTION: FunctionId = FunctionId::new(0);
    const SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(0);
    const ENTRY: BlockId = BlockId::new(0);

    fn validated(
        layouts: LayoutBundle,
        parameters: Vec<BlockParameter>,
        value_types: BTreeMap<ValueId, ValueType>,
        operations: Vec<Operation>,
        terminator: Terminator,
        result_types: Vec<ValueType>,
    ) -> ValidatedSafeWir {
        let parameter_types = parameters
            .iter()
            .map(|parameter| parameter.value_type)
            .collect::<Vec<_>>();
        let parameter_contracts = parameter_types
            .iter()
            .copied()
            .map(|value_type| ParameterContract {
                value_type,
                ownership: ParameterOwnership::Value,
            })
            .collect();
        let result_contracts = result_types
            .iter()
            .copied()
            .map(ResultContract::Value)
            .collect();
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([31; 32]),
            layouts,
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: parameter_types,
                results: result_types,
                contract: CallableContract {
                    parameters: parameter_contracts,
                    results: result_contracts,
                    parameter_effects: BTreeMap::new(),
                },
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types,
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters,
                    operations,
                    terminator,
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        let checked = check_initial_with_test_witness(candidate).expect("initial validation");
        validate_final_for_test(checked).expect("final validation")
    }

    fn empty_layouts() -> LayoutBundle {
        LayoutBundle::from_interner(&LayoutInterner::new(), [])
            .expect("empty canonical layout bundle")
    }

    fn packed_u32_layout() -> (LayoutBundle, LayoutId) {
        let mut layouts = LayoutInterner::new();
        let element = layouts
            .intern_scalar(ScalarKind::U32)
            .expect("u32 layout");
        let list = layouts
            .intern_packed_list(element, RcHeader::Required)
            .expect("packed u32 list layout");
        let bundle = LayoutBundle::from_interner(&layouts, [list]).expect("layout bundle");
        (bundle, list)
    }

    fn size_arithmetic_module(
        operator: ArithmeticOperator,
        left_value: u64,
        right_value: u64,
        overflow: OverflowBehavior,
    ) -> ValidatedSafeWir {
        let left = ValueId::new(0);
        let right = ValueId::new(1);
        let result = ValueId::new(2);
        validated(
            empty_layouts(),
            Vec::new(),
            [
                (left, ValueType::Size),
                (right, ValueType::Size),
                (result, ValueType::Size),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstSize {
                    destination: left,
                    value: left_value,
                },
                Operation::ConstSize {
                    destination: right,
                    value: right_value,
                },
                Operation::Arithmetic {
                    destination: result,
                    operator,
                    left,
                    right,
                    overflow,
                },
            ],
            Terminator::Return {
                values: vec![result],
            },
            vec![ValueType::Size],
        )
    }

    fn int_arithmetic_module(
        operator: ArithmeticOperator,
        left_value: i64,
        right_value: i64,
    ) -> ValidatedSafeWir {
        let left = ValueId::new(0);
        let right = ValueId::new(1);
        let result = ValueId::new(2);
        validated(
            empty_layouts(),
            Vec::new(),
            [
                (left, ValueType::Int),
                (right, ValueType::Int),
                (result, ValueType::Int),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstInt {
                    destination: left,
                    value: left_value,
                },
                Operation::ConstInt {
                    destination: right,
                    value: right_value,
                },
                Operation::Arithmetic {
                    destination: result,
                    operator,
                    left,
                    right,
                    overflow: OverflowBehavior::Trap,
                },
            ],
            Terminator::Return {
                values: vec![result],
            },
            vec![ValueType::Int],
        )
    }

    pub fn signed_remainder_by_zero_traps() {
        let size_module = size_arithmetic_module(
            ArithmeticOperator::RemainderSigned,
            1,
            0,
            OverflowBehavior::Trap,
        );
        assert_eq!(
            interpret(&size_module, FUNCTION, Vec::new()),
            Err(InterpretError::ArithmeticTrap)
        );

        let int_module = int_arithmetic_module(ArithmeticOperator::RemainderSigned, 1, 0);
        assert_eq!(
            interpret(&int_module, FUNCTION, Vec::new()),
            Err(InterpretError::ArithmeticTrap)
        );
    }

    pub fn validated_size_values_reject_values_wider_than_i32() {
        let input = ValueId::new(0);
        let zero = ValueId::new(1);
        let result = ValueId::new(2);
        let module = validated(
            empty_layouts(),
            vec![BlockParameter {
                value: input,
                value_type: ValueType::Size,
                ownership: None,
            }],
            [
                (input, ValueType::Size),
                (zero, ValueType::Size),
                (result, ValueType::Size),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstSize {
                    destination: zero,
                    value: 0,
                },
                Operation::Arithmetic {
                    destination: result,
                    operator: ArithmeticOperator::Add,
                    left: input,
                    right: zero,
                    overflow: OverflowBehavior::Wrap,
                },
            ],
            Terminator::Return {
                values: vec![result],
            },
            vec![ValueType::Size],
        );

        assert_eq!(
            interpret(
                &module,
                FUNCTION,
                vec![InterpreterValue::Size(SIZE_MAX + 1)],
            ),
            Err(InterpretError::ArithmeticTrap)
        );
    }

    pub fn reference_counts_trap_at_i32_maximum() {
        let (layouts, layout) = packed_u32_layout();
        let module = validated(
            layouts,
            Vec::new(),
            BTreeMap::new(),
            Vec::new(),
            Terminator::Return { values: Vec::new() },
            Vec::new(),
        );
        let mut engine = Engine::new(module.module(), InterpreterConfig::default());
        let object = engine
            .allocate(
                layout,
                AllocationClass::ReferenceCounted,
                RuntimeExtent::FixedBytes(0),
                false,
            )
            .expect("allocate reference-counted object");
        engine.object_mut(object).expect("object").references = SIZE_MAX;

        assert_eq!(
            engine.retain(object),
            Err(InterpretError::ReferenceCountOverflow(object))
        );
    }

    pub fn validated_scalar_arithmetic_executes() {
        let left = ValueId::new(0);
        let right = ValueId::new(1);
        let result = ValueId::new(2);
        let module = validated(
            empty_layouts(),
            Vec::new(),
            [
                (left, ValueType::Int),
                (right, ValueType::Int),
                (result, ValueType::Int),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstInt {
                    destination: left,
                    value: 40,
                },
                Operation::ConstInt {
                    destination: right,
                    value: 2,
                },
                Operation::Arithmetic {
                    destination: result,
                    operator: ArithmeticOperator::Add,
                    left,
                    right,
                    overflow: OverflowBehavior::Wrap,
                },
            ],
            Terminator::Return {
                values: vec![result],
            },
            vec![ValueType::Int],
        );

        assert_eq!(
            interpret(&module, FUNCTION, Vec::new()),
            Ok(vec![InterpreterValue::Int(42)])
        );
    }

    pub fn validated_false_guard_traps() {
        let index = ValueId::new(0);
        let length = ValueId::new(1);
        let condition = ValueId::new(2);
        let module = validated(
            empty_layouts(),
            vec![
                BlockParameter {
                    value: index,
                    value_type: ValueType::Size,
                    ownership: None,
                },
                BlockParameter {
                    value: length,
                    value_type: ValueType::Size,
                    ownership: None,
                },
            ],
            [
                (index, ValueType::Size),
                (length, ValueType::Size),
                (condition, ValueType::Bool),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::Compare {
                    destination: condition,
                    operator: ComparisonOperator::LessUnsigned,
                    left: index,
                    right: length,
                },
                Operation::Guard {
                    condition,
                    kind: crate::canonical_safewir::GuardKind::Bounds,
                },
            ],
            Terminator::Return { values: Vec::new() },
            Vec::new(),
        );

        assert_eq!(
            interpret(
                &module,
                FUNCTION,
                vec![InterpreterValue::Size(2), InterpreterValue::Size(1)],
            ),
            Err(InterpretError::GuardTrap)
        );
    }


    pub fn validated_counted_element_store_and_load_execute() {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let object = ValueId::new(2);
        let index = ValueId::new(3);
        let stored = ValueId::new(4);
        let loaded = ValueId::new(5);
        let within_length = ValueId::new(6);
        let within_capacity = ValueId::new(7);
        let (layouts, layout) = packed_u32_layout();
        let module = validated(
            layouts,
            Vec::new(),
            [
                (count, ValueType::Size),
                (capacity, ValueType::Size),
                (object, ValueType::Ref(layout)),
                (index, ValueType::Size),
                (stored, ValueType::Size),
                (loaded, ValueType::Size),
                (within_length, ValueType::Bool),
                (within_capacity, ValueType::Bool),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstSize {
                    destination: count,
                    value: 1,
                },
                Operation::ConstSize {
                    destination: capacity,
                    value: 3,
                },
                Operation::Alloc {
                    destination: object,
                    layout,
                    extent: Extent::Counted {
                        header_bytes: 8,
                        count,
                        capacity,
                        stride_bytes: 4,
                    },
                    class: AllocationClass::ReferenceCounted,
                },
                Operation::ConstSize {
                    destination: index,
                    value: 0,
                },
                Operation::ConstSize {
                    destination: stored,
                    value: 7,
                },
                Operation::Compare {
                    destination: within_length,
                    operator: ComparisonOperator::LessUnsigned,
                    left: index,
                    right: count,
                },
                Operation::Guard {
                    condition: within_length,
                    kind: crate::canonical_safewir::GuardKind::Bounds,
                },
                Operation::Compare {
                    destination: within_capacity,
                    operator: ComparisonOperator::LessUnsigned,
                    left: index,
                    right: capacity,
                },
                Operation::Guard {
                    condition: within_capacity,
                    kind: crate::canonical_safewir::GuardKind::Bounds,
                },
                Operation::Store {
                    place: Place::projected(
                        object,
                        Projection::Element { index },
                        ValueType::Size,
                        AccessMode::Write,
                    ),
                    value: stored,
                    transfer: StoreTransfer::CopyScalar,
                },
                Operation::Load {
                    destination: loaded,
                    place: Place::projected(
                        object,
                        Projection::Element { index },
                        ValueType::Size,
                        AccessMode::Read,
                    ),
                    transfer: LoadTransfer::CopyScalar,
                },
                Operation::DropRef { object },
            ],
            Terminator::Return {
                values: vec![loaded],
            },
            vec![ValueType::Size],
        );

        assert_eq!(
            interpret(&module, FUNCTION, Vec::new()),
            Ok(vec![InterpreterValue::Size(7)])
        );
    }

    pub fn validated_clone_and_two_drops_execute() {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let object = ValueId::new(2);
        let clone = ValueId::new(3);
        let (layouts, layout) = packed_u32_layout();
        let module = validated(
            layouts,
            Vec::new(),
            [
                (count, ValueType::Size),
                (capacity, ValueType::Size),
                (object, ValueType::Ref(layout)),
                (clone, ValueType::Ref(layout)),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstSize {
                    destination: count,
                    value: 0,
                },
                Operation::ConstSize {
                    destination: capacity,
                    value: 1,
                },
                Operation::Alloc {
                    destination: object,
                    layout,
                    extent: Extent::Counted {
                        header_bytes: 8,
                        count,
                        capacity,
                        stride_bytes: 4,
                    },
                    class: AllocationClass::ReferenceCounted,
                },
                Operation::CloneRef {
                    destination: clone,
                    source: object,
                },
                Operation::DropRef { object: clone },
                Operation::DropRef { object },
            ],
            Terminator::Return { values: Vec::new() },
            Vec::new(),
        );

        assert_eq!(interpret(&module, FUNCTION, Vec::new()), Ok(Vec::new()));
    }

    pub fn validated_direct_call_executes_callee() {
        let callee_id = FunctionId::new(1);
        let callee_signature = FunctionSignatureId::new(1);
        let caller_result = ValueId::new(0);
        let callee_result = ValueId::new(0);
        let scalar_contract = || CallableContract {
            parameters: Vec::new(),
            results: vec![ResultContract::Value(ValueType::Int)],
            parameter_effects: BTreeMap::new(),
        };
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([32; 32]),
            layouts: empty_layouts(),
            signatures: vec![
                FunctionSignature {
                    id: SIGNATURE,
                    parameters: Vec::new(),
                    results: vec![ValueType::Int],
                    contract: scalar_contract(),
                },
                FunctionSignature {
                    id: callee_signature,
                    parameters: Vec::new(),
                    results: vec![ValueType::Int],
                    contract: scalar_contract(),
                },
            ],
            imports: Vec::new(),
            functions: vec![
                Function {
                    id: FUNCTION,
                    signature: SIGNATURE,
                    entry: ENTRY,
                    value_types: [(caller_result, ValueType::Int)].into_iter().collect(),
                    blocks: vec![BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: vec![Operation::Call {
                            destinations: vec![caller_result],
                            callee: callee_id,
                            arguments: Vec::new(),
                        }],
                        terminator: Terminator::Return {
                            values: vec![caller_result],
                        },
                    }],
                },
                Function {
                    id: callee_id,
                    signature: callee_signature,
                    entry: ENTRY,
                    value_types: [(callee_result, ValueType::Int)].into_iter().collect(),
                    blocks: vec![BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstInt {
                            destination: callee_result,
                            value: 42,
                        }],
                        terminator: Terminator::Return {
                            values: vec![callee_result],
                        },
                    }],
                },
            ],
            proper_tail_sites: Vec::new(),
        });
        let checked = check_initial_with_test_witness(candidate).expect("initial validation");
        let module = validate_final_for_test(checked).expect("final validation");

        assert_eq!(
            interpret(&module, FUNCTION, Vec::new()),
            Ok(vec![InterpreterValue::Int(42)])
        );
    }

}

#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
#[path = "canonical_interpreter/capability_tests.rs"]
pub mod capability_self_checks;
