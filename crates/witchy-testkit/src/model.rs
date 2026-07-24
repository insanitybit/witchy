use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use witchy_cap_model::CapabilityKind;

pub const FIXTURE_PLAN_VERSION: u32 = 1;
pub const TEST_TRANSCRIPT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixturePlan {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console: Option<ConsoleFixture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<ClockFixture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rand: Option<RandFixture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<EnvFixture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<FilesystemFixture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch: Option<FetchFixture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<SecretStoreFixture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<ExecFixture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm: Option<VmFixture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    #[serde(default)]
    pub expectations: Expectations,
}

impl Default for FixturePlan {
    fn default() -> Self {
        Self {
            version: FIXTURE_PLAN_VERSION,
            console: None,
            clock: None,
            rand: None,
            env: None,
            filesystem: None,
            fetch: None,
            secrets: None,
            exec: None,
            vm: None,
            argv: None,
            expectations: Expectations::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expectations {
    #[serde(default)]
    pub require_complete_scripts: bool,
    #[serde(default)]
    pub ordered_calls: Vec<OrderedCallExpectation>,
    #[serde(default)]
    pub calls: Vec<CallExpectation>,
    #[serde(default)]
    pub absent_families: Vec<FixtureFamily>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderedCallExpectation {
    pub family: FixtureFamily,
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_rights: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallExpectation {
    pub family: FixtureFamily,
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<U64Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<U64Text>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsoleFixture {
    #[serde(default)]
    pub script: Vec<FixtureStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClockFixture {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_ns: Option<U64Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_ns: Option<U64Text>,
    #[serde(default)]
    pub repeat_last: bool,
    #[serde(default)]
    pub script: Vec<FixtureStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RandFixture {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<U64Text>,
    #[serde(default)]
    pub script: Vec<FixtureStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvFixture {
    #[serde(default)]
    pub values: BTreeMap<String, String>,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub script: Vec<FixtureStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemFixture {
    #[serde(default)]
    pub entries: BTreeMap<String, FilesystemEntry>,
    #[serde(default)]
    pub rights: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_policy: Option<String>,
    #[serde(default)]
    pub script: Vec<FixtureStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FilesystemEntry {
    Directory,
    File { hex: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchFixture {
    #[serde(default)]
    pub origins: Vec<String>,
    #[serde(default)]
    pub script: Vec<FixtureStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretStoreFixture {
    #[serde(default)]
    pub entries: BTreeMap<String, SecretFixture>,
    #[serde(default)]
    pub script: Vec<FixtureStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretFixture {
    pub hex: String,
    #[serde(default)]
    pub usage: SecretUsage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretUsage {
    #[default]
    Revealable,
    UseOnly,
    Signing,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecFixture {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub script: Vec<FixtureStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmFixture {
    #[serde(default)]
    pub script: Vec<FixtureStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureStep {
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default)]
    pub arguments: BTreeMap<String, FixtureValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_rights: Option<Vec<String>>,
    pub outcome: FixtureOutcome,
    #[serde(default = "default_true")]
    pub required: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum FixtureValue {
    Null,
    Bool(bool),
    String(String),
    Bytes(String),
    List(Vec<FixtureValue>),
    Map(BTreeMap<String, FixtureValue>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FixtureOutcome {
    Return { value: FixtureValue },
    Fail { error: FixtureFailure },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureFailure {
    pub code: FixtureErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureErrorCode {
    Denied,
    InvalidRequest,
    PermissionDenied,
    NotFound,
    AlreadyExists,
    NotDirectory,
    InvalidData,
    Interrupted,
    Timeout,
    Redirect,
    Network,
    ResponseTooLarge,
    SpawnFailed,
    ProviderFailure,
    Exhausted,
    UnexpectedCall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureFetchRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureFetchResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureFamily {
    Console,
    Clock,
    Rand,
    Env,
    Filesystem,
    Fetch,
    SecretStore,
    Exec,
    Vm,
    Argv,
}

impl FixtureFamily {
    pub const fn capability_kind(self) -> Option<CapabilityKind> {
        match self {
            Self::Console => Some(CapabilityKind::Console),
            Self::Clock => Some(CapabilityKind::Clock),
            Self::Rand => Some(CapabilityKind::Rand),
            Self::Env => Some(CapabilityKind::Env),
            Self::Filesystem => Some(CapabilityKind::Dir),
            Self::Fetch => Some(CapabilityKind::Fetch),
            Self::SecretStore => Some(CapabilityKind::SecretStore),
            Self::Exec => Some(CapabilityKind::Exec),
            Self::Vm | Self::Argv => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct U64Text(u64);

impl U64Text {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(&self) -> u64 {
        self.0
    }
}

impl Serialize for U64Text {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for U64Text {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        if text.is_empty() || (text.len() > 1 && text.starts_with('0')) {
            return Err(serde::de::Error::custom("expected canonical unsigned decimal string"));
        }
        text.parse::<u64>()
            .map(Self)
            .map_err(|_| serde::de::Error::custom("unsigned decimal value is out of range"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestTranscript {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<U64Text>,
    pub events: Vec<TestEvent>,
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub result: TestResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestEvent {
    pub sequence: U64Text,
    pub family: FixtureFamily,
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default)]
    pub arguments: BTreeMap<String, FixtureValue>,
    #[serde(default)]
    pub effective_rights: Vec<String>,
    pub outcome: FixtureOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLocation {
    pub module: String,
    pub line: U64Text,
    pub column: U64Text,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TestResult {
    Passed,
    Failed { message: String },
    InfrastructureError { message: String },
}
