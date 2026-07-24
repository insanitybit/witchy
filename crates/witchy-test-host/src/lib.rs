//! Backend-neutral fixture host dispatch.
//!
//! Interpreter, Wasmtime, and browser adapters translate their values or ABI
//! calls into [`HostRequest`] and translate [`HostResponse`] back. Matching,
//! state, failures, and transcript semantics remain in `witchy-testkit`.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use witchy_testkit::{
    FixtureCall, FixtureErrorCode, FixtureExecResponse, FixtureFailure,
    FixtureFamily, FixtureFetchRequest, FixtureFetchResponse, FixtureHandle,
    FixturePlan, FixtureSession, PlanValidationError, SourceLocation, TestResult,
    TestTranscript, parse_fixture_plan, parse_unique_json,
};

pub const FIXTURE_HOST_WIRE_VERSION: u32 = 1;

/// Adapter-visible handle. Arbitrary values grant nothing because every use is
/// checked against a private registry and the expected capability family.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HostHandle(u64);

impl HostHandle {
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn into_raw(self) -> u64 {
        self.0
    }
}

impl Serialize for HostHandle {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for HostHandle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        if text.is_empty() || (text.len() > 1 && text.starts_with('0')) {
            return Err(serde::de::Error::custom(
                "expected canonical unsigned decimal fixture handle",
            ));
        }
        text.parse::<u64>()
            .map(Self)
            .map_err(|_| serde::de::Error::custom("fixture handle is out of range"))
    }
}

/// Root fields assembled from the declared fixture plan and nothing else.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureRoots {
    pub console: bool,
    pub clock: bool,
    pub rand: bool,
    pub env: Option<HostHandle>,
    pub filesystem: Option<HostHandle>,
    pub fetch: Option<HostHandle>,
    pub secrets: Option<HostHandle>,
    pub exec: Option<HostHandle>,
    pub argv: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostRequest {
    ConsoleRead,
    ConsoleWrite { text: String },
    ClockNow,
    RandU64,
    EnvOnly { env: HostHandle, names: Vec<String> },
    EnvGet { env: HostHandle, name: String },
    Argv,
    DirOnly { dir: HostHandle, refine: String },
    DirSubdir { dir: HostHandle, name: String },
    DirRead { dir: HostHandle, path: String },
    DirExists { dir: HostHandle, path: String },
    DirIsDir { dir: HostHandle, path: String },
    DirList { dir: HostHandle },
    DirWrite { dir: HostHandle, path: String, bytes: Vec<u8> },
    DirAppend { dir: HostHandle, path: String, bytes: Vec<u8> },
    DirMakeDir { dir: HostHandle, path: String },
    DirOpen { dir: HostHandle, path: String },
    DirCreate { dir: HostHandle, path: String },
    FileRead { file: HostHandle },
    FileWrite { file: HostHandle, bytes: Vec<u8> },
    FetchOnly { fetch: HostHandle, origins: Vec<String> },
    FetchSend { fetch: HostHandle, request: FixtureFetchRequest },
    SecretStoreLookup { store: HostHandle, name: String },
    SecretStoreRequire { store: HostHandle, name: String },
    SecretReveal { secret: HostHandle },
    SecretSign { secret: HostHandle, message: String },
    SecretPublicKey { secret: HostHandle },
    ExecOnly { exec: HostHandle, tools: Vec<String> },
    ExecRun {
        exec: HostHandle,
        dir: HostHandle,
        path: String,
        arguments: Vec<String>,
        stdin: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum HostResponse {
    Unit,
    String(String),
    OptionalString(Option<String>),
    U64(#[serde(with = "decimal_u64")] u64),
    Strings(Vec<String>),
    Bytes(Vec<u8>),
    Bool(bool),
    Count(usize),
    Handle(HostHandle),
    OptionalHandle(Option<HostHandle>),
    Fetch(FixtureFetchResponse),
    Exec(FixtureExecResponse),
}

mod decimal_u64 {
    use serde::Serializer;

    pub fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    version: u32,
    request: serde_json::Value,
    #[serde(default)]
    source: Option<SourceLocation>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct WireRoots<'a> {
    version: u32,
    roots: &'a FixtureRoots,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct WireResponse {
    version: u32,
    outcome: WireOutcome,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireOutcome {
    Return { value: HostResponse },
    Fail { error: FixtureFailure },
}

fn decode_host_request(value: serde_json::Value) -> Result<HostRequest, WireProtocolError> {
    let object = value.as_object().ok_or_else(|| WireProtocolError {
        message: "fixture host request must be a JSON object".to_owned(),
    })?;
    let operation = object
        .get("operation")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| WireProtocolError {
            message: "fixture host request requires a string `operation`".to_owned(),
        })?;
    let fields: &[&str] = match operation {
        "console_read" | "clock_now" | "rand_u64" | "argv" => &["operation"],
        "console_write" => &["operation", "text"],
        "env_only" => &["operation", "env", "names"],
        "env_get" => &["operation", "env", "name"],
        "dir_only" => &["operation", "dir", "refine"],
        "dir_subdir" => &["operation", "dir", "name"],
        "dir_read" | "dir_exists" | "dir_is_dir" | "dir_open" | "dir_create" => {
            &["operation", "dir", "path"]
        }
        "dir_list" => &["operation", "dir"],
        "dir_write" | "dir_append" => &["operation", "dir", "path", "bytes"],
        "dir_make_dir" => &["operation", "dir", "path"],
        "file_read" => &["operation", "file"],
        "file_write" => &["operation", "file", "bytes"],
        "fetch_only" => &["operation", "fetch", "origins"],
        "fetch_send" => &["operation", "fetch", "request"],
        "secret_store_lookup" | "secret_store_require" => {
            &["operation", "store", "name"]
        }
        "secret_reveal" | "secret_public_key" => &["operation", "secret"],
        "secret_sign" => &["operation", "secret", "message"],
        "exec_only" => &["operation", "exec", "tools"],
        "exec_run" => &[
            "operation",
            "exec",
            "dir",
            "path",
            "arguments",
            "stdin",
        ],
        _ => {
            return Err(WireProtocolError {
                message: format!("unknown fixture host operation `{operation}`"),
            });
        }
    };
    if let Some(field) = object.keys().find(|field| !fields.contains(&field.as_str())) {
        return Err(WireProtocolError {
            message: format!(
                "unknown field `{field}` in fixture host `{operation}` request"
            ),
        });
    }
    serde_json::from_value(value).map_err(|error| WireProtocolError {
        message: error.to_string(),
    })
}

/// Malformed fixture plans and protocol envelopes fail before dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireProtocolError {
    pub message: String,
}

impl fmt::Display for WireProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for WireProtocolError {}

impl WireProtocolError {
    fn encode(error: serde_json::Error) -> Self {
        Self {
            message: format!("fixture host could not encode its response: {error}"),
        }
    }

    fn version(actual: u32) -> Self {
        Self {
            message: format!(
                "unsupported fixture host wire version {actual}; expected {FIXTURE_HOST_WIRE_VERSION}"
            ),
        }
    }
}

#[derive(Debug)]
pub enum HostCreationError {
    InvalidPlan(PlanValidationError),
    RootMint(FixtureFailure),
}

impl fmt::Display for HostCreationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPlan(error) => write!(formatter, "{error}"),
            Self::RootMint(error) => {
                write!(formatter, "fixture root mint failed: {}", error.message)
            }
        }
    }
}

impl Error for HostCreationError {}

struct StoredHandle {
    family: FixtureFamily,
    handle: FixtureHandle,
}

/// One isolated fixture execution and its host-side handle table.
pub struct FixtureHost {
    session: FixtureSession,
    roots: FixtureRoots,
    handles: BTreeMap<HostHandle, StoredHandle>,
    next_handle: u64,
}

impl FixtureHost {
    pub fn new(plan: FixturePlan) -> Result<Self, HostCreationError> {
        let session =
            FixtureSession::new(plan).map_err(HostCreationError::InvalidPlan)?;
        let mut host = Self {
            session,
            roots: FixtureRoots::default(),
            handles: BTreeMap::new(),
            next_handle: 1,
        };
        host.roots.console = host.session.has_fixture(FixtureFamily::Console);
        host.roots.clock = host.session.has_fixture(FixtureFamily::Clock);
        host.roots.rand = host.session.has_fixture(FixtureFamily::Rand);
        host.roots.argv = host.session.has_fixture(FixtureFamily::Argv);
        let env = host.mint_root(FixtureFamily::Env)?;
        let filesystem = host.mint_root(FixtureFamily::Filesystem)?;
        let fetch = host.mint_root(FixtureFamily::Fetch)?;
        let secrets = host.mint_root(FixtureFamily::SecretStore)?;
        let exec = host.mint_root(FixtureFamily::Exec)?;
        host.roots.env = env;
        host.roots.filesystem = filesystem;
        host.roots.fetch = fetch;
        host.roots.secrets = secrets;
        host.roots.exec = exec;
        Ok(host)
    }

    /// Open one isolated fixture host from the canonical plan JSON contract.
    pub fn from_plan_json(bytes: &[u8]) -> Result<Self, WireProtocolError> {
        let plan = parse_fixture_plan(bytes).map_err(|error| WireProtocolError {
            message: error.to_string(),
        })?;
        Self::new(plan).map_err(|error| WireProtocolError {
            message: error.to_string(),
        })
    }

    /// Describe only the roots minted by the validated plan.
    pub fn roots_json(&self) -> Result<String, WireProtocolError> {
        serde_json::to_string(&WireRoots {
            version: FIXTURE_HOST_WIRE_VERSION,
            roots: self.roots(),
        })
        .map_err(WireProtocolError::encode)
    }

    /// Decode and dispatch one versioned browser-adapter request.
    ///
    /// A malformed envelope is a protocol error. A valid provider call that
    /// fails is a typed `fail` outcome and remains in the shared transcript.
    pub fn invoke_json(&mut self, bytes: &[u8]) -> Result<String, WireProtocolError> {
        let envelope: WireRequest =
            parse_unique_json(bytes).map_err(|error| WireProtocolError {
                message: error.to_string(),
            })?;
        if envelope.version != FIXTURE_HOST_WIRE_VERSION {
            return Err(WireProtocolError::version(envelope.version));
        }
        let request = decode_host_request(envelope.request)?;
        let outcome = match self.invoke(request, envelope.source) {
            Ok(value) => WireOutcome::Return { value },
            Err(error) => WireOutcome::Fail { error },
        };
        serde_json::to_string(&WireResponse {
            version: FIXTURE_HOST_WIRE_VERSION,
            outcome,
        })
        .map_err(WireProtocolError::encode)
    }

    /// Consume the isolated host and emit the canonical transcript JSON.
    pub fn finish_json(self, result: TestResult) -> Result<String, WireProtocolError> {
        serde_json::to_string(&self.finish(result)).map_err(WireProtocolError::encode)
    }

    #[must_use]
    pub fn roots(&self) -> &FixtureRoots {
        &self.roots
    }

    pub fn invoke(
        &mut self,
        request: HostRequest,
        source: Option<SourceLocation>,
    ) -> Result<HostResponse, FixtureFailure> {
        match request {
            HostRequest::ConsoleRead => self
                .session
                .console_read(source)
                .map(HostResponse::String),
            HostRequest::ConsoleWrite { text } => self
                .session
                .console_write(text, source)
                .map(|()| HostResponse::Unit),
            HostRequest::ClockNow => {
                self.session.clock_now(source).map(HostResponse::U64)
            }
            HostRequest::RandU64 => {
                self.session.rand_u64(source).map(HostResponse::U64)
            }
            HostRequest::EnvOnly { env, names } => {
                let env =
                    self.resolve(env, FixtureFamily::Env, "env_only", source.clone())?;
                let handle = self.session.env_only(&env, &names, source)?;
                self.store(FixtureFamily::Env, handle).map(HostResponse::Handle)
            }
            HostRequest::EnvGet { env, name } => {
                let env =
                    self.resolve(env, FixtureFamily::Env, "env_get", source.clone())?;
                self.session
                    .env_get(&env, &name, source)
                    .map(HostResponse::OptionalString)
            }
            HostRequest::Argv => self.session.argv(source).map(HostResponse::Strings),
            HostRequest::DirOnly { dir, refine } => {
                let dir = self.fs_handle(dir, "dir_only", source.clone())?;
                let handle = self.session.dir_only(&dir, &refine, source)?;
                self.store(FixtureFamily::Filesystem, handle)
                    .map(HostResponse::Handle)
            }
            HostRequest::DirSubdir { dir, name } => {
                let dir = self.fs_handle(dir, "dir_subdir", source.clone())?;
                let handle = self.session.dir_subdir(&dir, &name, source)?;
                self.store(FixtureFamily::Filesystem, handle)
                    .map(HostResponse::Handle)
            }
            HostRequest::DirRead { dir, path } => {
                let dir = self.fs_handle(dir, "dir_read", source.clone())?;
                self.session.dir_read(&dir, &path, source).map(HostResponse::Bytes)
            }
            HostRequest::DirExists { dir, path } => {
                let dir = self.fs_handle(dir, "dir_exists", source.clone())?;
                self.session.dir_exists(&dir, &path, source).map(HostResponse::Bool)
            }
            HostRequest::DirIsDir { dir, path } => {
                let dir = self.fs_handle(dir, "dir_is_dir", source.clone())?;
                self.session.dir_is_dir(&dir, &path, source).map(HostResponse::Bool)
            }
            HostRequest::DirList { dir } => {
                let dir = self.fs_handle(dir, "dir_list", source.clone())?;
                self.session.dir_list(&dir, source).map(HostResponse::Strings)
            }
            HostRequest::DirWrite { dir, path, bytes } => {
                let dir = self.fs_handle(dir, "dir_write", source.clone())?;
                self.session
                    .dir_write(&dir, &path, &bytes, source)
                    .map(HostResponse::Count)
            }
            HostRequest::DirAppend { dir, path, bytes } => {
                let dir = self.fs_handle(dir, "dir_append", source.clone())?;
                self.session
                    .dir_append(&dir, &path, &bytes, source)
                    .map(HostResponse::Count)
            }
            HostRequest::DirMakeDir { dir, path } => {
                let dir = self.fs_handle(dir, "dir_make_dir", source.clone())?;
                self.session
                    .dir_make_dir(&dir, &path, source)
                    .map(|()| HostResponse::Unit)
            }
            HostRequest::DirOpen { dir, path } => {
                let dir = self.fs_handle(dir, "dir_open", source.clone())?;
                let handle = self.session.dir_open(&dir, &path, source)?;
                self.store(FixtureFamily::Filesystem, handle)
                    .map(HostResponse::Handle)
            }
            HostRequest::DirCreate { dir, path } => {
                let dir = self.fs_handle(dir, "dir_create", source.clone())?;
                let handle = self.session.dir_create(&dir, &path, source)?;
                self.store(FixtureFamily::Filesystem, handle)
                    .map(HostResponse::Handle)
            }
            HostRequest::FileRead { file } => {
                let file = self.fs_handle(file, "file_read", source.clone())?;
                self.session.file_read(&file, source).map(HostResponse::Bytes)
            }
            HostRequest::FileWrite { file, bytes } => {
                let file = self.fs_handle(file, "file_write", source.clone())?;
                self.session
                    .file_write(&file, &bytes, source)
                    .map(HostResponse::Count)
            }
            HostRequest::FetchOnly { fetch, origins } => {
                let fetch =
                    self.resolve(fetch, FixtureFamily::Fetch, "fetch_only", source.clone())?;
                let handle = self.session.fetch_only(&fetch, &origins, source)?;
                self.store(FixtureFamily::Fetch, handle).map(HostResponse::Handle)
            }
            HostRequest::FetchSend { fetch, request } => {
                let fetch =
                    self.resolve(fetch, FixtureFamily::Fetch, "fetch_send", source.clone())?;
                self.session
                    .fetch_send(&fetch, &request, source)
                    .map(HostResponse::Fetch)
            }
            HostRequest::SecretStoreLookup { store, name } => {
                let store = self.secret_handle(store, "secretstore_lookup", source.clone())?;
                self.session
                    .secretstore_lookup(&store, &name, source)?
                    .map(|handle| self.store(FixtureFamily::SecretStore, handle))
                    .transpose()
                    .map(HostResponse::OptionalHandle)
            }
            HostRequest::SecretStoreRequire { store, name } => {
                let store = self.secret_handle(store, "secretstore_require", source.clone())?;
                let handle = self.session.secretstore_require(&store, &name, source)?;
                self.store(FixtureFamily::SecretStore, handle)
                    .map(HostResponse::Handle)
            }
            HostRequest::SecretReveal { secret } => {
                let secret = self.secret_handle(secret, "secret_reveal", source.clone())?;
                self.session
                    .secret_reveal(&secret, source)
                    .map(HostResponse::String)
            }
            HostRequest::SecretSign { secret, message } => {
                let secret = self.secret_handle(secret, "secret_sign", source.clone())?;
                self.session
                    .secret_sign(&secret, &message, source)
                    .map(HostResponse::String)
            }
            HostRequest::SecretPublicKey { secret } => {
                let secret =
                    self.secret_handle(secret, "secret_public_key", source.clone())?;
                self.session
                    .secret_public_key(&secret, source)
                    .map(HostResponse::String)
            }
            HostRequest::ExecOnly { exec, tools } => {
                let exec =
                    self.resolve(exec, FixtureFamily::Exec, "exec_only", source.clone())?;
                let handle = self.session.exec_only(&exec, &tools, source)?;
                self.store(FixtureFamily::Exec, handle).map(HostResponse::Handle)
            }
            HostRequest::ExecRun {
                exec,
                dir,
                path,
                arguments,
                stdin,
            } => {
                let exec =
                    self.resolve(exec, FixtureFamily::Exec, "exec_run", source.clone())?;
                let dir = self.fs_handle(dir, "exec_run", source.clone())?;
                self.session
                    .exec_run(&exec, &dir, &path, &arguments, &stdin, source)
                    .map(HostResponse::Exec)
            }
        }
    }

    #[must_use]
    pub fn finish(self, result: TestResult) -> TestTranscript {
        self.session.finish(result)
    }

    fn mint_root(
        &mut self,
        family: FixtureFamily,
    ) -> Result<Option<HostHandle>, HostCreationError> {
        if !self.session.has_fixture(family) {
            return Ok(None);
        }
        let internal = match family {
            FixtureFamily::Env => self.session.mint_env(None),
            FixtureFamily::Filesystem => self.session.mint_fixture_dir(None),
            FixtureFamily::Fetch => self.session.mint_fixture_fetch(None),
            FixtureFamily::SecretStore => self.session.mint_fixture_secret_store(None),
            FixtureFamily::Exec => self.session.mint_fixture_exec(None),
            _ => return Ok(None),
        }
        .map_err(HostCreationError::RootMint)?;
        self.store(family, internal)
            .map(Some)
            .map_err(HostCreationError::RootMint)
    }

    fn store(
        &mut self,
        family: FixtureFamily,
        handle: FixtureHandle,
    ) -> Result<HostHandle, FixtureFailure> {
        let external = HostHandle(self.next_handle);
        self.next_handle = self.next_handle.checked_add(1).ok_or_else(|| {
            FixtureFailure {
                code: FixtureErrorCode::ProviderFailure,
                message: "fixture handle space exhausted".to_owned(),
            }
        })?;
        self.handles.insert(external, StoredHandle { family, handle });
        Ok(external)
    }

    fn fs_handle(
        &mut self,
        handle: HostHandle,
        operation: &str,
        source: Option<SourceLocation>,
    ) -> Result<FixtureHandle, FixtureFailure> {
        self.resolve(handle, FixtureFamily::Filesystem, operation, source)
    }

    fn secret_handle(
        &mut self,
        handle: HostHandle,
        operation: &str,
        source: Option<SourceLocation>,
    ) -> Result<FixtureHandle, FixtureFailure> {
        self.resolve(handle, FixtureFamily::SecretStore, operation, source)
    }

    fn resolve(
        &mut self,
        external: HostHandle,
        family: FixtureFamily,
        operation: &str,
        source: Option<SourceLocation>,
    ) -> Result<FixtureHandle, FixtureFailure> {
        if let Some(stored) = self.handles.get(&external)
            && stored.family == family
        {
            return Ok(stored.handle.clone());
        }
        let call = FixtureCall {
            family,
            operation: operation.to_owned(),
            target: Some(format!("fixture-handle:{}", external.into_raw())),
            arguments: BTreeMap::new(),
            effective_rights: Vec::new(),
            source,
        };
        Err(self.session.reject_adapter_call(
            call,
            FixtureErrorCode::Denied,
            "unknown or wrong-family fixture handle",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_testkit::{
        ClockFixture, ConsoleFixture, EnvFixture, ExecFixture, Expectations,
        FetchFixture, FilesystemEntry, FilesystemFixture, FixtureOutcome,
        RandFixture, SecretStoreFixture,
    };

    fn plan() -> FixturePlan {
        FixturePlan {
            version: 1,
            env: Some(EnvFixture {
                values: BTreeMap::from([
                    ("PUBLIC".to_owned(), "yes".to_owned()),
                    ("PRIVATE".to_owned(), "no".to_owned()),
                ]),
                allow: vec!["PUBLIC".to_owned()],
                script: Vec::new(),
            }),
            filesystem: Some(FilesystemFixture {
                entries: BTreeMap::from([(
                    "hello.txt".to_owned(),
                    FilesystemEntry::File {
                        hex: "6869".to_owned(),
                    },
                )]),
                rights: vec!["Read".to_owned()],
                entry_policy: None,
                script: Vec::new(),
            }),
            argv: Some(vec!["one".to_owned()]),
            expectations: Expectations::default(),
            ..FixturePlan::default()
        }
    }

    #[test]
    fn roots_are_assembled_only_from_declared_fixtures() {
        let host = FixtureHost::new(plan()).expect("valid host");
        assert!(host.roots().env.is_some());
        assert!(host.roots().filesystem.is_some());
        assert!(host.roots().argv);
        assert!(!host.roots().console);
        assert!(host.roots().fetch.is_none());
        assert!(host.roots().exec.is_none());
    }

    #[test]
    fn all_declared_fixture_families_are_discovered() {
        let host = FixtureHost::new(FixturePlan {
            version: 1,
            console: Some(ConsoleFixture::default()),
            clock: Some(ClockFixture::default()),
            rand: Some(RandFixture::default()),
            env: Some(EnvFixture::default()),
            filesystem: Some(FilesystemFixture::default()),
            fetch: Some(FetchFixture::default()),
            secrets: Some(SecretStoreFixture::default()),
            exec: Some(ExecFixture::default()),
            argv: Some(Vec::new()),
            expectations: Expectations::default(),
        })
        .expect("all fixture families are valid");
        let roots = host.roots();
        assert!(roots.console);
        assert!(roots.clock);
        assert!(roots.rand);
        assert!(roots.env.is_some());
        assert!(roots.filesystem.is_some());
        assert!(roots.fetch.is_some());
        assert!(roots.secrets.is_some());
        assert!(roots.exec.is_some());
        assert!(roots.argv);
    }

    #[test]
    fn forged_and_wrong_family_handles_are_denied_and_transcripted() {
        let mut host = FixtureHost::new(plan()).expect("valid host");
        let error = host
            .invoke(
                HostRequest::EnvGet {
                    env: HostHandle::from_raw(u64::MAX),
                    name: "PUBLIC".to_owned(),
                },
                None,
            )
            .expect_err("forged handle must fail");
        assert_eq!(error.code, FixtureErrorCode::Denied);

        let dir = host.roots().filesystem.expect("dir root");
        let error = host
            .invoke(
                HostRequest::EnvGet {
                    env: dir,
                    name: "PUBLIC".to_owned(),
                },
                None,
            )
            .expect_err("wrong family must fail");
        assert_eq!(error.code, FixtureErrorCode::Denied);

        let transcript = host.finish(TestResult::Passed);
        assert_eq!(transcript.events.len(), 4);
        assert!(matches!(
            transcript.events[2].outcome,
            FixtureOutcome::Fail {
                error: FixtureFailure {
                    code: FixtureErrorCode::Denied,
                    ..
                }
            }
        ));
    }

    #[test]
    fn operations_delegate_to_shared_state_and_attenuation() {
        let mut host = FixtureHost::new(plan()).expect("valid host");
        let env = host.roots().env.expect("env root");
        let narrowed = match host
            .invoke(
                HostRequest::EnvOnly {
                    env,
                    names: vec!["PUBLIC".to_owned()],
                },
                None,
            )
            .expect("narrow env")
        {
            HostResponse::Handle(handle) => handle,
            other => panic!("unexpected response: {other:?}"),
        };
        assert_eq!(
            host.invoke(
                HostRequest::EnvGet {
                    env: narrowed,
                    name: "PUBLIC".to_owned(),
                },
                None,
            )
            .expect("allowed env"),
            HostResponse::OptionalString(Some("yes".to_owned()))
        );
        let denied = host
            .invoke(
                HostRequest::EnvGet {
                    env: narrowed,
                    name: "PRIVATE".to_owned(),
                },
                None,
            )
            .expect_err("attenuation must not widen");
        assert_eq!(denied.code, FixtureErrorCode::PermissionDenied);
    }

    #[test]
    fn wire_protocol_rejects_ambiguous_unknown_and_wrong_version_requests() {
        let duplicate_plan =
            br#"{"version":1,"env":{"values":{"A":"1","A":"2"}}}"#;
        assert!(
            FixtureHost::from_plan_json(duplicate_plan)
                .err()
                .expect("duplicate plan keys must fail")
                .message
                .contains("duplicate")
        );

        let mut host = FixtureHost::new(plan()).expect("valid host");
        let unknown = br#"{"version":1,"request":{"operation":"argv","extra":true}}"#;
        assert!(
            host.invoke_json(unknown)
                .expect_err("unknown request fields must fail")
                .message
                .contains("unknown field")
        );
        let duplicate =
            br#"{"version":1,"version":1,"request":{"operation":"argv"}}"#;
        assert!(
            host.invoke_json(duplicate)
                .expect_err("duplicate envelope keys must fail")
                .message
                .contains("duplicate")
        );
        let wrong = br#"{"version":2,"request":{"operation":"argv"}}"#;
        assert_eq!(
            host.invoke_json(wrong)
                .expect_err("wrong protocol version must fail")
                .message,
            "unsupported fixture host wire version 2; expected 1"
        );
    }

    #[test]
    fn wire_protocol_preserves_handles_u64_failures_and_transcript() {
        let mut host = FixtureHost::new(FixturePlan {
            version: 1,
            clock: Some(ClockFixture {
                start_ns: Some(witchy_testkit::U64Text::new(u64::MAX)),
                ..ClockFixture::default()
            }),
            env: Some(EnvFixture {
                values: BTreeMap::from([("PUBLIC".to_owned(), "yes".to_owned())]),
                allow: vec!["PUBLIC".to_owned()],
                script: Vec::new(),
            }),
            expectations: Expectations::default(),
            ..FixturePlan::default()
        })
        .expect("valid host");

        let roots: serde_json::Value =
            serde_json::from_str(&host.roots_json().expect("roots JSON")).unwrap();
        assert_eq!(roots["version"], 1);
        assert_eq!(roots["roots"]["env"], "1");

        let clock = host
            .invoke_json(br#"{"version":1,"request":{"operation":"clock_now"}}"#)
            .expect("clock response");
        let clock: serde_json::Value = serde_json::from_str(&clock).unwrap();
        assert_eq!(clock["outcome"]["kind"], "return");
        assert_eq!(clock["outcome"]["value"]["kind"], "u64");
        assert_eq!(clock["outcome"]["value"]["value"], u64::MAX.to_string());

        let denied = host
            .invoke_json(
                br#"{"version":1,"request":{"operation":"env_get","env":"999","name":"PUBLIC"}}"#,
            )
            .expect("provider failure is a typed response");
        let denied: serde_json::Value = serde_json::from_str(&denied).unwrap();
        assert_eq!(denied["outcome"]["kind"], "fail");
        assert_eq!(denied["outcome"]["error"]["code"], "denied");

        let transcript = host
            .finish_json(TestResult::Passed)
            .expect("transcript JSON");
        let transcript: TestTranscript = serde_json::from_str(&transcript).unwrap();
        assert_eq!(transcript.events.len(), 3);
        assert_eq!(transcript.result, TestResult::Passed);
    }
}
