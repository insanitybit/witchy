//! Private mechanical WasmIR bring-up for the admitted scalar SafeWIR subset.
//!
//! This module is deliberately not public.  The current canonical checker
//! mints `InitialValid`, but there is no final SafeWIR/authority typestate yet,
//! so the lowering entry is test-only.  When that authority exists, the entry
//! point takes final `ValidatedSafeWir` authority, and no weaker state is
//! accepted.
//!
//! The target is an IR, not wasm bytes.  It has no raw constructors visible to
//! callers.  Every scalar source item emits exactly one target item, which
//! makes the receipt matcher independently checkable without replaying the
//! lowering function.  The private encoder handles only the structured CFG
//! forms recognized below.

#![allow(dead_code)]

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use sha2::{Digest, Sha256};
use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, CustomSection, DataSection, ExportKind, ExportSection,
    ElementSection, Elements, EntityType, Function as WasmFunctionEncoder, FunctionSection,
    GlobalSection, GlobalType, Ieee64, ImportSection, MemArg, MemorySection, MemoryType,
    Module as WasmModule, RefType, TableSection, TableType, TypeSection, ValType,
};

use crate::canonical_safewir::{
    AccessMode, ArithmeticOperator, BasicBlock, CallArgument, CanonicalModule,
    ComparisonOperator, Extent, Function, FunctionId, FunctionSignature, HeaderField,
    HostCallArgument, HostBufferExtent, HostMutation, HostParameterRole, HostResultRole,
    ImportContract, ImportContractIdentity, ImportId, LoadTransfer, MemoryEffect, ModuleStage,
    Operation, InitialValid, ParameterContract, ParameterOwnership, Place, Projection,
    ResultContract, StoreTransfer, Terminator, ValidatedSafeWir, ValueId, ValueType,
};
use crate::layout::{
    HeaderLayout, LayoutInterner, LayoutKind, LayoutSize, PointerSlot, RcHeader, ScalarKind,
};

// Ref values point at the canonical layout root.  RC metadata occupies the
// eight bytes immediately before that root; the private module keeps one
// fixed linear-memory page and refuses an allocation before returning a
// pointer when the checked extent does not fit.
pub(crate) mod drop_worklist;
mod outlined_memory;
mod scalar_locals;
pub(crate) mod closure_owner_kernel;
mod owner_bridges;
mod owner_operation_kernel;
mod lowering;
use self::lowering::*;
mod correspondence;
pub(crate) use self::correspondence::*;
mod emit;
use self::emit::*;
mod structured;
mod forward_cfg;
use self::structured::*;
mod plan;
use self::plan::*;
mod plan_match;
use self::plan_match::*;
pub use owner_bridges::{AdmittedOwnerBridge, OwnerBridgeRecipe};

const RC_PREFIX_BYTES: u32 = 8;
const LINEAR_MEMORY_BYTES: u32 = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum SourceItemId {
    Operation { function: FunctionId, block: crate::canonical_safewir::BlockId, index: usize },
    Terminator { function: FunctionId, block: crate::canonical_safewir::BlockId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum TargetItemId {
    Instruction { function: FunctionId, block: crate::canonical_safewir::BlockId, index: usize },
    Terminator { function: FunctionId, block: crate::canonical_safewir::BlockId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScalarInstruction {
    ProofNoop,
    ClosureOwner(crate::canonical_safewir::closure_owner_contract::OwnerOperation),
    IntConst { destination: ValueId, value: i64 },
    FloatConst { destination: ValueId, bits: u64 },
    UnitConst { destination: ValueId },
    BoolConst { destination: ValueId, value: bool },
    SizeConst { destination: ValueId, value: u64 },
    CapabilityNone { destination: ValueId },
    CapabilitySome { destination: ValueId, source: ValueId },
    CapabilityIsPresent { destination: ValueId, source: ValueId },
    CapabilityUnwrap { destination: ValueId, source: ValueId },
    Arithmetic {
        destination: ValueId,
        operator: ArithmeticOperator,
        left: ValueId,
        right: ValueId,
        overflow: crate::canonical_safewir::OverflowBehavior,
    },
    Compare {
        destination: ValueId,
        operator: ComparisonOperator,
        left: ValueId,
        right: ValueId,
    },
    NarrowSize { destination: ValueId, value: ValueId },
    WidenSize { destination: ValueId, value: ValueId },
    FloatUnary { destination: ValueId, operator: crate::canonical_safewir::FloatUnaryOperator, value: ValueId },
    Guard { condition: ValueId, kind: crate::canonical_safewir::GuardKind },
    MoveValue { destination: ValueId, source: ValueId },
    /// A source-level aggregate marker. It owns no Wasm local; construction is
    /// erased and extraction is lowered from its checked scalar field map.
    MakeInlineAggregate { destination: ValueId, layout: crate::layout::LayoutId, fields: Vec<ValueId> },
    ExtractInlineField { destination: ValueId, aggregate: ValueId, index: u32, field: ValueId },
    FunctionValue { destination: ValueId, function: FunctionId, table_index: u32 },
    Alloc {
        destination: ValueId,
        layout: crate::layout::LayoutId,
        extent: Extent,
        class: crate::canonical_safewir::AllocationClass,
    },
    StaticData { destination: ValueId, data: crate::canonical_safewir::StaticDataId },
    InitNullableOwned { place: Place },
    Load { destination: ValueId, place: Place, transfer: LoadTransfer },
    Store { place: Place, value: ValueId, transfer: StoreTransfer },
    CopyBytes { destination: Place, source: Place, length: ValueId },
    MoveRange { destination: Place, source: Place, count: ValueId },
    CloneRange { destination: Place, source: Place, count: ValueId },
    UpdateCount { object: ValueId, count: ValueId },
    CloneRef { destination: ValueId, source: ValueId },
    Release { object: ValueId },
    Reclaim { object: ValueId },
    Call { destinations: Vec<ValueId>, callee: FunctionId, arguments: Vec<ValueId> },
    /// A direct call to an authenticated separately compiled scalar Wasm
    /// export.  It has a distinct source operation and encoder path from a
    /// host call: no host role or linear-memory adapter is available.
    ExternalCall { destinations: Vec<ValueId>, import: ImportId, arguments: Vec<ValueId> },
    HostCall { destinations: Vec<ValueId>, import: ImportId, arguments: Vec<HostCallArgument> },
    VarCall {
        destinations: Vec<ValueId>,
        callee: ValueId,
        signature: crate::canonical_safewir::FunctionSignatureId,
        arguments: Vec<ValueId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WasmInstruction {
    source: SourceItemId,
    operation: ScalarInstruction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CollectionLayout {
    layout: crate::layout::LayoutId,
    length_offset: u32,
    capacity_offset: Option<u32>,
    data_offset: u32,
    stride: u32,
    scalar: ScalarKind,
    pointer_layout: Option<crate::layout::LayoutId>,
    pointer_slot: Option<PointerSlot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemoryAccess {
    object: ValueId,
    collection: CollectionLayout,
    offset: u32,
    width: u32,
    scalar: ScalarKind,
    pointer_layout: Option<crate::layout::LayoutId>,
    pointer_slot: Option<PointerSlot>,
    index: Option<ValueId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScalarTerminator {
    Branch { target: crate::canonical_safewir::BlockId, arguments: Vec<ValueId> },
    BranchIf {
        condition: ValueId,
        then_target: crate::canonical_safewir::BlockId,
        then_arguments: Vec<ValueId>,
        else_target: crate::canonical_safewir::BlockId,
        else_arguments: Vec<ValueId>,
    },
    Return { values: Vec<ValueId> },
    TailCall {
        site: crate::canonical_safewir::TailSiteId,
        callee: FunctionId,
        arguments: Vec<ValueId>,
    },
    VarTailCall {
        site: crate::canonical_safewir::TailSiteId,
        callee: ValueId,
        signature: crate::canonical_safewir::FunctionSignatureId,
        arguments: Vec<ValueId>,
    },
    Trap,
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WasmTerminator {
    source: SourceItemId,
    operation: ScalarTerminator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WasmBlock {
    id: crate::canonical_safewir::BlockId,
    instructions: Vec<WasmInstruction>,
    terminator: WasmTerminator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WasmFunction {
    id: FunctionId,
    signature: crate::canonical_safewir::FunctionSignatureId,
    entry: crate::canonical_safewir::BlockId,
    blocks: Vec<WasmBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WasmIr {
    owner_bridges: Vec<AdmittedOwnerBridge>,
    revision: crate::canonical_safewir::Revision,
    checked_source: crate::canonical_safewir::CheckedSourceIdentity,
    launch_contract_payload: Vec<u8>,
    /// The encoded launch entry (`witchy.entry`), present only for a program
    /// with a validated `main`; derived inside admission, never from bytes.
    entry_payload: Option<Vec<u8>>,
    static_data: Vec<WasmStaticData>,
    imports: Vec<WasmImport>,
    table: Option<WasmTable>,
    heap_start: u32,
    functions: Vec<WasmFunction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WasmTable {
    /// Entries are the exact checked module functions, in module order. A
    /// callable value is only a private table index; `call_indirect` still
    /// checks the authenticated signature at execution.
    functions: Vec<FunctionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WasmImport {
    id: ImportId,
    signature: crate::canonical_safewir::FunctionSignatureId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WasmStaticData {
    id: crate::canonical_safewir::StaticDataId,
    offset: u32,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CorrespondenceRule {
    ScalarConstant,
    ScalarArithmetic,
    ScalarComparison,
    ScalarConversion,
    ScalarGuard,
    ScalarMove,
    MemoryAlloc,
    MemoryStaticData,
    MemoryLoad,
    MemoryStore,
    MemoryCopy,
    MemoryUpdateCount,
    MemoryOwnership,
    ScalarCall,
    ExternalCall,
    HostCall,
    Control,
    ClosureOwner,
}

/// The backend disposition of a canonical rule.  Most rules produce one
/// instruction receipt.  A subjudgment instead constrains named parent rules
/// and never produces a receipt of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuleBackend {
    Receipt(CorrespondenceRule),
    Subjudgment(&'static [crate::canonical_safewir::CanonicalRuleId]),
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CorrespondenceMapping {
    source: SourceItemId,
    target: TargetItemId,
    rule: CorrespondenceRule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CorrespondenceReceipt {
    revision: crate::canonical_safewir::Revision,
    checked_source: crate::canonical_safewir::CheckedSourceIdentity,
    mappings: Vec<CorrespondenceMapping>,
}

/// Witness derived only from the exact emitted bytes after independent Wasm
/// parsing and validation. It has no access to SafeWIR authority or lowering
/// state and therefore cannot be a self-attestation of the source candidate.
struct TargetWasmWitness {
    wasm_digest: [u8; 32],
    decoded_digest: [u8; 32],
}

impl TargetWasmWitness {
    fn from_decoded(
        bytes: &[u8],
        section_order: &[u8],
        functions: &[(Vec<(u32, String)>, Vec<PlanToken>)],
        data: &[(u32, Vec<u8>)],
    ) -> Self {
        let decoded = format!("{section_order:?}{functions:?}{data:?}");
        Self {
            wasm_digest: digest_bytes(b"witchy/canonical-wasmir/wasm/v1", &[bytes]),
            decoded_digest: digest_bytes(
                b"witchy/canonical-wasmir/decoded-target/v1",
                &[decoded.as_bytes()],
            ),
        }
    }

    fn matches_bytes(&self, bytes: &[u8]) -> bool {
        self.wasm_digest == digest_bytes(b"witchy/canonical-wasmir/wasm/v1", &[bytes])
            && self.decoded_digest != [0; 32]
    }
}

/// Private encoder authority minted only after final SafeWIR authority,
/// independently checked correspondence, and the AuthorityWIR witness agree
/// on one exact SafeWIR/WasmIR pair. Unchecked `WasmIr` cannot cross the
/// encoding boundary.
struct CheckedWasmIr {
    source: CanonicalModule<InitialValid>,
    target: WasmIr,
    receipt: CorrespondenceReceipt,
    checked_source: crate::canonical_safewir::CheckedSourceIdentity,
    final_module_digest: crate::canonical_safewir::ModuleDigest,
    authority_digest: crate::canonical_safewir::ModuleDigest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LoweringError {
    UnsupportedOperation { function: FunctionId, operation: &'static str },
    UnsupportedCallArgument { function: FunctionId },
    UnsupportedOverflow { function: FunctionId },
    UnsupportedControlFlow { function: FunctionId, operation: &'static str },
    UnsupportedLayout { function: FunctionId, operation: &'static str },
    InvalidLayoutBundle,
    StaticDataAddressOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CorrespondenceError {
    ModuleIdentityMismatch,
    MissingFunction(FunctionId),
    UnexpectedFunction(FunctionId),
    FunctionShapeMismatch(FunctionId),
    MissingBlock { function: FunctionId, block: crate::canonical_safewir::BlockId },
    UnexpectedBlock { function: FunctionId, block: crate::canonical_safewir::BlockId },
    DuplicateSourceItem(SourceItemId),
    MissingSourceItem(SourceItemId),
    UnexpectedSourceItem(SourceItemId),
    WrongSourcePosition { expected: SourceItemId, actual: SourceItemId },
    InstructionMismatch(SourceItemId),
    TerminatorMismatch(SourceItemId),
    UnsupportedSourceItem(SourceItemId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EncodeError {
    Lowering(LoweringError),
    Correspondence(CorrespondenceError),
    SourceWitness,
    UnsupportedValueType { function: FunctionId, value: ValueId, value_type: crate::canonical_safewir::ValueType },
    UnsupportedSignature { signature: crate::canonical_safewir::FunctionSignatureId },
    ParameterLayoutMismatch { function: FunctionId },
    MissingFunction(FunctionId),
    MissingValue { function: FunctionId, value: ValueId },
    MissingBlock { function: FunctionId, block: crate::canonical_safewir::BlockId },
    UnsupportedLayout { function: FunctionId, operation: &'static str },
    WasmValidation(String),
}

/// Opaque exact bytes admitted by the canonical backend.
///
/// `wasm` is a read-only byte view for existing consumers. This artifact does
/// not itself seal or authorize launch; receipt-bound launch remains a later
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedVmService { id: u32, callback_export: String }
impl AdmittedVmService {
    pub fn id(&self) -> u32 { self.id }
    pub fn callback_export(&self) -> &str { &self.callback_export }
}

/// One externally linked Wasm function required by an admitted caller.  This
/// record is derived from final SafeWIR, not from the raw Wasm import section;
/// the launcher matches the full authenticated identity before it considers
/// the textual Wasm module/export names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedExternalWasmImport {
    module: String,
    name: String,
    identity: crate::canonical_safewir::ExternalWasmIdentity,
}

impl AdmittedExternalWasmImport {
    pub fn module(&self) -> &str { &self.module }
    pub fn name(&self) -> &str { &self.name }
    pub fn identity(&self) -> crate::canonical_safewir::ExternalWasmIdentity { self.identity }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AdmittedCanonicalArtifact {
    compiler_session: Option<witchy_syntax::compiler_session::CompilerSessionContract>,
    vm_services: Vec<AdmittedVmService>,
    external_imports: Vec<AdmittedExternalWasmImport>,
    owner_bridges: Vec<AdmittedOwnerBridge>,
    bytes: Vec<u8>,
    checked_source: crate::canonical_safewir::CheckedSourceIdentity,
    final_module_digest: crate::canonical_safewir::ModuleDigest,
    authority_digest: crate::canonical_safewir::ModuleDigest,
    correspondence_digest: [u8; 32],
    wasm_digest: [u8; 32],
    entry: Option<CanonicalEntry>,
}

/// A capability parameter of the program entry, by host family.
///
/// The runtime mints the corresponding opaque host reference at launch. The
/// variant set is exactly the families the canonical foundation lowers; an
/// entry demanding any other capability kind is refused at admission rather
/// than launched with a token the host cannot mint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalEntryCapability {
    Exec,
    Clock,
    /// An explicitly supplied namespace; an empty namespace is still a grant.
    SecretStore,
    /// A separate bare signing-secret grant, not inferred from a Store entry.
    Secret,
    /// An explicit origin-scoped grant, in Fetch-parameter ordinal order.
    Fetch,
    Console,
    /// A recipe-only, write-only build output grant (kind 9).
    BuildOut,
    /// A recipe-only, read-only build input grant (kind 12).
    BuildRead,
    /// A recipe-only snapshot of explicitly named environment keys (kind 14).
    BuildEnv,
    /// A recipe-only grant of host-selected confined tools (kind 15).
    BuildExec,
    /// A recipe-only exact host:port allow-list (kind 16).
    BuildNet,
    /// A root `Dir` grant, minted by the launcher from the grant ordinal
    /// among the entry's `Dir` parameters (kind 1).
    Dir,
    /// The process environment grant (kind 2).
    Env,
    /// A supplied Net grant, in Net-parameter ordinal order (kind 4).
    Net,
}

/// What the program entry returns, for the frozen exit-code convention: an
/// `Int` or `Float` result is printed by the host, anything else is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalEntryResult {
    Unit,
    Int,
    Float,
}

/// How a launcher enters an admitted canonical artifact.
///
/// Canonical modules export every function as `f<id>`; the frozen compiler's
/// parameterless `run` wrapper does not exist because the authority `main`
/// receives is minted by the host, not synthesized in the module. This
/// descriptor is derived only inside admission from the validated SafeWIR
/// signature of the checked entry declaration, so the runtime calls exactly
/// the function the checker validated, with exactly the capability tokens its
/// signature declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalEntry {
    export: String,
    capabilities: Vec<CanonicalEntryCapability>,
    argument_positions: Vec<u8>,
    result: CanonicalEntryResult,
}

/// Custom section carrying the encoded [`CanonicalEntry`] so a Tier-1
/// `.wasm` file launches without the in-memory artifact. Admission emits it
/// from the derived entry and matches it exactly, so its bytes are covered
/// by the artifact's Wasm digest like every other section. Reading it back
/// from bytes lives in `canonical_entry`, outside this private encoder.
pub(crate) const ENTRY_SECTION: &str = "witchy.entry";
/// Exported allocator globals of an admitted module: the bump pointer, the
/// reclaimed-object free list head, and the heap base below which pointers
/// name static data.
pub const HEAP_TOP_EXPORT: &str = "__witchy_heap_bump";
pub const HEAP_FREE_EXPORT: &str = "__witchy_heap_free";
pub const HEAP_BASE_EXPORT: &str = "__witchy_heap_base";
const ENTRY_VERSION: u8 = 1;

impl CanonicalEntry {
    /// Wire form: version, export name (length-prefixed), capability tags
    /// (count-prefixed), result tag. Every field is derived from the validated
    /// signature; nothing here is authority on its own.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![if self.argument_positions.is_empty() { ENTRY_VERSION } else { 2 }];
        let export = self.export.as_bytes();
        bytes.push(u8::try_from(export.len()).expect("canonical export names are short"));
        bytes.extend_from_slice(export);
        bytes.push(u8::try_from(self.capabilities.len()).expect("entry capability count fits a byte"));
        for capability in &self.capabilities {
            bytes.push(match capability {
                CanonicalEntryCapability::Console => 0,
                CanonicalEntryCapability::Dir => 1,
                CanonicalEntryCapability::Env => 2,
                CanonicalEntryCapability::Net => 4,
                CanonicalEntryCapability::BuildOut => 9,
                CanonicalEntryCapability::BuildRead => 12,
                CanonicalEntryCapability::BuildEnv => 14,
                CanonicalEntryCapability::BuildExec => 15,
                CanonicalEntryCapability::BuildNet => 16,
                CanonicalEntryCapability::Exec => 10,
                CanonicalEntryCapability::Fetch => 17,
                CanonicalEntryCapability::Clock => 19,
                CanonicalEntryCapability::SecretStore => 20,
                CanonicalEntryCapability::Secret => 21,
            });
        }
        bytes.push(match self.result {
            CanonicalEntryResult::Unit => 0,
            CanonicalEntryResult::Int => 1,
            CanonicalEntryResult::Float => 2,
        });
        if !self.argument_positions.is_empty() {
            bytes.push(self.argument_positions.len() as u8);
            bytes.extend_from_slice(&self.argument_positions);
        }
        bytes
    }

    pub(crate) fn decode(data: &[u8]) -> Result<Self, String> {
        let invalid = |detail: &str| format!("invalid `{ENTRY_SECTION}` metadata: {detail}");
        let (&version, rest) = data.split_first().ok_or_else(|| invalid("truncated header"))?;
        if version != ENTRY_VERSION && version != 2 {
            return Err(invalid(&format!("unsupported version {version}")));
        }
        let (&export_len, rest) = rest.split_first().ok_or_else(|| invalid("truncated export name"))?;
        if rest.len() < usize::from(export_len) {
            return Err(invalid("truncated export name"));
        }
        let (export, rest) = rest.split_at(usize::from(export_len));
        let export = std::str::from_utf8(export).map_err(|_| invalid("export name is not UTF-8"))?.to_owned();
        let (&count, rest) = rest.split_first().ok_or_else(|| invalid("truncated capability count"))?;
        if rest.len() < usize::from(count) + 1 {
            return Err(invalid("capability count does not match the payload length"));
        }
        let (tags, result) = rest.split_at(usize::from(count));
        let capabilities = tags
            .iter()
            .map(|tag| match tag {
                0 => Ok(CanonicalEntryCapability::Console),
                1 => Ok(CanonicalEntryCapability::Dir),
                2 => Ok(CanonicalEntryCapability::Env),
                4 => Ok(CanonicalEntryCapability::Net),
                9 => Ok(CanonicalEntryCapability::BuildOut),
                12 => Ok(CanonicalEntryCapability::BuildRead),
                14 => Ok(CanonicalEntryCapability::BuildEnv),
                15 => Ok(CanonicalEntryCapability::BuildExec),
                16 => Ok(CanonicalEntryCapability::BuildNet),
                10 => Ok(CanonicalEntryCapability::Exec),
                17 => Ok(CanonicalEntryCapability::Fetch),
                19 => Ok(CanonicalEntryCapability::Clock),
                20 => Ok(CanonicalEntryCapability::SecretStore),
                21 => Ok(CanonicalEntryCapability::Secret),
                other => Err(invalid(&format!("unknown capability tag {other}"))),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result = match result[0] {
            0 => CanonicalEntryResult::Unit,
            1 => CanonicalEntryResult::Int,
            2 => CanonicalEntryResult::Float,
            other => return Err(invalid(&format!("unknown result tag {other}"))),
        };
        let suffix = &rest[usize::from(count) + 1..];
        let argument_positions = if version == ENTRY_VERSION {
            if !suffix.is_empty() {
                return Err(invalid("trailing entry bytes"));
            }
            Vec::new()
        } else {
            let (&argument_count, positions) = suffix.split_first()
                .ok_or_else(|| invalid("truncated argument positions"))?;
            let total = usize::from(count) + usize::from(argument_count);
            if argument_count == 0 || positions.len() != usize::from(argument_count)
                || total > usize::from(u8::MAX)
                || positions.iter().any(|position| usize::from(*position) >= total)
                || positions.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(invalid("invalid argument positions"));
            }
            positions.to_vec()
        };
        Ok(Self { export, capabilities, argument_positions, result })
    }

    pub fn export(&self) -> &str {
        &self.export
    }

    pub fn capabilities(&self) -> &[CanonicalEntryCapability] {
        &self.capabilities
    }

    /// Indices in the complete parameter list that receive fresh owned
    /// `List(String)` values. Remaining positions receive capability tokens.
    pub fn argument_positions(&self) -> &[u8] {
        &self.argument_positions
    }

    pub fn result(&self) -> CanonicalEntryResult {
        self.result
    }
}

impl AdmittedCanonicalArtifact {
    pub fn compiler_session(&self) -> Option<&witchy_syntax::compiler_session::CompilerSessionContract> {
        self.compiler_session.as_ref()
    }
    pub fn vm_services(&self) -> &[AdmittedVmService] { &self.vm_services }
    /// Exact separately compiled scalar Wasm imports the launcher must bind.
    /// An empty list is not permission to supply arbitrary external modules.
    pub fn external_imports(&self) -> &[AdmittedExternalWasmImport] {
        &self.external_imports
    }
    pub fn owner_bridges(&self) -> &[AdmittedOwnerBridge] { &self.owner_bridges }
    /// Borrow the exact admitted Wasm bytes without transferring ownership.
    pub fn wasm(&self) -> &[u8] {
        &self.bytes
    }

    /// Digest of the exact final SafeWIR graph from which this artifact was
    /// admitted. This is audit metadata and grants no construction authority.
    pub fn safe_wir_digest(&self) -> [u8; 32] {
        self.final_module_digest.bytes()
    }

    /// Digest of the private validation lineage that authorized this artifact.
    /// Exposing the digest does not expose a way to mint that authority.
    pub fn authority_digest(&self) -> [u8; 32] {
        self.authority_digest.bytes()
    }

    /// The validated program entry, or `None` for a library artifact without
    /// a checked `main`.
    pub fn entry(&self) -> Option<&CanonicalEntry> {
        self.entry.as_ref()
    }

    /// Refuse any byte sequence other than the exact artifact admitted from
    /// final SafeWIR. Launchers use the artifact's immutable byte view; this
    /// method makes attempted detached-byte substitution observable in focused
    /// boundary tests and future receipt-bound sinks.
    pub fn verify_exact_bytes(&self, bytes: &[u8]) -> Result<(), CanonicalAdmissionError> {
        let digest = digest_bytes(b"witchy/canonical-wasmir/wasm/v1", &[bytes]);
        (digest == self.wasm_digest)
            .then_some(())
            .ok_or(CanonicalAdmissionError::PlanMismatch("admitted artifact bytes"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalAdmissionError {
    /// The reviewed SafeWIR-to-WasmIR mapping has no encoding for a construct
    /// the checker accepted. The payload names it: a validated program that
    /// the backend cannot encode is a backend gap to record, not a silent
    /// refusal.
    Lowering(String),
    /// The checked entry's validated signature demands a parameter or result
    /// the launch protocol cannot supply; the payload names it.
    UnsupportedEntry(String),
    Correspondence,
    SourceWitnessMismatch,
    /// The module failed structural validation or decoding; the payload is
    /// the decoder's message, the diagnostic that pins down an encoder gap.
    WasmValidation(String),
    UnsupportedPlan,
    PlanMismatch(&'static str),
    SectionOrderMismatch { expected: Vec<u8>, actual: Vec<u8> },
}

impl std::fmt::Display for CanonicalAdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "canonical Wasm admission failed: {self:?}")
    }
}

impl std::error::Error for CanonicalAdmissionError {}
fn authorize_wasm_ir(
    validated: &ValidatedSafeWir,
    source_witness: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    entry: Option<&CanonicalEntry>,
) -> Result<CheckedWasmIr, EncodeError> {
    authorize_wasm_ir_inner(validated, Some(source_witness), entry.map(CanonicalEntry::encode))
}

#[cfg(feature = "raw-encoder-test-api")]
fn authorize_wasm_ir_for_test(validated: ValidatedSafeWir) -> Result<CheckedWasmIr, EncodeError> {
    authorize_wasm_ir_inner(&validated, None, None)
}

fn authorize_wasm_ir_inner(
    validated: &ValidatedSafeWir,
    source_witness: Option<&witchy_canonical_source::source::CanonicalSourceView<'_>>,
    entry_payload: Option<Vec<u8>>,
) -> Result<CheckedWasmIr, EncodeError> {
    validated.validate_pre_admission().map_err(|_| {
        EncodeError::Correspondence(CorrespondenceError::ModuleIdentityMismatch)
    })?;
    if let Some(source_witness) = source_witness {
        validated
            .validate_external_source_witness(source_witness)
            .map_err(|_| EncodeError::SourceWitness)?;
    }
    let source = validated.module().clone();
    let checked_source = validated.checked_source();
    let final_module_digest = validated.final_module_digest();
    let authority_digest = validated.authority_digest();
    let mut target = lower_validated(validated).map_err(EncodeError::Lowering)?;
    target.entry_payload = entry_payload;
    let receipt = check_correspondence(&source, &target).map_err(EncodeError::Correspondence)?;
    if receipt.mappings.len() != expected_source_items(&source).len() {
        return Err(EncodeError::Correspondence(CorrespondenceError::MissingSourceItem(
            expected_source_items(&source)
                .into_iter()
                .next()
                .expect("nonempty canonical module"),
        )));
    }
    Ok(CheckedWasmIr {
        source,
        target,
        receipt,
        checked_source,
        final_module_digest,
        authority_digest,
    })
}

/// Encode only correspondence-checked private WasmIR authority. This function
/// cannot lower SafeWIR or accept an unchecked backend value.
fn owner_cleanup_bridge(bridges: &[AdmittedOwnerBridge]) -> Option<&AdmittedOwnerBridge> {
    if bridges.iter().any(AdmittedOwnerBridge::requires_cleanup_helper) {
        bridges.first()
    } else {
        None
    }
}

/// Encode and structurally validate the module.
fn encode_checked(checked: &CheckedWasmIr) -> Result<Vec<u8>, EncodeError> {
    let bytes = encode_checked_unvalidated(checked)?;
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::default() | wasmparser::WasmFeatures::TAIL_CALL)
        .validate_all(&bytes)
        .map_err(|error| EncodeError::WasmValidation(error.to_string()))?;
    Ok(bytes)
}

/// Encode without validating, for admission, whose `decode_and_match_plan`
/// validates these exact bytes with the same features before anything else.
/// Validating here as well ran wasmparser over the whole module twice per
/// admission: 13% of a warm async compile.
fn encode_checked_unvalidated(checked: &CheckedWasmIr) -> Result<Vec<u8>, EncodeError> {
    let source = &checked.source;
    let target = &checked.target;
    let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&source.layouts().canonical_bytes())
        .map_err(|_| EncodeError::Lowering(LoweringError::InvalidLayoutBundle))?;
    let import_count = u32::try_from(source.imports().len() + target.owner_bridges.len()).map_err(|_| EncodeError::Lowering(LoweringError::UnsupportedOperation { function: FunctionId::new(u32::MAX), operation: "import count" }))?;
    let owner_cleanup_bridge = owner_cleanup_bridge(&target.owner_bridges);
    let owner_cleanup_function = owner_cleanup_bridge
        .map(|_| import_count + target.functions.len() as u32);
    let memory_helpers = outlined_memory::Helpers::new(source, import_count + target.functions.len() as u32 + u32::from(owner_cleanup_bridge.is_some()))?;
    let function_indices: BTreeMap<_, _> = target.functions.iter().enumerate().map(|(index, function)| (function.id, import_count + index as u32)).collect();
    let mut import_indices: BTreeMap<_, _> = source.imports().iter().enumerate().map(|(index, import)| (import.id, index as u32)).collect();
    for (index, bridge) in target.owner_bridges.iter().enumerate() { import_indices.insert(bridge.id(), source.imports().len() as u32 + index as u32); }
    let import_contracts: BTreeMap<_, _> = source.imports().iter().map(|import| (import.id, import.clone())).collect();
    let signature_indices: BTreeMap<_, _> = source.signatures().iter().enumerate().map(|(index, signature)| (signature.id, index as u32)).collect();
    let signatures_by_id: BTreeMap<_, _> = source.signatures().iter()
        .map(|signature| (signature.id, signature.clone())).collect();
    let mut types = TypeSection::new();
    for signature in source.signatures() {
        let parameters = signature.parameters.iter().enumerate().map(|(index, value_type)| wasm_value_type(FunctionId::new(u32::MAX), ValueId::new(index as u32), *value_type));
        let results = signature.results.iter().enumerate().map(|(index, value_type)| wasm_value_type(FunctionId::new(u32::MAX), ValueId::new(index as u32), *value_type));
        let parameters = parameters.collect::<Result<Vec<_>, _>>()?;
        let results = results.collect::<Result<Vec<_>, _>>()?;
        types.ty().function(parameters, results);
    }
    let mut import_type_indices = BTreeMap::new();
    for (index, import) in source.imports().iter().enumerate() {
        let (parameters, results) = import_wasm_signature(FunctionId::new(u32::MAX), import, &signatures_by_id)
            .map_err(EncodeError::Lowering)?;
        let type_index = u32::try_from(source.signatures().len() + index)
            .map_err(|_| EncodeError::Lowering(LoweringError::UnsupportedOperation { function: FunctionId::new(u32::MAX), operation: "import type count" }))?;
        types.ty().function(parameters, results);
        import_type_indices.insert(import.id, type_index);
    }
    owner_bridges::append_types(&mut types, &target.owner_bridges)?;
    let owner_cleanup_type = owner_cleanup_bridge.map(|_| {
        let index = types.len();
        types.ty().function([ValType::Ref(RefType::ANYREF)], []);
        index
    });
    let address_type = types.len();
    types.ty().function([ValType::I32; 3], [ValType::I32]);
    let drop_type = types.len();
    types.ty().function([ValType::I32], []);
    let retain_type = types.len();
    types.ty().function([ValType::I32], [ValType::I32]);
    let mut imports = ImportSection::new();
    for import in source.imports() {
        let type_index = import_type_indices.get(&import.id).copied().ok_or(
            EncodeError::Lowering(LoweringError::UnsupportedOperation { function: FunctionId::new(u32::MAX), operation: "HostCall import" }),
        )?;
        imports.import(&import.module, &import.name, EntityType::Function(type_index));
    }
    owner_bridges::append_imports(&mut imports, &target.owner_bridges, (source.signatures().len() + source.imports().len()) as u32);
    let mut functions = FunctionSection::new();
    let mut table_section = TableSection::new();
    let mut elements = ElementSection::new();
    let table_function_indices = target.table.as_ref().map(|table| {
        table
            .functions
            .iter()
            .map(|function| function_indices.get(function).copied().ok_or(EncodeError::MissingFunction(*function)))
            .collect::<Result<Vec<_>, _>>()
    }).transpose()?;
    if let Some(table) = &target.table {
        table_section.table(TableType {
            element_type: RefType::FUNCREF,
            table64: false,
            minimum: table.functions.len() as u64,
            maximum: None,
            shared: false,
        });
        if let Some(functions) = &table_function_indices {
            elements.active(None, &ConstExpr::i32_const(0), Elements::Functions(functions.clone().into()));
        }
    }
    let mut memory = MemorySection::new();
    memory.memory(MemoryType {
        minimum: initial_memory_pages(target.heap_start),
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    let mut globals = GlobalSection::new();
    globals.global(
        GlobalType { val_type: ValType::I32, mutable: true, shared: false },
        &ConstExpr::i32_const(target.heap_start as i32),
    );
    globals.global(
        GlobalType { val_type: ValType::I32, mutable: true, shared: false },
        &ConstExpr::i32_const(0),
    );
    globals.global(
        GlobalType { val_type: ValType::I32, mutable: false, shared: false },
        &ConstExpr::i32_const(target.heap_start as i32),
    );
    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    // The allocator's three globals are exported read-only for the host's
    // heap audit (a launcher may check that an admitted program released
    // every object); they grant no authority the module does not already
    // exercise on its own memory.
    exports.export(HEAP_TOP_EXPORT, ExportKind::Global, 0);
    exports.export(HEAP_FREE_EXPORT, ExportKind::Global, 1);
    exports.export(HEAP_BASE_EXPORT, ExportKind::Global, 2);
    let mut code = CodeSection::new();
    let static_data_offsets: BTreeMap<_, _> = target.static_data.iter().map(|segment| (segment.id, segment.offset)).collect();
    let mut data = DataSection::new();
    for segment in &target.static_data {
        data.active(0, &ConstExpr::i32_const(segment.offset as i32), segment.bytes.clone());
    }
    // Indexed once rather than scanned per function. Both lookups are
    // functions of the module, so inside the loop they were O(functions^2)
    // and O(functions x signatures). Function count measures linear today
    // (3200 functions, 2026-09-20), so this is a latent scaling defect
    // rather than a cost anyone is paying yet -- the same shape already
    // fixed in the plan mirror by 83b00f1de.
    let source_functions_by_id: BTreeMap<_, _> = source.functions().iter()
        .map(|function| (function.id, function)).collect();
    let source_signatures_by_id: BTreeMap<_, _> = source.signatures().iter()
        .map(|signature| (signature.id, signature)).collect();
    for target_function in &target.functions {
        let source_function = *source_functions_by_id.get(&target_function.id).ok_or(EncodeError::MissingFunction(target_function.id))?;
        let signature = *source_signatures_by_id.get(&source_function.signature).ok_or(EncodeError::UnsupportedSignature { signature: source_function.signature })?;
        let type_index = signature_indices.get(&source_function.signature).copied().ok_or(EncodeError::UnsupportedSignature { signature: source_function.signature })?;
        functions.function(type_index);
        exports.export(&format!("f{}", target_function.id.get()), ExportKind::Func, function_indices[&target_function.id]);
        code.function(&encode_function(source_function, signature, target_function, &function_indices, &signature_indices, &import_indices, &import_contracts, &static_data_offsets, &layouts, &target.owner_bridges, owner_cleanup_function, &memory_helpers)?);
    }
    if let (Some(bridge), Some(type_index)) = (owner_cleanup_bridge, owner_cleanup_type) {
        functions.function(type_index);
        code.function(&owner_operation_kernel::emit_cleanup_helper(&layouts, bridge)?);
    }
    functions.function(address_type);
    code.function(&outlined_memory::emit_address());
    for root in memory_helpers.drops.keys() {
        functions.function(drop_type);
        code.function(&outlined_memory::emit_drop(&layouts, *root)?);
    }
    functions.function(retain_type);
    code.function(&outlined_memory::emit_retain());
    let mut module = WasmModule::new();
    module
        .section(&types)
        .section(&imports)
        .section(&functions);
    if target.table.is_some() {
        module.section(&table_section);
    }
    module
        .section(&memory)
        .section(&globals)
        .section(&exports);
    if target.table.is_some() {
        module.section(&elements);
    }
    module.section(&code);
    if !target.static_data.is_empty() {
        module.section(&data);
    }
    module.section(&CustomSection {
        name: Cow::Borrowed("witchy.launch"),
        data: Cow::Borrowed(&target.launch_contract_payload),
    });
    if let Some(entry_payload) = &target.entry_payload {
        module.section(&CustomSection {
            name: Cow::Borrowed(ENTRY_SECTION),
            data: Cow::Borrowed(entry_payload),
        });
    }
    Ok(module.finish())
}

#[cfg(feature = "raw-encoder-test-api")]
fn encode_validated(validated: ValidatedSafeWir) -> Result<Vec<u8>, EncodeError> {
    let checked = authorize_wasm_ir_for_test(validated)?;
    encode_checked(&checked)
}

fn digest_bytes(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn correspondence_digest(receipt: &CorrespondenceReceipt) -> [u8; 32] {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&receipt.revision.get().to_le_bytes());
    bytes.extend_from_slice(&receipt.checked_source.digest());
    for mapping in &receipt.mappings {
        match mapping.source {
            SourceItemId::Operation { function, block, index } => {
                bytes.push(0);
                bytes.extend_from_slice(&function.get().to_le_bytes());
                bytes.extend_from_slice(&block.get().to_le_bytes());
                bytes.extend_from_slice(&(index as u64).to_le_bytes());
            }
            SourceItemId::Terminator { function, block } => {
                bytes.push(1);
                bytes.extend_from_slice(&function.get().to_le_bytes());
                bytes.extend_from_slice(&block.get().to_le_bytes());
            }
        }
        match mapping.target {
            TargetItemId::Instruction { function, block, index } => {
                bytes.push(0);
                bytes.extend_from_slice(&function.get().to_le_bytes());
                bytes.extend_from_slice(&block.get().to_le_bytes());
                bytes.extend_from_slice(&(index as u64).to_le_bytes());
            }
            TargetItemId::Terminator { function, block } => {
                bytes.push(1);
                bytes.extend_from_slice(&function.get().to_le_bytes());
                bytes.extend_from_slice(&block.get().to_le_bytes());
            }
        }
        bytes.push(mapping.rule as u8);
    }
    digest_bytes(b"witchy/canonical-wasmir/correspondence/v1", &[&bytes])
}

fn admit_parts(
    _source: CanonicalModule<InitialValid>,
    _target: WasmIr,
    receipt: CorrespondenceReceipt,
    bytes: Vec<u8>,
    checked_source: crate::canonical_safewir::CheckedSourceIdentity,
    final_module_digest: crate::canonical_safewir::ModuleDigest,
    authority_digest: crate::canonical_safewir::ModuleDigest,
    target_witness: TargetWasmWitness,
    entry: Option<CanonicalEntry>,
) -> Result<AdmittedCanonicalArtifact, CanonicalAdmissionError> {
    if !target_witness.matches_bytes(&bytes) {
        return Err(CanonicalAdmissionError::PlanMismatch("target witness"));
    }
    let wasm_digest = digest_bytes(b"witchy/canonical-wasmir/wasm/v1", &[&bytes]);
    Ok(AdmittedCanonicalArtifact {
        compiler_session: witchy_syntax::compiler_session::CompilerSessionContract::decode(_source.launch_contract_payload()),
        vm_services: _source.vm_services().iter().map(|site| AdmittedVmService {
            id: site.id(), callback_export: format!("f{}", site.callback().get()),
        }).collect(),
        external_imports: _source.imports().iter().filter_map(|import| {
            let crate::canonical_safewir::ImportContractIdentity::AuthenticatedExternal(identity) = &import.abi_identity else {
                return None;
            };
            Some(AdmittedExternalWasmImport {
                module: import.module.clone(),
                name: import.name.clone(),
                identity: *identity,
            })
        }).collect(),
        owner_bridges: _target.owner_bridges,
        bytes,
        checked_source,
        final_module_digest,
        authority_digest,
        correspondence_digest: correspondence_digest(&receipt),
        wasm_digest,
        entry,
    })
}

/// Derive the launch entry from the validated SafeWIR signature of the checked
/// entry declaration. Nothing here consults a function spelling: the entry
/// declaration comes from checked source, its ordinal from the callable ABI
/// manifest, its function from the candidate's validated ABI bindings, and its
/// parameter kinds from the validated signature. A library without a checked
/// entry has no launch entry.
fn derive_entry(
    validated: &ValidatedSafeWir,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> Result<Option<CanonicalEntry>, CanonicalAdmissionError> {
    use crate::canonical_safewir::{CapabilityKindId, ValueType};
    use witchy_canonical_source::abi::CanonicalTargetPolicy;

    // The linker leaves the program entry unqualified as `main` whatever the
    // entry module is called; a library has no `main` and no launch entry.
    let Some(abi) = source.closed_callable_abi(source.entry_declaration()) else {
        return Ok(None);
    };
    let CanonicalTargetPolicy::ClosedLocal(target) = abi.target_policy else {
        return Err(CanonicalAdmissionError::UnsupportedEntry("entry is not a closed local target".into()));
    };
    let module = validated.module();
    let function_id = module
        .source_abi_bindings()
        .entries()
        .iter()
        .find(|binding| binding.source_declaration_ordinal() == target.function_ordinal)
        .map(|binding| binding.function())
        .ok_or_else(|| CanonicalAdmissionError::UnsupportedEntry("entry has no validated ABI binding".into()))?;
    let function = module
        .functions()
        .iter()
        .find(|function| function.id == function_id)
        .ok_or_else(|| CanonicalAdmissionError::UnsupportedEntry("entry binding names no function".into()))?;
    let signature = module
        .signatures()
        .iter()
        .find(|signature| signature.id == function.signature)
        .ok_or_else(|| CanonicalAdmissionError::UnsupportedEntry("entry function has no signature".into()))?;
    let Some(witchy_syntax::ast::Type::Fn(source_parameters, _, _, _)) =
        source.callable_signature(source.entry_declaration()).map(|ty| ty.unqualified())
    else {
        return Err(CanonicalAdmissionError::UnsupportedEntry("entry lacks its checked source signature".into()));
    };
    if signature.parameters.len() != source_parameters.len() || signature.parameters.len() > usize::from(u8::MAX) {
        return Err(CanonicalAdmissionError::UnsupportedEntry("entry parameter count is not representable".into()));
    }
    let (argument_layout, _) = crate::canonical_launch::argument_layouts()
        .map_err(|error| CanonicalAdmissionError::UnsupportedEntry(error.to_string()))?;
    let mut capabilities = Vec::with_capacity(signature.parameters.len());
    let mut argument_positions = Vec::new();
    for (index, (parameter, source_parameter)) in signature.parameters.iter().zip(source_parameters).enumerate() {
        match parameter {
            ValueType::Capability(kind) if *kind == CapabilityKindId::new(0) => {
                capabilities.push(CanonicalEntryCapability::Console);
            }
            ValueType::Capability(kind) if *kind == CapabilityKindId::new(1) => {
                capabilities.push(CanonicalEntryCapability::Dir);
            }
            ValueType::Capability(kind) if *kind == CapabilityKindId::new(2) => {
                capabilities.push(CanonicalEntryCapability::Env);
            }
ValueType::Capability(kind) if *kind == crate::canonical_safewir::BUILD_OUT_CAPABILITY_KIND && source.is_build_recipe() => {
    capabilities.push(CanonicalEntryCapability::BuildOut);
}
            ValueType::Capability(kind) if *kind == crate::canonical_safewir::BUILD_READ_CAPABILITY_KIND && source.is_build_recipe() => {
                capabilities.push(CanonicalEntryCapability::BuildRead);
            }
            ValueType::Capability(kind) if *kind == crate::canonical_safewir::BUILD_ENV_CAPABILITY_KIND && source.is_build_recipe() => {
                capabilities.push(CanonicalEntryCapability::BuildEnv);
            }
            ValueType::Capability(kind) if *kind == crate::canonical_safewir::BUILD_EXEC_CAPABILITY_KIND && source.is_build_recipe() => {
                capabilities.push(CanonicalEntryCapability::BuildExec);
            }
            ValueType::Capability(kind) if *kind == crate::canonical_safewir::BUILD_NET_CAPABILITY_KIND && source.is_build_recipe() => {
                capabilities.push(CanonicalEntryCapability::BuildNet);
            }
            ValueType::Capability(kind) if *kind == crate::canonical_safewir::EXEC_CAPABILITY_KIND => {
                capabilities.push(CanonicalEntryCapability::Exec);
            }
            ValueType::Capability(kind) if *kind == crate::canonical_safewir::FETCH_CAPABILITY_KIND => {
                capabilities.push(CanonicalEntryCapability::Fetch);
            }
            ValueType::Capability(kind) if *kind == crate::canonical_safewir::CLOCK_CAPABILITY_KIND => {
                capabilities.push(CanonicalEntryCapability::Clock);
            }
            ValueType::Capability(kind) if *kind == crate::canonical_safewir::SECRET_STORE_CAPABILITY_KIND => {
                capabilities.push(CanonicalEntryCapability::SecretStore);
            }
            ValueType::Capability(kind) if *kind == crate::canonical_safewir::SECRET_CAPABILITY_KIND => {
                capabilities.push(CanonicalEntryCapability::Secret);
            }
            ValueType::Capability(kind) if *kind == crate::canonical_safewir::NET_CAPABILITY_KIND => {
                capabilities.push(CanonicalEntryCapability::Net);
            }
            ValueType::Ref(layout) if *layout == argument_layout && matches!(
                source_parameter.unqualified(),
                witchy_syntax::ast::Type::Named(name, elements) if name == "List" && elements.len() == 1
                    && matches!(elements[0].unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
                        if name == "String" && arguments.is_empty())
            ) => {
                argument_positions.push(index as u8);
            }
            other => {
                return Err(CanonicalAdmissionError::UnsupportedEntry(format!(
                    "entry parameter {other:?} cannot be supplied by the launch protocol"
                )));
            }
        }
    }
    let result = match signature.results.as_slice() {
        [] | [ValueType::Unit] => CanonicalEntryResult::Unit,
        [ValueType::Int] => CanonicalEntryResult::Int,
        [ValueType::Float] => CanonicalEntryResult::Float,
        other => {
            return Err(CanonicalAdmissionError::UnsupportedEntry(format!(
                "entry result {other:?} has no launch convention"
            )));
        }
    };
    Ok(Some(CanonicalEntry {
        export: format!("f{}", function_id.get()),
        capabilities,
        argument_positions,
        result,
    }))
}

pub fn admit_canonical(
    validated: ValidatedSafeWir,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> Result<AdmittedCanonicalArtifact, CanonicalAdmissionError> {
    admit_canonical_with_observer(validated, source, |_, _| ())
        .map(|(artifact, ())| artifact)
}

/// Observe the original immutable validated graph only after full production
/// admission succeeds. The graph is neither cloned nor retained by the artifact.
pub fn admit_canonical_with_observer<R>(
    validated: ValidatedSafeWir,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    observe: impl FnOnce(&ValidatedSafeWir, &AdmittedCanonicalArtifact) -> R,
) -> Result<(AdmittedCanonicalArtifact, R), CanonicalAdmissionError> {
    let entry = derive_entry(&validated, source)?;
    let checked = authorize_wasm_ir(&validated, source, entry.as_ref()).map_err(|error| match error {
        EncodeError::Lowering(_) | EncodeError::UnsupportedLayout { .. } => {
            CanonicalAdmissionError::Lowering(format!("{error:?}"))
        }
        EncodeError::Correspondence(_) => CanonicalAdmissionError::Correspondence,
        EncodeError::SourceWitness => CanonicalAdmissionError::SourceWitnessMismatch,
        EncodeError::WasmValidation(message) => CanonicalAdmissionError::WasmValidation(message),
        _ => CanonicalAdmissionError::UnsupportedPlan,
    })?;
    let bytes = encode_checked_unvalidated(&checked)
        .map_err(|error| CanonicalAdmissionError::Lowering(format!("{error:?}")))?;
    let CheckedWasmIr {
        source,
        target,
        receipt,
        checked_source,
        final_module_digest,
        authority_digest,
    } = checked;
    let target_witness = decode_and_match_plan(&source, &target, &bytes)?;
    let artifact = admit_parts(source, target, receipt, bytes, checked_source, final_module_digest, authority_digest, target_witness, entry)?;
    let observation = observe(&validated, &artifact);
    Ok((artifact, observation))
}

#[cfg(feature = "raw-encoder-test-api")]
fn admit_canonical_for_test(
    validated: ValidatedSafeWir,
) -> Result<AdmittedCanonicalArtifact, CanonicalAdmissionError> {
    let checked = authorize_wasm_ir_for_test(validated).map_err(|error| match error {
        EncodeError::Lowering(_) | EncodeError::UnsupportedLayout { .. } => {
            CanonicalAdmissionError::Lowering(format!("{error:?}"))
        }
        EncodeError::Correspondence(_) => CanonicalAdmissionError::Correspondence,
        EncodeError::SourceWitness => CanonicalAdmissionError::SourceWitnessMismatch,
        EncodeError::WasmValidation(message) => CanonicalAdmissionError::WasmValidation(message),
        _ => CanonicalAdmissionError::UnsupportedPlan,
    })?;
    let bytes = encode_checked_unvalidated(&checked).map_err(|error| match error {
        EncodeError::WasmValidation(message) => CanonicalAdmissionError::WasmValidation(message),
        _ => CanonicalAdmissionError::UnsupportedPlan,
    })?;
    let CheckedWasmIr {
        source,
        target,
        receipt,
        checked_source,
        final_module_digest,
        authority_digest,
    } = checked;
    let target_witness = decode_and_match_plan(&source, &target, &bytes)?;
    admit_parts(
        source,
        target,
        receipt,
        bytes,
        checked_source,
        final_module_digest,
        authority_digest,
        target_witness,
        None,
    )
}

#[cfg(feature = "raw-encoder-test-api")]
fn admit_canonical_with_bytes_for_test(
    validated: ValidatedSafeWir,
    bytes: Vec<u8>,
) -> Result<AdmittedCanonicalArtifact, CanonicalAdmissionError> {
    let checked = authorize_wasm_ir_for_test(validated).map_err(|_| CanonicalAdmissionError::Correspondence)?;
    let CheckedWasmIr {
        source,
        target,
        receipt,
        checked_source,
        final_module_digest,
        authority_digest,
    } = checked;
    let target_witness = decode_and_match_plan(&source, &target, &bytes)?;
    admit_parts(source, target, receipt, bytes, checked_source, final_module_digest, authority_digest, target_witness, None)
}
#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
#[path = "canonical_wasmir/tests"]
pub mod self_checks {
    pub mod unique_pop;
    pub mod forward_cfg;
    use super::*;
    use crate::canonical_safewir::{
        BasicBlock, BlockId, BlockParameter, CallableContract, CheckedSourceIdentity,
        FunctionSignature, FunctionSignatureId,
        GuardKind,
        LoweredCandidate, LoweredModuleParts, OverflowBehavior, ParameterContract,
        ParameterOwnership, Revision, ResultContract, check_initial_with_test_witness,
        validate_final_for_test,
    };

    use crate::layout::{LayoutBundle, LayoutInterner, PointerSlot, RcHeader, ScalarKind};

    fn checked_scalar() -> ValidatedSafeWir {
        let value = ValueId::new(1);
        let signature = FunctionSignature {
            id: FunctionSignatureId::new(1),
            parameters: Vec::new(),
            results: vec![crate::canonical_safewir::ValueType::Int],
            contract: CallableContract {
                parameters: Vec::new(),
                results: vec![ResultContract::Value(crate::canonical_safewir::ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        };
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(7),
            checked_source: CheckedSourceIdentity::from_digest([4; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), [])
                .expect("empty layout bundle is valid"),
            signatures: vec![signature],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(1),
                signature: FunctionSignatureId::new(1),
                entry: crate::canonical_safewir::BlockId::new(1),
                value_types: [(value, crate::canonical_safewir::ValueType::Int)].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(1),
                    parameters: Vec::new(),
                    operations: vec![Operation::ConstInt { destination: value, value: 9 }],
                    terminator: Terminator::Return { values: vec![value] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        let checked = check_initial_with_test_witness(candidate).expect("scalar fixture is initially valid");
        validate_final_for_test(checked).expect("scalar fixture is final-valid")
    }

    fn checked_int_subtraction() -> ValidatedSafeWir {
        let left = ValueId::new(1);
        let right = ValueId::new(2);
        let result = ValueId::new(3);
        let signature = FunctionSignature {
            id: FunctionSignatureId::new(1),
            parameters: Vec::new(),
            results: vec![ValueType::Int],
            contract: CallableContract {
                parameters: Vec::new(),
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        };
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(8),
            checked_source: CheckedSourceIdentity::from_digest([8; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), [])
                .expect("empty layout bundle is valid"),
            signatures: vec![signature],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(1),
                signature: FunctionSignatureId::new(1),
                entry: BlockId::new(1),
                value_types: [
                    (left, ValueType::Int),
                    (right, ValueType::Int),
                    (result, ValueType::Int),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: BlockId::new(1),
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstInt { destination: left, value: 9 },
                        Operation::ConstInt { destination: right, value: 4 },
                        Operation::Arithmetic {
                            destination: result,
                            operator: ArithmeticOperator::Subtract,
                            left,
                            right,
                            overflow: OverflowBehavior::Wrap,
                        },
                    ],
                    terminator: Terminator::Return { values: vec![result] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        let checked =
            check_initial_with_test_witness(candidate).expect("subtraction fixture is valid");
        validate_final_for_test(checked).expect("subtraction fixture is final-valid")
    }

    fn scalar_signature(id: FunctionSignatureId) -> FunctionSignature {
        scalar_signature_with(id, Vec::new(), vec![crate::canonical_safewir::ValueType::Int])
    }

    fn scalar_signature_with(
        id: FunctionSignatureId,
        parameters: Vec<crate::canonical_safewir::ValueType>,
        results: Vec<crate::canonical_safewir::ValueType>,
    ) -> FunctionSignature {
        FunctionSignature {
            id,
            parameters: parameters.clone(),
            results: results.clone(),
            contract: CallableContract {
                parameters: parameters
                    .into_iter()
                    .map(|value_type| ParameterContract {
                        value_type,
                        ownership: ParameterOwnership::Value,
                    })
                    .collect(),
                results: results.into_iter().map(ResultContract::Value).collect(),
                parameter_effects: BTreeMap::new(),
            },
        }
    }

    fn finalize(candidate: CanonicalModule<LoweredCandidate>) -> ValidatedSafeWir {
        let checked = check_initial_with_test_witness(candidate).expect("fixture is initially valid");
        validate_final_for_test(checked).expect("fixture is final-valid")
    }

    fn checked_size_add(
        left_value: u32,
        right_value: u32,
        overflow: crate::canonical_safewir::OverflowBehavior,
    ) -> ValidatedSafeWir {
        let left = ValueId::new(0);
        let right = ValueId::new(1);
        let result = ValueId::new(2);
        let signature_id = FunctionSignatureId::new(70);
        let function_id = FunctionId::new(70);
        let signature = scalar_signature_with(
            signature_id,
            Vec::new(),
            vec![ValueType::Size],
        );
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(70),
            checked_source: CheckedSourceIdentity::from_digest([70; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), [])
                .expect("empty layout bundle is valid"),
            signatures: vec![signature],
            imports: Vec::new(),
            functions: vec![Function {
                id: function_id,
                signature: signature_id,
                entry: crate::canonical_safewir::BlockId::new(0),
                value_types: [
                    (left, ValueType::Size),
                    (right, ValueType::Size),
                    (result, ValueType::Size),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(0),
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize {
                            destination: left,
                            value: u64::from(left_value),
                        },
                        Operation::ConstSize {
                            destination: right,
                            value: u64::from(right_value),
                        },
                        Operation::Arithmetic {
                            destination: result,
                            operator: ArithmeticOperator::Add,
                            left,
                            right,
                            overflow,
                        },
                    ],
                    terminator: Terminator::Return {
                        values: vec![result],
                    },
                }],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_int_add(left_value: i64, right_value: i64) -> ValidatedSafeWir {
        let left = ValueId::new(0);
        let right = ValueId::new(1);
        let result = ValueId::new(2);
        let signature_id = FunctionSignatureId::new(71);
        let function_id = FunctionId::new(71);
        let signature = scalar_signature_with(signature_id, Vec::new(), vec![ValueType::Int]);
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(71),
            checked_source: CheckedSourceIdentity::from_digest([71; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), [])
                .expect("empty layout bundle is valid"),
            signatures: vec![signature],
            imports: Vec::new(),
            functions: vec![Function {
                id: function_id,
                signature: signature_id,
                entry: crate::canonical_safewir::BlockId::new(0),
                value_types: [
                    (left, ValueType::Int),
                    (right, ValueType::Int),
                    (result, ValueType::Int),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(0),
                    parameters: Vec::new(),
                    operations: vec![
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
                            operator: ArithmeticOperator::Add,
                            left,
                            right,
                            overflow: crate::canonical_safewir::OverflowBehavior::Trap,
                        },
                    ],
                    terminator: Terminator::Return {
                        values: vec![result],
                    },
                }],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    #[cfg(feature = "native")]
    fn run_size_add(module: ValidatedSafeWir) -> Result<u32, wasmtime::Error> {
        let wasm = encode_validated(module).expect("validated size addition encodes");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, wasm)?;
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
        let run = instance.get_typed_func::<(), i32>(&mut store, "f70")?;
        run.call(&mut store, ()).map(|value| value as u32)
    }

    #[cfg(feature = "native")]
    fn run_int_add(module: ValidatedSafeWir) -> Result<i64, wasmtime::Error> {
        let artifact = admit_canonical_for_test(module).expect("validated int addition admits");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())?;
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
        let run = instance.get_typed_func::<(), i64>(&mut store, "f71")?;
        run.call(&mut store, ())
    }

    #[cfg(feature = "native")]
    pub fn size_add_overflow_traps_or_wraps_exactly_as_declared() {
        assert!(run_size_add(checked_size_add(
            u32::MAX,
            1,
            crate::canonical_safewir::OverflowBehavior::Trap,
        ))
        .is_err());
        assert_eq!(
            run_size_add(checked_size_add(
                u32::MAX,
                1,
                crate::canonical_safewir::OverflowBehavior::Wrap,
            ))
            .unwrap(),
            0,
        );
    }

    fn checked_branch() -> ValidatedSafeWir {
        let flag = ValueId::new(1);
        let then_value = ValueId::new(2);
        let else_value = ValueId::new(3);
        let result = ValueId::new(4);
        let signature = scalar_signature_with(
            FunctionSignatureId::new(6),
            vec![crate::canonical_safewir::ValueType::Bool],
            vec![crate::canonical_safewir::ValueType::Int],
        );
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(12),
            checked_source: CheckedSourceIdentity::from_digest([9; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![signature],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(6),
                signature: FunctionSignatureId::new(6),
                entry: crate::canonical_safewir::BlockId::new(0),
                value_types: [
                    (flag, crate::canonical_safewir::ValueType::Bool),
                    (then_value, crate::canonical_safewir::ValueType::Int),
                    (else_value, crate::canonical_safewir::ValueType::Int),
                    (result, crate::canonical_safewir::ValueType::Int),
                ].into_iter().collect(),
                blocks: vec![
                    BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(0),
                        parameters: vec![BlockParameter { value: flag, value_type: crate::canonical_safewir::ValueType::Bool, ownership: None }],
                        operations: Vec::new(),
                        terminator: Terminator::BranchIf { condition: flag, then_target: crate::canonical_safewir::BlockId::new(1), then_arguments: Vec::new(), else_target: crate::canonical_safewir::BlockId::new(2), else_arguments: Vec::new() },
                    },
                    BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(1),
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstInt { destination: then_value, value: 11 }],
                        terminator: Terminator::Branch { target: crate::canonical_safewir::BlockId::new(3), arguments: vec![then_value] },
                    },
                    BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(2),
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstInt { destination: else_value, value: 22 }],
                        terminator: Terminator::Branch { target: crate::canonical_safewir::BlockId::new(3), arguments: vec![else_value] },
                    },
                    BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(3),
                        parameters: vec![BlockParameter { value: result, value_type: crate::canonical_safewir::ValueType::Int, ownership: None }],
                        operations: Vec::new(),
                        terminator: Terminator::Return { values: vec![result] },
                    },
                ],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_loop() -> ValidatedSafeWir {
        let limit = ValueId::new(1);
        let zero = ValueId::new(2);
        let header_limit = ValueId::new(3);
        let header_i = ValueId::new(4);
        let condition = ValueId::new(5);
        let one = ValueId::new(6);
        let next_i = ValueId::new(7);
        let exit_limit = ValueId::new(8);
        let result = ValueId::new(9);
        let signature = scalar_signature_with(
            FunctionSignatureId::new(7),
            vec![crate::canonical_safewir::ValueType::Int],
            vec![crate::canonical_safewir::ValueType::Int],
        );
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(13),
            checked_source: CheckedSourceIdentity::from_digest([10; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![signature],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(7),
                signature: FunctionSignatureId::new(7),
                entry: crate::canonical_safewir::BlockId::new(0),
                value_types: [
                    (limit, crate::canonical_safewir::ValueType::Int),
                    (zero, crate::canonical_safewir::ValueType::Int),
                    (header_limit, crate::canonical_safewir::ValueType::Int),
                    (header_i, crate::canonical_safewir::ValueType::Int),
                    (condition, crate::canonical_safewir::ValueType::Bool),
                    (one, crate::canonical_safewir::ValueType::Int),
                    (next_i, crate::canonical_safewir::ValueType::Int),
                    (exit_limit, crate::canonical_safewir::ValueType::Int),
                    (result, crate::canonical_safewir::ValueType::Int),
                ].into_iter().collect(),
                blocks: vec![
                    BasicBlock { id: crate::canonical_safewir::BlockId::new(0), parameters: vec![BlockParameter { value: limit, value_type: crate::canonical_safewir::ValueType::Int, ownership: None }], operations: vec![Operation::ConstInt { destination: zero, value: 0 }], terminator: Terminator::Branch { target: crate::canonical_safewir::BlockId::new(1), arguments: vec![limit, zero] } },
                    BasicBlock { id: crate::canonical_safewir::BlockId::new(1), parameters: vec![BlockParameter { value: header_limit, value_type: crate::canonical_safewir::ValueType::Int, ownership: None }, BlockParameter { value: header_i, value_type: crate::canonical_safewir::ValueType::Int, ownership: None }], operations: vec![Operation::Compare { destination: condition, operator: ComparisonOperator::LessSigned, left: header_i, right: header_limit }], terminator: Terminator::BranchIf { condition, then_target: crate::canonical_safewir::BlockId::new(2), then_arguments: Vec::new(), else_target: crate::canonical_safewir::BlockId::new(3), else_arguments: vec![header_limit, header_i] } },
                    BasicBlock { id: crate::canonical_safewir::BlockId::new(2), parameters: Vec::new(), operations: vec![Operation::ConstInt { destination: one, value: 1 }, Operation::Arithmetic { destination: next_i, operator: ArithmeticOperator::Add, left: header_i, right: one, overflow: crate::canonical_safewir::OverflowBehavior::Wrap }], terminator: Terminator::Branch { target: crate::canonical_safewir::BlockId::new(1), arguments: vec![header_limit, next_i] } },
                    BasicBlock { id: crate::canonical_safewir::BlockId::new(3), parameters: vec![BlockParameter { value: exit_limit, value_type: crate::canonical_safewir::ValueType::Int, ownership: None }, BlockParameter { value: result, value_type: crate::canonical_safewir::ValueType::Int, ownership: None }], operations: Vec::new(), terminator: Terminator::Return { values: vec![result] } },
                ],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_trap() -> ValidatedSafeWir {
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(14),
            checked_source: CheckedSourceIdentity::from_digest([11; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![scalar_signature_with(FunctionSignatureId::new(9), Vec::new(), Vec::new())],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(9),
                signature: FunctionSignatureId::new(9),
                entry: crate::canonical_safewir::BlockId::new(0),
                value_types: BTreeMap::new(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(0),
                    parameters: Vec::new(),
                    operations: Vec::new(),
                    terminator: Terminator::Trap,
                }],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_irreducible() -> ValidatedSafeWir {
        let condition = ValueId::new(1);
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(15),
            checked_source: CheckedSourceIdentity::from_digest([12; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![scalar_signature_with(FunctionSignatureId::new(10), Vec::new(), Vec::new())],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(10),
                signature: FunctionSignatureId::new(10),
                entry: crate::canonical_safewir::BlockId::new(0),
                value_types: [(condition, crate::canonical_safewir::ValueType::Bool)].into_iter().collect(),
                blocks: vec![
                    BasicBlock { id: crate::canonical_safewir::BlockId::new(0), parameters: Vec::new(), operations: vec![Operation::ConstBool { destination: condition, value: true }], terminator: Terminator::BranchIf { condition, then_target: crate::canonical_safewir::BlockId::new(1), then_arguments: Vec::new(), else_target: crate::canonical_safewir::BlockId::new(2), else_arguments: Vec::new() } },
                    BasicBlock { id: crate::canonical_safewir::BlockId::new(1), parameters: Vec::new(), operations: Vec::new(), terminator: Terminator::Branch { target: crate::canonical_safewir::BlockId::new(3), arguments: Vec::new() } },
                    BasicBlock { id: crate::canonical_safewir::BlockId::new(2), parameters: Vec::new(), operations: Vec::new(), terminator: Terminator::Branch { target: crate::canonical_safewir::BlockId::new(3), arguments: Vec::new() } },
                    BasicBlock { id: crate::canonical_safewir::BlockId::new(3), parameters: Vec::new(), operations: Vec::new(), terminator: Terminator::BranchIf { condition, then_target: crate::canonical_safewir::BlockId::new(1), then_arguments: Vec::new(), else_target: crate::canonical_safewir::BlockId::new(2), else_arguments: Vec::new() } },
                ],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_join_bypass() -> ValidatedSafeWir {
        let condition = ValueId::new(1);
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(16),
            checked_source: CheckedSourceIdentity::from_digest([13; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![scalar_signature_with(FunctionSignatureId::new(11), Vec::new(), Vec::new())],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(11),
                signature: FunctionSignatureId::new(11),
                entry: crate::canonical_safewir::BlockId::new(0),
                value_types: [(condition, crate::canonical_safewir::ValueType::Bool)].into_iter().collect(),
                blocks: vec![
                    BasicBlock { id: crate::canonical_safewir::BlockId::new(0), parameters: Vec::new(), operations: vec![Operation::ConstBool { destination: condition, value: true }], terminator: Terminator::BranchIf { condition, then_target: crate::canonical_safewir::BlockId::new(1), then_arguments: Vec::new(), else_target: crate::canonical_safewir::BlockId::new(2), else_arguments: Vec::new() } },
                    BasicBlock { id: crate::canonical_safewir::BlockId::new(1), parameters: Vec::new(), operations: Vec::new(), terminator: Terminator::BranchIf { condition, then_target: crate::canonical_safewir::BlockId::new(3), then_arguments: Vec::new(), else_target: crate::canonical_safewir::BlockId::new(4), else_arguments: Vec::new() } },
                    BasicBlock { id: crate::canonical_safewir::BlockId::new(2), parameters: Vec::new(), operations: Vec::new(), terminator: Terminator::Branch { target: crate::canonical_safewir::BlockId::new(3), arguments: Vec::new() } },
                    BasicBlock { id: crate::canonical_safewir::BlockId::new(3), parameters: Vec::new(), operations: Vec::new(), terminator: Terminator::Return { values: Vec::new() } },
                    BasicBlock { id: crate::canonical_safewir::BlockId::new(4), parameters: Vec::new(), operations: Vec::new(), terminator: Terminator::Branch { target: crate::canonical_safewir::BlockId::new(4), arguments: Vec::new() } },
                ],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_diamond_chain(depth: u32, cyclic_suffix: bool) -> ValidatedSafeWir {
        assert!(depth > 0);
        let condition = ValueId::new(1);
        let mut blocks = Vec::with_capacity((depth * 3 + 1) as usize);
        for level in 0..depth {
            let branch = crate::canonical_safewir::BlockId::new(level * 3);
            let then_target = crate::canonical_safewir::BlockId::new(level * 3 + 1);
            let else_target = crate::canonical_safewir::BlockId::new(level * 3 + 2);
            let join = crate::canonical_safewir::BlockId::new(level * 3 + 3);
            blocks.push(BasicBlock {
                id: branch,
                parameters: Vec::new(),
                operations: if level == 0 {
                    vec![Operation::ConstBool { destination: condition, value: true }]
                } else {
                    Vec::new()
                },
                terminator: Terminator::BranchIf {
                    condition,
                    then_target,
                    then_arguments: Vec::new(),
                    else_target,
                    else_arguments: Vec::new(),
                },
            });
            for id in [then_target, else_target] {
                blocks.push(BasicBlock {
                    id,
                    parameters: Vec::new(),
                    operations: Vec::new(),
                    terminator: Terminator::Branch { target: join, arguments: Vec::new() },
                });
            }
        }
        let final_block = crate::canonical_safewir::BlockId::new(depth * 3);
        blocks.push(BasicBlock {
            id: final_block,
            parameters: Vec::new(),
            operations: Vec::new(),
            terminator: if cyclic_suffix {
                Terminator::Branch { target: final_block, arguments: Vec::new() }
            } else {
                Terminator::Return { values: Vec::new() }
            },
        });
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(if cyclic_suffix { 18 } else { 17 }),
            checked_source: CheckedSourceIdentity::from_digest([if cyclic_suffix { 15 } else { 14 }; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![scalar_signature_with(FunctionSignatureId::new(12), Vec::new(), Vec::new())],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(12),
                signature: FunctionSignatureId::new(12),
                entry: crate::canonical_safewir::BlockId::new(0),
                value_types: [(condition, crate::canonical_safewir::ValueType::Bool)].into_iter().collect(),
                blocks,
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    #[cfg(feature = "native")]
    pub fn encoded_structured_branch_executes_both_arms() {
        let wasm = encode_validated(checked_branch()).expect("structured branch encodes");
        wasmparser::validate(&wasm).expect("structured branch is valid Wasm");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let function = instance.get_typed_func::<i32, i64>(&mut store, "f6").unwrap();
        assert_eq!(function.call(&mut store, 1).unwrap(), 11);
        assert_eq!(function.call(&mut store, 0).unwrap(), 22);
    }

    #[cfg(feature = "native")]
    pub fn encoded_structured_loop_executes_carried_arguments() {
        let artifact = admit_canonical_for_test(checked_loop()).expect("structured loop artifact admits");
        let wasm = artifact.wasm();
        wasmparser::validate(&wasm).expect("structured loop is valid Wasm");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let function = instance.get_typed_func::<i64, i64>(&mut store, "f7").unwrap();
        assert_eq!(function.call(&mut store, 0).unwrap(), 0);
        assert_eq!(function.call(&mut store, 5).unwrap(), 5);
    }

    pub fn irreducible_cfg_is_refused_without_dispatcher_lowering() {
        assert!(matches!(
            encode_validated(checked_irreducible()),
            Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow { .. }))
        ));
    }

    pub fn variable_call_is_refused_fail_closed() {
        let operation = Operation::VarCall {
            destinations: Vec::new(),
            callee: ValueId::new(1),
            signature: FunctionSignatureId::new(1),
            arguments: Vec::new(),
        };
        assert!(matches!(
            lower_operation(
                FunctionId::new(1),
                crate::canonical_safewir::BlockId::new(0),
                0,
                &operation,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &BTreeMap::new(),
                &LayoutInterner::new(),
                &BTreeMap::new(),
                &mut BTreeMap::new(),
            ),
            Err(LoweringError::UnsupportedOperation { .. })
        ));
    }

    fn checked_var_call() -> ValidatedSafeWir {
        let callable = ValueId::new(1);
        let result = ValueId::new(2);
        let callee_result = ValueId::new(3);
        let caller_signature = scalar_signature(FunctionSignatureId::new(20));
        let callee_signature = scalar_signature(FunctionSignatureId::new(21));
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(20),
            checked_source: CheckedSourceIdentity::from_digest([0x20; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![caller_signature, callee_signature],
            imports: Vec::new(),
            functions: vec![
                Function {
                    id: FunctionId::new(20),
                    signature: FunctionSignatureId::new(20),
                    entry: crate::canonical_safewir::BlockId::new(20),
                    value_types: [(callable, ValueType::Callable(FunctionSignatureId::new(21))), (result, ValueType::Int)].into_iter().collect(),
                    blocks: vec![BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(20),
                        parameters: Vec::new(),
                        operations: vec![
                            Operation::FunctionValue {
                                destination: callable,
                                function: FunctionId::new(21),
                            },
                            Operation::VarCall {
                                destinations: vec![result],
                                callee: callable,
                                signature: FunctionSignatureId::new(21),
                                arguments: Vec::new(),
                            },
                        ],
                        terminator: Terminator::Return { values: vec![result] },
                    }],
                },
                Function {
                    id: FunctionId::new(21),
                    signature: FunctionSignatureId::new(21),
                    entry: crate::canonical_safewir::BlockId::new(21),
                    value_types: [(callee_result, ValueType::Int)].into_iter().collect(),
                    blocks: vec![BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(21),
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstInt { destination: callee_result, value: 42 }],
                        terminator: Terminator::Return { values: vec![callee_result] },
                    }],
                },
            ],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_var_tail_call() -> ValidatedSafeWir {
        let callable = ValueId::new(10);
        let callee_result = ValueId::new(11);
        let tail_site = crate::canonical_safewir::TailSiteId::from_digest([0x21; 32]);
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(21),
            checked_source: CheckedSourceIdentity::from_digest([0x21; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![
                scalar_signature(FunctionSignatureId::new(22)),
                scalar_signature(FunctionSignatureId::new(23)),
            ],
            imports: Vec::new(),
            functions: vec![
                Function {
                    id: FunctionId::new(22),
                    signature: FunctionSignatureId::new(22),
                    entry: crate::canonical_safewir::BlockId::new(22),
                    value_types: [(
                        callable,
                        ValueType::Callable(FunctionSignatureId::new(23)),
                    )]
                    .into_iter()
                    .collect(),
                    blocks: vec![BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(22),
                        parameters: Vec::new(),
                        operations: vec![Operation::FunctionValue {
                            destination: callable,
                            function: FunctionId::new(23),
                        }],
                        terminator: Terminator::VarTailCall {
                            site: tail_site,
                            callee: callable,
                            signature: FunctionSignatureId::new(23),
                            arguments: Vec::new(),
                        },
                    }],
                },
                Function {
                    id: FunctionId::new(23),
                    signature: FunctionSignatureId::new(23),
                    entry: crate::canonical_safewir::BlockId::new(23),
                    value_types: [(callee_result, ValueType::Int)].into_iter().collect(),
                    blocks: vec![BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(23),
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstInt {
                            destination: callee_result,
                            value: 42,
                        }],
                        terminator: Terminator::Return { values: vec![callee_result] },
                    }],
                },
            ],
            proper_tail_sites: vec![crate::canonical_safewir::ProperTailSite {
                id: tail_site,
                function: FunctionId::new(22), discharged_by: None,
            }],
        }).with_proper_tail_locations(vec![
            crate::canonical_safewir::ProperTailLocation::new(
                tail_site,
                FunctionId::new(22),
                crate::canonical_safewir::BlockId::new(22),
            ),
        ]))
    }

    #[cfg(feature = "native")]
    pub fn admitted_scalar_var_tail_call_executes_with_return_call_indirect() {
        let artifact = admit_canonical_for_test(checked_var_tail_call())
            .expect("scalar indirect proper-tail call artifact admits");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let function = instance.get_typed_func::<(), i64>(&mut store, "f22").unwrap();
        assert_eq!(function.call(&mut store, ()).unwrap(), 42);
    }

    fn capability_indirect_parts(tail: bool, value_type: ValueType) -> LoweredModuleParts {
        let caller_id = FunctionId::new(20);
        let callee_id = FunctionId::new(21);
        let caller_signature = FunctionSignatureId::new(20);
        let callee_signature = FunctionSignatureId::new(21);
        let parameter = ValueId::new(0);
        let callable = ValueId::new(1);
        let result = ValueId::new(2);
        let site = crate::canonical_safewir::TailSiteId::from_digest([0x32; 32]);
        let mut operations = vec![Operation::FunctionValue {
            destination: callable,
            function: callee_id,
        }];
        let mut value_types: BTreeMap<_, _> = [
            (parameter, value_type),
            (callable, ValueType::Callable(callee_signature)),
        ].into_iter().collect();
        let terminator = if tail {
            Terminator::VarTailCall {
                site, callee: callable, signature: callee_signature,
                arguments: vec![CallArgument::Value(parameter)],
            }
        } else {
            value_types.insert(result, value_type);
            operations.push(Operation::VarCall {
                destinations: vec![result], callee: callable, signature: callee_signature,
                arguments: vec![CallArgument::Value(parameter)],
            });
            Terminator::Return { values: vec![result] }
        };
        LoweredModuleParts {
            revision: Revision::new(32),
            checked_source: CheckedSourceIdentity::from_digest([0x32; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![
                scalar_signature_with(caller_signature, vec![value_type], vec![value_type]),
                scalar_signature_with(callee_signature, vec![value_type], vec![value_type]),
            ],
            imports: Vec::new(),
            functions: vec![
                Function {
                    id: caller_id, signature: caller_signature,
                    entry: crate::canonical_safewir::BlockId::new(0),
                    value_types,
                    blocks: vec![BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(0),
                        parameters: vec![BlockParameter {
                            value: parameter, value_type, ownership: None,
                        }],
                        operations, terminator,
                    }],
                },
                Function {
                    id: callee_id, signature: callee_signature,
                    entry: crate::canonical_safewir::BlockId::new(0),
                    value_types: [(parameter, value_type)].into_iter().collect(),
                    blocks: vec![BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(0),
                        parameters: vec![BlockParameter {
                            value: parameter, value_type, ownership: None,
                        }],
                        operations: Vec::new(),
                        terminator: Terminator::Return { values: vec![parameter] },
                    }],
                },
            ],
            proper_tail_sites: if tail {
                vec![crate::canonical_safewir::ProperTailSite {
                    id: site, function: caller_id, discharged_by: None,
                }]
            } else { Vec::new() },
        }
    }

    fn capability_indirect_candidate(
        tail: bool,
        value_type: ValueType,
    ) -> CanonicalModule<LoweredCandidate> {
        let candidate = CanonicalModule::from_lowering(capability_indirect_parts(tail, value_type));
        if tail {
            candidate.with_proper_tail_locations(vec![
                crate::canonical_safewir::ProperTailLocation::new(
                    crate::canonical_safewir::TailSiteId::from_digest([0x32; 32]),
                    FunctionId::new(20),
                    crate::canonical_safewir::BlockId::new(0),
                ),
            ])
        } else {
            candidate
        }
    }

    fn indirect_capability_types() -> [ValueType; 2] {
        let kind = crate::canonical_safewir::CapabilityKindId::new(17);
        [ValueType::Capability(kind), ValueType::NullableCapability(kind)]
    }

    fn lower_capability_indirect_for_test(parts: &LoweredModuleParts) -> Result<(), LoweringError> {
        let function = &parts.functions[0];
        let block = &function.blocks[0];
        let signatures = parts.signatures.iter().map(|signature| {
            (signature.id, signature.clone())
        }).collect();
        if matches!(&block.terminator, Terminator::VarTailCall { .. }) {
            lower_terminator(function, block.id, &block.terminator, &signatures).map(|_| ())
        } else {
            lower_operation(
                function.id, block.id, 1, &block.operations[1], &function.value_types,
                &signatures, &BTreeMap::new(), &LayoutInterner::new(),
                &BTreeMap::new(), &mut BTreeMap::new(),
            ).map(|_| ())
        }
    }




    pub fn capability_indirect_contract_forgery_fails_initial_admission() {
        for tail in [false, true] {
            for value_type in indirect_capability_types() {
                for ownership in [ParameterOwnership::Unique, ParameterOwnership::Shared,
                    ParameterOwnership::WriteBack, ParameterOwnership::UniqueWriteBack]
                {
                    let mut parts = capability_indirect_parts(tail, value_type);
                    parts.signatures[1].contract.parameters[0].ownership = ownership;
                    assert!(check_initial_with_test_witness(CanonicalModule::from_lowering(parts)).is_err());
                }
                for effect in [MemoryEffect::Read, MemoryEffect::Retain, MemoryEffect::Release,
                    MemoryEffect::Write, MemoryEffect::Reclaim]
                {
                    let mut parts = capability_indirect_parts(tail, value_type);
                    parts.signatures[1].contract.parameter_effects.insert(0, vec![effect]);
                    assert!(check_initial_with_test_witness(CanonicalModule::from_lowering(parts)).is_err());
                }
                let mut parts = capability_indirect_parts(tail, value_type);
                let block = &mut parts.functions[0].blocks[0];
                if let Terminator::VarTailCall { arguments, .. } = &mut block.terminator {
                    arguments[0] = CallArgument::Move(ValueId::new(0));
                } else if let Operation::VarCall { arguments, .. } = &mut block.operations[1] {
                    arguments[0] = CallArgument::Move(ValueId::new(0));
                } else {
                    panic!("fixture must contain the indirect call");
                }
                assert!(check_initial_with_test_witness(CanonicalModule::from_lowering(parts)).is_err());
            }
        }
    }

    fn checked_direct_call(tail: bool) -> ValidatedSafeWir {
        let result = ValueId::new(9);
        let callee_result = ValueId::new(10);
        let tail_site = crate::canonical_safewir::TailSiteId::from_digest([8; 32]);
        let caller_terminator = if tail {
            Terminator::TailCall { site: tail_site, callee: FunctionId::new(2), arguments: Vec::new() }
        } else {
            Terminator::Return { values: vec![result] }
        };
        let caller_operations = if tail {
            Vec::new()
        } else {
            vec![Operation::Call { destinations: vec![result], callee: FunctionId::new(2), arguments: Vec::new() }]
        };
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(8),
            checked_source: CheckedSourceIdentity::from_digest([5; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).expect("empty layout bundle is valid"),
            signatures: vec![scalar_signature(FunctionSignatureId::new(1)), scalar_signature(FunctionSignatureId::new(2))],
            imports: Vec::new(),
            functions: vec![
                Function {
                    id: FunctionId::new(1),
                    signature: FunctionSignatureId::new(1),
                    entry: crate::canonical_safewir::BlockId::new(1),
                    value_types: if tail { BTreeMap::new() } else { [(result, crate::canonical_safewir::ValueType::Int)].into_iter().collect() },
                    blocks: vec![BasicBlock { id: crate::canonical_safewir::BlockId::new(1), parameters: Vec::new(), operations: caller_operations, terminator: caller_terminator }],
                },
                Function {
                    id: FunctionId::new(2),
                    signature: FunctionSignatureId::new(2),
                    entry: crate::canonical_safewir::BlockId::new(2),
                    value_types: [(callee_result, crate::canonical_safewir::ValueType::Int)].into_iter().collect(),
                    blocks: vec![BasicBlock { id: crate::canonical_safewir::BlockId::new(2), parameters: Vec::new(), operations: vec![Operation::ConstInt { destination: callee_result, value: 17 }], terminator: Terminator::Return { values: vec![callee_result] } }],
                },
            ],
            proper_tail_sites: if tail { vec![crate::canonical_safewir::ProperTailSite { id: tail_site, function: FunctionId::new(1), discharged_by: None }] } else { Vec::new() },
        });
        let candidate = if tail {
            candidate.with_proper_tail_locations(vec![
                crate::canonical_safewir::ProperTailLocation::new(
                    tail_site,
                    FunctionId::new(1),
                    crate::canonical_safewir::BlockId::new(1),
                ),
            ])
        } else {
            candidate
        };
        let checked = check_initial_with_test_witness(candidate).expect("direct scalar call is initially valid");
        validate_final_for_test(checked).expect("direct scalar call is final-valid")
    }

    fn checked_parameterized_scalar_call() -> ValidatedSafeWir {
        let parameter = ValueId::new(50);
        let one = ValueId::new(51);
        let incremented = ValueId::new(52);
        let argument = ValueId::new(53);
        let result = ValueId::new(54);
        let increment_signature = scalar_signature_with(
            FunctionSignatureId::new(50),
            vec![ValueType::Int],
            vec![ValueType::Int],
        );
        let main_signature = scalar_signature_with(
            FunctionSignatureId::new(51),
            Vec::new(),
            vec![ValueType::Int],
        );
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(50),
            checked_source: CheckedSourceIdentity::from_digest([0x50; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), [])
                .expect("empty layout bundle is valid"),
            signatures: vec![increment_signature, main_signature],
            imports: Vec::new(),
            functions: vec![
                Function {
                    id: FunctionId::new(50),
                    signature: FunctionSignatureId::new(50),
                    entry: crate::canonical_safewir::BlockId::new(50),
                    value_types: [
                        (parameter, ValueType::Int),
                        (one, ValueType::Int),
                        (incremented, ValueType::Int),
                    ]
                    .into_iter()
                    .collect(),
                    blocks: vec![BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(50),
                        parameters: vec![BlockParameter {
                            value: parameter,
                            value_type: ValueType::Int,
                            ownership: None,
                        }],
                        operations: vec![
                            Operation::ConstInt { destination: one, value: 1 },
                            Operation::Arithmetic {
                                destination: incremented,
                                operator: ArithmeticOperator::Add,
                                left: parameter,
                                right: one,
                                overflow: crate::canonical_safewir::OverflowBehavior::Wrap,
                            },
                        ],
                        terminator: Terminator::Return { values: vec![incremented] },
                    }],
                },
                Function {
                    id: FunctionId::new(51),
                    signature: FunctionSignatureId::new(51),
                    entry: crate::canonical_safewir::BlockId::new(51),
                    value_types: [(argument, ValueType::Int), (result, ValueType::Int)]
                        .into_iter()
                        .collect(),
                    blocks: vec![BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(51),
                        parameters: Vec::new(),
                        operations: vec![
                            Operation::ConstInt { destination: argument, value: 41 },
                            Operation::Call {
                                destinations: vec![result],
                                callee: FunctionId::new(50),
                                arguments: vec![CallArgument::Value(argument)],
                            },
                        ],
                        terminator: Terminator::Return { values: vec![result] },
                    }],
                },
            ],
            proper_tail_sites: Vec::new(),
        });
        let checked = check_initial_with_test_witness(candidate)
            .expect("parameterized scalar call is initially valid");
        validate_final_for_test(checked).expect("parameterized scalar call is final-valid")
    }

    fn checked_object_call_move() -> ValidatedSafeWir {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::U32).unwrap();
        let layout = interner.intern_packed_list(element, RcHeader::Required).unwrap();
        let layouts = LayoutBundle::from_interner(&interner, [layout]).unwrap();
        let count = ValueId::new(40);
        let capacity = ValueId::new(41);
        let object = ValueId::new(42);
        let parameter = ValueId::new(43);
        let clone = ValueId::new(44);
        let index = ValueId::new(45);
        let value = ValueId::new(46);
        let within_capacity = ValueId::new(47);
        let caller_signature = FunctionSignature {
            id: FunctionSignatureId::new(40),
            parameters: Vec::new(),
            results: Vec::new(),
            contract: CallableContract { parameters: Vec::new(), results: Vec::new(), parameter_effects: BTreeMap::new() },
        };
        let callee_signature = FunctionSignature {
            id: FunctionSignatureId::new(41),
            parameters: vec![ValueType::Ref(layout)],
            results: Vec::new(),
            contract: CallableContract {
                parameters: vec![ParameterContract {
                    value_type: ValueType::Ref(layout),
                    ownership: ParameterOwnership::Shared,
                }],
                results: Vec::new(),
                parameter_effects: [(0, vec![MemoryEffect::Release])].into_iter().collect(),
            },
        };
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(40),
            checked_source: CheckedSourceIdentity::from_digest([0x40; 32]),
            layouts,
            signatures: vec![caller_signature, callee_signature],
            imports: Vec::new(),
            functions: vec![
                Function {
                    id: FunctionId::new(40),
                    signature: FunctionSignatureId::new(40),
                    entry: crate::canonical_safewir::BlockId::new(40),
                    value_types: [
                        (count, ValueType::Size),
                        (capacity, ValueType::Size),
                        (object, ValueType::Ref(layout)),
                        (clone, ValueType::Ref(layout)),
                        (index, ValueType::Size),
                        (value, ValueType::Size),
                        (within_capacity, ValueType::Bool),
                    ]
                    .into_iter()
                    .collect(),
                    blocks: vec![BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(40),
                        parameters: Vec::new(),
                        operations: vec![
                            Operation::ConstSize { destination: count, value: 1 },
                            Operation::ConstSize { destination: capacity, value: 1 },
                            Operation::Alloc {
                                destination: object,
                                layout,
                                extent: Extent::Counted { header_bytes: 8, count, capacity, stride_bytes: 4 },
                                class: crate::canonical_safewir::AllocationClass::ReferenceCounted,
                            },
                            Operation::ConstSize { destination: index, value: 0 },
                            Operation::ConstSize { destination: value, value: 0 },
                            Operation::Compare {
                                destination: within_capacity,
                                operator: ComparisonOperator::LessUnsigned,
                                left: index,
                                right: capacity,
                            },
                            Operation::Guard { condition: within_capacity, kind: GuardKind::Bounds },
                            Operation::Store {
                                place: Place::projected(object, Projection::Element { index }, ValueType::Size, AccessMode::Write),
                                value,
                                transfer: StoreTransfer::CopyScalar,
                            },
                            Operation::CloneRef { destination: clone, source: object },
                            Operation::Call {
                                destinations: Vec::new(),
                                callee: FunctionId::new(41),
                                arguments: vec![CallArgument::Move(clone)],
                            },
                            Operation::DropRef { object },
                        ],
                        terminator: Terminator::Return { values: Vec::new() },
                    }],
                },
                Function {
                    id: FunctionId::new(41),
                    signature: FunctionSignatureId::new(41),
                    entry: crate::canonical_safewir::BlockId::new(41),
                    value_types: [(parameter, ValueType::Ref(layout))].into_iter().collect(),
                    blocks: vec![BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(41),
                        parameters: vec![BlockParameter { value: parameter, value_type: ValueType::Ref(layout), ownership: Some(crate::canonical_safewir::Ownership::Shared) }],
                        operations: vec![Operation::DropRef { object: parameter }],
                        terminator: Terminator::Return { values: Vec::new() },
                    }],
                },
            ],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_comparison() -> ValidatedSafeWir {
        let left = ValueId::new(1);
        let right = ValueId::new(2);
        let result = ValueId::new(3);
        let signature = FunctionSignature {
            id: FunctionSignatureId::new(3),
            parameters: Vec::new(),
            results: vec![crate::canonical_safewir::ValueType::Bool],
            contract: CallableContract {
                parameters: Vec::new(),
                results: vec![ResultContract::Value(crate::canonical_safewir::ValueType::Bool)],
                parameter_effects: BTreeMap::new(),
            },
        };
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(9),
            checked_source: CheckedSourceIdentity::from_digest([6; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![signature],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(3),
                signature: FunctionSignatureId::new(3),
                entry: crate::canonical_safewir::BlockId::new(3),
                value_types: [
                    (left, crate::canonical_safewir::ValueType::Int),
                    (right, crate::canonical_safewir::ValueType::Int),
                    (result, crate::canonical_safewir::ValueType::Bool),
                ].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(3),
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstInt { destination: left, value: 4 },
                        Operation::ConstInt { destination: right, value: 9 },
                        Operation::Compare { destination: result, operator: ComparisonOperator::LessSigned, left, right },
                    ],
                    terminator: Terminator::Return { values: vec![result] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        let checked = check_initial_with_test_witness(candidate).unwrap();
        validate_final_for_test(checked).unwrap()
    }

    pub fn comparison_encodes_and_validates() {
        let wasm = encode_validated(checked_comparison()).expect("validated comparison encodes");
        wasmparser::validate(&wasm).expect("comparison module is structurally valid");
    }

    fn checked_size_operation_with_input(operation: Operation, input: ValueId, result: ValueId, input_value: i64) -> ValidatedSafeWir {
        let signature = FunctionSignature {
            id: FunctionSignatureId::new(4),
            parameters: Vec::new(),
            results: vec![crate::canonical_safewir::ValueType::Size],
            contract: CallableContract {
                parameters: Vec::new(),
                results: vec![ResultContract::Value(crate::canonical_safewir::ValueType::Size)],
                parameter_effects: BTreeMap::new(),
            },
        };
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(10),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![signature],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(4),
                signature: FunctionSignatureId::new(4),
                entry: crate::canonical_safewir::BlockId::new(4),
                value_types: [(input, crate::canonical_safewir::ValueType::Int), (result, crate::canonical_safewir::ValueType::Size)].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(4),
                    parameters: Vec::new(),
                    operations: vec![Operation::ConstInt { destination: input, value: input_value }, operation],
                    terminator: Terminator::Return { values: vec![result] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        let checked = check_initial_with_test_witness(candidate).unwrap();
        validate_final_for_test(checked).unwrap()
    }

    fn checked_size_operation(operation: Operation, input: ValueId, result: ValueId) -> ValidatedSafeWir {
        checked_size_operation_with_input(operation, input, result, -1)
    }

    #[cfg(feature = "native")]
    pub fn narrow_size_u32_overflow_traps_in_wasmtime() {
        let input = ValueId::new(1);
        let result = ValueId::new(2);
        let validated = checked_size_operation_with_input(
            Operation::NarrowSize { destination: result, value: input },
            input,
            result,
            i64::from(u32::MAX) + 1,
        );
        let wasm = encode_validated(validated).expect("overflow narrow encodes with a trap");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let result = instance.get_typed_func::<(), i32>(&mut store, "f4").unwrap().call(&mut store, ());
        assert!(result.is_err());
    }

    #[cfg(feature = "native")]
    pub fn narrow_size_valid_value_executes() {
        let input = ValueId::new(1);
        let result = ValueId::new(2);
        let validated = checked_size_operation_with_input(
            Operation::NarrowSize { destination: result, value: input },
            input,
            result,
            19,
        );
        let wasm = encode_validated(validated).expect("valid narrow encodes");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let result = instance.get_typed_func::<(), i32>(&mut store, "f4").unwrap().call(&mut store, ()).unwrap();
        assert_eq!(result, 19);
    }

    fn checked_full_u32_size() -> ValidatedSafeWir {
        let result = ValueId::new(1);
        let signature = FunctionSignature {
            id: FunctionSignatureId::new(5),
            parameters: Vec::new(),
            results: vec![crate::canonical_safewir::ValueType::Size],
            contract: CallableContract {
                parameters: Vec::new(),
                results: vec![ResultContract::Value(crate::canonical_safewir::ValueType::Size)],
                parameter_effects: BTreeMap::new(),
            },
        };
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(11),
            checked_source: CheckedSourceIdentity::from_digest([8; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![signature],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(5),
                signature: FunctionSignatureId::new(5),
                entry: crate::canonical_safewir::BlockId::new(5),
                value_types: [(result, crate::canonical_safewir::ValueType::Size)].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(5),
                    parameters: Vec::new(),
                    operations: vec![Operation::ConstSize { destination: result, value: u64::from(u32::MAX) }],
                    terminator: Terminator::Return { values: vec![result] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        let checked = check_initial_with_test_witness(candidate).unwrap();
        validate_final_for_test(checked).unwrap()
    }

    fn checked_packed_list_fixture(oob: bool) -> ValidatedSafeWir {
        let mut layouts = LayoutInterner::new();
        let element = layouts.intern_scalar(ScalarKind::U32).unwrap();
        let list = layouts.intern_packed_list(element, RcHeader::Required).unwrap();
        let layouts = LayoutBundle::from_interner(&layouts, [list]).unwrap();
        let count = ValueId::new(1);
        let capacity = ValueId::new(2);
        let index = ValueId::new(3);
        let condition = ValueId::new(4);
        let object = ValueId::new(5);
        let value = ValueId::new(6);
        let capacity_loaded = ValueId::new(7);
        let new_count = ValueId::new(8);
        let second_index = ValueId::new(9);
        let second_condition = ValueId::new(10);
        let second_value = ValueId::new(11);
        let length_loaded = ValueId::new(12);
        let element_loaded = ValueId::new(13);
        let capacity_condition = ValueId::new(14);
        let second_capacity_condition = ValueId::new(15);
        let (operations, results) = if oob {
            (
                vec![
                    Operation::ConstSize { destination: count, value: 1 },
                    Operation::ConstSize { destination: capacity, value: 1 },
                    Operation::Alloc { destination: object, layout: list, extent: Extent::Counted { header_bytes: 8, count, capacity, stride_bytes: 4 }, class: crate::canonical_safewir::AllocationClass::ReferenceCounted },
                    Operation::ConstSize { destination: second_index, value: 0 },
                    Operation::Compare { destination: second_capacity_condition, operator: ComparisonOperator::LessUnsigned, left: second_index, right: capacity },
                    Operation::Guard { condition: second_capacity_condition, kind: GuardKind::Bounds },
                    Operation::ConstSize { destination: value, value: 0 },
                    Operation::Store { place: Place::projected(object, Projection::Element { index: second_index }, ValueType::Size, AccessMode::Write), value, transfer: StoreTransfer::CopyScalar },
                    Operation::ConstSize { destination: index, value: 1 },
                    Operation::Compare { destination: condition, operator: ComparisonOperator::LessUnsigned, left: index, right: count },
                    Operation::Guard { condition, kind: GuardKind::Bounds },
                    Operation::Compare { destination: capacity_condition, operator: ComparisonOperator::LessUnsigned, left: index, right: capacity },
                    Operation::Guard { condition: capacity_condition, kind: GuardKind::Bounds },
                    Operation::Load { destination: element_loaded, place: Place::projected(object, Projection::Element { index }, ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                    Operation::Reclaim { object },
                ],
                Vec::new(),
            )
        } else {
            (
                vec![
                    Operation::ConstSize { destination: count, value: 1 },
                    Operation::ConstSize { destination: capacity, value: 2 },
                    Operation::ConstSize { destination: index, value: 0 },
                    Operation::Compare { destination: condition, operator: ComparisonOperator::LessUnsigned, left: index, right: count },
                    Operation::Guard { condition, kind: GuardKind::Bounds },
                    Operation::Compare { destination: capacity_condition, operator: ComparisonOperator::LessUnsigned, left: index, right: capacity },
                    Operation::Guard { condition: capacity_condition, kind: GuardKind::Bounds },
                    Operation::Alloc { destination: object, layout: list, extent: Extent::Counted { header_bytes: 8, count, capacity, stride_bytes: 4 }, class: crate::canonical_safewir::AllocationClass::ReferenceCounted },
                    Operation::ConstSize { destination: value, value: 7 },
                    Operation::Store { place: Place::projected(object, Projection::Element { index }, ValueType::Size, AccessMode::Write), value, transfer: StoreTransfer::CopyScalar },
                    Operation::ConstSize { destination: new_count, value: 2 },
                    Operation::ConstSize { destination: second_index, value: 1 },
                    Operation::Compare { destination: second_capacity_condition, operator: ComparisonOperator::LessUnsigned, left: second_index, right: capacity },
                    Operation::Guard { condition: second_capacity_condition, kind: GuardKind::Bounds },
                    Operation::ConstSize { destination: second_value, value: 42 },
                    Operation::Store { place: Place::projected(object, Projection::Element { index: second_index }, ValueType::Size, AccessMode::Write), value: second_value, transfer: StoreTransfer::CopyScalar },
                    Operation::UpdateCount { object, count: new_count },
                    Operation::Load { destination: capacity_loaded, place: Place::projected(object, Projection::Header(HeaderField::Capacity), ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                    Operation::Compare { destination: second_condition, operator: ComparisonOperator::LessUnsigned, left: second_index, right: new_count },
                    Operation::Guard { condition: second_condition, kind: GuardKind::Bounds },
                    Operation::Load { destination: length_loaded, place: Place::projected(object, Projection::Header(HeaderField::Length), ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                    Operation::Load { destination: element_loaded, place: Place::projected(object, Projection::Element { index: second_index }, ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                    Operation::Reclaim { object },
                ],
                vec![capacity_loaded, length_loaded, element_loaded],
            )
        };
        let mut value_types = BTreeMap::new();
        for value in [count, capacity, index] {
            value_types.insert(value, ValueType::Size);
        }
        value_types.insert(condition, ValueType::Bool);
        value_types.insert(capacity_condition, ValueType::Bool);
        value_types.insert(object, ValueType::Ref(list));
        value_types.insert(element_loaded, ValueType::Size);
        value_types.insert(value, ValueType::Size);
        value_types.insert(second_index, ValueType::Size);
        value_types.insert(second_capacity_condition, ValueType::Bool);
        if !oob {
            for value in [capacity_loaded, new_count, second_value, length_loaded] {
                value_types.insert(value, ValueType::Size);
            }
            value_types.insert(second_condition, ValueType::Bool);
        }
        let result_types = results.iter().map(|_| ValueType::Size).collect::<Vec<_>>();
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(if oob { 31 } else { 30 }),
            checked_source: CheckedSourceIdentity::from_digest([if oob { 0x31 } else { 0x30 }; 32]),
            layouts,
            signatures: vec![scalar_signature_with(FunctionSignatureId::new(30), Vec::new(), result_types)],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(30),
                signature: FunctionSignatureId::new(30),
                entry: crate::canonical_safewir::BlockId::new(30),
                value_types,
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(30),
                    parameters: Vec::new(),
                    operations,
                    terminator: Terminator::Return { values: results },
                }],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_alloc_only(
        layout: crate::layout::LayoutId,
        layouts: LayoutBundle,
        class: crate::canonical_safewir::AllocationClass,
        header_bytes: u32,
        stride_bytes: u32,
        capacity_value: u64,
    ) -> ValidatedSafeWir {
        finalize(alloc_only_candidate(layout, layouts, class, header_bytes, stride_bytes, capacity_value))
    }

    fn alloc_only_candidate(
        layout: crate::layout::LayoutId,
        layouts: LayoutBundle,
        class: crate::canonical_safewir::AllocationClass,
        header_bytes: u32,
        stride_bytes: u32,
        capacity_value: u64,
    ) -> CanonicalModule<LoweredCandidate> {
        let count = ValueId::new(1);
        let capacity = ValueId::new(2);
        let object = ValueId::new(3);
        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(32),
            checked_source: CheckedSourceIdentity::from_digest([0x32; 32]),
            layouts,
            signatures: vec![scalar_signature_with(FunctionSignatureId::new(32), Vec::new(), Vec::new())],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(32),
                signature: FunctionSignatureId::new(32),
                entry: crate::canonical_safewir::BlockId::new(32),
                value_types: [(count, ValueType::Size), (capacity, ValueType::Size), (object, ValueType::Ref(layout))].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(32),
                    parameters: Vec::new(),
                    operations: {
                        // Callee-drops: a reference-counted token the function
                        // neither returns nor transfers is released before exit.
                        let mut operations = vec![
                            Operation::ConstSize { destination: count, value: 0 },
                            Operation::ConstSize { destination: capacity, value: capacity_value },
                            Operation::Alloc { destination: object, layout, extent: Extent::Counted { header_bytes, count, capacity, stride_bytes }, class },
                        ];
                        if class == crate::canonical_safewir::AllocationClass::ReferenceCounted {
                            operations.push(Operation::DropRef { object });
                        }
                        operations
                    },
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            }],
            proper_tail_sites: Vec::new(),
        })
    }

    fn checked_int_list_fixture() -> ValidatedSafeWir {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::Int).unwrap();
        let list = interner.intern_packed_list(element, RcHeader::Required).unwrap();
        let layouts = LayoutBundle::from_interner(&interner, [list]).unwrap();
        let count = ValueId::new(1);
        let capacity = ValueId::new(2);
        let index = ValueId::new(3);
        let condition = ValueId::new(4);
        let object = ValueId::new(5);
        let value = ValueId::new(6);
        let loaded = ValueId::new(7);
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(36),
            checked_source: CheckedSourceIdentity::from_digest([0x36; 32]),
            layouts,
            signatures: vec![scalar_signature_with(FunctionSignatureId::new(36), Vec::new(), vec![ValueType::Int])],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(36),
                signature: FunctionSignatureId::new(36),
                entry: crate::canonical_safewir::BlockId::new(36),
                value_types: [
                    (count, ValueType::Size),
                    (capacity, ValueType::Size),
                    (index, ValueType::Size),
                    (condition, ValueType::Bool),
                    (object, ValueType::Ref(list)),
                    (value, ValueType::Int),
                    (loaded, ValueType::Int),
                ].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(36),
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: count, value: 1 },
                        Operation::ConstSize { destination: capacity, value: 1 },
                        Operation::ConstSize { destination: index, value: 0 },
                        Operation::Compare { destination: condition, operator: ComparisonOperator::LessUnsigned, left: index, right: count },
                        Operation::Guard { condition, kind: GuardKind::Bounds },
                        Operation::Alloc { destination: object, layout: list, extent: Extent::Counted { header_bytes: 8, count, capacity, stride_bytes: 8 }, class: crate::canonical_safewir::AllocationClass::ReferenceCounted },
                        Operation::ConstInt { destination: value, value: 19 },
                        Operation::Store { place: Place::projected(object, Projection::Element { index }, ValueType::Int, AccessMode::Write), value, transfer: StoreTransfer::CopyScalar },
                        Operation::Load { destination: loaded, place: Place::projected(object, Projection::Element { index }, ValueType::Int, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                        Operation::Reclaim { object },
                    ],
                    terminator: Terminator::Return { values: vec![loaded] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_scalar_range(
        range_count: u64,
        source_start: u64,
        destination_start: u64,
        move_cells: bool,
    ) -> ValidatedSafeWir {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::Int).unwrap();
        let list = interner.intern_packed_list(element, RcHeader::Required).unwrap();
        let layouts = LayoutBundle::from_interner(&interner, [list]).unwrap();
        let allocation_count = ValueId::new(1);
        let source_start_value = ValueId::new(2);
        let destination_start_value = ValueId::new(3);
        let range_count_value = ValueId::new(4);
        let source = ValueId::new(5);
        let destination = ValueId::new(6);
        let index = ValueId::new(7);
        let value = ValueId::new(8);
        let loaded = ValueId::new(9);
        let index_in_bounds = ValueId::new(10);
        let source_start_in_bounds = ValueId::new(11);
        let destination_start_in_bounds = ValueId::new(12);
        let signature_id = FunctionSignatureId::new(165);
        let function_id = FunctionId::new(165);
        let source_place = Place::projected(
            source,
            Projection::Slice { start: source_start_value, length: range_count_value },
            ValueType::Ref(list),
            AccessMode::Read,
        );
        let destination_place = Place::projected(
            destination,
            Projection::Slice { start: destination_start_value, length: range_count_value },
            ValueType::Ref(list),
            AccessMode::Write,
        );
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(165),
            checked_source: CheckedSourceIdentity::from_digest([0x65; 32]),
            layouts,
            signatures: vec![scalar_signature_with(signature_id, Vec::new(), vec![ValueType::Int])],
            imports: Vec::new(),
            functions: vec![Function {
                id: function_id,
                signature: signature_id,
                entry: crate::canonical_safewir::BlockId::new(165),
                value_types: [
                    (allocation_count, ValueType::Size),
                    (source_start_value, ValueType::Size),
                    (destination_start_value, ValueType::Size),
                    (range_count_value, ValueType::Size),
                    (source, ValueType::Ref(list)),
                    (destination, ValueType::Ref(list)),
                    (index, ValueType::Size),
                    (value, ValueType::Int),
                    (loaded, ValueType::Int),
                    (index_in_bounds, ValueType::Bool),
                    (source_start_in_bounds, ValueType::Bool),
                    (destination_start_in_bounds, ValueType::Bool),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(165),
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: allocation_count, value: 1 },
                        Operation::ConstSize { destination: source_start_value, value: source_start },
                        Operation::ConstSize { destination: destination_start_value, value: destination_start },
                        Operation::ConstSize { destination: range_count_value, value: range_count },
                        Operation::Alloc {
                            destination: source,
                            layout: list,
                            extent: Extent::Counted {
                                header_bytes: 8,
                                count: allocation_count,
                                capacity: allocation_count,
                                stride_bytes: 8,
                            },
                            class: crate::canonical_safewir::AllocationClass::ReferenceCounted,
                        },
                        Operation::Alloc {
                            destination: destination,
                            layout: list,
                            extent: Extent::Counted {
                                header_bytes: 8,
                                count: allocation_count,
                                capacity: allocation_count,
                                stride_bytes: 8,
                            },
                            class: crate::canonical_safewir::AllocationClass::ReferenceCounted,
                        },
                        Operation::ConstSize { destination: index, value: 0 },
                        Operation::ConstInt { destination: value, value: 42 },
                        Operation::Compare {
                            destination: index_in_bounds,
                            operator: ComparisonOperator::LessUnsigned,
                            left: index,
                            right: allocation_count,
                        },
                        Operation::Guard { condition: index_in_bounds, kind: GuardKind::Bounds },
                        Operation::Store {
                            place: Place::projected(source, Projection::Element { index }, ValueType::Int, AccessMode::Write),
                            value,
                            transfer: StoreTransfer::CopyScalar,
                        },
                        Operation::Compare {
                            destination: source_start_in_bounds,
                            operator: ComparisonOperator::LessUnsigned,
                            left: source_start_value,
                            right: allocation_count,
                        },
                        Operation::Guard { condition: source_start_in_bounds, kind: GuardKind::Bounds },
                        Operation::Compare {
                            destination: destination_start_in_bounds,
                            operator: ComparisonOperator::LessUnsigned,
                            left: destination_start_value,
                            right: allocation_count,
                        },
                        Operation::Guard { condition: destination_start_in_bounds, kind: GuardKind::Bounds },
                        if move_cells {
                            Operation::MoveRange {
                                destination: destination_place,
                                source: source_place,
                                count: range_count_value,
                            }
                        } else {
                            Operation::CloneRange {
                                destination: destination_place,
                                source: source_place,
                                count: range_count_value,
                            }
                        },
                        Operation::Load {
                            destination: loaded,
                            place: Place::projected(destination, Projection::Element { index }, ValueType::Int, AccessMode::Read),
                            transfer: LoadTransfer::CopyScalar,
                        },
                        Operation::DropRef { object: source },
                        Operation::DropRef { object: destination },
                    ],
                    terminator: Terminator::Return { values: vec![loaded] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_int_list_loop() -> ValidatedSafeWir {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::Int).unwrap();
        let list = interner.intern_packed_list(element, RcHeader::Required).unwrap();
        let layouts = LayoutBundle::from_interner(&interner, [list]).unwrap();
        let count = ValueId::new(1);
        let capacity = ValueId::new(2);
        let object = ValueId::new(3);
        let index = ValueId::new(4);
        let value = ValueId::new(5);
        let initial_condition = ValueId::new(6);
        let header_object = ValueId::new(7);
        let header_index = ValueId::new(8);
        let header_count = ValueId::new(9);
        let condition = ValueId::new(10);
        let one = ValueId::new(11);
        let next_index = ValueId::new(12);
        let loaded = ValueId::new(13);
        let exit_object = ValueId::new(14);
        let exit_index = ValueId::new(15);
        let exit_count = ValueId::new(16);
        let exit_condition = ValueId::new(17);
        let values = [
            (count, ValueType::Size),
            (capacity, ValueType::Size),
            (object, ValueType::Ref(list)),
            (index, ValueType::Size),
            (value, ValueType::Int),
            (initial_condition, ValueType::Bool),
            (header_object, ValueType::Ref(list)),
            (header_index, ValueType::Size),
            (header_count, ValueType::Size),
            (condition, ValueType::Bool),
            (one, ValueType::Size),
            (next_index, ValueType::Size),
            (loaded, ValueType::Int),
            (exit_object, ValueType::Ref(list)),
            (exit_index, ValueType::Size),
            (exit_count, ValueType::Size),
            (exit_condition, ValueType::Bool),
        ];
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(37),
            checked_source: CheckedSourceIdentity::from_digest([0x37; 32]),
            layouts,
            signatures: vec![scalar_signature_with(FunctionSignatureId::new(37), Vec::new(), vec![ValueType::Int])],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(37),
                signature: FunctionSignatureId::new(37),
                entry: crate::canonical_safewir::BlockId::new(37),
                value_types: values.into_iter().collect(),
                blocks: vec![
                    BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(37),
                        parameters: Vec::new(),
                        operations: vec![
                            Operation::ConstSize { destination: count, value: 1 },
                            Operation::ConstSize { destination: capacity, value: 1 },
                            Operation::Alloc {
                                destination: object,
                                layout: list,
                                extent: Extent::Counted { header_bytes: 8, count, capacity, stride_bytes: 8 },
                                class: crate::canonical_safewir::AllocationClass::ReferenceCounted,
                            },
                            Operation::ConstSize { destination: index, value: 0 },
                            Operation::Compare { destination: initial_condition, operator: ComparisonOperator::LessUnsigned, left: index, right: count },
                            Operation::Guard { condition: initial_condition, kind: GuardKind::Bounds },
                            Operation::ConstInt { destination: value, value: 42 },
                            Operation::Store { place: Place::projected(object, Projection::Element { index }, ValueType::Int, AccessMode::Write), value, transfer: StoreTransfer::CopyScalar },
                        ],
                        terminator: Terminator::Branch {
                            target: crate::canonical_safewir::BlockId::new(38),
                            arguments: vec![object, index, count],
                        },
                    },
                    BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(38),
                        parameters: vec![
                            BlockParameter { value: header_object, value_type: ValueType::Ref(list), ownership: None },
                            BlockParameter { value: header_index, value_type: ValueType::Size, ownership: None },
                            BlockParameter { value: header_count, value_type: ValueType::Size, ownership: None },
                        ],
                        operations: vec![Operation::Compare { destination: condition, operator: ComparisonOperator::LessUnsigned, left: header_index, right: header_count }],
                        terminator: Terminator::BranchIf {
                            condition,
                            then_target: crate::canonical_safewir::BlockId::new(39),
                            then_arguments: Vec::new(),
                            else_target: crate::canonical_safewir::BlockId::new(40),
                            else_arguments: vec![header_object, header_index, header_count],
                        },
                    },
                    BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(39),
                        parameters: Vec::new(),
                        operations: vec![
                            Operation::ConstSize { destination: one, value: 1 },
                            Operation::Arithmetic { destination: next_index, operator: ArithmeticOperator::Add, left: header_index, right: one, overflow: crate::canonical_safewir::OverflowBehavior::Wrap },
                        ],
                        terminator: Terminator::Branch {
                            target: crate::canonical_safewir::BlockId::new(38),
                            arguments: vec![header_object, next_index, header_count],
                        },
                    },
                    BasicBlock {
                        id: crate::canonical_safewir::BlockId::new(40),
                        parameters: vec![
                            BlockParameter { value: exit_object, value_type: ValueType::Ref(list), ownership: None },
                            BlockParameter { value: exit_index, value_type: ValueType::Size, ownership: None },
                            BlockParameter { value: exit_count, value_type: ValueType::Size, ownership: None },
                        ],
                        operations: vec![
                            Operation::Compare { destination: exit_condition, operator: ComparisonOperator::LessUnsigned, left: index, right: exit_count },
                            Operation::Guard { condition: exit_condition, kind: GuardKind::Bounds },
                            Operation::Load { destination: loaded, place: Place::projected(exit_object, Projection::Element { index }, ValueType::Int, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                            Operation::Reclaim { object: exit_object },
                        ],
                        terminator: Terminator::Return { values: vec![loaded] },
                    },
                ],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    // A wide, nullable owned record models the repeated inactive-field
    // initialization and release pattern in the real task.run source body.
    // It must not replicate a graph destructor at every release site.
    fn checked_outlined_memory_fixture() -> ValidatedSafeWir {
        let mut interner = LayoutInterner::new();
        let scalar = interner.intern_scalar(ScalarKind::U32).unwrap();
        let slot = interner.intern_runtime_pointer_value_slot(scalar, PointerSlot::OwnedNullable).unwrap();
        let record = interner.intern_packed_record(vec![slot; 128]).unwrap();
        // Pointer value slots include their catalog-defined storage lane;
        // their extent is not the width of the pointer stored in that lane.
        let crate::layout::LayoutSize::Fixed(record_bytes) = interner.get(record).unwrap().size() else {
            panic!("outlined fixture requires a fixed record layout");
        };
        let layouts = LayoutBundle::from_interner(&interner, [record]).unwrap();
        let result = ValueId::new(0);
        let mut value_types = BTreeMap::from([(result, ValueType::Size)]);
        let mut operations = Vec::new();
        for index in 0..64 {
            let object = ValueId::new(index * 2 + 1);
            let shared = ValueId::new(index * 2 + 2);
            value_types.insert(object, ValueType::Ref(record));
            value_types.insert(shared, ValueType::Ref(record));
            operations.push(Operation::Alloc {
                destination: object,
                layout: record,
                extent: Extent::FixedBytes(u64::from(record_bytes)),
                class: crate::canonical_safewir::AllocationClass::ReferenceCounted,
            });
            for field in 0..128 {
                operations.push(Operation::InitNullableOwned {
                    place: Place::projected(object, Projection::Field { index: field }, ValueType::Ref(scalar), AccessMode::Write),
                });
            }
            operations.push(Operation::CloneRef { destination: shared, source: object });
            operations.push(Operation::DropRef { object: shared });
        }
        // Keep every allocation live until construction finishes, so the
        // free-list assertion measures 64 distinct reclaimed containers even
        // if the allocator reuses free blocks.
        for index in 0..64 {
            operations.push(Operation::DropRef { object: ValueId::new(index * 2 + 1) });
        }
        // Reuse one of the reclaimed blocks. A bump-only allocator would add
        // a 65th node to the free list when this object is dropped below.
        let reused = ValueId::new(129);
        value_types.insert(reused, ValueType::Ref(record));
        operations.push(Operation::Alloc {
            destination: reused,
            layout: record,
            extent: Extent::FixedBytes(u64::from(record_bytes)),
            class: crate::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        for field in 0..128 {
            operations.push(Operation::InitNullableOwned {
                place: Place::projected(reused, Projection::Field { index: field }, ValueType::Ref(scalar), AccessMode::Write),
            });
        }
        operations.push(Operation::DropRef { object: reused });
        operations.push(Operation::ConstSize { destination: result, value: 7 });
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(141),
            checked_source: CheckedSourceIdentity::from_digest([141; 32]),
            layouts,
            signatures: vec![scalar_signature_with(FunctionSignatureId::new(141), Vec::new(), vec![ValueType::Size])],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(141),
                signature: FunctionSignatureId::new(141),
                entry: crate::canonical_safewir::BlockId::new(0),
                value_types,
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(0),
                    parameters: Vec::new(),
                    operations,
                    terminator: Terminator::Return { values: vec![result] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_share_fixture() -> ValidatedSafeWir {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::U32).unwrap();
        let layout = interner.intern_packed_list(element, RcHeader::Required).unwrap();
        let layouts = LayoutBundle::from_interner(&interner, [layout]).unwrap();
        let count = ValueId::new(1);
        let capacity = ValueId::new(2);
        let object = ValueId::new(3);
        let shared = ValueId::new(4);
        let loaded = ValueId::new(5);
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(33),
            checked_source: CheckedSourceIdentity::from_digest([0x33; 32]),
            layouts,
            signatures: vec![scalar_signature_with(FunctionSignatureId::new(33), Vec::new(), vec![ValueType::Size])],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(33),
                signature: FunctionSignatureId::new(33),
                entry: crate::canonical_safewir::BlockId::new(33),
                value_types: [
                    (count, ValueType::Size),
                    (capacity, ValueType::Size),
                    (object, ValueType::Ref(layout)),
                    (shared, ValueType::Ref(layout)),
                    (loaded, ValueType::Size),
                ].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(33),
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: count, value: 1 },
                        Operation::ConstSize { destination: capacity, value: 1 },
                        Operation::Alloc { destination: object, layout, extent: Extent::Counted { header_bytes: 8, count, capacity, stride_bytes: 4 }, class: crate::canonical_safewir::AllocationClass::ReferenceCounted },
                        Operation::CloneRef { destination: shared, source: object },
                        Operation::Release { object: shared },
                        Operation::Load { destination: loaded, place: Place::projected(object, Projection::Header(HeaderField::Length), ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                        Operation::DropRef { object },
                    ],
                    terminator: Terminator::Return { values: vec![loaded] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_two_allocations_fixture() -> ValidatedSafeWir {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::U32).unwrap();
        let layout = interner.intern_packed_list(element, RcHeader::Required).unwrap();
        let layouts = LayoutBundle::from_interner(&interner, [layout]).unwrap();
        let count = ValueId::new(1);
        let capacity = ValueId::new(2);
        let first = ValueId::new(3);
        let index = ValueId::new(4);
        let value = ValueId::new(5);
        let condition = ValueId::new(6);
        let second = ValueId::new(7);
        let loaded = ValueId::new(8);
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(34),
            checked_source: CheckedSourceIdentity::from_digest([0x34; 32]),
            layouts,
            signatures: vec![scalar_signature_with(FunctionSignatureId::new(34), Vec::new(), vec![ValueType::Size])],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(34),
                signature: FunctionSignatureId::new(34),
                entry: crate::canonical_safewir::BlockId::new(34),
                value_types: [
                    (count, ValueType::Size),
                    (capacity, ValueType::Size),
                    (first, ValueType::Ref(layout)),
                    (index, ValueType::Size),
                    (value, ValueType::Size),
                    (condition, ValueType::Bool),
                    (second, ValueType::Ref(layout)),
                    (loaded, ValueType::Size),
                ].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(34),
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: count, value: 1 },
                        Operation::ConstSize { destination: capacity, value: 1 },
                        Operation::Alloc { destination: first, layout, extent: Extent::Counted { header_bytes: 8, count, capacity, stride_bytes: 4 }, class: crate::canonical_safewir::AllocationClass::ReferenceCounted },
                        Operation::ConstSize { destination: index, value: 0 },
                        Operation::Compare { destination: condition, operator: ComparisonOperator::LessUnsigned, left: index, right: count },
                        Operation::Guard { condition, kind: GuardKind::Bounds },
                        Operation::ConstSize { destination: value, value: 7 },
                        Operation::Store { place: Place::projected(first, Projection::Element { index }, ValueType::Size, AccessMode::Write), value, transfer: StoreTransfer::CopyScalar },
                        Operation::Alloc { destination: second, layout, extent: Extent::Counted { header_bytes: 8, count, capacity, stride_bytes: 4 }, class: crate::canonical_safewir::AllocationClass::ReferenceCounted },
                        Operation::Load { destination: loaded, place: Place::projected(first, Projection::Element { index }, ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                        Operation::Reclaim { object: second },
                        Operation::Reclaim { object: first },
                    ],
                    terminator: Terminator::Return { values: vec![loaded] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn rc_boundary_module(share: bool, max_rc: bool) -> Vec<u8> {
        let object = ValueId::new(1);
        let destination = ValueId::new(2);
        let mut layouts = LayoutInterner::new();
        let byte = layouts.intern_scalar(ScalarKind::RawByte).unwrap();
        let object_layout = layouts.intern_runtime_buffer(byte).unwrap();
        let function = Function {
            id: FunctionId::new(90),
            signature: crate::canonical_safewir::FunctionSignatureId::new(90),
            entry: crate::canonical_safewir::BlockId::new(90),
            value_types: [
                (object, ValueType::Ref(object_layout)),
                (destination, ValueType::Ref(object_layout)),
            ]
            .into_iter()
            .collect(),
            blocks: Vec::new(),
        };
        let scratch_count = u32::try_from(drop_worklist::SCRATCH).unwrap();
        // One non-parameter local is the clone destination; the rest are
        // exactly the production destructor's scratch area.
        let local_count = scratch_count + 1;
        let layout = LocalLayout {
            memory_helpers: None,
            owner_scratch: None,
            owner_bridges: Vec::new(),
            indices: [(object, 0), (destination, 1)].into_iter().collect(),
            extra_types: vec![ValType::I32; usize::try_from(local_count).unwrap()],
            scratch: (2..2 + scratch_count).collect(),
            owner_context: Default::default(),
        };
        let mut boundary = WasmFunctionEncoder::new(vec![(local_count, ValType::I32)]);
        if share {
            emit_clone_ref(&mut boundary, &layout, destination, object, &function).unwrap();
            boundary.instruction(&wasm_encoder::Instruction::I32Const(0));
            boundary.instruction(&wasm_encoder::Instruction::LocalGet(1));
            boundary.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
        } else {
            emit_release(&mut boundary, &layout, &function, object, &layouts).unwrap();
        }
        boundary.instruction(&wasm_encoder::Instruction::End);

        let mut state = WasmFunctionEncoder::new(Vec::new());
        state.instruction(&wasm_encoder::Instruction::I32Const(0));
        state.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
        state.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
        state.instruction(&wasm_encoder::Instruction::I64Const(32));
        state.instruction(&wasm_encoder::Instruction::I64Shl);
        state.instruction(&wasm_encoder::Instruction::I32Const(16));
        state.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
        state.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
        state.instruction(&wasm_encoder::Instruction::I64Or);
        state.instruction(&wasm_encoder::Instruction::End);

        let mut types = TypeSection::new();
        types.ty().function(vec![ValType::I32], Vec::<ValType>::new());
        types.ty().function(Vec::<ValType>::new(), vec![ValType::I64]);
        let mut functions = FunctionSection::new();
        functions.function(0);
        functions.function(1);
        let mut memory = MemorySection::new();
        memory.memory(MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        let mut globals = GlobalSection::new();
        globals.global(
            GlobalType { val_type: ValType::I32, mutable: true, shared: false },
            &ConstExpr::i32_const(16),
        );
        globals.global(
            GlobalType { val_type: ValType::I32, mutable: true, shared: false },
            &ConstExpr::i32_const(0),
        );
        globals.global(
            GlobalType { val_type: ValType::I32, mutable: false, shared: false },
            &ConstExpr::i32_const(16),
        );
        let mut exports = ExportSection::new();
        exports.export("boundary", ExportKind::Func, 0);
        exports.export("state", ExportKind::Func, 1);
        let mut code = CodeSection::new();
        code.function(&boundary);
        code.function(&state);
        let mut data = DataSection::new();
        data.active(0, &ConstExpr::i32_const(16), if max_rc { vec![0xff; 4] } else { vec![0; 4] });
        let mut module = WasmModule::new();
        module
            .section(&types)
            .section(&functions)
            .section(&memory)
            .section(&globals)
            .section(&exports)
            .section(&code)
            .section(&data);
        module.finish()
    }

    fn checked_packed_list_allocation_artifact() -> ValidatedSafeWir {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::U32).unwrap();
        let layout = interner.intern_packed_list(element, RcHeader::Required).unwrap();
        let bundle = LayoutBundle::from_interner(&interner, [layout]).unwrap();
        checked_alloc_only(
            layout,
            bundle,
            crate::canonical_safewir::AllocationClass::ReferenceCounted,
            8,
            4,
            1,
        )
    }

    fn checked_pointer_list_transfer_artifact() -> ValidatedSafeWir {
        let mut interner = LayoutInterner::new();
        let byte = interner.intern_scalar(ScalarKind::RawByte).unwrap();
        let string = interner.intern_runtime_buffer(byte).unwrap();
        let slot = interner
            .intern_runtime_pointer_value_slot(string, PointerSlot::Owned)
            .unwrap();
        let list = interner.intern_packed_list(slot, RcHeader::Required).unwrap();
        let LayoutSize::Dynamic {
            base: string_header,
            stride: string_stride,
        } = interner.get(string).unwrap().size()
        else {
            panic!("string layout must be dynamic");
        };
        let LayoutSize::Dynamic {
            base: list_header,
            stride: list_stride,
        } = interner.get(list).unwrap().size()
        else {
            panic!("pointer-list layout must be dynamic");
        };
        let bundle = LayoutBundle::from_interner(&interner, [string, list]).unwrap();
        let zero = ValueId::new(0);
        let one = ValueId::new(1);
        let child = ValueId::new(2);
        let container = ValueId::new(3);
        let in_bounds = ValueId::new(4);
        let loaded = ValueId::new(5);
        let signature = FunctionSignature {
            id: FunctionSignatureId::new(36),
            parameters: Vec::new(),
            results: vec![ValueType::Ref(string)],
            contract: CallableContract {
                parameters: Vec::new(),
                results: vec![ResultContract::Shared { layout: string }],
                parameter_effects: BTreeMap::new(),
            },
        };
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(36),
            checked_source: CheckedSourceIdentity::from_digest([0x36; 32]),
            layouts: bundle,
            signatures: vec![signature],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(36),
                signature: FunctionSignatureId::new(36),
                entry: crate::canonical_safewir::BlockId::new(36),
                value_types: [
                    (zero, ValueType::Size),
                    (one, ValueType::Size),
                    (child, ValueType::Ref(string)),
                    (container, ValueType::Ref(list)),
                    (in_bounds, ValueType::Bool),
                    (loaded, ValueType::Ref(string)),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(36),
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: zero, value: 0 },
                        Operation::ConstSize { destination: one, value: 1 },
                        Operation::Alloc {
                            destination: child,
                            layout: string,
                            extent: Extent::Counted {
                                header_bytes: string_header,
                                count: zero,
                                capacity: one,
                                stride_bytes: string_stride,
                            },
                            class: crate::canonical_safewir::AllocationClass::ReferenceCounted,
                        },
                        Operation::Alloc {
                            destination: container,
                            layout: list,
                            extent: Extent::Counted {
                                header_bytes: list_header,
                                count: one,
                                capacity: one,
                                stride_bytes: list_stride,
                            },
                            class: crate::canonical_safewir::AllocationClass::ReferenceCounted,
                        },
                        Operation::Compare {
                            destination: in_bounds,
                            operator: ComparisonOperator::LessUnsigned,
                            left: zero,
                            right: one,
                        },
                        Operation::Guard { condition: in_bounds, kind: GuardKind::Bounds },
                        Operation::Store {
                            place: Place::projected(
                                container,
                                Projection::Element { index: zero },
                                ValueType::Ref(string),
                                AccessMode::Write,
                            ),
                            value: child,
                            transfer: StoreTransfer::MoveUnique,
                        },
                        Operation::Load {
                            destination: loaded,
                            place: Place::projected(
                                container,
                                Projection::Element { index: zero },
                                ValueType::Ref(string),
                                AccessMode::Read,
                            ),
                            transfer: LoadTransfer::CloneShared,
                        },
                        // Callee-drops: the container's token is released (its
                        // stored child once); the clone is returned and keeps
                        // the child alive.
                        Operation::DropRef { object: container },
                    ],
                    terminator: Terminator::Return { values: vec![loaded] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    fn checked_guard_artifact() -> ValidatedSafeWir {
        let left = ValueId::new(1);
        let right = ValueId::new(2);
        let condition = ValueId::new(3);
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(35),
            checked_source: CheckedSourceIdentity::from_digest([0x35; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![scalar_signature_with(FunctionSignatureId::new(35), Vec::new(), Vec::new())],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(35),
                signature: FunctionSignatureId::new(35),
                entry: crate::canonical_safewir::BlockId::new(35),
                value_types: [
                    (left, ValueType::Size),
                    (right, ValueType::Size),
                    (condition, ValueType::Bool),
                ].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: crate::canonical_safewir::BlockId::new(35),
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: left, value: 0 },
                        Operation::ConstSize { destination: right, value: 1 },
                        Operation::Compare { destination: condition, operator: ComparisonOperator::LessUnsigned, left, right },
                        Operation::Guard { condition, kind: GuardKind::Bounds },
                    ],
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            }],
            proper_tail_sites: Vec::new(),
        }))
    }

    #[path = "nullable_admission.rs"]
    pub mod nullable_admission;

    #[cfg(feature = "native")]
    #[path = "unit_storage.rs"]
    pub mod unit_storage;

    pub fn foreign_entry_section_fails_admission() {
        // The test admission path derives no entry, so an entry section in the
        // bytes is not the encoder's and must be refused before launch.
        let mut bytes = encode_validated(checked_scalar()).unwrap();
        wasm_encoder::Section::append_to(
            &CustomSection { name: Cow::Borrowed(ENTRY_SECTION), data: Cow::Owned(vec![1, 2, b'f', b'0', 0, 1]) },
            &mut bytes,
        );
        assert!(admit_canonical_with_bytes_for_test(checked_scalar(), bytes).is_err());
    }


    #[cfg(feature = "native")]
    pub fn packed_scalar_collection_oob_guard_traps_in_wasmtime() {
        let wasm = encode_validated(checked_packed_list_fixture(true)).expect("OOB fixture encodes");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance.get_typed_func::<(), ()>(&mut store, "f30").unwrap();
        assert!(run.call(&mut store, ()).is_err());
    }

    #[cfg(feature = "native")]
    pub fn consecutive_rc_allocations_reserve_non_overlapping_prefixes() {
        let source = checked_two_allocations_fixture();
        let target = lower_validated(&source).unwrap();
        let receipt = check_correspondence(source.module(), &target).unwrap();
        assert_eq!(receipt.mappings.len(), 13);
        let wasm = encode_validated(source).expect("two scalar collections encode");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance.get_typed_func::<(), i32>(&mut store, "f34").unwrap();
        assert_eq!(run.call(&mut store, ()).unwrap(), 7);
    }

    #[cfg(feature = "native")]
    pub fn allocation_past_linear_memory_grows_or_traps_when_growth_is_refused() {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::U32).unwrap();
        let layout = interner.intern_packed_list(element, RcHeader::Required).unwrap();
        let bundle = LayoutBundle::from_interner(&interner, [layout]).unwrap();
        let wasm = encode_validated(checked_alloc_only(
            layout,
            bundle,
            crate::canonical_safewir::AllocationClass::ReferenceCounted,
            8,
            4,
            16_383,
        ))
        .expect("large allocation module encodes");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        // The allocator grows linear memory for an object past the current
        // size; the growth is observable through the exported bump pointer.
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance.get_typed_func::<(), ()>(&mut store, "f32").unwrap();
        run.call(&mut store, ()).expect("growth succeeds without a store limit");
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        assert!(memory.size(&store) >= 2, "memory grew past its single initial page");
        // A refused growth (the host caps the memory) traps before any write
        // past the end of memory.
        let limits = wasmtime::StoreLimitsBuilder::new().memory_size(65_536).build();
        let mut store = wasmtime::Store::new(&engine, limits);
        store.limiter(|limits| limits);
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance.get_typed_func::<(), ()>(&mut store, "f32").unwrap();
        assert!(run.call(&mut store, ()).is_err(), "refused growth traps");
    }

    #[cfg(feature = "native")]
    pub fn share_at_max_refcount_traps_before_destination_or_refcount_write() {
        let wasm = rc_boundary_module(true, true);
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let boundary = instance.get_typed_func::<i32, ()>(&mut store, "boundary").unwrap();
        assert!(boundary.call(&mut store, 24).is_err());
        let state = instance.get_typed_func::<(), i64>(&mut store, "state").unwrap();
        assert_eq!(state.call(&mut store, ()).unwrap() as u64, u64::from(u32::MAX));
    }

    #[cfg(feature = "native")]
    pub fn release_at_zero_refcount_traps_before_decrement() {
        let wasm = rc_boundary_module(false, false);
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let boundary = instance.get_typed_func::<i32, ()>(&mut store, "boundary").unwrap();
        assert!(boundary.call(&mut store, 24).is_err());
        let state = instance.get_typed_func::<(), i64>(&mut store, "state").unwrap();
        assert_eq!(state.call(&mut store, ()).unwrap(), 0);
    }

    pub fn bump_allocator_class_is_refused_by_initial_check() {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::U32).unwrap();
        let layout = interner.intern_packed_list(element, RcHeader::Required).unwrap();
        let bundle = LayoutBundle::from_interner(&interner, [layout]).unwrap();
        assert!(matches!(
            check_initial_with_test_witness(alloc_only_candidate(
                layout,
                bundle,
                crate::canonical_safewir::AllocationClass::Bump,
                8,
                4,
                1,
            )),
            Err(crate::canonical_safewir::InitialCheckError::UnsupportedMemoryOperation {
                operation: "Alloc(Bump)",
                ..
            })
        ));
    }

    #[cfg(feature = "native")]
    fn run_scalar_range(
        range_count: u64,
        source_start: u64,
        destination_start: u64,
        move_cells: bool,
    ) -> Result<i64, wasmtime::Error> {
        let artifact = admit_canonical_for_test(checked_scalar_range(
            range_count,
            source_start,
            destination_start,
            move_cells,
        ))
        .expect("scalar range fixture admits before execution");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())?;
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
        let run = instance.get_typed_func::<(), i64>(&mut store, "f165")?;
        run.call(&mut store, ())
    }

    #[cfg(feature = "native")]
    fn raw_scalar_clone_range_probe() -> Vec<u8> {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::Int).unwrap();
        let list = interner.intern_packed_list(element, RcHeader::Required).unwrap();
        let source = ValueId::new(1);
        let destination = ValueId::new(2);
        let source_start = ValueId::new(3);
        let destination_start = ValueId::new(4);
        let count = ValueId::new(5);
        let parameters = [
            (source, ValueType::Ref(list)),
            (destination, ValueType::Ref(list)),
            (source_start, ValueType::Size),
            (destination_start, ValueType::Size),
            (count, ValueType::Size),
        ];
        let signature = scalar_signature_with(
            FunctionSignatureId::new(166),
            parameters.iter().map(|(_, value_type)| *value_type).collect(),
            Vec::new(),
        );
        let function = Function {
            id: FunctionId::new(166),
            signature: signature.id,
            entry: crate::canonical_safewir::BlockId::new(166),
            value_types: parameters.iter().copied().collect(),
            blocks: vec![BasicBlock {
                id: crate::canonical_safewir::BlockId::new(166),
                parameters: parameters
                    .iter()
                    .map(|(value, value_type)| BlockParameter { value: *value, value_type: *value_type, ownership: None })
                    .collect(),
                operations: Vec::new(),
                terminator: Terminator::Return { values: Vec::new() },
            }],
        };
        let layout = local_layout(&function, &signature, &interner).unwrap();
        let mut body = WasmFunctionEncoder::new(layout.extra_types.iter().copied().map(|value_type| (1, value_type)));
        let source_place = Place::projected(
            source,
            Projection::Slice { start: source_start, length: count },
            ValueType::Ref(list),
            AccessMode::Read,
        );
        let destination_place = Place::projected(
            destination,
            Projection::Slice { start: destination_start, length: count },
            ValueType::Ref(list),
            AccessMode::Write,
        );
        emit_clone_range(&mut body, &function, &layout, &destination_place, &source_place, count, &interner).unwrap();
        body.instruction(&wasm_encoder::Instruction::End);

        let mut types = TypeSection::new();
        types.ty().function(vec![ValType::I32; 5], Vec::new());
        let mut functions = FunctionSection::new();
        functions.function(0);
        let mut memories = MemorySection::new();
        memories.memory(MemoryType { minimum: 1, maximum: None, memory64: false, shared: false, page_size_log2: None });
        let mut exports = ExportSection::new();
        exports.export("probe", ExportKind::Func, 0);
        let mut code = CodeSection::new();
        code.function(&body);
        let mut module = WasmModule::new();
        module
            .section(&types)
            .section(&functions)
            .section(&memories)
            .section(&exports)
            .section(&code);
        module.finish()
    }

    #[cfg(feature = "native")]
    fn run_raw_scalar_clone_range_probe(arguments: [i32; 5]) -> Result<(), wasmtime::Error> {
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, raw_scalar_clone_range_probe())?;
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
        let run = instance.get_typed_func::<(i32, i32, i32, i32, i32), ()>(&mut store, "probe")?;
        run.call(&mut store, arguments.into())
    }

    #[cfg(feature = "native")]
    fn raw_pointer_clone_range_probe() -> (Vec<u8>, u32, u32) {
        let mut interner = LayoutInterner::new();
        let byte = interner.intern_scalar(ScalarKind::RawByte).unwrap();
        let child = interner.intern_runtime_buffer(byte).unwrap();
        let slot = interner
            .intern_runtime_pointer_value_slot(child, PointerSlot::Owned)
            .unwrap();
        let list = interner.intern_packed_list(slot, RcHeader::Required).unwrap();
        let collection = collection_layout(FunctionId::new(167), &interner, list).unwrap();
        let source = ValueId::new(1);
        let destination = ValueId::new(2);
        let source_start = ValueId::new(3);
        let destination_start = ValueId::new(4);
        let count = ValueId::new(5);
        let parameters = [
            (source, ValueType::Ref(list)),
            (destination, ValueType::Ref(list)),
            (source_start, ValueType::Size),
            (destination_start, ValueType::Size),
            (count, ValueType::Size),
        ];
        let source_place = Place::projected(
            source,
            Projection::Slice { start: source_start, length: count },
            ValueType::Ref(list),
            AccessMode::Read,
        );
        let destination_place = Place::projected(
            destination,
            Projection::Slice { start: destination_start, length: count },
            ValueType::Ref(list),
            AccessMode::Write,
        );
        let operation = Operation::CloneRange {
            destination: destination_place.clone(),
            source: source_place.clone(),
            count,
        };
        let signature = scalar_signature_with(
            FunctionSignatureId::new(167),
            parameters.iter().map(|(_, value_type)| *value_type).collect(),
            Vec::new(),
        );
        let function = Function {
            id: FunctionId::new(167),
            signature: signature.id,
            entry: BlockId::new(167),
            value_types: parameters.iter().copied().collect(),
            blocks: vec![BasicBlock {
                id: BlockId::new(167),
                parameters: parameters
                    .iter()
                    .map(|(value, value_type)| BlockParameter {
                        value: *value,
                        value_type: *value_type,
                        ownership: None,
                    })
                    .collect(),
                operations: vec![operation],
                terminator: Terminator::Return { values: Vec::new() },
            }],
        };
        let layout = local_layout(&function, &signature, &interner).unwrap();
        let mut body = WasmFunctionEncoder::new(
            layout
                .extra_types
                .iter()
                .copied()
                .map(|value_type| (1, value_type)),
        );
        emit_clone_range(
            &mut body,
            &function,
            &layout,
            &destination_place,
            &source_place,
            count,
            &interner,
        )
        .unwrap();
        body.instruction(&wasm_encoder::Instruction::End);

        let mut types = TypeSection::new();
        types.ty().function(vec![ValType::I32; 5], Vec::new());
        let mut functions = FunctionSection::new();
        functions.function(0);
        let mut memories = MemorySection::new();
        memories.memory(MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        let mut globals = GlobalSection::new();
        for value in [0, 0, 16] {
            globals.global(
                GlobalType {
                    val_type: ValType::I32,
                    mutable: true,
                    shared: false,
                },
                &ConstExpr::i32_const(value),
            );
        }
        let mut exports = ExportSection::new();
        exports.export("probe", ExportKind::Func, 0);
        exports.export("memory", ExportKind::Memory, 0);
        let mut code = CodeSection::new();
        code.function(&body);
        let mut module = WasmModule::new();
        module
            .section(&types)
            .section(&functions)
            .section(&memories)
            .section(&globals)
            .section(&exports)
            .section(&code);
        (module.finish(), collection.data_offset, collection.stride)
    }

    fn checked_pointer_clone_range_execution() -> (ValidatedSafeWir, BTreeSet<usize>) {
        let mut interner = LayoutInterner::new();
        let int = interner.intern_scalar(ScalarKind::Int).unwrap();
        let child_list = interner.intern_packed_list(int, RcHeader::Required).unwrap();
        let slot = interner
            .intern_runtime_pointer_value_slot(child_list, PointerSlot::Owned)
            .unwrap();
        let list = interner.intern_packed_list(slot, RcHeader::Required).unwrap();
        let LayoutSize::Dynamic { base: child_header, stride: child_stride } =
            interner.get(child_list).unwrap().size()
        else {
            panic!("child list layout is dynamic")
        };
        let LayoutSize::Dynamic { base: list_header, stride: list_stride } =
            interner.get(list).unwrap().size()
        else {
            panic!("list layout is dynamic")
        };
        let child_root = 24usize;
        let source_root = child_root
            + usize::try_from(child_header + child_stride + RC_PREFIX_BYTES).unwrap();
        let destination_root = source_root
            + usize::try_from(list_header + list_stride + RC_PREFIX_BYTES).unwrap();
        let expected_reclaimed = [child_root, source_root, destination_root]
            .into_iter()
            .collect();
        let zero = ValueId::new(0);
        let one = ValueId::new(1);
        let in_capacity = ValueId::new(2);
        let child = ValueId::new(3);
        let source = ValueId::new(4);
        let destination = ValueId::new(5);
        let loaded = ValueId::new(6);
        let result = ValueId::new(7);
        let element = ValueId::new(8);
        let signature = scalar_signature_with(
            FunctionSignatureId::new(166),
            Vec::new(),
            vec![ValueType::Size],
        );
        let validated = finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(166),
            checked_source: CheckedSourceIdentity::from_digest([0xa6; 32]),
            layouts: LayoutBundle::from_interner(&interner, [list]).unwrap(),
            signatures: vec![signature],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(166),
                signature: FunctionSignatureId::new(166),
                entry: BlockId::new(166),
                value_types: [
                    (zero, ValueType::Size),
                    (one, ValueType::Size),
                    (in_capacity, ValueType::Bool),
                    (child, ValueType::Ref(child_list)),
                    (source, ValueType::Ref(list)),
                    (destination, ValueType::Ref(list)),
                    (loaded, ValueType::Ref(child_list)),
                    (result, ValueType::Size),
                    (element, ValueType::Int),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: BlockId::new(166),
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: zero, value: 0 },
                        Operation::ConstSize { destination: one, value: 1 },
                        Operation::Compare {
                            destination: in_capacity,
                            operator: ComparisonOperator::LessUnsigned,
                            left: zero,
                            right: one,
                        },
                        Operation::Guard { condition: in_capacity, kind: GuardKind::Bounds },
                        Operation::ConstInt { destination: element, value: 73 },
                        Operation::Alloc {
                            destination: child,
                            layout: child_list,
                            extent: Extent::Counted {
                                header_bytes: child_header,
                                count: zero,
                                capacity: one,
                                stride_bytes: child_stride,
                            },
                            class: crate::canonical_safewir::AllocationClass::ReferenceCounted,
                        },
                        Operation::Store {
                            place: Place::projected(
                                child,
                                Projection::Element { index: zero },
                                ValueType::Int,
                                AccessMode::Write,
                            ),
                            value: element,
                            transfer: StoreTransfer::CopyScalar,
                        },
                        Operation::UpdateCount { object: child, count: one },
                        Operation::Alloc {
                            destination: source,
                            layout: list,
                            extent: Extent::Counted {
                                header_bytes: list_header,
                                count: zero,
                                capacity: one,
                                stride_bytes: list_stride,
                            },
                            class: crate::canonical_safewir::AllocationClass::ReferenceCounted,
                        },
                        Operation::Store {
                            place: Place::projected(
                                source,
                                Projection::Element { index: zero },
                                ValueType::Ref(child_list),
                                AccessMode::Write,
                            ),
                            value: child,
                            transfer: StoreTransfer::MoveUnique,
                        },
                        Operation::UpdateCount { object: source, count: one },
                        Operation::Alloc {
                            destination,
                            layout: list,
                            extent: Extent::Counted {
                                header_bytes: list_header,
                                count: zero,
                                capacity: one,
                                stride_bytes: list_stride,
                            },
                            class: crate::canonical_safewir::AllocationClass::ReferenceCounted,
                        },
                        Operation::CloneRange {
                            destination: Place::projected(
                                destination,
                                Projection::Slice { start: zero, length: one },
                                ValueType::Ref(list),
                                AccessMode::Write,
                            ),
                            source: Place::projected(
                                source,
                                Projection::Slice { start: zero, length: one },
                                ValueType::Ref(list),
                                AccessMode::Read,
                            ),
                            count: one,
                        },
                        Operation::UpdateCount { object: destination, count: one },
                        Operation::DropRef { object: source },
                        Operation::Load {
                            destination: loaded,
                            place: Place::projected(
                                destination,
                                Projection::Element { index: zero },
                                ValueType::Ref(child_list),
                                AccessMode::Read,
                            ),
                            transfer: LoadTransfer::CloneShared,
                        },
                        Operation::Load {
                            destination: result,
                            place: Place::projected(
                                loaded,
                                Projection::Header(crate::canonical_safewir::HeaderField::Length),
                                ValueType::Size,
                                AccessMode::Read,
                            ),
                            transfer: LoadTransfer::CopyScalar,
                        },
                        Operation::DropRef { object: loaded },
                        Operation::DropRef { object: destination },
                    ],
                    terminator: Terminator::Return { values: vec![result] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        }));
        (validated, expected_reclaimed)
    }

    #[cfg(feature = "native")]
    pub fn pointer_clone_range_retains_executes_and_recursively_releases() {
        let (validated, expected_reclaimed) = checked_pointer_clone_range_execution();
        let artifact = admit_canonical_for_test(validated)
            .expect("pointer CloneRange admits through exact-byte matching");
        wasmparser::validate(artifact.wasm()).expect("admitted pointer CloneRange is valid Wasm");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance
            .get_typed_func::<(), i32>(&mut store, "f166")
            .unwrap();
        assert_eq!(run.call(&mut store, ()).unwrap(), 1);
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        let mut free = instance
            .get_global(&mut store, HEAP_FREE_EXPORT)
            .unwrap()
            .get(&mut store)
            .i32()
            .unwrap() as usize;
        let mut reclaimed = BTreeSet::new();
        while free != 0 {
            assert!(reclaimed.insert(free), "free-list cycle or double release");
            free = u32::from_le_bytes(memory.data(&store)[free..free + 4].try_into().unwrap()) as usize;
        }
        assert_eq!(
            reclaimed, expected_reclaimed,
            "child, source, and destination must each be reclaimed exactly once",
        );
    }

    pub fn malformed_clone_range_places_are_refused_by_canonical_backend() {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::Int).unwrap();
        let list = interner.intern_packed_list(element, RcHeader::Required).unwrap();
        let count = ValueId::new(1);
        let wrong_length = ValueId::new(2);
        let source_start = ValueId::new(3);
        let destination_start = ValueId::new(4);
        let source = ValueId::new(5);
        let destination = ValueId::new(6);
        let value_types = [
            (count, ValueType::Size),
            (wrong_length, ValueType::Size),
            (source_start, ValueType::Size),
            (destination_start, ValueType::Size),
            (source, ValueType::Ref(list)),
            (destination, ValueType::Ref(list)),
        ]
        .into_iter()
        .collect();
        let source_place = Place::projected(
            source,
            Projection::Slice { start: source_start, length: count },
            ValueType::Ref(list),
            AccessMode::Read,
        );
        let mismatched_destination = Place::projected(
            destination,
            Projection::Slice { start: destination_start, length: wrong_length },
            ValueType::Ref(list),
            AccessMode::Write,
        );
        let mismatched = Operation::CloneRange {
            destination: mismatched_destination,
            source: source_place.clone(),
            count,
        };
        let non_slice = Operation::CloneRange {
            destination: Place::projected(destination, Projection::Element { index: destination_start }, ValueType::Int, AccessMode::Write),
            source: source_place,
            count,
        };
        for operation in [mismatched, non_slice] {
            assert!(matches!(
                lower_operation(
                    FunctionId::new(165),
                    crate::canonical_safewir::BlockId::new(165),
                    0,
                    &operation,
                    &value_types,
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                    &interner,
                    &BTreeMap::new(),
                    &mut BTreeMap::new(),
                ),
                Err(LoweringError::UnsupportedLayout { operation: "CloneRange", .. })
            ));
        }
    }

    #[cfg(feature = "native")]
    pub fn scalar_clone_range_count_stride_overflow_traps_before_copy() {
        assert!(run_raw_scalar_clone_range_probe([0, 0, 0, 0, u32::MAX as i32]).is_err());
    }


    pub fn full_u32_size_constant_encodes_and_validates() {
        let wasm = encode_validated(checked_full_u32_size()).expect("full u32 size encodes");
        wasmparser::validate(&wasm).expect("full u32 size module is structurally valid");
    }

    #[cfg(feature = "native")]
    pub fn encoded_scalar_module_executes() {
        let wasm = encode_validated(checked_scalar()).expect("validated scalar authority encodes");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).expect("wasmtime accepts encoded scalar");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("instance creates");
        let function = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
            .expect("scalar export has the checked signature");
        assert_eq!(function.call(&mut store, ()).unwrap(), 9);
    }



}
