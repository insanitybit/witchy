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
mod drop_worklist;
mod outlined_memory;
mod scalar_locals;
mod closure_owner_kernel;
mod owner_bridges;
mod owner_operation_kernel;
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

fn scalar_arguments(
    _function: FunctionId,
    arguments: &[CallArgument],
) -> Result<Vec<ValueId>, LoweringError> {
    arguments
        .iter()
        .map(|argument| match argument {
            CallArgument::Value(value) | CallArgument::Move(value) => Ok(*value),
        })
        .collect()
}

fn host_scalar_wasm_type(
    function: FunctionId,
    value: ValueId,
    value_type: ValueType,
) -> Result<ValType, LoweringError> {
    match value_type {
        ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Size | ValueType::V128 =>
            wasm_value_type(function, value, value_type).map_err(|_| LoweringError::UnsupportedOperation { function, operation: "HostCall value" }),
        _ => Err(LoweringError::UnsupportedOperation { function, operation: "HostCall value" }),
    }
}

fn runtime_byte_slice(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    argument: &HostCallArgument,
) -> Result<(ValueId, ValueId, ValueId), LoweringError> {
    let HostCallArgument::Buffer { place, extent: HostBufferExtent::Slice } = argument else {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer" });
    };
    let Projection::Slice { start, length } = place.projection() else {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer projection" });
    };
    raw_byte_slice_place(function, value_types, layouts, place, AccessMode::Read, *length)?;
    Ok((place.object(), *start, *length))
}

fn raw_byte_slice_place(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    place: &Place,
    access: AccessMode,
    operation_length: ValueId,
) -> Result<(ValueId, ValueId, ValueId), LoweringError> {
    if place.access() != access {
        return Err(LoweringError::UnsupportedOperation { function, operation: "CopyBytes access" });
    }
    let Projection::Slice { start, length } = place.projection() else {
        return Err(LoweringError::UnsupportedOperation { function, operation: "CopyBytes projection" });
    };
    if *length != operation_length {
        return Err(LoweringError::UnsupportedOperation { function, operation: "CopyBytes length" });
    }
    if value_types.get(start) != Some(&ValueType::Size) || value_types.get(length) != Some(&ValueType::Size) {
        return Err(LoweringError::UnsupportedOperation { function, operation: "CopyBytes slice bounds" });
    }
    let ValueType::Ref(layout) = value_types.get(&place.object()).copied().ok_or(
        LoweringError::UnsupportedOperation { function, operation: "CopyBytes object" },
    )? else {
        return Err(LoweringError::UnsupportedOperation { function, operation: "CopyBytes object" });
    };
    if place.value_type() != ValueType::Ref(layout) {
        return Err(LoweringError::UnsupportedOperation { function, operation: "CopyBytes type" });
    }
    let physical_layout = resolve_backend_layout(function, layouts, layout, "CopyBytes layout")?;
    let descriptor = layouts.get(physical_layout).ok_or(LoweringError::UnsupportedLayout { function, operation: "CopyBytes layout" })?;
    let LayoutKind::RuntimeBuffer { element } = descriptor.kind() else {
        return Err(LoweringError::UnsupportedLayout { function, operation: "CopyBytes layout" });
    };
    let physical_element = resolve_backend_layout(function, layouts, *element, "CopyBytes element")?;
    let element = layouts.get(physical_element).ok_or(LoweringError::UnsupportedLayout { function, operation: "CopyBytes element" })?;
    if element.kind() != &LayoutKind::Scalar(ScalarKind::RawByte) {
        return Err(LoweringError::UnsupportedLayout { function, operation: "CopyBytes element" });
    }
    Ok((place.object(), *start, *length))
}

fn validate_host_call_lowering(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    import: &ImportContract,
    arguments: &[HostCallArgument],
    destinations: &[ValueId],
) -> Result<(), LoweringError> {
    if !matches!(import.abi_identity, ImportContractIdentity::AuthenticatedAbi(_))
        || !matches!(import.mutation, HostMutation::ReadOnly | HostMutation::WriteBuffers)
    {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall contract" });
    }
    if import.parameter_roles.len() != arguments.len() || import.result_roles.len() != destinations.len() {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall arity" });
    }
    for (role, argument) in import.parameter_roles.iter().zip(arguments) {
        match (role, argument) {
            (HostParameterRole::Value(expected), HostCallArgument::Value(value)) => {
                if value_types.get(value) != Some(expected) {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall value type" });
                }
                host_scalar_wasm_type(function, *value, *expected)?;
            }
            (HostParameterRole::Capability(expected), HostCallArgument::Capability(value)) => {
                if value_types.get(value) != Some(&ValueType::Capability(*expected)) {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall capability type" });
                }
            }
            (
                HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice },
                HostCallArgument::Buffer { place, extent: HostBufferExtent::Slice },
            ) => {
                if place.value_type() != ValueType::Ref(*layout) {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer type" });
                }
                let _ = runtime_byte_slice(function, value_types, layouts, argument)?;
            }
            // A checked write slice: the same (ptr, len) pair as a read
            // slice, with the length the buffer's capacity. The checker
            // bounds the range by that capacity, and the guest publishes the
            // written count itself under its own guard.
            (
                HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Write, extent: HostBufferExtent::Slice },
                HostCallArgument::Buffer { place, extent: HostBufferExtent::Slice },
            ) => {
                if place.value_type() != ValueType::Ref(*layout) {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer type" });
                }
                let Projection::Slice { length, .. } = place.projection() else {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer projection" });
                };
                let _ = raw_byte_slice_place(function, value_types, layouts, place, AccessMode::Write, *length)?;
            }
            // A write buffer of a declared minimum size: the host receives
            // the checked slice's address only; the checker bounded the
            // slice by the object's capacity.
            (
                HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Write, extent: HostBufferExtent::AtLeast(_) },
                HostCallArgument::Buffer { place, extent: HostBufferExtent::AtLeast(_) },
            ) => {
                if place.value_type() != ValueType::Ref(*layout) {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer type" });
                }
                let Projection::Slice { length, .. } = place.projection() else {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer projection" });
                };
                let _ = raw_byte_slice_place(function, value_types, layouts, place, AccessMode::Write, *length)?;
            }
            _ => return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall argument" }),
        }
    }
    for (role, destination) in import.result_roles.iter().zip(destinations) {
        let expected = match role {
            HostResultRole::Value(value_type) => *value_type,
            HostResultRole::Capability(kind) => ValueType::Capability(*kind),
            HostResultRole::NullableCapability(kind) => ValueType::NullableCapability(*kind),
        };
        if value_types.get(destination) != Some(&expected) {
            return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall result type" });
        }
        if let HostResultRole::Value(value_type) = role {
            host_scalar_wasm_type(function, *destination, *value_type)?;
        }
    }
    Ok(())
}

fn validate_external_call_lowering(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    import: &ImportContract,
    signature: &FunctionSignature,
    arguments: &[CallArgument],
    destinations: &[ValueId],
) -> Result<(), LoweringError> {
    let scalar = |value| matches!(value, ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit);
    if !matches!(import.abi_identity, ImportContractIdentity::AuthenticatedExternal(_))
        || !import.parameter_roles.is_empty()
        || !import.result_roles.is_empty()
        || !matches!(import.mutation, HostMutation::ReadOnly)
        || signature.parameters.len() != arguments.len()
        || signature.results.len() != destinations.len()
        || signature.contract.parameters.len() != signature.parameters.len()
        || signature.contract.results.len() != signature.results.len()
        || !signature.parameters.iter().copied().all(scalar)
        || !signature.results.iter().copied().all(scalar)
        || !signature.contract.parameter_effects.is_empty()
    {
        return Err(LoweringError::UnsupportedOperation { function, operation: "ExternalCall contract" });
    }
    for ((argument, expected), contract) in arguments.iter().zip(&signature.parameters).zip(&signature.contract.parameters) {
        let CallArgument::Value(value) = argument else {
            return Err(LoweringError::UnsupportedOperation { function, operation: "ExternalCall move argument" });
        };
        if value_types.get(value) != Some(expected)
            || contract.value_type != *expected
            || contract.ownership != ParameterOwnership::Value
        {
            return Err(LoweringError::UnsupportedOperation { function, operation: "ExternalCall argument" });
        }
        wasm_value_type(function, *value, *expected)
            .map_err(|_| LoweringError::UnsupportedOperation { function, operation: "ExternalCall scalar argument" })?;
    }
    for ((destination, expected), contract) in destinations.iter().zip(&signature.results).zip(&signature.contract.results) {
        if value_types.get(destination) != Some(expected)
            || !matches!(contract, crate::canonical_safewir::ResultContract::Value(value) if value == expected)
        {
            return Err(LoweringError::UnsupportedOperation { function, operation: "ExternalCall result" });
        }
        wasm_value_type(function, *destination, *expected)
            .map_err(|_| LoweringError::UnsupportedOperation { function, operation: "ExternalCall scalar result" })?;
    }
    Ok(())
}

fn host_import_wasm_signature(
    function: FunctionId,
    import: &ImportContract,
) -> Result<(Vec<ValType>, Vec<ValType>), LoweringError> {
    let mut parameters = Vec::new();
    for role in &import.parameter_roles {
        match role {
            HostParameterRole::Value(value_type) => parameters.push(host_scalar_wasm_type(function, ValueId::new(u32::MAX), *value_type)?),
            HostParameterRole::Capability(_) => parameters.push(ValType::EXTERNREF),
            HostParameterRole::Buffer { value_type: ValueType::Ref(_), access: AccessMode::Read, extent: HostBufferExtent::Slice }
            | HostParameterRole::Buffer { value_type: ValueType::Ref(_), access: AccessMode::Write, extent: HostBufferExtent::Slice } => {
                parameters.extend([ValType::I32, ValType::I32]);
            }
            HostParameterRole::Buffer { value_type: ValueType::Ref(_), access: AccessMode::Write, extent: HostBufferExtent::AtLeast(_) } => {
                parameters.push(ValType::I32);
            }
            _ => return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall import signature" }),
        }
    }
    let mut results = Vec::new();
    for role in &import.result_roles {
        match role {
            HostResultRole::Value(value_type) => results.push(host_scalar_wasm_type(function, ValueId::new(u32::MAX), *value_type)?),
            HostResultRole::Capability(_) | HostResultRole::NullableCapability(_) => results.push(ValType::EXTERNREF),
        }
    }
    Ok((parameters, results))
}

fn external_import_wasm_signature(
    function: FunctionId,
    signature: &FunctionSignature,
) -> Result<(Vec<ValType>, Vec<ValType>), LoweringError> {
    let scalar = |value| matches!(value, ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit);
    if !signature.parameters.iter().copied().all(scalar)
        || !signature.results.iter().copied().all(scalar)
    {
        return Err(LoweringError::UnsupportedOperation { function, operation: "ExternalCall import signature" });
    }
    // Unit is source-level evidence only at this ABI: it consumes and
    // produces no Wasm lane.  In particular, an exported `() -> ()` has an
    // empty result vector rather than an invented i32 result.
    let parameters = signature.parameters.iter().enumerate()
        .filter(|(_, value)| **value != ValueType::Unit)
        .map(|(index, value)| wasm_value_type(function, ValueId::new(index as u32), *value)
            .map_err(|_| LoweringError::UnsupportedOperation { function, operation: "ExternalCall import parameter" }))
        .collect::<Result<Vec<_>, _>>()?;
    let results = signature.results.iter().enumerate()
        .filter(|(_, value)| **value != ValueType::Unit)
        .map(|(index, value)| wasm_value_type(function, ValueId::new(index as u32), *value)
            .map_err(|_| LoweringError::UnsupportedOperation { function, operation: "ExternalCall import result" }))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((parameters, results))
}

fn import_wasm_signature(
    function: FunctionId,
    import: &ImportContract,
    signatures: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, FunctionSignature>,
) -> Result<(Vec<ValType>, Vec<ValType>), LoweringError> {
    match &import.abi_identity {
        ImportContractIdentity::AuthenticatedAbi(_) => host_import_wasm_signature(function, import),
        ImportContractIdentity::AuthenticatedExternal(_) => {
            let signature = signatures.get(&import.signature)
                .ok_or(LoweringError::UnsupportedOperation { function, operation: "ExternalCall missing signature" })?;
            external_import_wasm_signature(function, signature)
        }
        ImportContractIdentity::Unknown => Err(LoweringError::UnsupportedOperation { function, operation: "unknown import identity" }),
    }
}

fn collection_layout(
    function: FunctionId,
    layouts: &LayoutInterner,
    layout: crate::layout::LayoutId,
) -> Result<CollectionLayout, LoweringError> {
    let physical_layout = resolve_backend_layout(function, layouts, layout, "layout")?;
    let descriptor = layouts
        .get(physical_layout)
        .ok_or(LoweringError::UnsupportedLayout { function, operation: "layout" })?;
    let (element, raw_runtime_buffer) = match descriptor.kind() {
        LayoutKind::PackedList { element, .. } => (*element, false),
        LayoutKind::RuntimeBuffer { element } => (*element, true),
        _ => return Err(LoweringError::UnsupportedLayout { function, operation: "layout" }),
    };
    let physical_element = resolve_backend_layout(function, layouts, element, "layout")?;
    let element_descriptor = layouts
        .get(physical_element)
        .ok_or(LoweringError::UnsupportedLayout { function, operation: "layout" })?;
    let (scalar, pointer_layout, pointer_slot) = match element_descriptor.kind() {
        LayoutKind::Scalar(scalar) if *scalar == ScalarKind::Unit || scalar.is_ordinary_lowering_target() => (*scalar, None, None),
        LayoutKind::Scalar(ScalarKind::RawByte) if raw_runtime_buffer => {
            (ScalarKind::RawByte, None, None)
        }
        LayoutKind::RuntimePointerValueSlot {
            pointee,
            semantics,
        } if matches!(semantics, PointerSlot::Owned | PointerSlot::OwnedNullable) => {
            (ScalarKind::U32, Some(*pointee), Some(*semantics))
        }
        _ => return Err(LoweringError::UnsupportedLayout { function, operation: "pointer-bearing layout" }),
    };
    let LayoutSize::Dynamic { base, stride } = descriptor.size() else {
        return Err(LoweringError::UnsupportedLayout { function, operation: "layout" });
    };
    let (length_offset, capacity_offset, data_offset) = match (
        raw_runtime_buffer,
        descriptor.header(),
    ) {
        (true, HeaderLayout::None) if base == 4 && stride == 1 => (0, None, 4),
        (
            false,
            HeaderLayout::PackedList {
                length_offset,
                capacity_offset,
                data_offset,
                rc: RcHeader::Required,
            },
        ) => (length_offset, Some(capacity_offset), data_offset),
        _ => return Err(LoweringError::UnsupportedLayout { function, operation: "layout" }),
    };
    let LayoutSize::Fixed(element_size) = element_descriptor.size() else {
        return Err(LoweringError::UnsupportedLayout { function, operation: "layout" });
    };
    if data_offset != base || stride != element_size {
        return Err(LoweringError::UnsupportedLayout { function, operation: "layout" });
    }
    Ok(CollectionLayout {
        layout,
        length_offset,
        capacity_offset,
        data_offset,
        stride,
        scalar,
        pointer_layout,
        pointer_slot,
    })
}

/// Resolve a nominal identity only at the physical descriptor boundary. SSA
/// `ValueType::Ref` and the pointer metadata returned by this module retain
/// the original identity so exact source/ABI correspondence remains visible.
fn resolve_backend_layout(
    function: FunctionId,
    layouts: &LayoutInterner,
    layout: crate::layout::LayoutId,
    operation: &'static str,
) -> Result<crate::layout::LayoutId, LoweringError> {
    layouts
        .resolve_nominal(layout)
        .map_err(|_| LoweringError::UnsupportedLayout { function, operation })
}

fn resolve_collection_layout(
    function: &Function,
    layouts: &LayoutInterner,
    object: ValueId,
) -> Result<CollectionLayout, LoweringError> {
    let Some(ValueType::Ref(layout)) = function.value_types.get(&object).copied() else {
        return Err(LoweringError::UnsupportedLayout { function: function.id, operation: "object" });
    };
    collection_layout(function.id, layouts, layout)
}

fn resolve_collection_layout_for_values(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    object: ValueId,
) -> Result<CollectionLayout, LoweringError> {
    let Some(ValueType::Ref(layout)) = value_types.get(&object).copied() else {
        return Err(LoweringError::UnsupportedLayout { function, operation: "object" });
    };
    collection_layout(function, layouts, layout)
}

fn resolve_memory_place(
    function: &Function,
    layouts: &LayoutInterner,
    place: &Place,
) -> Result<MemoryAccess, LoweringError> {
    if let Projection::Field { index } = place.projection() {
        let Some(ValueType::Ref(object_layout)) = function.value_types.get(&place.object()).copied() else {
            return Err(LoweringError::UnsupportedLayout { function: function.id, operation: "object" });
        };
        return field_access(function.id, layouts, place.object(), object_layout, *index);
    }
    let collection = resolve_collection_layout(function, layouts, place.object())?;
    match place.projection() {
        Projection::Header(field) => {
            let offset = match field {
                HeaderField::Length => collection.length_offset,
                HeaderField::Capacity => collection.capacity_offset.ok_or(LoweringError::UnsupportedLayout {
                    function: function.id,
                    operation: "header",
                })?,
            };
            Ok(MemoryAccess { object: place.object(), collection, offset, width: 4, scalar: ScalarKind::U32, pointer_layout: None, pointer_slot: None, index: None })
        }
        Projection::Element { index } => Ok(MemoryAccess {
            object: place.object(),
            collection,
            offset: collection.data_offset,
            width: collection.scalar.size(),
            scalar: collection.scalar,
            pointer_layout: collection.pointer_layout,
            pointer_slot: collection.pointer_slot,
            index: Some(*index),
        }),
        Projection::Field { .. } | Projection::Slice { .. } => Err(LoweringError::UnsupportedLayout {
            function: function.id,
            operation: "projection",
        }),
    }
}

/// A scalar field of a fixed-layout object: its address is the object plus
/// the field's offset, with no index. The access carries the field's own
/// stride so the shared address emitter needs no collection.
fn field_access(
    function: FunctionId,
    layouts: &LayoutInterner,
    object: ValueId,
    layout: crate::layout::LayoutId,
    index: u32,
) -> Result<MemoryAccess, LoweringError> {
    let physical_layout = resolve_backend_layout(function, layouts, layout, "layout")?;
    let descriptor = layouts
        .get(physical_layout)
        .ok_or(LoweringError::UnsupportedLayout { function, operation: "layout" })?;
    if let LayoutKind::Scalar(scalar) = descriptor.kind() {
        if index != 0 || !matches!(scalar, ScalarKind::Int) {
            return Err(LoweringError::UnsupportedLayout { function, operation: "field" });
        }
        let width = scalar.size();
        return Ok(MemoryAccess {
            object,
            collection: CollectionLayout {
                layout,
                length_offset: 0,
                capacity_offset: None,
                data_offset: 0,
                stride: width,
                scalar: *scalar,
                pointer_layout: None,
                pointer_slot: None,
            },
            offset: 0,
            width,
            scalar: *scalar,
            pointer_layout: None,
            pointer_slot: None,
            index: None,
        });
    }
    let field = descriptor
        .fields()
        .get(index as usize)
        .ok_or(LoweringError::UnsupportedLayout { function, operation: "field" })?;
    // A scalar field is loaded as its kind; a pointer field (an owned pointer
    // value slot) is a 32-bit object address whose pointee the reference
    // transfer needs.
    let (scalar, pointer_layout, pointer_slot) = match field.kind() {
        crate::layout::FieldKind::Scalar(scalar) if scalar == ScalarKind::Unit || scalar.is_ordinary_lowering_target() => (scalar, None, None),
        crate::layout::FieldKind::Pointer { pointee, slot } if matches!(slot, PointerSlot::Owned | PointerSlot::OwnedNullable) => {
            (ScalarKind::U32, Some(pointee), Some(slot))
        }
        crate::layout::FieldKind::Inline(slot) => {
            let physical_slot = resolve_backend_layout(function, layouts, slot, "field kind")?;
            match layouts.get(physical_slot).map(|slot| slot.kind()) {
            Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics })
                if matches!(semantics, PointerSlot::Owned | PointerSlot::OwnedNullable) =>
            {
                (ScalarKind::U32, Some(*pointee), Some(*semantics))
            }
            _ => return Err(LoweringError::UnsupportedLayout { function, operation: "field kind" }),
            }
        }
        _ => return Err(LoweringError::UnsupportedLayout { function, operation: "field kind" }),
    };
    let width = scalar.size();
    Ok(MemoryAccess {
        object,
        collection: CollectionLayout {
            layout,
            length_offset: 0,
            capacity_offset: None,
            data_offset: field.offset(),
            stride: width,
            scalar,
            pointer_layout,
            pointer_slot,
        },
        offset: field.offset(),
        width,
        scalar,
        pointer_layout,
        pointer_slot,
        index: None,
    })
}

fn resolve_memory_place_for_values(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    place: &Place,
) -> Result<MemoryAccess, LoweringError> {
    if let Projection::Field { index } = place.projection() {
        let Some(ValueType::Ref(object_layout)) = value_types.get(&place.object()).copied() else {
            return Err(LoweringError::UnsupportedLayout { function, operation: "object" });
        };
        return field_access(function, layouts, place.object(), object_layout, *index);
    }
    let collection = resolve_collection_layout_for_values(function, value_types, layouts, place.object())?;
    match place.projection() {
        Projection::Header(field) => {
            let offset = match field {
                HeaderField::Length => collection.length_offset,
                HeaderField::Capacity => collection.capacity_offset.ok_or(LoweringError::UnsupportedLayout {
                    function,
                    operation: "header",
                })?,
            };
            Ok(MemoryAccess { object: place.object(), collection, offset, width: 4, scalar: ScalarKind::U32, pointer_layout: None, pointer_slot: None, index: None })
        }
        Projection::Element { index } => Ok(MemoryAccess {
            object: place.object(),
            collection,
            offset: collection.data_offset,
            width: collection.scalar.size(),
            scalar: collection.scalar,
            pointer_layout: collection.pointer_layout,
            pointer_slot: collection.pointer_slot,
            index: Some(*index),
        }),
        Projection::Field { .. } | Projection::Slice { .. } => Err(LoweringError::UnsupportedLayout {
            function,
            operation: "projection",
        }),
    }
}

fn is_indirect_scalar(value_type: ValueType) -> bool {
    matches!(
        value_type,
        ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit | ValueType::Size | ValueType::V128
    )
}

fn is_indirect_result(value_type: ValueType) -> bool {
    is_indirect_scalar(value_type)
        || matches!(value_type, ValueType::Ref(_) | ValueType::Gc(_)
            | ValueType::Capability(_) | ValueType::NullableCapability(_))
}

fn is_indirect_parameter(signature: &FunctionSignature, index: usize, value_type: ValueType) -> bool {
    if is_indirect_scalar(value_type) {
        return true;
    }
    if matches!(value_type, ValueType::Capability(_) | ValueType::NullableCapability(_)) {
        // Externref capability lanes carry their exact checked kind, not
        // linear-memory ownership or permission to apply memory effects.
        return signature.contract.parameters.get(index).is_some_and(|parameter| {
            parameter.value_type == value_type
                && parameter.ownership == ParameterOwnership::Value
        }) && signature.contract.parameter_effects.get(&(index as u32))
            .is_none_or(|effects| effects.is_empty());
    }
    matches!(value_type, ValueType::Ref(_) | ValueType::Gc(_))
        && signature.contract.parameters.get(index).is_some_and(|parameter| {
            let effects = signature.contract.parameter_effects.get(&(index as u32));
            match parameter.ownership {
                ParameterOwnership::Shared
                | ParameterOwnership::WriteBack
                | ParameterOwnership::Unique => effects
                    .is_none_or(|effects| effects.iter().all(|effect| matches!(
                        effect,
                        MemoryEffect::Read | MemoryEffect::Retain | MemoryEffect::Release,
                    ))),
                // A `var unique` function-value lane has a checked write-back
                // contract.  Its caller still has to move the exact token and
                // the final SafeWIR checks the replacement token; accepting a
                // broader targetless effect summary here would be unsound.
                ParameterOwnership::UniqueWriteBack => effects.is_some_and(|effects| {
                    effects.iter().any(|effect| *effect == MemoryEffect::Write)
                        && effects.iter().all(|effect| matches!(
                            effect,
                            MemoryEffect::Read
                                | MemoryEffect::Retain
                                | MemoryEffect::Release
                                | MemoryEffect::Write,
                        ))
                }),
                ParameterOwnership::Value => is_authenticated_indirect_shared_alias(signature, index),
            }
        })
}

/// The non-escaping shared-reference closure lane: one closure environment,
/// one moved `&'a String` token, and one result aliasing exactly that source
/// parameter. Initial SafeWIR validation and canonical lowering have already
/// tied this shape to the pointer-bound source callable identity.
fn is_authenticated_indirect_shared_alias(signature: &FunctionSignature, index: usize) -> bool {
    let ([environment, ValueType::Ref(parameter)], [ValueType::Ref(result)]) =
        (signature.parameters.as_slice(), signature.results.as_slice())
    else { return false };
    index == 1
        && *parameter == *result
        && matches!(environment, ValueType::Ref(_))
        && matches!(signature.contract.parameters.as_slice(), [
            ParameterContract { value_type: ValueType::Ref(environment_layout), ownership: ParameterOwnership::Shared },
            ParameterContract { value_type: ValueType::Ref(layout), ownership: ParameterOwnership::Value },
        ] if *environment == ValueType::Ref(*environment_layout) && *layout == *parameter)
        && matches!(signature.contract.results.as_slice(), [
            ResultContract::AliasesParameter { parameter: 1, layout },
        ] if *layout == *parameter)
        && matches!(signature.contract.parameter_effects.get(&0), Some(effects)
            if effects.as_slice() == [MemoryEffect::Read, MemoryEffect::Retain, MemoryEffect::Release])
        && matches!(signature.contract.parameter_effects.get(&1), Some(effects)
            if effects.as_slice() == [MemoryEffect::Read, MemoryEffect::Retain, MemoryEffect::Release])
        && signature.contract.parameter_effects.len() == 2
}

/// The one memory-bearing indirect ABI this encoder admits.  Its authority is
/// not inferred here: initial SafeWIR validation has already bound the
/// closure-layout adapter to the checked source declaration and its exact
/// access identity.  This merely preserves that one physical lane through
/// `call_indirect`; every other write-bearing reference signature remains
/// refused.
fn is_authenticated_indirect_callable_writeback(signature: &FunctionSignature) -> bool {
    let ([environment, ValueType::Ref(parameter)], [ordinary, write_back]) =
        (signature.parameters.as_slice(), signature.results.as_slice())
    else {
        return false;
    };
    *ordinary == ValueType::Ref(*parameter)
        && *write_back == ValueType::Ref(*parameter)
        && matches!(environment, ValueType::Ref(_))
        && matches!(signature.contract.parameters.as_slice(), [
            ParameterContract {
                value_type: ValueType::Ref(environment_layout),
                ownership: ParameterOwnership::Shared,
            },
            ParameterContract {
                value_type: ValueType::Ref(layout),
                ownership: ParameterOwnership::WriteBack,
            },
        ] if *environment == ValueType::Ref(*environment_layout) && *layout == *parameter)
        && matches!(signature.contract.results.as_slice(), [
            ResultContract::Shared { layout: ordinary_layout },
            ResultContract::Shared { layout: write_back_layout },
        ] if *ordinary_layout == *parameter && *write_back_layout == *parameter)
        && matches!(signature.contract.parameter_effects.get(&0), Some(effects)
            if effects.as_slice() == [
                MemoryEffect::Read,
                MemoryEffect::Retain,
                MemoryEffect::Release,
            ])
        && matches!(signature.contract.parameter_effects.get(&1), Some(effects)
            if effects.as_slice() == [
                MemoryEffect::Read,
                MemoryEffect::Retain,
                MemoryEffect::Release,
                MemoryEffect::Write,
            ])
        && signature.contract.parameter_effects.len() == 2
}

fn checked_table_index(
    function_indices: &BTreeMap<FunctionId, u32>,
    function: FunctionId,
) -> Result<u32, CanonicalAdmissionError> {
    let first_defined = function_indices
        .values()
        .copied()
        .min()
        .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
    function_indices
        .get(&function)
        .copied()
        .and_then(|index| index.checked_sub(first_defined))
        .ok_or(CanonicalAdmissionError::UnsupportedPlan)
}

fn lower_operation(
    function: FunctionId,
    block: crate::canonical_safewir::BlockId,
    index: usize,
    operation: &Operation,
    value_types: &BTreeMap<ValueId, ValueType>,
    signatures: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, FunctionSignature>,
    table_indices: &BTreeMap<FunctionId, u32>,
    layouts: &LayoutInterner,
    imports: &BTreeMap<ImportId, ImportContract>,
    aggregates: &mut BTreeMap<ValueId, Vec<ValueId>>,
) -> Result<WasmInstruction, LoweringError> {
    let source = SourceItemId::Operation { function, block, index };
    let operation = match operation {
        Operation::ReauthenticateRawHeaderExtent { .. } => ScalarInstruction::ProofNoop,
        Operation::ClosureOwner(operation) => ScalarInstruction::ClosureOwner(operation.clone()),
        Operation::ConstUnit { destination } => ScalarInstruction::UnitConst { destination: *destination },
        Operation::ConstInt { destination, value } => ScalarInstruction::IntConst { destination: *destination, value: *value },
        Operation::ConstFloat { destination, bits } => ScalarInstruction::FloatConst { destination: *destination, bits: *bits },
        Operation::ConstBool { destination, value } => ScalarInstruction::BoolConst { destination: *destination, value: *value },
        Operation::ConstSize { destination, value } => ScalarInstruction::SizeConst { destination: *destination, value: *value },
        Operation::CapabilityNone { destination, .. } => ScalarInstruction::CapabilityNone { destination: *destination },
        Operation::CapabilitySome { destination, source } => ScalarInstruction::CapabilitySome { destination: *destination, source: *source },
        Operation::CapabilityIsPresent { destination, source } => ScalarInstruction::CapabilityIsPresent { destination: *destination, source: *source },
        Operation::CapabilityUnwrap { destination, source } => ScalarInstruction::CapabilityUnwrap { destination: *destination, source: *source },
        Operation::Arithmetic { destination, operator, left, right, overflow } => {
            ScalarInstruction::Arithmetic {
                destination: *destination,
                operator: *operator,
                left: *left,
                right: *right,
                overflow: *overflow,
            }
        }
        Operation::Compare { destination, operator, left, right } => ScalarInstruction::Compare {
            destination: *destination,
            operator: *operator,
            left: *left,
            right: *right,
        },
        Operation::NarrowSize { destination, value } => ScalarInstruction::NarrowSize { destination: *destination, value: *value },
        Operation::WidenSize { destination, value } => ScalarInstruction::WidenSize { destination: *destination, value: *value },
        Operation::FloatUnary { destination, operator, value } => ScalarInstruction::FloatUnary { destination: *destination, operator: *operator, value: *value },
        Operation::Guard { condition, kind } => ScalarInstruction::Guard { condition: *condition, kind: *kind },
        Operation::MoveValue { destination, source } => {
            ScalarInstruction::MoveValue { destination: *destination, source: *source }
        }
        Operation::MakeInlineAggregate { destination, layout, fields } => {
            aggregates.insert(*destination, fields.clone());
            ScalarInstruction::MakeInlineAggregate { destination: *destination, layout: *layout, fields: fields.clone() }
        }
        Operation::ExtractInlineField { destination, aggregate, index } => {
            let fields = aggregates.get(aggregate).ok_or(LoweringError::UnsupportedOperation {
                function, operation: "ExtractInlineField aggregate lifetime",
            })?;
            let field = *fields.get(*index as usize).ok_or(LoweringError::UnsupportedOperation {
                function, operation: "ExtractInlineField index",
            })?;
            ScalarInstruction::ExtractInlineField {
                destination: *destination,
                aggregate: *aggregate,
                index: *index,
                field,
            }
        }
        Operation::FunctionValue { destination, function: target } => {
            let table_index = table_indices.get(target).copied().ok_or(
                LoweringError::UnsupportedOperation {
                    function,
                    operation: "FunctionValue target",
                },
            )?;
            ScalarInstruction::FunctionValue {
                destination: *destination,
                function: *target,
                table_index,
            }
        }
        Operation::Alloc { destination, layout, extent, class } => {
            // A counted collection allocates by capacity; a fixed layout (a
            // scalar record) allocates its constant size.
            let supported = match extent {
                Extent::Counted { .. } => collection_layout(function, layouts, *layout).is_ok(),
                Extent::FixedBytes(bytes) => resolve_backend_layout(function, layouts, *layout, "Alloc layout")
                    .ok()
                    .and_then(|physical_layout| layouts.get(physical_layout))
                    .is_some_and(|descriptor| matches!(descriptor.size(), LayoutSize::Fixed(size) if u64::from(size) == *bytes)),
                Extent::HeaderPrefix { .. } => false,
            };
            if *class != crate::canonical_safewir::AllocationClass::ReferenceCounted || !supported {
                return Err(LoweringError::UnsupportedLayout { function, operation: "Alloc" });
            }
            ScalarInstruction::Alloc { destination: *destination, layout: *layout, extent: extent.clone(), class: *class }
        }
        Operation::StaticData { destination, data, .. } => {
            ScalarInstruction::StaticData { destination: *destination, data: *data }
        }
        Operation::InitNullableOwned { place } => {
            let access = resolve_memory_place_for_values(function, value_types, layouts, place)?;
            if access.pointer_slot != Some(PointerSlot::OwnedNullable) || place.access() != AccessMode::Write {
                return Err(LoweringError::UnsupportedLayout { function, operation: "InitNullableOwned" });
            }
            ScalarInstruction::InitNullableOwned { place: place.clone() }
        }
        Operation::Load { destination, place, transfer } => {
            let access = resolve_memory_place_for_values(function, value_types, layouts, place)?;
            let valid_transfer = match access.pointer_layout {
                Some(pointee) if access.pointer_slot == Some(PointerSlot::OwnedNullable) => {
                    *transfer == LoadTransfer::ClonePresent
                        && place.value_type() == ValueType::Ref(pointee)
                }
                Some(pointee) => {
                    matches!(transfer, LoadTransfer::CloneShared | LoadTransfer::MoveUnique | LoadTransfer::TakeLast)
                        && place.value_type() == ValueType::Ref(pointee)
                }
                None => matches!(transfer, LoadTransfer::CopyScalar | LoadTransfer::TakeLast),
            };
            if !valid_transfer || place.access() != AccessMode::Read {
                return Err(LoweringError::UnsupportedLayout { function, operation: "Load" });
            }
            ScalarInstruction::Load { destination: *destination, place: place.clone(), transfer: *transfer }
        }
        Operation::Store { place, value, transfer } => {
            let access = resolve_memory_place_for_values(function, value_types, layouts, place)?;
            let valid_transfer = match access.pointer_layout {
                Some(pointee) => {
                    matches!(*transfer, StoreTransfer::MoveUnique | StoreTransfer::CloneShared)
                        && place.value_type() == ValueType::Ref(pointee)
                        && value_types.get(value) == Some(&ValueType::Ref(pointee))
                }
                None => *transfer == StoreTransfer::CopyScalar,
            };
            if !valid_transfer || place.access() != AccessMode::Write {
                return Err(LoweringError::UnsupportedLayout { function, operation: "Store" });
            }
            ScalarInstruction::Store { place: place.clone(), value: *value, transfer: *transfer }
        }
        Operation::UpdateCount { object, count } => {
            if resolve_collection_layout_for_values(function, value_types, layouts, *object).is_err() {
                return Err(LoweringError::UnsupportedLayout { function, operation: "UpdateCount" });
            }
            ScalarInstruction::UpdateCount { object: *object, count: *count }
        }
        Operation::CloneRef { destination, source } => {
            // Reachable only through lower_validated. RC class and affine
            // ownership were established by the independent final checker,
            // including at parameters, calls, and CFG joins. Do not reconstruct
            // them from the order in which this backend visits instructions.
            ScalarInstruction::CloneRef { destination: *destination, source: *source }
        }
        Operation::Release { object: _ } | Operation::DropRef { object: _ } | Operation::Reclaim { object: _ } => {
            match operation {
                Operation::Release { object } => ScalarInstruction::Release { object: *object },
                Operation::DropRef { object } => ScalarInstruction::Release { object: *object },
                Operation::Reclaim { object } => ScalarInstruction::Reclaim { object: *object },
                _ => unreachable!(),
            }
        }
        Operation::Call { destinations, callee, arguments } => ScalarInstruction::Call {
            destinations: destinations.clone(),
            callee: *callee,
            arguments: scalar_arguments(function, arguments)?,
        },
        Operation::VarCall { destinations, callee, signature, arguments } => {
            let signature_def = signatures.get(signature).ok_or(LoweringError::UnsupportedOperation {
                function,
                operation: "VarCall signature",
            })?;
            let callable_writeback = is_authenticated_indirect_callable_writeback(signature_def);
            if value_types.get(callee) != Some(&ValueType::Callable(*signature))
                || signature_def.parameters.len() != arguments.len()
                || signature_def.results.len() != destinations.len()
                || signature_def.parameters.iter().copied().enumerate().any(|(index, value_type)| {
                    !is_indirect_parameter(signature_def, index, value_type)
                        && !(callable_writeback && index == 1 && value_type == signature_def.parameters[1])
                })
                || signature_def.results.iter().copied().any(|value_type| !is_indirect_result(value_type))
            {
                return Err(LoweringError::UnsupportedOperation {
                    function,
                    operation: "VarCall memory-bearing or mismatched signature",
                });
            }
            for (argument, expected) in arguments.iter().zip(&signature_def.parameters) {
                let value = match argument {
                    CallArgument::Value(value) | CallArgument::Move(value) => *value,
                };
                if value_types.get(&value) != Some(expected) {
                    return Err(LoweringError::UnsupportedOperation {
                        function,
                        operation: "VarCall argument type",
                    });
                }
                if matches!(expected, ValueType::Ref(_)) && !matches!(argument, CallArgument::Move(_)) {
                    return Err(LoweringError::UnsupportedCallArgument { function });
                }
            }
            for (destination, expected) in destinations.iter().zip(&signature_def.results) {
                if value_types.get(destination) != Some(expected) {
                    return Err(LoweringError::UnsupportedOperation {
                        function,
                        operation: "VarCall result type",
                    });
                }
            }
            ScalarInstruction::VarCall {
                destinations: destinations.clone(),
                callee: *callee,
                signature: *signature,
                arguments: scalar_arguments(function, arguments)?,
            }
        }
        Operation::ExternalCall { destinations, import, arguments } => {
            let contract = imports.get(import).ok_or(LoweringError::UnsupportedOperation {
                function,
                operation: "ExternalCall contract",
            })?;
            let signature = signatures.get(&contract.signature).ok_or(LoweringError::UnsupportedOperation {
                function,
                operation: "ExternalCall signature",
            })?;
            validate_external_call_lowering(function, value_types, contract, signature, arguments, destinations)?;
            ScalarInstruction::ExternalCall {
                destinations: destinations.clone(),
                import: *import,
                arguments: scalar_arguments(function, arguments)?,
            }
        }
        Operation::HostCall { destinations, import, arguments } => {
            let contract = imports.get(import).ok_or(LoweringError::UnsupportedOperation { function, operation: "HostCall contract" })?;
            validate_host_call_lowering(function, value_types, layouts, contract, arguments, destinations)?;
            ScalarInstruction::HostCall { destinations: destinations.clone(), import: *import, arguments: arguments.clone() }
        }
        Operation::CopyBytes { destination, source, length } => {
            raw_byte_slice_place(function, value_types, layouts, destination, AccessMode::Write, *length)?;
            raw_byte_slice_place(function, value_types, layouts, source, AccessMode::Read, *length)?;
            ScalarInstruction::CopyBytes {
                destination: destination.clone(),
                source: source.clone(),
                length: *length,
            }
        }
        Operation::MoveRange { destination, source, count } => {
            let destination_layout = resolve_collection_layout_for_values(function, value_types, layouts, destination.object())?;
            let source_layout = resolve_collection_layout_for_values(function, value_types, layouts, source.object())?;
            if destination_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
                || source_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
                || destination_layout != source_layout
                || !matches!(destination.projection(), Projection::Slice { length, .. } if *length == *count)
                || !matches!(source.projection(), Projection::Slice { length, .. } if *length == *count)
            {
                return Err(LoweringError::UnsupportedLayout { function, operation: "MoveRange" });
            }
            ScalarInstruction::MoveRange { destination: destination.clone(), source: source.clone(), count: *count }
        }
        Operation::CloneRange { destination, source, count } => {
            let destination_layout = resolve_collection_layout_for_values(function, value_types, layouts, destination.object())?;
            let source_layout = resolve_collection_layout_for_values(function, value_types, layouts, source.object())?;
            if destination_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
                || source_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
                || destination_layout != source_layout
                || !matches!(destination.projection(), Projection::Slice { length, .. } if *length == *count)
                || !matches!(source.projection(), Projection::Slice { length, .. } if *length == *count)
            {
                return Err(LoweringError::UnsupportedLayout { function, operation: "CloneRange" });
            }
            ScalarInstruction::CloneRange { destination: destination.clone(), source: source.clone(), count: *count }
        }
        Operation::Retain { .. } => {
            return Err(LoweringError::UnsupportedOperation { function, operation: operation_name(operation) });
        }
    };
    Ok(WasmInstruction { source, operation })
}

fn operation_name(operation: &Operation) -> &'static str {
    match operation {
        Operation::ClosureOwner(_) => "ClosureOwner",
        Operation::ConstUnit { .. } => "ConstUnit",
        Operation::ConstInt { .. } => "ConstInt",
        Operation::ConstFloat { .. } => "ConstFloat",
        Operation::ConstBool { .. } => "ConstBool",
        Operation::ConstSize { .. } => "ConstSize",
        Operation::CapabilityNone { .. } => "CapabilityNone",
        Operation::CapabilitySome { .. } => "CapabilitySome",
        Operation::CapabilityIsPresent { .. } => "CapabilityIsPresent",
        Operation::CapabilityUnwrap { .. } => "CapabilityUnwrap",
        Operation::Arithmetic { .. } => "Arithmetic",
        Operation::Compare { .. } => "Compare",
        Operation::NarrowSize { .. } => "NarrowSize",
        Operation::WidenSize { .. } => "WidenSize",
        Operation::FloatUnary { .. } => "FloatUnary",
        Operation::Guard { .. } => "Guard",
        Operation::ReauthenticateRawHeaderExtent { .. } => "ReauthenticateRawHeaderExtent",
        Operation::Alloc { .. } => "Alloc",
        Operation::StaticData { .. } => "StaticData",
        Operation::InitNullableOwned { .. } => "InitNullableOwned",
        Operation::Load { .. } => "Load",
        Operation::Store { .. } => "Store",
        Operation::CopyBytes { .. } => "CopyBytes",
        Operation::MoveRange { .. } => "MoveRange",
        Operation::CloneRange { .. } => "CloneRange",
        Operation::UpdateCount { .. } => "UpdateCount",
        Operation::MoveValue { .. } => "MoveValue",
        Operation::MakeInlineAggregate { .. } => "MakeInlineAggregate",
        Operation::ExtractInlineField { .. } => "ExtractInlineField",
        Operation::FunctionValue { .. } => "FunctionValue",
        Operation::CloneRef { .. } => "CloneRef",
        Operation::Retain { .. } => "Retain",
        Operation::Release { .. } => "Release",
        Operation::DropRef { .. } => "DropRef",
        Operation::Reclaim { .. } => "Reclaim",
        Operation::Call { .. } => "Call",
        Operation::VarCall { .. } => "VarCall",
        Operation::ExternalCall { .. } => "ExternalCall",
        Operation::HostCall { .. } => "HostCall",
    }
}

fn lower_terminator(
    function: &Function,
    block: crate::canonical_safewir::BlockId,
    terminator: &Terminator,
    signatures: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, FunctionSignature>,
) -> Result<WasmTerminator, LoweringError> {
    let source = SourceItemId::Terminator { function: function.id, block };
    let operation = match terminator {
        Terminator::Branch { target, arguments } => ScalarTerminator::Branch { target: *target, arguments: arguments.clone() },
        Terminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments } => ScalarTerminator::BranchIf {
            condition: *condition,
            then_target: *then_target,
            then_arguments: then_arguments.clone(),
            else_target: *else_target,
            else_arguments: else_arguments.clone(),
        },
        Terminator::Return { values } => ScalarTerminator::Return { values: values.clone() },
        Terminator::TailCall { site, callee, arguments } => ScalarTerminator::TailCall {
            site: *site,
            callee: *callee,
            arguments: scalar_arguments(function.id, arguments)?,
        },
        Terminator::VarTailCall { site, callee, signature, arguments } => {
            let signature_def = signatures.get(signature).ok_or(
                LoweringError::UnsupportedOperation {
                    function: function.id,
                    operation: "VarTailCall signature",
                },
            )?;
            if function.value_types.get(callee) != Some(&ValueType::Callable(*signature))
                || signature_def.parameters.len() != arguments.len()
                || signature_def
                    .parameters
                    .iter()
                    .copied()
                    .enumerate()
                    .any(|(index, value_type)| !is_indirect_parameter(signature_def, index, value_type))
                || signature_def
                    .results
                    .iter()
                    .copied()
                    .any(|value_type| !is_indirect_result(value_type))
            {
                return Err(LoweringError::UnsupportedOperation {
                    function: function.id,
                    operation: "VarTailCall memory-bearing or mismatched signature",
                });
            }
            for (argument, expected) in arguments.iter().zip(&signature_def.parameters) {
                let value = match argument {
                    CallArgument::Value(value) | CallArgument::Move(value) => *value,
                };
                if function.value_types.get(&value) != Some(expected) {
                    return Err(LoweringError::UnsupportedOperation {
                        function: function.id,
                        operation: "VarTailCall argument type",
                    });
                }
                if matches!(expected, ValueType::Ref(_)) && !matches!(argument, CallArgument::Move(_)) {
                    return Err(LoweringError::UnsupportedCallArgument { function: function.id });
                }
            }
            ScalarTerminator::VarTailCall {
                site: *site,
                callee: *callee,
                signature: *signature,
                arguments: scalar_arguments(function.id, arguments)?,
            }
        }
        Terminator::Trap => ScalarTerminator::Trap,
        Terminator::Unreachable => ScalarTerminator::Unreachable,
    };
    Ok(WasmTerminator { source, operation })
}

fn lower_block(
    function: &Function,
    block: &BasicBlock,
    layouts: &LayoutInterner,
    imports: &BTreeMap<ImportId, ImportContract>,
    signatures: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, FunctionSignature>,
    table_indices: &BTreeMap<FunctionId, u32>,
    aggregates: &mut BTreeMap<ValueId, Vec<ValueId>>,
) -> Result<WasmBlock, LoweringError> {
    let mut instructions = Vec::with_capacity(block.operations.len());
    for (index, operation) in block.operations.iter().enumerate() {
        instructions.push(lower_operation(function.id, block.id, index, operation, &function.value_types, signatures, table_indices, layouts, imports, aggregates)?);
    }
    Ok(WasmBlock {
        id: block.id,
        instructions,
        terminator: lower_terminator(function, block.id, &block.terminator, signatures)?,
    })
}

fn lower_function(
    function: &Function,
    layouts: &LayoutInterner,
    imports: &BTreeMap<ImportId, ImportContract>,
    signatures: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, FunctionSignature>,
    table_indices: &BTreeMap<FunctionId, u32>,
) -> Result<WasmFunction, LoweringError> {
    let mut aggregates = BTreeMap::new();
    Ok(WasmFunction {
        id: function.id,
        signature: function.signature,
        entry: function.entry,
        blocks: function
            .blocks
            .iter()
            .map(|block| lower_block(function, block, layouts, imports, signatures, table_indices, &mut aggregates))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn lower_validated(module: &ValidatedSafeWir) -> Result<WasmIr, LoweringError> {
    let module = module.module();
    let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&module.layouts().canonical_bytes())
        .map_err(|_| LoweringError::InvalidLayoutBundle)?;
    let (static_data, heap_start) = static_data_plan(module.static_data_segments())?;
    let imports = module.imports().iter().map(|import| (import.id, import.clone())).collect::<BTreeMap<_, _>>();
    let signatures = module.signatures().iter().map(|signature| (signature.id, signature.clone())).collect::<BTreeMap<_, _>>();
    let has_function_table = module.functions().iter().flat_map(|function| function.blocks.iter()).any(|block| {
        matches!(block.terminator, Terminator::VarTailCall { .. })
            || block.operations.iter().any(|operation| matches!(operation,
                Operation::VarCall { .. } | Operation::FunctionValue { .. }
                | Operation::ClosureOwner(
                    crate::canonical_safewir::closure_owner_contract::OwnerOperation::Construct { .. }
                    | crate::canonical_safewir::closure_owner_contract::OwnerOperation::ReadCallable { .. }
                )
            ))
    });
    let table_indices = module
        .functions()
        .iter()
        .enumerate()
        .map(|(index, function)| {
            u32::try_from(index)
                .map(|index| (function.id, index))
                .map_err(|_| LoweringError::UnsupportedOperation {
                    function: function.id,
                    operation: "function table size",
                })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    Ok(WasmIr {
        owner_bridges: owner_bridges::build(module)?,
        revision: module.revision(),
        checked_source: module.checked_source(),
        launch_contract_payload: module.launch_contract_payload().to_vec(),
        entry_payload: None,
        static_data,
        imports: module.imports().iter().map(|import| WasmImport { id: import.id, signature: import.signature }).collect(),
        table: has_function_table.then(|| WasmTable {
            functions: module.functions().iter().map(|function| function.id).collect(),
        }),
        heap_start,
        functions: module
            .functions()
            .iter()
            .map(|function| lower_function(function, &layouts, &imports, &signatures, &table_indices))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

const STATIC_DATA_BASE: u32 = 16;
const WASM_PAGE_BYTES: u64 = 65_536;

fn initial_memory_pages(heap_start: u32) -> u64 {
    (u64::from(heap_start).saturating_add(WASM_PAGE_BYTES - 1) / WASM_PAGE_BYTES).max(1)
}

fn static_data_plan(
    segments: &[crate::canonical_safewir::StaticDataSegment],
) -> Result<(Vec<WasmStaticData>, u32), LoweringError> {
    let mut ordered = segments.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|segment| segment.id());
    let mut offset = STATIC_DATA_BASE;
    let mut plan = Vec::with_capacity(ordered.len());
    for segment in ordered {
        let bytes = u32::try_from(segment.bytes().len()).map_err(|_| LoweringError::StaticDataAddressOverflow)?;
        let end = offset.checked_add(bytes).ok_or(LoweringError::StaticDataAddressOverflow)?;
        plan.push(WasmStaticData { id: segment.id(), offset, bytes: segment.bytes().to_vec() });
        offset = end;
    }
    let heap_start = offset.checked_add(7).map(|value| value & !7).ok_or(LoweringError::StaticDataAddressOverflow)?;
    Ok((plan, heap_start))
}

/// The reviewed SafeWIR-to-WasmIR mapping, keyed by canonical rule: every
/// rule is either mapped under one correspondence rule or refused by the
/// backend (`None`). `canonical_safewir::manifest` cites this table row by
/// row, so a new rule cannot reach the backend without a manifest entry.
pub(crate) fn rule_backend(rule: crate::canonical_safewir::CanonicalRuleId) -> RuleBackend {
    use crate::canonical_safewir::CanonicalRuleId as Rule;
    match rule {
        Rule::ConstUnit | Rule::ConstInt | Rule::ConstFloat | Rule::ConstBool | Rule::ConstSize | Rule::FunctionValue => RuleBackend::Receipt(CorrespondenceRule::ScalarConstant),
        Rule::CapabilityNone | Rule::CapabilitySome | Rule::CapabilityIsPresent | Rule::CapabilityUnwrap => RuleBackend::Receipt(CorrespondenceRule::ScalarGuard),
        Rule::Arithmetic => RuleBackend::Receipt(CorrespondenceRule::ScalarArithmetic),
        Rule::Compare => RuleBackend::Receipt(CorrespondenceRule::ScalarComparison),
        Rule::NarrowSize | Rule::WidenSize | Rule::FloatUnary => RuleBackend::Receipt(CorrespondenceRule::ScalarConversion),
        Rule::Guard => RuleBackend::Receipt(CorrespondenceRule::ScalarGuard),
        Rule::ReauthenticateRawHeaderExtent => RuleBackend::Receipt(CorrespondenceRule::ScalarGuard),
        Rule::MoveValue | Rule::MakeInlineAggregate | Rule::ExtractInlineField => RuleBackend::Receipt(CorrespondenceRule::ScalarMove),
        Rule::Alloc => RuleBackend::Receipt(CorrespondenceRule::MemoryAlloc),
        Rule::StaticData => RuleBackend::Receipt(CorrespondenceRule::MemoryStaticData),
        Rule::InitNullableOwned => RuleBackend::Receipt(CorrespondenceRule::MemoryStore),
        Rule::Load => RuleBackend::Receipt(CorrespondenceRule::MemoryLoad),
        Rule::ClosureSlot => RuleBackend::Subjudgment(&[Rule::Load, Rule::Store]),
        Rule::Store => RuleBackend::Receipt(CorrespondenceRule::MemoryStore),
        Rule::CopyBytes | Rule::CloneRange => RuleBackend::Receipt(CorrespondenceRule::MemoryCopy),
        Rule::MoveRange | Rule::CloneRef | Rule::Release | Rule::DropRef | Rule::Reclaim => RuleBackend::Receipt(CorrespondenceRule::MemoryOwnership),
        Rule::UpdateCount => RuleBackend::Receipt(CorrespondenceRule::MemoryUpdateCount),
        Rule::Call | Rule::VarCall => RuleBackend::Receipt(CorrespondenceRule::ScalarCall),
        Rule::ExternalCall => RuleBackend::Receipt(CorrespondenceRule::ExternalCall),
        Rule::HostCall => RuleBackend::Receipt(CorrespondenceRule::HostCall),
        // v1 has no standalone retain: a second owner comes from `CloneRef`.
        Rule::Retain => RuleBackend::Refused,
        Rule::ClosureOwner => RuleBackend::Receipt(CorrespondenceRule::ClosureOwner),
        Rule::Branch | Rule::BranchIf | Rule::Return | Rule::TailCall | Rule::VarTailCall | Rule::Trap | Rule::Unreachable => {
            RuleBackend::Receipt(CorrespondenceRule::Control)
        }
    }
}

fn operation_rule(operation: &Operation) -> Option<CorrespondenceRule> {
    match rule_backend(operation.rule_id()) {
        RuleBackend::Receipt(rule) => Some(rule),
        RuleBackend::Subjudgment(_) | RuleBackend::Refused => None,
    }
}

fn matches_operation(operation: &Operation, target: &ScalarInstruction) -> bool {
    match (operation, target) {
        (Operation::ReauthenticateRawHeaderExtent { .. }, ScalarInstruction::ProofNoop) => true,
        (Operation::ClosureOwner(source), ScalarInstruction::ClosureOwner(target)) => source == target,
        (Operation::ConstUnit { destination }, ScalarInstruction::UnitConst { destination: actual }) => destination == actual,
        (Operation::ConstInt { destination, value }, ScalarInstruction::IntConst { destination: actual, value: actual_value }) => destination == actual && value == actual_value,
        (Operation::ConstFloat { destination, bits }, ScalarInstruction::FloatConst { destination: actual, bits: actual_bits }) => destination == actual && bits == actual_bits,
        (Operation::ConstBool { destination, value }, ScalarInstruction::BoolConst { destination: actual, value: actual_value }) => destination == actual && value == actual_value,
        (Operation::ConstSize { destination, value }, ScalarInstruction::SizeConst { destination: actual, value: actual_value }) => destination == actual && value == actual_value,
        (Operation::CapabilityNone { destination, .. }, ScalarInstruction::CapabilityNone { destination: actual }) => destination == actual,
        (Operation::CapabilitySome { destination, source }, ScalarInstruction::CapabilitySome { destination: actual_destination, source: actual_source }) => destination == actual_destination && source == actual_source,
        (Operation::CapabilityIsPresent { destination, source }, ScalarInstruction::CapabilityIsPresent { destination: actual_destination, source: actual_source }) => destination == actual_destination && source == actual_source,
        (Operation::CapabilityUnwrap { destination, source }, ScalarInstruction::CapabilityUnwrap { destination: actual_destination, source: actual_source }) => destination == actual_destination && source == actual_source,
        (Operation::Arithmetic { destination, operator, left, right, overflow }, ScalarInstruction::Arithmetic { destination: actual_destination, operator: actual_operator, left: actual_left, right: actual_right, overflow: actual_overflow }) => destination == actual_destination && operator == actual_operator && left == actual_left && right == actual_right && overflow == actual_overflow,
        (Operation::Compare { destination, operator, left, right }, ScalarInstruction::Compare { destination: actual_destination, operator: actual_operator, left: actual_left, right: actual_right }) => destination == actual_destination && operator == actual_operator && left == actual_left && right == actual_right,
        (Operation::NarrowSize { destination, value }, ScalarInstruction::NarrowSize { destination: actual_destination, value: actual_value }) => destination == actual_destination && value == actual_value,
        (Operation::WidenSize { destination, value }, ScalarInstruction::WidenSize { destination: actual_destination, value: actual_value }) => destination == actual_destination && value == actual_value,
        (Operation::FloatUnary { destination, operator, value }, ScalarInstruction::FloatUnary { destination: actual_destination, operator: actual_operator, value: actual_value }) => destination == actual_destination && operator == actual_operator && value == actual_value,
        (Operation::Guard { condition, kind }, ScalarInstruction::Guard { condition: actual_condition, kind: actual_kind }) => condition == actual_condition && kind == actual_kind,
        (Operation::MoveValue { destination, source }, ScalarInstruction::MoveValue { destination: actual_destination, source: actual_source }) => destination == actual_destination && source == actual_source,
        (Operation::MakeInlineAggregate { destination, layout, fields }, ScalarInstruction::MakeInlineAggregate { destination: actual_destination, layout: actual_layout, fields: actual_fields }) => destination == actual_destination && layout == actual_layout && fields == actual_fields,
        (Operation::ExtractInlineField { destination, aggregate, index }, ScalarInstruction::ExtractInlineField { destination: actual_destination, aggregate: actual_aggregate, index: actual_index, .. }) => destination == actual_destination && aggregate == actual_aggregate && index == actual_index,
        (Operation::FunctionValue { destination, function }, ScalarInstruction::FunctionValue { destination: actual_destination, function: actual_function, .. }) => destination == actual_destination && function == actual_function,
        (Operation::Alloc { destination, layout, extent, class }, ScalarInstruction::Alloc { destination: actual_destination, layout: actual_layout, extent: actual_extent, class: actual_class }) => destination == actual_destination && layout == actual_layout && extent == actual_extent && class == actual_class,
        (Operation::StaticData { destination, data, .. }, ScalarInstruction::StaticData { destination: actual_destination, data: actual_data }) => destination == actual_destination && data == actual_data,
        (Operation::InitNullableOwned { place }, ScalarInstruction::InitNullableOwned { place: actual_place }) => place == actual_place,
        (Operation::Load { destination, place, transfer }, ScalarInstruction::Load { destination: actual_destination, place: actual_place, transfer: actual_transfer }) => destination == actual_destination && place == actual_place && transfer == actual_transfer,
        (Operation::Store { place, value, transfer }, ScalarInstruction::Store { place: actual_place, value: actual_value, transfer: actual_transfer }) => place == actual_place && value == actual_value && transfer == actual_transfer,
        (Operation::CopyBytes { destination, source, length }, ScalarInstruction::CopyBytes { destination: actual_destination, source: actual_source, length: actual_length }) => destination == actual_destination && source == actual_source && length == actual_length,
        (Operation::MoveRange { destination, source, count }, ScalarInstruction::MoveRange { destination: actual_destination, source: actual_source, count: actual_count }) => destination == actual_destination && source == actual_source && count == actual_count,
        (Operation::CloneRange { destination, source, count }, ScalarInstruction::CloneRange { destination: actual_destination, source: actual_source, count: actual_count }) => destination == actual_destination && source == actual_source && count == actual_count,
        (Operation::UpdateCount { object, count }, ScalarInstruction::UpdateCount { object: actual_object, count: actual_count }) => object == actual_object && count == actual_count,
        (Operation::CloneRef { destination, source }, ScalarInstruction::CloneRef { destination: actual_destination, source: actual_source }) => destination == actual_destination && source == actual_source,
        (Operation::Release { object }, ScalarInstruction::Release { object: actual_object })
        | (Operation::DropRef { object }, ScalarInstruction::Release { object: actual_object })
        | (Operation::Reclaim { object }, ScalarInstruction::Reclaim { object: actual_object }) => object == actual_object,
        (Operation::Call { destinations, callee, arguments }, ScalarInstruction::Call { destinations: actual_destinations, callee: actual_callee, arguments: actual_arguments }) => *callee == *actual_callee && destinations == actual_destinations && scalar_argument_values(arguments) == Some(actual_arguments.clone()),
        (Operation::ExternalCall { destinations, import, arguments }, ScalarInstruction::ExternalCall { destinations: actual_destinations, import: actual_import, arguments: actual_arguments }) => import == actual_import && destinations == actual_destinations && scalar_argument_values(arguments) == Some(actual_arguments.clone()),
        (Operation::HostCall { destinations, import, arguments }, ScalarInstruction::HostCall { destinations: actual_destinations, import: actual_import, arguments: actual_arguments }) => import == actual_import && destinations == actual_destinations && arguments == actual_arguments,
        (Operation::VarCall { destinations, callee, signature, arguments }, ScalarInstruction::VarCall { destinations: actual_destinations, callee: actual_callee, signature: actual_signature, arguments: actual_arguments }) => *callee == *actual_callee && *signature == *actual_signature && destinations == actual_destinations && scalar_argument_values(arguments) == Some(actual_arguments.clone()),
        _ => false,
    }
}

fn scalar_argument_values(arguments: &[CallArgument]) -> Option<Vec<ValueId>> {
    arguments.iter().map(|argument| match argument {
        CallArgument::Value(value) | CallArgument::Move(value) => Some(*value),
    }).collect()
}

fn matches_terminator(terminator: &Terminator, target: &ScalarTerminator) -> bool {
    match (terminator, target) {
        (Terminator::Branch { target, arguments }, ScalarTerminator::Branch { target: actual_target, arguments: actual_arguments }) => target == actual_target && arguments == actual_arguments,
        (Terminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments }, ScalarTerminator::BranchIf { condition: actual_condition, then_target: actual_then_target, then_arguments: actual_then_arguments, else_target: actual_else_target, else_arguments: actual_else_arguments }) => condition == actual_condition && then_target == actual_then_target && then_arguments == actual_then_arguments && else_target == actual_else_target && else_arguments == actual_else_arguments,
        (Terminator::Return { values }, ScalarTerminator::Return { values: actual_values }) => values == actual_values,
        (Terminator::TailCall { site, callee, arguments }, ScalarTerminator::TailCall { site: actual_site, callee: actual_callee, arguments: actual_arguments }) => site == actual_site && callee == actual_callee && scalar_argument_values(arguments) == Some(actual_arguments.clone()),
        (Terminator::VarTailCall { site, callee, signature, arguments }, ScalarTerminator::VarTailCall { site: actual_site, callee: actual_callee, signature: actual_signature, arguments: actual_arguments }) => site == actual_site && callee == actual_callee && signature == actual_signature && scalar_argument_values(arguments) == Some(actual_arguments.clone()),
        (Terminator::Trap, ScalarTerminator::Trap) | (Terminator::Unreachable, ScalarTerminator::Unreachable) => true,
        _ => false,
    }
}

fn expected_source_items<S: ModuleStage>(module: &CanonicalModule<S>) -> BTreeSet<SourceItemId> {
    module
        .functions()
        .iter()
        .flat_map(|function| function.blocks.iter().flat_map(move |block| {
            block
                .operations
                .iter()
                .enumerate()
                .map(move |(index, _)| SourceItemId::Operation { function: function.id, block: block.id, index })
                .chain(std::iter::once(SourceItemId::Terminator { function: function.id, block: block.id }))
        }))
        .collect()
}

/// Independently check a target against source structure and mint its receipt.
/// This matcher does not call the lowering functions above: it validates the
/// source claim, position, semantic payload, and ownership rule directly.
fn check_correspondence(
    source: &CanonicalModule<InitialValid>,
    target: &WasmIr,
) -> Result<CorrespondenceReceipt, CorrespondenceError> {
    if owner_bridges::build(source).map_err(|_| CorrespondenceError::ModuleIdentityMismatch)? != target.owner_bridges {
        return Err(CorrespondenceError::ModuleIdentityMismatch);
    }
    if source.revision() != target.revision || source.checked_source() != target.checked_source {
        return Err(CorrespondenceError::ModuleIdentityMismatch);
    }

    let source_functions: BTreeMap<_, _> = source.functions().iter().map(|function| (function.id, function)).collect();
    let target_functions: BTreeMap<_, _> = target.functions.iter().map(|function| (function.id, function)).collect();
    for id in source_functions.keys() {
        if !target_functions.contains_key(id) {
            return Err(CorrespondenceError::MissingFunction(*id));
        }
    }
    for id in target_functions.keys() {
        if !source_functions.contains_key(id) {
            return Err(CorrespondenceError::UnexpectedFunction(*id));
        }
    }

    let expected = expected_source_items(source);
    let mut claimed = BTreeSet::new();
    for function in &target.functions {
        let source_function = source_functions[&function.id];
        if function.signature != source_function.signature || function.entry != source_function.entry || function.blocks.len() != source_function.blocks.len() {
            return Err(CorrespondenceError::FunctionShapeMismatch(function.id));
        }
        let source_blocks: BTreeMap<_, _> = source_function.blocks.iter().map(|block| (block.id, block)).collect();
        let target_blocks: BTreeMap<_, _> = function.blocks.iter().map(|block| (block.id, block)).collect();
        for id in source_blocks.keys() {
            if !target_blocks.contains_key(id) {
                return Err(CorrespondenceError::MissingBlock { function: function.id, block: *id });
            }
        }
        for id in target_blocks.keys() {
            if !source_blocks.contains_key(id) {
                return Err(CorrespondenceError::UnexpectedBlock { function: function.id, block: *id });
            }
        }
        for block in &function.blocks {
            let source_block = source_blocks[&block.id];
            for instruction in &block.instructions {
                if !claimed.insert(instruction.source) {
                    return Err(CorrespondenceError::DuplicateSourceItem(instruction.source));
                }
            }
            if !claimed.insert(block.terminator.source) {
                return Err(CorrespondenceError::DuplicateSourceItem(block.terminator.source));
            }
            if block.instructions.len() != source_block.operations.len() {
                continue;
            }
        }
    }
    if let Some(item) = expected.difference(&claimed).next() {
        return Err(CorrespondenceError::MissingSourceItem(*item));
    }
    if let Some(item) = claimed.difference(&expected).next() {
        return Err(CorrespondenceError::UnexpectedSourceItem(*item));
    }

    let mut mappings = Vec::new();
    for function in &target.functions {
        let source_function = source_functions[&function.id];
        let source_blocks: BTreeMap<_, _> = source_function.blocks.iter().map(|block| (block.id, block)).collect();
        for block in &function.blocks {
            let source_block = source_blocks[&block.id];
            if block.instructions.len() != source_block.operations.len() {
                return Err(CorrespondenceError::FunctionShapeMismatch(function.id));
            }
            for (index, instruction) in block.instructions.iter().enumerate() {
                let expected_source = SourceItemId::Operation { function: function.id, block: block.id, index };
                if instruction.source != expected_source {
                    return Err(CorrespondenceError::WrongSourcePosition { expected: expected_source, actual: instruction.source });
                }
                if !matches_operation(&source_block.operations[index], &instruction.operation) {
                    return Err(CorrespondenceError::InstructionMismatch(expected_source));
                }
                let Some(rule) = operation_rule(&source_block.operations[index]) else {
                    return Err(CorrespondenceError::UnsupportedSourceItem(expected_source));
                };
                mappings.push(CorrespondenceMapping { source: expected_source, target: TargetItemId::Instruction { function: function.id, block: block.id, index }, rule });
            }
            let expected_source = SourceItemId::Terminator { function: function.id, block: block.id };
            if block.terminator.source != expected_source {
                return Err(CorrespondenceError::WrongSourcePosition { expected: expected_source, actual: block.terminator.source });
            }
            if !matches_terminator(&source_block.terminator, &block.terminator.operation) {
                return Err(CorrespondenceError::TerminatorMismatch(expected_source));
            }
            mappings.push(CorrespondenceMapping { source: expected_source, target: TargetItemId::Terminator { function: function.id, block: block.id }, rule: CorrespondenceRule::Control });
        }
    }
    Ok(CorrespondenceReceipt { revision: source.revision(), checked_source: source.checked_source(), mappings })
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
fn wasm_value_type(
    function: FunctionId,
    value: ValueId,
    value_type: crate::canonical_safewir::ValueType,
) -> Result<ValType, EncodeError> {
    match value_type {
        crate::canonical_safewir::ValueType::Int => Ok(ValType::I64),
        crate::canonical_safewir::ValueType::Float => Ok(ValType::F64),
        crate::canonical_safewir::ValueType::Bool | crate::canonical_safewir::ValueType::Unit => Ok(ValType::I32),
        crate::canonical_safewir::ValueType::Size => Ok(ValType::I32),
        crate::canonical_safewir::ValueType::Ref(_) => Ok(ValType::I32),
        crate::canonical_safewir::ValueType::Gc(_) => Ok(ValType::EXTERNREF),
        crate::canonical_safewir::ValueType::Capability(_) => Ok(ValType::EXTERNREF),
        crate::canonical_safewir::ValueType::NullableCapability(_) => Ok(ValType::EXTERNREF),
        crate::canonical_safewir::ValueType::V128 => Ok(ValType::V128),
        // SafeWIR never exposes this representation. The callable is an
        // authenticated table-index value only after private WasmIR lowering.
        crate::canonical_safewir::ValueType::Callable(_) => Ok(ValType::I32),
        crate::canonical_safewir::ValueType::InlineAggregate(_) => {
            Err(EncodeError::UnsupportedValueType { function, value, value_type })
        }
    }
}

#[derive(Clone)]
struct LocalLayout<'a> {
    memory_helpers: Option<std::sync::Arc<outlined_memory::Helpers>>,
    owner_scratch: Option<[u32; 4]>,
    // Immutable admitted recipes are module-wide, not per-function state.
    // Copy only references: cleanup recipes contain the complete owner family
    // graph and deep-cloning them here multiplies that graph by function count.
    owner_bridges: Vec<&'a AdmittedOwnerBridge>,
    indices: BTreeMap<ValueId, u32>,
    extra_types: Vec<ValType>,
    scratch: Vec<u32>,
    // This function's owner-kernel context, parked between environment
    // operations (see `owner_operation_kernel::OwnerScratch`).
    owner_context: owner_operation_kernel::ParkedContext,
}

fn local_layout<'a>(
    function: &Function,
    signature: &FunctionSignature,
    layouts: &LayoutInterner,
) -> Result<LocalLayout<'a>, EncodeError> {
    let entry = function
        .blocks
        .iter()
        .find(|block| block.id == function.entry)
        .ok_or(EncodeError::MissingBlock { function: function.id, block: function.entry })?;
    if entry.parameters.len() != signature.parameters.len() {
        return Err(EncodeError::ParameterLayoutMismatch { function: function.id });
    }
    let mut indices = BTreeMap::new();
    for (index, parameter) in entry.parameters.iter().enumerate() {
        if parameter.value_type != signature.parameters[index]
            || function.value_types.get(&parameter.value) != Some(&parameter.value_type)
            || indices.insert(parameter.value, index as u32).is_some()
        {
            return Err(EncodeError::ParameterLayoutMismatch { function: function.id });
        }
    }
    let mut extra_types = Vec::new();
    let temporary_slots = scalar_locals::slots(function);
    scalar_locals::verify(function, &temporary_slots).map_err(|_| EncodeError::UnsupportedLayout {
        function: function.id, operation: "scalar local slot assignment",
    })?;
    let mut temporary_indices = BTreeMap::new();
    for (value, value_type) in &function.value_types {
        if indices.contains_key(value) {
            continue;
        }
        if matches!(value_type, ValueType::InlineAggregate(_)) {
            continue;
        }
        let ty = wasm_value_type(function.id, *value, *value_type)?;
        let index = if let Some(slot) = temporary_slots.get(value) {
            *temporary_indices.entry(*slot).or_insert_with(|| {
                let index = entry.parameters.len() as u32 + extra_types.len() as u32;
                extra_types.push(ty);
                index
            })
        } else {
            let index = entry.parameters.len() as u32 + extra_types.len() as u32;
            extra_types.push(ty);
            index
        };
        indices.insert(*value, index);
    }
// Derive scratch space from the whole-function value table. The independent
// operation planner retains that table when it plans one block at a time;
// inspecting only that block's operations would shift every allocator local.
let owner_scratch = if function.value_types.values().any(|value| matches!(value, ValueType::Gc(_))) {
    let base = entry.parameters.len() as u32 + extra_types.len() as u32;
    // The first two slots are linear-reference temporaries. The third holds
    // an opaque Gc token (for example a nested rooted-slice ReadOwner result)
    // and must therefore be externref. The fourth preserves the local slice
    // coordinate across linear-environment cleanup, which consumes the
    // ordinary worklist scratch slots.
    extra_types.extend([
        ValType::I32,
        ValType::I32,
        ValType::Ref(RefType::EXTERNREF),
        ValType::I32,
    ]);
    Some([base, base + 1, base + 2, base + 3])
} else { None };
let mut scratch = vec![0; drop_worklist::SCRATCH];

    for (slot, index) in scratch.iter_mut().enumerate() {
        *index = entry.parameters.len() as u32 + extra_types.len() as u32;
        extra_types.push(ValType::I32);
        if slot == 0 {
            // Keep the first scratch slot adjacent to the value locals. The
            // explicit indices below are still independent of value IDs.
        }
    }
    Ok(LocalLayout {
        memory_helpers: None,
        owner_scratch, owner_bridges: Vec::new(),
        indices,
        extra_types,
        scratch,
        owner_context: Default::default(),
    })
}

fn scratch_index(layout: &LocalLayout, slot: usize) -> u32 {
    layout.scratch[slot]
}

fn memarg(width: u32) -> MemArg {
    MemArg { offset: 0, align: width.next_power_of_two().trailing_zeros() as u32, memory_index: 0 }
}

fn emit_u32_trap_if_gt(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    value: ValueId,
    limit: u32,
) -> Result<(), EncodeError> {
    emit_get(body, layout, function, value)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(limit)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(())
}

fn emit_projected_address(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    access: MemoryAccess,
) -> Result<(), EncodeError> {
    if let Some(index) = access.index {
        let maximum = (u32::MAX
            .checked_sub(access.offset)
            .and_then(|value| value.checked_sub(access.width))
            .ok_or(EncodeError::UnsupportedLayout { function: function.id, operation: "address" })?)
            / access.collection.stride;
        emit_u32_trap_if_gt(body, layout, function.id, index, maximum)?;
        emit_get(body, layout, function.id, access.object)?;
        body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
        emit_get(body, layout, function.id, index)?;
        body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
        body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(access.collection.stride)));
        body.instruction(&wasm_encoder::Instruction::I64Mul);
        body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(access.offset)));
        body.instruction(&wasm_encoder::Instruction::I64Add);
        body.instruction(&wasm_encoder::Instruction::I64Add);
    } else {
        let maximum = u32::MAX.checked_sub(access.width).ok_or(EncodeError::UnsupportedLayout {
            function: function.id,
            operation: "address",
        })?;
    if let Some(helpers) = &layout.memory_helpers {
        emit_get(body, layout, function.id, access.object)?;
        body.instruction(&wasm_encoder::Instruction::I32Const(access.offset as i32));
        body.instruction(&wasm_encoder::Instruction::I32Const(maximum as i32));
        body.instruction(&wasm_encoder::Instruction::Call(helpers.address));
        return Ok(());
    }
        emit_get(body, layout, function.id, access.object)?;
        body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
        body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(access.offset)));
        body.instruction(&wasm_encoder::Instruction::I64Add);
        body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(maximum)));
        body.instruction(&wasm_encoder::Instruction::I64GtU);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Unreachable);
        body.instruction(&wasm_encoder::Instruction::End);
        emit_get(body, layout, function.id, access.object)?;
        body.instruction(&wasm_encoder::Instruction::I32Const(access.offset as i32));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        return Ok(());
    }
    emit_get(body, layout, function.id, access.object)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    emit_get(body, layout, function.id, access.index.expect("element access has an index"))?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(access.collection.stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(access.offset)));
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX - access.width)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::I32WrapI64);
    Ok(())
}


fn local_index(layout: &LocalLayout, function: FunctionId, value: ValueId) -> Result<u32, EncodeError> {
    layout.indices.get(&value).copied().ok_or(EncodeError::MissingValue { function, value })
}

fn emit_get(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    value: ValueId,
) -> Result<(), EncodeError> {
    body.instruction(&wasm_encoder::Instruction::LocalGet(local_index(layout, function, value)?));
    Ok(())
}

fn emit_set(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    value: ValueId,
) -> Result<(), EncodeError> {
    body.instruction(&wasm_encoder::Instruction::LocalSet(local_index(layout, function, value)?));
    Ok(())
}

fn emit_get_index(body: &mut wasm_encoder::Function, index: u32) {
    body.instruction(&wasm_encoder::Instruction::LocalGet(index));
}

fn emit_set_index(body: &mut wasm_encoder::Function, index: u32) {
    body.instruction(&wasm_encoder::Instruction::LocalSet(index));
}

fn emit_arithmetic(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: ValueId,
    operator: ArithmeticOperator,
    left: ValueId,
    right: ValueId,
) -> Result<(), EncodeError> {
    let value_type = function
        .value_types
        .get(&destination)
        .copied()
        .ok_or(EncodeError::MissingValue { function: function.id, value: destination })?;
    emit_get(body, layout, function.id, left)?;
    emit_get(body, layout, function.id, right)?;
    match value_type {
        crate::canonical_safewir::ValueType::Int => match operator {
            ArithmeticOperator::Add => { body.instruction(&wasm_encoder::Instruction::I64Add); }
            ArithmeticOperator::Subtract => { body.instruction(&wasm_encoder::Instruction::I64Sub); }
            ArithmeticOperator::Multiply => { body.instruction(&wasm_encoder::Instruction::I64Mul); }
            ArithmeticOperator::DivideSigned => { body.instruction(&wasm_encoder::Instruction::I64DivS); }
            ArithmeticOperator::DivideUnsigned => { body.instruction(&wasm_encoder::Instruction::I64DivU); }
            ArithmeticOperator::RemainderSigned => { body.instruction(&wasm_encoder::Instruction::I64RemS); }
            ArithmeticOperator::RemainderUnsigned => { body.instruction(&wasm_encoder::Instruction::I64RemU); }
            ArithmeticOperator::BitAnd => { body.instruction(&wasm_encoder::Instruction::I64And); }
            ArithmeticOperator::BitOr => { body.instruction(&wasm_encoder::Instruction::I64Or); }
            ArithmeticOperator::BitXor => { body.instruction(&wasm_encoder::Instruction::I64Xor); }
            ArithmeticOperator::ShiftLeft => { body.instruction(&wasm_encoder::Instruction::I64Shl); }
            ArithmeticOperator::ShiftRightSigned => { body.instruction(&wasm_encoder::Instruction::I64ShrS); }
            ArithmeticOperator::ShiftRightUnsigned => { body.instruction(&wasm_encoder::Instruction::I64ShrU); }
        },
        crate::canonical_safewir::ValueType::Size => match operator {
            ArithmeticOperator::Add => { body.instruction(&wasm_encoder::Instruction::I32Add); }
            ArithmeticOperator::Subtract => { body.instruction(&wasm_encoder::Instruction::I32Sub); }
            ArithmeticOperator::Multiply => { body.instruction(&wasm_encoder::Instruction::I32Mul); }
            ArithmeticOperator::DivideSigned => { body.instruction(&wasm_encoder::Instruction::I32DivS); }
            ArithmeticOperator::DivideUnsigned => { body.instruction(&wasm_encoder::Instruction::I32DivU); }
            ArithmeticOperator::RemainderSigned => { body.instruction(&wasm_encoder::Instruction::I32RemS); }
            ArithmeticOperator::RemainderUnsigned => { body.instruction(&wasm_encoder::Instruction::I32RemU); }
            ArithmeticOperator::BitAnd => { body.instruction(&wasm_encoder::Instruction::I32And); }
            ArithmeticOperator::BitOr => { body.instruction(&wasm_encoder::Instruction::I32Or); }
            ArithmeticOperator::BitXor => { body.instruction(&wasm_encoder::Instruction::I32Xor); }
            ArithmeticOperator::ShiftLeft => { body.instruction(&wasm_encoder::Instruction::I32Shl); }
            ArithmeticOperator::ShiftRightSigned => { body.instruction(&wasm_encoder::Instruction::I32ShrS); }
            ArithmeticOperator::ShiftRightUnsigned => { body.instruction(&wasm_encoder::Instruction::I32ShrU); }
        },
        crate::canonical_safewir::ValueType::Float => match operator {
            ArithmeticOperator::Add => { body.instruction(&wasm_encoder::Instruction::F64Add); }
            ArithmeticOperator::Subtract => { body.instruction(&wasm_encoder::Instruction::F64Sub); }
            ArithmeticOperator::Multiply => { body.instruction(&wasm_encoder::Instruction::F64Mul); }
            ArithmeticOperator::DivideSigned | ArithmeticOperator::DivideUnsigned => {
                body.instruction(&wasm_encoder::Instruction::F64Div);
            }
            _ => return Err(EncodeError::UnsupportedValueType { function: function.id, value: destination, value_type }),
        },
        _ => return Err(EncodeError::UnsupportedValueType { function: function.id, value: destination, value_type }),
    }
    emit_set(body, layout, function.id, destination)
}

fn emit_checked_arithmetic(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: ValueId,
    operator: ArithmeticOperator,
    left: ValueId,
    right: ValueId,
    overflow: crate::canonical_safewir::OverflowBehavior,
) -> Result<(), EncodeError> {
    match (
        function.value_types.get(&destination),
        operator,
        overflow,
    ) {
        (
            Some(ValueType::Int),
            ArithmeticOperator::Add,
            crate::canonical_safewir::OverflowBehavior::Trap,
        ) => {
            // Signed addition overflows exactly when both operands have the
            // same sign and the sum has the opposite sign:
            // `((sum ^ left) & (sum ^ right)) < 0`.
            emit_get(body, layout, function.id, left)?;
            emit_get(body, layout, function.id, right)?;
            body.instruction(&wasm_encoder::Instruction::I64Add);
            body.instruction(&wasm_encoder::Instruction::LocalTee(local_index(
                layout,
                function.id,
                destination,
            )?));
            emit_get(body, layout, function.id, left)?;
            body.instruction(&wasm_encoder::Instruction::I64Xor);
            emit_get(body, layout, function.id, destination)?;
            emit_get(body, layout, function.id, right)?;
            body.instruction(&wasm_encoder::Instruction::I64Xor);
            body.instruction(&wasm_encoder::Instruction::I64And);
            body.instruction(&wasm_encoder::Instruction::I64Const(0));
            body.instruction(&wasm_encoder::Instruction::I64LtS);
            body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
            body.instruction(&wasm_encoder::Instruction::Unreachable);
            body.instruction(&wasm_encoder::Instruction::End);
            return Ok(());
        }
        (
            Some(ValueType::Size),
            ArithmeticOperator::Add,
            crate::canonical_safewir::OverflowBehavior::Trap,
        ) => {
            emit_get(body, layout, function.id, left)?;
            body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
            emit_get(body, layout, function.id, right)?;
            body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
            body.instruction(&wasm_encoder::Instruction::I64Add);
            body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
            body.instruction(&wasm_encoder::Instruction::I64GtU);
            body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
            body.instruction(&wasm_encoder::Instruction::Unreachable);
            body.instruction(&wasm_encoder::Instruction::End);
        }
        (
            Some(ValueType::Int | ValueType::Size),
            ArithmeticOperator::DivideSigned
            | ArithmeticOperator::DivideUnsigned
            | ArithmeticOperator::RemainderSigned
            | ArithmeticOperator::RemainderUnsigned,
            crate::canonical_safewir::OverflowBehavior::Trap,
        )
        | (_, _, crate::canonical_safewir::OverflowBehavior::Wrap) => {}
        _ => {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedOverflow {
                function: function.id,
            }));
        }
    }
    emit_arithmetic(body, function, layout, destination, operator, left, right)
}

fn emit_compare(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: ValueId,
    operator: ComparisonOperator,
    left: ValueId,
    right: ValueId,
) -> Result<(), EncodeError> {
    let value_type = function
        .value_types
        .get(&left)
        .copied()
        .ok_or(EncodeError::MissingValue { function: function.id, value: left })?;
    let instruction = match value_type {
        crate::canonical_safewir::ValueType::Int => match operator {
            ComparisonOperator::Equal => wasm_encoder::Instruction::I64Eq,
            ComparisonOperator::NotEqual => wasm_encoder::Instruction::I64Ne,
            ComparisonOperator::LessSigned => wasm_encoder::Instruction::I64LtS,
            ComparisonOperator::LessUnsigned => wasm_encoder::Instruction::I64LtU,
            ComparisonOperator::LessEqualSigned => wasm_encoder::Instruction::I64LeS,
            ComparisonOperator::LessEqualUnsigned => wasm_encoder::Instruction::I64LeU,
            ComparisonOperator::GreaterSigned => wasm_encoder::Instruction::I64GtS,
            ComparisonOperator::GreaterUnsigned => wasm_encoder::Instruction::I64GtU,
            ComparisonOperator::GreaterEqualSigned => wasm_encoder::Instruction::I64GeS,
            ComparisonOperator::GreaterEqualUnsigned => wasm_encoder::Instruction::I64GeU,
        },
        crate::canonical_safewir::ValueType::Size | crate::canonical_safewir::ValueType::Bool => match operator {
            ComparisonOperator::Equal => wasm_encoder::Instruction::I32Eq,
            ComparisonOperator::NotEqual => wasm_encoder::Instruction::I32Ne,
            ComparisonOperator::LessSigned => wasm_encoder::Instruction::I32LtS,
            ComparisonOperator::LessUnsigned => wasm_encoder::Instruction::I32LtU,
            ComparisonOperator::LessEqualSigned => wasm_encoder::Instruction::I32LeS,
            ComparisonOperator::LessEqualUnsigned => wasm_encoder::Instruction::I32LeU,
            ComparisonOperator::GreaterSigned => wasm_encoder::Instruction::I32GtS,
            ComparisonOperator::GreaterUnsigned => wasm_encoder::Instruction::I32GtU,
            ComparisonOperator::GreaterEqualSigned => wasm_encoder::Instruction::I32GeS,
            ComparisonOperator::GreaterEqualUnsigned => wasm_encoder::Instruction::I32GeU,
        },
        crate::canonical_safewir::ValueType::Float => match operator {
            ComparisonOperator::Equal => wasm_encoder::Instruction::F64Eq,
            ComparisonOperator::NotEqual => wasm_encoder::Instruction::F64Ne,
            ComparisonOperator::LessSigned | ComparisonOperator::LessUnsigned => wasm_encoder::Instruction::F64Lt,
            ComparisonOperator::LessEqualSigned | ComparisonOperator::LessEqualUnsigned => wasm_encoder::Instruction::F64Le,
            ComparisonOperator::GreaterSigned | ComparisonOperator::GreaterUnsigned => wasm_encoder::Instruction::F64Gt,
            ComparisonOperator::GreaterEqualSigned | ComparisonOperator::GreaterEqualUnsigned => wasm_encoder::Instruction::F64Ge,
        },
        _ => return Err(EncodeError::UnsupportedValueType { function: function.id, value: left, value_type }),
    };
    emit_get(body, layout, function.id, left)?;
    emit_get(body, layout, function.id, right)?;
    body.instruction(&instruction);
    emit_set(body, layout, function.id, destination)
}

fn emit_narrow_size(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: ValueId,
    value: ValueId,
) -> Result<(), EncodeError> {
    emit_get(body, layout, function.id, value)?;
    body.instruction(&wasm_encoder::Instruction::I64Const(0));
    body.instruction(&wasm_encoder::Instruction::I64LtS);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get(body, layout, function.id, value)?;
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtS);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get(body, layout, function.id, value)?;
    body.instruction(&wasm_encoder::Instruction::I32WrapI64);
    emit_set(body, layout, function.id, destination)
}

fn emit_typed_load(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    access: MemoryAccess,
    destination: ValueId,
) -> Result<(), EncodeError> {
    emit_projected_address(body, function, layout, access)?;
    let arg = memarg(access.width);
    match access.scalar {
        ScalarKind::Unit => {
            body.instruction(&wasm_encoder::Instruction::I32Load(arg));
            body.instruction(&wasm_encoder::Instruction::Drop);
            body.instruction(&wasm_encoder::Instruction::I32Const(0))
        }
        ScalarKind::Bool | ScalarKind::Tag8 | ScalarKind::RawByte => body.instruction(&wasm_encoder::Instruction::I32Load8U(arg)),
        ScalarKind::Tag16 => body.instruction(&wasm_encoder::Instruction::I32Load16U(arg)),
        ScalarKind::U32 | ScalarKind::Tag32 | ScalarKind::Callable => body.instruction(&wasm_encoder::Instruction::I32Load(arg)),
        ScalarKind::Int | ScalarKind::Duration => body.instruction(&wasm_encoder::Instruction::I64Load(arg)),
        ScalarKind::Float => body.instruction(&wasm_encoder::Instruction::F64Load(arg)),
        _ => return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "scalar load" }),
    };
    emit_set(body, layout, function.id, destination)
}

fn emit_typed_store(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    access: MemoryAccess,
    value: ValueId,
) -> Result<(), EncodeError> {
    emit_projected_address(body, function, layout, access)?;
    if access.scalar == ScalarKind::Unit {
        body.instruction(&wasm_encoder::Instruction::I32Const(0));
    } else {
        emit_get(body, layout, function.id, value)?;
    }
    let arg = memarg(access.width);
    match access.scalar {
        ScalarKind::Bool | ScalarKind::Tag8 | ScalarKind::RawByte => body.instruction(&wasm_encoder::Instruction::I32Store8(arg)),
        ScalarKind::Tag16 => body.instruction(&wasm_encoder::Instruction::I32Store16(arg)),
        ScalarKind::Unit | ScalarKind::U32 | ScalarKind::Tag32 | ScalarKind::Callable => body.instruction(&wasm_encoder::Instruction::I32Store(arg)),
        ScalarKind::Int | ScalarKind::Duration => body.instruction(&wasm_encoder::Instruction::I64Store(arg)),
        ScalarKind::Float => body.instruction(&wasm_encoder::Instruction::F64Store(arg)),
        _ => return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "scalar store" }),
    };
    Ok(())
}

fn emit_init_nullable_owned(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    place: &Place,
) -> Result<(), EncodeError> {
    let access = resolve_memory_place(function, layouts, place).map_err(EncodeError::Lowering)?;
    if access.pointer_slot != Some(PointerSlot::OwnedNullable) || place.access() != AccessMode::Write {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "InitNullableOwned" });
    }
    emit_projected_address(body, function, layout, access)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(0));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    Ok(())
}

fn emit_clone_present(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    object: ValueId,
    function: &Function,
) -> Result<(), EncodeError> {
    emit_get(body, layout, function.id, object)?;
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_clone_ref(body, layout, object, object, function)
}

fn emit_alloc(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    destination: ValueId,
    layout_id: crate::layout::LayoutId,
    extent: &Extent,
) -> Result<(), EncodeError> {
    // The object's byte size lands in scratch 0: a counted collection
    // computes it from its capacity, a fixed layout (a record) is a constant.
    let collection = match extent {
        Extent::Counted { capacity, header_bytes, stride_bytes, .. } => {
            let collection = collection_layout(function.id, layouts, layout_id).map_err(EncodeError::Lowering)?;
            if *header_bytes != collection.data_offset || *stride_bytes != collection.stride {
                return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Alloc extent" });
            }
            let maximum = (u32::MAX - collection.data_offset) / collection.stride;
            emit_u32_trap_if_gt(body, layout, function.id, *capacity, maximum)?;
            emit_get(body, layout, function.id, *capacity)?;
            body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
            body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(collection.stride)));
            body.instruction(&wasm_encoder::Instruction::I64Mul);
            body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(collection.data_offset)));
            body.instruction(&wasm_encoder::Instruction::I64Add);
            body.instruction(&wasm_encoder::Instruction::I32WrapI64);
            emit_set_index(body, scratch_index(layout, 0));
            Some(collection)
        }
        Extent::FixedBytes(bytes) => {
            let bytes = fixed_alloc_bytes(*bytes)
                .ok_or(EncodeError::UnsupportedLayout { function: function.id, operation: "Alloc extent" })?;
            body.instruction(&wasm_encoder::Instruction::I32Const(bytes as i32));
            emit_set_index(body, scratch_index(layout, 0));
            None
        }
        Extent::HeaderPrefix { .. } => {
            return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Alloc extent" });
        }
    };
    // Search reclaimed storage first. Scratch 2 is the current free-list
    // handle and scratch 3 is its predecessor. Reuse requires an exact
    // physical extent: shrinking the header of a larger block would lose its
    // suffix and corrupt the runtime's contiguous heap walk.
    body.instruction(&wasm_encoder::Instruction::GlobalGet(1));
    emit_set_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Const(0));
    emit_set_index(body, scratch_index(layout, 3));
    body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Loop(BlockType::Empty));
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::GlobalGet(0));
    emit_set_index(body, scratch_index(layout, 1));
    // The object's end in bytes (64-bit, so the sum cannot wrap). When it
    // lies past the current linear memory, grow by the pages needed; a
    // refused growth traps rather than writing past memory.
    let emit_end = |body: &mut wasm_encoder::Function| {
        emit_get_index(body, scratch_index(layout, 1));
        body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
        body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(RC_PREFIX_BYTES)));
        body.instruction(&wasm_encoder::Instruction::I64Add);
        emit_get_index(body, scratch_index(layout, 0));
        body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
        body.instruction(&wasm_encoder::Instruction::I64Add);
    };
    emit_end(body);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_end(body);
    body.instruction(&wasm_encoder::Instruction::MemorySize(0));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(LINEAR_MEMORY_BYTES)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    emit_end(body);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(LINEAR_MEMORY_BYTES - 1)));
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(LINEAR_MEMORY_BYTES)));
    body.instruction(&wasm_encoder::Instruction::I64DivU);
    body.instruction(&wasm_encoder::Instruction::MemorySize(0));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Sub);
    body.instruction(&wasm_encoder::Instruction::I32WrapI64);
    body.instruction(&wasm_encoder::Instruction::MemoryGrow(0));
    body.instruction(&wasm_encoder::Instruction::I32Const(-1));
    body.instruction(&wasm_encoder::Instruction::I32Eq);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(RC_PREFIX_BYTES as i32));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_set_index(body, scratch_index(layout, 2));
    emit_get_index(body, scratch_index(layout, 2));
    emit_get_index(body, scratch_index(layout, 0));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::GlobalSet(0));
    body.instruction(&wasm_encoder::Instruction::Br(2));
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Const(4));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_get_index(body, scratch_index(layout, 0));
    body.instruction(&wasm_encoder::Instruction::I32Eq);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    emit_get_index(body, scratch_index(layout, 3));
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::GlobalSet(1));
    body.instruction(&wasm_encoder::Instruction::Else);
    emit_get_index(body, scratch_index(layout, 3));
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Const(RC_PREFIX_BYTES as i32));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    emit_set_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::Br(2));
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 2));
    emit_set_index(body, scratch_index(layout, 3));
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_set_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::Br(0));
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(4));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_get_index(body, scratch_index(layout, 0));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    emit_get_index(body, scratch_index(layout, 2));
    emit_set(body, layout, function.id, destination)?;
    if let (Some(collection), Extent::Counted { count, capacity, .. }) = (collection, extent) {
        emit_get_index(body, scratch_index(layout, 2));
        body.instruction(&wasm_encoder::Instruction::I32Const(collection.length_offset as i32));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        emit_get(body, layout, function.id, *count)?;
        body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
        if let Some(capacity_offset) = collection.capacity_offset {
            emit_get_index(body, scratch_index(layout, 2));
            body.instruction(&wasm_encoder::Instruction::I32Const(capacity_offset as i32));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_get(body, layout, function.id, *capacity)?;
            body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
        }
    }
    Ok(())
}

/// A fixed layout's allocation size, when it and the allocator prefix fit
/// the 32-bit address space.
fn fixed_alloc_bytes(bytes: u64) -> Option<u32> {
    u32::try_from(bytes)
        .ok()
        .filter(|bytes| *bytes <= u32::MAX - RC_PREFIX_BYTES)
        // A reclaimed object stores its next-link in the dead payload. Keep
        // the physical cell wide enough even for zero-sized and compact
        // scalar layouts; SafeWIR still authorizes only the logical layout.
        .map(|bytes| bytes.max(4))
}

#[cfg(test)]
#[test]
fn fixed_alloc_reserves_a_complete_free_list_link() {
    assert_eq!(fixed_alloc_bytes(0), Some(4));
    assert_eq!(fixed_alloc_bytes(1), Some(4));
    assert_eq!(fixed_alloc_bytes(2), Some(4));
    assert_eq!(fixed_alloc_bytes(4), Some(4));
    assert_eq!(fixed_alloc_bytes(5), Some(5));
}

fn emit_update_count(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    object: ValueId,
    count: ValueId,
) -> Result<(), EncodeError> {
    let collection = resolve_collection_layout(function, layouts, object).map_err(EncodeError::Lowering)?;
    if let Some(capacity_offset) = collection.capacity_offset {
        emit_get(body, layout, function.id, count)?;
        emit_get(body, layout, function.id, object)?;
        body.instruction(&wasm_encoder::Instruction::I32Const(capacity_offset as i32));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
        body.instruction(&wasm_encoder::Instruction::I32GtU);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Unreachable);
        body.instruction(&wasm_encoder::Instruction::End);
    }
    emit_get(body, layout, function.id, object)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(collection.length_offset as i32));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_get(body, layout, function.id, count)?;
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    Ok(())
}

fn emit_clone_ref(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    destination: ValueId,
    object: ValueId,
    function: &Function,
) -> Result<(), EncodeError> {
    emit_get(body, layout, function.id, object)?;
    if let Some(helpers) = &layout.memory_helpers {
        body.instruction(&wasm_encoder::Instruction::Call(helpers.retain));
        return emit_set(body, layout, function.id, destination);
    }
    emit_set_index(body, scratch_index(layout, 1));
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::GlobalGet(2));
    body.instruction(&wasm_encoder::Instruction::I32LtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Else);
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(8));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_set_index(body, scratch_index(layout, 2));
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Const(-1));
    body.instruction(&wasm_encoder::Instruction::I32Eq);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(8));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 1));
    emit_set(body, layout, function.id, destination)?;
    Ok(())
}

fn emit_reclaim(body: &mut wasm_encoder::Function, layout: &LocalLayout, function: &Function, object: ValueId) -> Result<(), EncodeError> {
    emit_get(body, layout, function.id, object)?;
    emit_set_index(body, scratch_index(layout, 1));
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::GlobalGet(1));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::GlobalSet(1));
    Ok(())
}

fn emit_reclaim_local(body: &mut wasm_encoder::Function, layout: &LocalLayout, slot: usize) {
    emit_get_index(body, scratch_index(layout, slot));
    body.instruction(&wasm_encoder::Instruction::GlobalGet(1));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    emit_get_index(body, scratch_index(layout, slot));
    body.instruction(&wasm_encoder::Instruction::GlobalSet(1));
}

/// The owned pointer fields of a fixed layout as `(byte offset, pointee)`.
fn fixed_pointer_fields(
    function: FunctionId,
    layouts: &LayoutInterner,
    layout: crate::layout::LayoutId,
) -> Result<Vec<(u32, crate::layout::LayoutId, bool)>, LoweringError> {
    let physical_layout = resolve_backend_layout(function, layouts, layout, "fixed pointer layout")?;
    let Some(descriptor) = layouts.get(physical_layout) else {
        return Err(LoweringError::UnsupportedLayout { function, operation: "fixed pointer layout" });
    };
    if !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. }) {
        return Ok(Vec::new());
    }
    descriptor
        .fields()
        .iter()
        .map(|field| match field.kind() {
            crate::layout::FieldKind::Pointer { pointee, slot } if matches!(slot, PointerSlot::Owned | PointerSlot::OwnedNullable) => {
                Ok(Some((field.offset(), pointee, slot == PointerSlot::OwnedNullable)))
            }
            crate::layout::FieldKind::Inline(slot) => {
                let physical_slot = resolve_backend_layout(function, layouts, slot, "fixed pointer field")?;
                match layouts.get(physical_slot).map(|slot| slot.kind()) {
                Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics })
                    if matches!(semantics, PointerSlot::Owned | PointerSlot::OwnedNullable) =>
                {
                    Ok(Some((field.offset(), *pointee, *semantics == PointerSlot::OwnedNullable)))
                }
                _ => Ok(None),
                }
            }
            _ => Ok(None),
        })
        .collect::<Result<Vec<_>, LoweringError>>()
        .map(|fields| fields.into_iter().flatten().collect())
}

fn emit_release(body: &mut wasm_encoder::Function, layout: &LocalLayout, function: &Function, object: ValueId, layouts: &LayoutInterner) -> Result<(), EncodeError> {
    let Some(ValueType::Ref(object_layout)) = function.value_types.get(&object).copied() else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "DropRef object" });
    };
    emit_get(body, layout, function.id, object)?;
    if let Some(index) = layout.memory_helpers.as_ref().and_then(|helpers| helpers.drops.get(&object_layout)) {
        body.instruction(&wasm_encoder::Instruction::Call(*index));
        return Ok(());
    }
    emit_set_index(body, scratch_index(layout, 0));
    drop_worklist::emit(body, layout, function.id, layouts, object_layout)
}

fn emit_call_arguments(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    arguments: &[ValueId],
) -> Result<(), EncodeError> {
    for value in arguments {
        emit_get(body, layout, function, *value)?;
    }
    Ok(())
}

fn emit_host_slice(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    argument: &HostCallArgument,
) -> Result<(), EncodeError> {
    let (place, access, with_length) = match argument {
        // A checked slice passes (ptr, len) in either direction; the place's
        // own access mode decides whether the range is bounded by the
        // object's count or by its capacity.
        HostCallArgument::Buffer { place, extent: HostBufferExtent::Slice } => (place, place.access(), true),
        // A write buffer of a declared minimum size passes its address only.
        HostCallArgument::Buffer { place, extent: HostBufferExtent::AtLeast(_) } => (place, AccessMode::Write, false),
        _ => return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer" })),
    };
    let Projection::Slice { length, .. } = place.projection() else {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer projection" }));
    };
    emit_raw_byte_slice_address(body, layout, function, value_types, layouts, place, access, *length)?;
    if with_length {
        emit_get(body, layout, function, *length)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_raw_byte_slice_address(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    place: &Place,
    access: AccessMode,
    length: ValueId,
) -> Result<(), EncodeError> {
    let (object, start, length) = raw_byte_slice_place(function, value_types, layouts, place, access, length)
        .map_err(EncodeError::Lowering)?;
    // Authenticate the complete physical range before narrowing it to the
    // two i32 ABI lanes. This is redundant with the SafeWIR checker by design:
    // the emitted host pointer must never be allowed to wrap independently.
    emit_get(body, layout, function, object)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(4));
    body.instruction(&wasm_encoder::Instruction::I64Add);
    emit_get(body, layout, function, start)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    emit_get(body, layout, function, length)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get(body, layout, function, object)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(4));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_get(body, layout, function, start)?;
    body.instruction(&wasm_encoder::Instruction::I32Add);
    Ok(())
}

fn emit_copy_bytes(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: &Place,
    source: &Place,
    length: ValueId,
    layouts: &LayoutInterner,
) -> Result<(), EncodeError> {
    emit_raw_byte_slice_address(
        body,
        layout,
        function.id,
        &function.value_types,
        layouts,
        destination,
        AccessMode::Write,
        length,
    )?;
    emit_raw_byte_slice_address(
        body,
        layout,
        function.id,
        &function.value_types,
        layouts,
        source,
        AccessMode::Read,
        length,
    )?;
    emit_get(body, layout, function.id, length)?;
    body.instruction(&wasm_encoder::Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
    Ok(())
}

fn emit_move_range_base(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    object: ValueId,
    start: ValueId,
    collection: CollectionLayout,
    scratch: usize,
) -> Result<(), EncodeError> {
    emit_get(body, layout, function.id, object)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    emit_get(body, layout, function.id, start)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(collection.stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(collection.data_offset)));
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get(body, layout, function.id, object)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    emit_get(body, layout, function.id, start)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(collection.stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(collection.data_offset)));
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I32WrapI64);
    body.instruction(&wasm_encoder::Instruction::LocalSet(scratch_index(layout, scratch)));
    Ok(())
}

fn emit_move_range_element_address(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    base_scratch: usize,
    index_scratch: usize,
    stride: u32,
) {
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, base_scratch)));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, index_scratch)));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, base_scratch)));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, index_scratch)));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I32WrapI64);
}

fn emit_move_range(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: &Place,
    source: &Place,
    count: ValueId,
    layouts: &LayoutInterner,
) -> Result<(), EncodeError> {
    let destination_layout = resolve_collection_layout(function, layouts, destination.object()).map_err(EncodeError::Lowering)?;
    let source_layout = resolve_collection_layout(function, layouts, source.object()).map_err(EncodeError::Lowering)?;
    if destination_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
        || source_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
        || destination_layout != source_layout
    {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { function: function.id, operation: "MoveRange" }));
    }
    if destination_layout.pointer_layout.is_none() {
        // Scalar movement is logical ownership transfer. The checked source
        // becomes unreadable, so the backend may leave its physical bytes in
        // place and use the same overlap-safe copy sequence as CloneRange.
        return emit_clone_range(body, function, layout, destination, source, count, layouts);
    }
    let (destination_start, source_start) = match (destination.projection(), source.projection()) {
        (Projection::Slice { start: destination_start, length: destination_length }, Projection::Slice { start: source_start, length: source_length })
            if *destination_length == count && *source_length == count => (*destination_start, *source_start),
        _ => return Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { function: function.id, operation: "MoveRange" })),
    };
    emit_get(body, layout, function.id, count)?;
    body.instruction(&wasm_encoder::Instruction::LocalSet(scratch_index(layout, 3)));
    emit_move_range_base(body, function, layout, source.object(), source_start, source_layout, 1)?;
    emit_move_range_base(body, function, layout, destination.object(), destination_start, destination_layout, 2)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(0));
    body.instruction(&wasm_encoder::Instruction::LocalSet(scratch_index(layout, 0)));
    body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Loop(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, 0)));
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, 3)));
    body.instruction(&wasm_encoder::Instruction::I32LtU);
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Br(2));
    body.instruction(&wasm_encoder::Instruction::End);
    emit_move_range_element_address(body, layout, 2, 0, destination_layout.stride);
    emit_move_range_element_address(body, layout, 1, 0, source_layout.stride);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    emit_move_range_element_address(body, layout, 1, 0, source_layout.stride);
    body.instruction(&wasm_encoder::Instruction::I32Const(0));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, 0)));
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::LocalSet(scratch_index(layout, 0)));
    body.instruction(&wasm_encoder::Instruction::Br(0));
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(())
}

fn emit_clone_range(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: &Place,
    source: &Place,
    count: ValueId,
    layouts: &LayoutInterner,
) -> Result<(), EncodeError> {
    let destination_layout = resolve_collection_layout(function, layouts, destination.object()).map_err(EncodeError::Lowering)?;
    let source_layout = resolve_collection_layout(function, layouts, source.object()).map_err(EncodeError::Lowering)?;
    if destination_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
        || source_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
        || destination_layout != source_layout
    {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { function: function.id, operation: "CloneRange" }));
    }
    let (destination_start, source_start) = match (destination.projection(), source.projection()) {
        (Projection::Slice { start: destination_start, length: destination_length }, Projection::Slice { start: source_start, length: source_length })
            if *destination_length == count && *source_length == count => (*destination_start, *source_start),
        _ => return Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { function: function.id, operation: "CloneRange" })),
    };
    emit_move_range_base(body, function, layout, source.object(), source_start, source_layout, 1)?;
    emit_move_range_base(body, function, layout, destination.object(), destination_start, destination_layout, 2)?;
    emit_get(body, layout, function.id, count)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(destination_layout.stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    if destination_layout.pointer_layout.is_some() {
        // Retain every source element before publishing the copied prefix.
        // A zero or saturated child count may trap; until the whole loop
        // succeeds, the destination remains untouched.
        if layout.scratch.len() < 5 {
            return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "CloneRange scratch depth" });
        }
        body.instruction(&wasm_encoder::Instruction::I32Const(0));
        emit_set_index(body, scratch_index(layout, 0));
        body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Loop(BlockType::Empty));
        emit_get_index(body, scratch_index(layout, 0));
        emit_get(body, layout, function.id, count)?;
        body.instruction(&wasm_encoder::Instruction::I32LtU);
        body.instruction(&wasm_encoder::Instruction::I32Eqz);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Br(2));
        body.instruction(&wasm_encoder::Instruction::End);
        emit_get_index(body, scratch_index(layout, 1));
        emit_get_index(body, scratch_index(layout, 0));
        body.instruction(&wasm_encoder::Instruction::I32Const(destination_layout.stride as i32));
        body.instruction(&wasm_encoder::Instruction::I32Mul);
        body.instruction(&wasm_encoder::Instruction::I32Add);
        body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
        emit_set_index(body, scratch_index(layout, 3));
        emit_get_index(body, scratch_index(layout, 3));
        body.instruction(&wasm_encoder::Instruction::GlobalGet(2));
        body.instruction(&wasm_encoder::Instruction::I32LtU);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Else);
        emit_get_index(body, scratch_index(layout, 3));
        body.instruction(&wasm_encoder::Instruction::I32Const(8));
        body.instruction(&wasm_encoder::Instruction::I32Sub);
        body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
        emit_set_index(body, scratch_index(layout, 4));
        emit_get_index(body, scratch_index(layout, 4));
        body.instruction(&wasm_encoder::Instruction::I32Eqz);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Unreachable);
        body.instruction(&wasm_encoder::Instruction::End);
        emit_get_index(body, scratch_index(layout, 4));
        body.instruction(&wasm_encoder::Instruction::I32Const(-1));
        body.instruction(&wasm_encoder::Instruction::I32Eq);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Unreachable);
        body.instruction(&wasm_encoder::Instruction::End);
        emit_get_index(body, scratch_index(layout, 3));
        body.instruction(&wasm_encoder::Instruction::I32Const(8));
        body.instruction(&wasm_encoder::Instruction::I32Sub);
        emit_get_index(body, scratch_index(layout, 4));
        body.instruction(&wasm_encoder::Instruction::I32Const(1));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
        body.instruction(&wasm_encoder::Instruction::End);
        emit_get_index(body, scratch_index(layout, 0));
        body.instruction(&wasm_encoder::Instruction::I32Const(1));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        emit_set_index(body, scratch_index(layout, 0));
        body.instruction(&wasm_encoder::Instruction::Br(0));
        body.instruction(&wasm_encoder::Instruction::End);
        body.instruction(&wasm_encoder::Instruction::End);
    }
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, 2)));
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, 1)));
    emit_get(body, layout, function.id, count)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(destination_layout.stride as i32));
    body.instruction(&wasm_encoder::Instruction::I32Mul);
    body.instruction(&wasm_encoder::Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
    Ok(())
}

fn emit_host_call(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destinations: &[ValueId],
    import: ImportId,
    arguments: &[HostCallArgument],
    contracts: &BTreeMap<ImportId, ImportContract>,
    import_indices: &BTreeMap<ImportId, u32>,
    layouts: &LayoutInterner,
) -> Result<(), EncodeError> {
    let contract = contracts.get(&import).ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
        function: function.id,
        operation: "HostCall contract",
    }))?;
    for (role, argument) in contract.parameter_roles.iter().zip(arguments) {
        match (role, argument) {
            (HostParameterRole::Value(_), HostCallArgument::Value(value))
            | (HostParameterRole::Capability(_), HostCallArgument::Capability(value)) => {
                emit_get(body, layout, function.id, *value)?;
            }
            (HostParameterRole::Buffer { .. }, HostCallArgument::Buffer { .. }) => {
                emit_host_slice(body, layout, function.id, &function.value_types, layouts, argument)?;
            }
            _ => return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation { function: function.id, operation: "HostCall argument" })),
        }
    }
    let index = import_indices.get(&import).copied().ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
        function: function.id,
        operation: "HostCall import",
    }))?;
    body.instruction(&wasm_encoder::Instruction::Call(index));
    for destination in destinations.iter().rev() {
        emit_set(body, layout, function.id, *destination)?;
    }
    Ok(())
}

fn emit_external_call(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destinations: &[ValueId],
    import: ImportId,
    arguments: &[ValueId],
    import_indices: &BTreeMap<ImportId, u32>,
) -> Result<(), EncodeError> {
    for argument in arguments {
        if function.value_types.get(argument) != Some(&ValueType::Unit) {
            emit_get(body, layout, function.id, *argument)?;
        }
    }
    let index = import_indices.get(&import).copied().ok_or(
        EncodeError::Lowering(LoweringError::UnsupportedOperation {
            function: function.id,
            operation: "ExternalCall import",
        }),
    )?;
    body.instruction(&wasm_encoder::Instruction::Call(index));
    // Only non-unit results arrived on the Wasm operand stack.  Materialize
    // source unit destinations locally after draining them, preserving the
    // source-level value map without exporting a physical unit lane.
    for destination in destinations.iter().rev() {
        if function.value_types.get(destination) != Some(&ValueType::Unit) {
            emit_set(body, layout, function.id, *destination)?;
        }
    }
    for destination in destinations {
        if function.value_types.get(destination) == Some(&ValueType::Unit) {
            body.instruction(&wasm_encoder::Instruction::I32Const(0));
            emit_set(body, layout, function.id, *destination)?;
        }
    }
    Ok(())
}

fn emit_results(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    destinations: &[ValueId],
) -> Result<(), EncodeError> {
    for value in destinations.iter().rev() {
        emit_set(body, layout, function, *value)?;
    }
    Ok(())
}

fn emit_instruction(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    instruction: &ScalarInstruction,
    function_indices: &BTreeMap<FunctionId, u32>,
    signature_indices: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    import_indices: &BTreeMap<ImportId, u32>,
    imports: &BTreeMap<ImportId, ImportContract>,
    static_data: &BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    layouts: &LayoutInterner,
    owner_cleanup_function: Option<u32>,
) -> Result<(), EncodeError> {
    match instruction {
        ScalarInstruction::ProofNoop => {}
        ScalarInstruction::ClosureOwner(operation) => { owner_operation_kernel::emit(body, function, layout, layouts, operation, import_indices, owner_cleanup_function)?; }
        ScalarInstruction::UnitConst { destination } => { body.instruction(&wasm_encoder::Instruction::I32Const(0)); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::IntConst { destination, value } => { body.instruction(&wasm_encoder::Instruction::I64Const(*value)); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::FloatConst { destination, bits } => { body.instruction(&wasm_encoder::Instruction::F64Const(Ieee64::new(*bits))); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::BoolConst { destination, value } => { body.instruction(&wasm_encoder::Instruction::I32Const(i32::from(*value))); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::SizeConst { destination, value } => {
            let value = u32::try_from(*value).map_err(|_| EncodeError::UnsupportedValueType { function: function.id, value: *destination, value_type: crate::canonical_safewir::ValueType::Size })?;
            body.instruction(&wasm_encoder::Instruction::I32Const(value as i32));
            emit_set(body, layout, function.id, *destination)?;
        }
        ScalarInstruction::CapabilityNone { destination } => { body.instruction(&wasm_encoder::Instruction::RefNull(wasm_encoder::HeapType::EXTERN)); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::CapabilitySome { destination, source } => { emit_get(body, layout, function.id, *source)?; emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::CapabilityIsPresent { destination, source } => { emit_get(body, layout, function.id, *source)?; body.instruction(&wasm_encoder::Instruction::RefIsNull); body.instruction(&wasm_encoder::Instruction::I32Eqz); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::CapabilityUnwrap { destination, source } => { emit_get(body, layout, function.id, *source)?; body.instruction(&wasm_encoder::Instruction::RefAsNonNull); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::Arithmetic { destination, operator, left, right, overflow } => {
            emit_checked_arithmetic(
                body,
                function,
                layout,
                *destination,
                *operator,
                *left,
                *right,
                *overflow,
            )?;
        }
        ScalarInstruction::Compare { destination, operator, left, right } => { emit_compare(body, function, layout, *destination, *operator, *left, *right)?; }
        ScalarInstruction::NarrowSize { destination, value } => emit_narrow_size(body, function, layout, *destination, *value)?,
        ScalarInstruction::WidenSize { destination, value } => {
            emit_get(body, layout, function.id, *value)?;
            body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
            emit_set(body, layout, function.id, *destination)?;
        }
        ScalarInstruction::FloatUnary { destination, operator, value } => {
            emit_get(body, layout, function.id, *value)?;
            body.instruction(&match operator {
                crate::canonical_safewir::FloatUnaryOperator::Sqrt => wasm_encoder::Instruction::F64Sqrt,
                crate::canonical_safewir::FloatUnaryOperator::TruncateToInt => wasm_encoder::Instruction::I64TruncSatF64S,
                crate::canonical_safewir::FloatUnaryOperator::FromInt => wasm_encoder::Instruction::F64ConvertI64S,
            });
            emit_set(body, layout, function.id, *destination)?;
        }
        ScalarInstruction::Guard { condition, .. } => { emit_get(body, layout, function.id, *condition)?; body.instruction(&wasm_encoder::Instruction::I32Eqz); body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty)); body.instruction(&wasm_encoder::Instruction::Unreachable); body.instruction(&wasm_encoder::Instruction::End); }
        ScalarInstruction::MoveValue { destination, source } => { emit_get(body, layout, function.id, *source)?; emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::MakeInlineAggregate { .. } => {}
        ScalarInstruction::ExtractInlineField { destination, field, .. } => { emit_get(body, layout, function.id, *field)?; emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::FunctionValue { destination, table_index, .. } => {
            body.instruction(&wasm_encoder::Instruction::I32Const(*table_index as i32));
            emit_set(body, layout, function.id, *destination)?;
        }
        ScalarInstruction::Alloc { destination, layout: layout_id, extent, class } => {
            if *class != crate::canonical_safewir::AllocationClass::ReferenceCounted {
                return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Alloc class" });
            }
            emit_alloc(body, function, layout, layouts, *destination, *layout_id, extent)?;
        }
        ScalarInstruction::StaticData { destination, data } => {
            let offset = static_data.get(data).copied().ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
                function: function.id,
                operation: "StaticData",
            }))?;
            body.instruction(&wasm_encoder::Instruction::I32Const(offset as i32));
            emit_set(body, layout, function.id, *destination)?;
        }
        ScalarInstruction::InitNullableOwned { place } => {
            emit_init_nullable_owned(body, function, layout, layouts, place)?;
        }
        ScalarInstruction::Load { destination, place, transfer } => {
            let access = resolve_memory_place(function, layouts, place).map_err(EncodeError::Lowering)?;
            emit_typed_load(body, function, layout, access, *destination)?;
            match transfer {
                LoadTransfer::CloneShared => emit_clone_ref(body, layout, *destination, *destination, function)?,
                LoadTransfer::ClonePresent => emit_clone_present(body, layout, *destination, function)?,
                LoadTransfer::TakeLast => {
                    let Projection::Element { index } = place.projection() else { return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "TakeLast" }); };
                    emit_update_count(body, function, layout, layouts, place.object(), *index)?;
                }
                LoadTransfer::MoveUnique | LoadTransfer::CopyScalar => {}
            }
        }
        ScalarInstruction::Store { place, value, transfer } => {
            if !matches!(*transfer, StoreTransfer::CopyScalar | StoreTransfer::MoveUnique | StoreTransfer::CloneShared) {
                return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Store transfer" });
            }
            let access = resolve_memory_place(function, layouts, place).map_err(EncodeError::Lowering)?;
            if *transfer == StoreTransfer::CloneShared {
                // Retain before publishing the second edge. Assigning the
                // source local to itself preserves its existing owned token.
                emit_clone_ref(body, layout, *value, *value, function)?;
            }
            emit_typed_store(body, function, layout, access, *value)?;
        }
        ScalarInstruction::CopyBytes { destination, source, length } => {
            emit_copy_bytes(body, function, layout, destination, source, *length, layouts)?;
        }
        ScalarInstruction::MoveRange { destination, source, count } => {
            emit_move_range(body, function, layout, destination, source, *count, layouts)?;
        }
        ScalarInstruction::CloneRange { destination, source, count } => {
            emit_clone_range(body, function, layout, destination, source, *count, layouts)?;
        }
        ScalarInstruction::UpdateCount { object, count } => emit_update_count(body, function, layout, layouts, *object, *count)?,
        ScalarInstruction::CloneRef { destination, source } => emit_clone_ref(body, layout, *destination, *source, function)?,
        ScalarInstruction::Release { object } => emit_release(body, layout, function, *object, layouts)?,
        ScalarInstruction::Reclaim { object } => emit_reclaim(body, layout, function, *object)?,
        ScalarInstruction::Call { destinations, callee, arguments } => {
            emit_call_arguments(body, layout, function.id, arguments)?;
            let index = function_indices.get(callee).copied().ok_or(EncodeError::MissingFunction(*callee))?;
            body.instruction(&wasm_encoder::Instruction::Call(index));
            emit_results(body, layout, function.id, destinations)?;
        }
        ScalarInstruction::ExternalCall { destinations, import, arguments } => {
            emit_external_call(body, function, layout, destinations, *import, arguments, import_indices)?;
        }
        ScalarInstruction::HostCall { destinations, import, arguments } => emit_host_call(body, function, layout, destinations, *import, arguments, imports, import_indices, layouts)?,
        ScalarInstruction::VarCall { destinations, callee, signature, arguments } => {
            emit_call_arguments(body, layout, function.id, arguments)?;
            emit_get(body, layout, function.id, *callee)?;
            let type_index = signature_indices.get(signature).copied().ok_or(EncodeError::UnsupportedSignature { signature: *signature })?;
            body.instruction(&wasm_encoder::Instruction::CallIndirect { type_index, table_index: 0 });
            emit_results(body, layout, function.id, destinations)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StructuredLabel {
    Block,
    Loop,
    If,
}

/// Immediate dominators of every block reachable from `entry`
/// (Cooper-Harvey-Kennedy over a reverse postorder). Unreachable blocks are
/// absent. Computed once per emitter; both the encoder and the plan mirror
/// answer `dominates` from it, so they keep choosing identical loops.
fn immediate_dominators(
    entry: crate::canonical_safewir::BlockId,
    successors_by_block: &BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
) -> BTreeMap<crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId> {
    // Reverse postorder by an explicit DFS: an 11k-block executor must not
    // recurse on the host stack.
    let mut postorder = Vec::new();
    let mut seen = BTreeSet::from([entry]);
    let mut stack = vec![(entry, 0usize)];
    while let Some((block, next)) = stack.last_mut() {
        let successors = successors_by_block.get(block).map(Vec::as_slice).unwrap_or(&[]);
        if *next < successors.len() {
            let successor = successors[*next];
            *next += 1;
            if seen.insert(successor) {
                stack.push((successor, 0));
            }
        } else {
            postorder.push(*block);
            stack.pop();
        }
    }
    let order: BTreeMap<_, _> = postorder.iter().rev().enumerate().map(|(index, block)| (*block, index)).collect();
    let mut predecessors: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for (block, successors) in successors_by_block {
        if !order.contains_key(block) {
            continue;
        }
        for successor in successors {
            predecessors.entry(*successor).or_default().push(*block);
        }
    }
    let mut idom = BTreeMap::from([(entry, entry)]);
    let intersect = |idom: &BTreeMap<_, _>, mut left, mut right| {
        while left != right {
            while order[&left] > order[&right] {
                left = idom[&left];
            }
            while order[&right] > order[&left] {
                right = idom[&right];
            }
        }
        left
    };
    let mut changed = true;
    while changed {
        changed = false;
        for block in postorder.iter().rev().copied() {
            if block == entry {
                continue;
            }
            let mut candidate = None;
            for predecessor in predecessors.get(&block).map(Vec::as_slice).unwrap_or(&[]) {
                if !idom.contains_key(predecessor) {
                    continue;
                }
                candidate = Some(match candidate {
                    None => *predecessor,
                    Some(current) => intersect(&idom, current, *predecessor),
                });
            }
            let Some(candidate) = candidate else { continue };
            if idom.get(&block) != Some(&candidate) {
                idom.insert(block, candidate);
                changed = true;
            }
        }
    }
    idom.remove(&entry);
    idom
}

/// One more than the largest block id an emitter's edge map names: block
/// ids are minted densely by lowering and release placement, so a `Vec`
/// indexed by id is a cheap visited set for the emitter's walks.
fn block_slot_count(
    successors_by_block: &BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
) -> usize {
    successors_by_block
        .iter()
        .flat_map(|(block, successors)| std::iter::once(*block).chain(successors.iter().copied()))
        .map(|id| id.get() as usize + 1)
        .max()
        .unwrap_or(0)
}

/// `dominator` lies on every path from `entry` to `node`. A block the entry
/// does not reach has no path at all, so every block dominates it.
fn dominates_in(
    idom: &BTreeMap<crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId>,
    entry: crate::canonical_safewir::BlockId,
    dominator: crate::canonical_safewir::BlockId,
    node: crate::canonical_safewir::BlockId,
) -> bool {
    if node != entry && !idom.contains_key(&node) {
        return true;
    }
    let mut current = node;
    loop {
        if current == dominator {
            return true;
        }
        if current == entry {
            return false;
        }
        current = idom[&current];
    }
}

fn reverse_loop_members(
    header: crate::canonical_safewir::BlockId,
    back_edge_sources: Vec<crate::canonical_safewir::BlockId>,
    predecessors: &BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
) -> BTreeSet<crate::canonical_safewir::BlockId> {
    let mut members = BTreeSet::new();
    let mut pending = VecDeque::from(back_edge_sources);
    while let Some(block) = pending.pop_front() {
        if block == header || !members.insert(block) {
            continue;
        }
        if let Some(sources) = predecessors.get(&block) {
            pending.extend(sources.iter().copied());
        }
    }
    members
}

struct StructuredEmitter<'a> {
    body: &'a mut WasmFunctionEncoder,
    source_function: &'a Function,
    target_function: &'a WasmFunction,
    /// Target blocks by id, for the same reason as the plan emitter's map.
    blocks_by_id: BTreeMap<crate::canonical_safewir::BlockId, &'a WasmBlock>,
    layout: &'a LocalLayout<'a>,
    function_indices: &'a BTreeMap<FunctionId, u32>,
    signature_indices: &'a BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    import_indices: &'a BTreeMap<ImportId, u32>,
    imports: &'a BTreeMap<ImportId, ImportContract>,
    static_data: &'a BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    layouts: &'a LayoutInterner,
    owner_cleanup_function: Option<u32>,
    successors_by_block: BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
    predecessors_by_block: BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
    emitted: BTreeSet<crate::canonical_safewir::BlockId>,
    labels: Vec<StructuredLabel>,
    loops: Vec<(crate::canonical_safewir::BlockId, usize)>,
    /// Enclosing loops' exit blocks with their `block` labels: a branch to one
    /// leaves the loop with a `br`.
    loop_exits: Vec<(crate::canonical_safewir::BlockId, usize)>,
    /// Memoized `is_loop_header` judgments; the dominance walk is quadratic
    /// and the structuring queries ask about the same blocks repeatedly.
    loop_headers: RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>,
    loop_exit_targets: RefCell<BTreeMap<crate::canonical_safewir::BlockId, Option<crate::canonical_safewir::BlockId>>>,
    back_edge_source_facts: RefCell<BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>>,
    natural_loop_facts: RefCell<BTreeMap<crate::canonical_safewir::BlockId, BTreeSet<crate::canonical_safewir::BlockId>>>,
    reaches_avoiding_facts: RefCell<BTreeMap<(crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId), bool>>,
    immediate_dominators: std::cell::OnceCell<BTreeMap<crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId>>,
    block_slots: std::cell::OnceCell<usize>,
    /// Memoized path judgments keyed by the exact normalized merge set.
    /// Both answers are reusable: a cut cycle proves a path from `start`
    /// avoids every merge and terminal, independent of which caller first
    /// encountered it. Caching refusal is essential for diamond graphs with
    /// a shared cyclic suffix.
    path_facts: RefCell<BTreeMap<Vec<crate::canonical_safewir::BlockId>, std::rc::Rc<RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>>>>,
}

impl StructuredEmitter<'_> {
    fn block(&self, id: crate::canonical_safewir::BlockId) -> Result<&WasmBlock, EncodeError> {
        self.blocks_by_id
            .get(&id)
            .copied()
            .ok_or(EncodeError::MissingBlock {
                function: self.source_function.id,
                block: id,
            })
    }

    fn successors(
        &self,
        id: crate::canonical_safewir::BlockId,
    ) -> Result<Vec<crate::canonical_safewir::BlockId>, EncodeError> {
        self.successors_by_block.get(&id).cloned().ok_or(EncodeError::MissingBlock {
            function: self.source_function.id,
            block: id,
        })
    }

    /// `distances_from` stopping at `stops` (which are never entered).
    fn distances_until(
        &self,
        start: crate::canonical_safewir::BlockId,
        stops: &[crate::canonical_safewir::BlockId],
    ) -> Result<BTreeMap<crate::canonical_safewir::BlockId, usize>, EncodeError> {
        let mut distances = BTreeMap::new();
        let mut pending = VecDeque::from([(start, 0)]);
        while let Some((id, distance)) = pending.pop_front() {
            if stops.contains(&id) || distances.contains_key(&id) {
                continue;
            }
            self.block(id)?;
            distances.insert(id, distance);
            pending.extend(self.successors(id)?.into_iter().map(|successor| (successor, distance + 1)));
        }
        Ok(distances)
    }

    fn distances_from(
        &self,
        start: crate::canonical_safewir::BlockId,
    ) -> Result<BTreeMap<crate::canonical_safewir::BlockId, usize>, EncodeError> {
        let mut distances = BTreeMap::new();
        let mut pending = VecDeque::from([(start, 0)]);
        while let Some((id, distance)) = pending.pop_front() {
            if distances.contains_key(&id) {
                continue;
            }
            self.block(id)?;
            distances.insert(id, distance);
            pending.extend(
                self.successors(id)?
                    .into_iter()
                    .map(|successor| (successor, distance + 1)),
            );
        }
        Ok(distances)
    }

    fn reaches(
        &self,
        start: crate::canonical_safewir::BlockId,
        goal: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        // encode_function checks every successor while constructing the
        // predecessor map, before any query can succeed early. Keep distance
        // maps for join scores; this query needs only graph membership.
        let mut visited = BTreeSet::new();
        let mut pending = VecDeque::from([start]);
        while let Some(id) = pending.pop_front() {
            if !visited.insert(id) { continue; }
            self.block(id)?;
            if id == goal { return Ok(true); }
            let successors = self.successors_by_block.get(&id).ok_or(EncodeError::MissingBlock {
                function: self.source_function.id, block: id,
            })?;
            pending.extend(successors.iter().copied());
        }
        Ok(false)
    }

    /// One slot per block id, for dense visited marks in the walks below.
    fn block_slots(&self) -> usize {
        *self.block_slots.get_or_init(|| block_slot_count(&self.successors_by_block))
    }

    /// A side only terminates when every path from it ends in a terminal
    /// block (structured loops included: their bodies return to their
    /// headers or leave through their exits, and the exits terminate) and it
    /// shares no block with the other side, where the other side is what
    /// `other` reaches without passing back through `avoid` (the branching
    /// block: a loop back edge would otherwise make a returning arm look
    /// reachable from the continuing side). Such a side never reaches a
    /// merge and needs none.
    fn only_terminates_avoiding(
        &self,
        start: crate::canonical_safewir::BlockId,
        other: crate::canonical_safewir::BlockId,
        avoid: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        let slots = self.block_slots();
        let mut visited = vec![false; slots];
        let mut side = Vec::new();
        let mut pending = VecDeque::from([start]);
        while let Some(id) = pending.pop_front() {
            if std::mem::replace(&mut visited[id.get() as usize], true) {
                continue;
            }
            self.block(id)?;
            side.push(id);
            pending.extend(self.successors(id)?);
        }
        // The sides share a block exactly when `other` reaches one of this
        // side's blocks without passing through `avoid`: walk that path
        // backwards from the side. The side is small (a terminating arm);
        // the forward sweep from `other` this replaces covered the rest of
        // the function on every call -- 13.5M block visits in one async
        // compile, against 47k for the side itself.
        if other != avoid {
            let mut reached = vec![false; slots];
            let mut backward = VecDeque::from(side);
            while let Some(id) = backward.pop_front() {
                if id == avoid || std::mem::replace(&mut reached[id.get() as usize], true) {
                    continue;
                }
                if id == other {
                    return Ok(false);
                }
                backward.extend(self.predecessors_by_block.get(&id).into_iter().flatten().copied());
            }
        }
        self.all_paths_reach_or_terminate(start, &[], &mut BTreeSet::new())
    }

    fn is_terminal(
        &self,
        id: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        Ok(matches!(
            &self.block(id)?.terminator.operation,
            ScalarTerminator::Return { .. }
                | ScalarTerminator::TailCall { .. }
                | ScalarTerminator::VarTailCall { .. }
                | ScalarTerminator::Trap
                | ScalarTerminator::Unreachable
        ))
    }

    /// True only for a loop-leaving tail that can be emitted inside the loop:
    /// it cannot return to `header`, and the structural walk accepts only
    /// terminal paths or recognized nested loops with recursively checked
    /// exits; this is not a proof of runtime termination. No block, including
    /// a terminal sink, may be shared with code outside that loop and tail.
    /// Shared sinks remain continuation candidates for single emission.
    fn is_exclusive_terminal_loop_tail(
        &self,
        start: crate::canonical_safewir::BlockId,
        header: crate::canonical_safewir::BlockId,
        members: &[crate::canonical_safewir::BlockId],
    ) -> Result<bool, EncodeError> {
        if self.reaches(start, header)? {
            return Ok(false);
        }
        let reach = self.reachable_until(start, &[header])?;
        if !self.all_paths_reach_or_terminate(start, &[], &mut BTreeSet::new())? {
            return Ok(false);
        }
        for reached in &reach {
            // A terminal sink shared with another continuation must remain a
            // merge, otherwise structured emission can visit its block twice.
            let predecessors = self.predecessors(*reached)?;
            let mut external = predecessors.iter().filter(|predecessor| !reach.contains(predecessor));
            if *reached == start {
                let Some(entry) = external.next() else { return Ok(false) };
                if !members.contains(entry) || external.next().is_some() {
                    return Ok(false);
                }
            } else if external.next().is_some() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Check finite paths to a proposed merge (any of `merges`).  A terminal
    /// path is allowed to bypass a merge, but a cycle that can avoid it is
    /// not.  Recognized loop headers are treated as structured boundaries:
    /// every path through the body must return to the header, leave through
    /// the loop's exit, or terminate, and the exit must still reach the merge.
    fn all_paths_reach_or_terminate(
        &self,
        start: crate::canonical_safewir::BlockId,
        merges: &[crate::canonical_safewir::BlockId],
        active: &mut BTreeSet<crate::canonical_safewir::BlockId>,
    ) -> Result<bool, EncodeError> {
        if merges.contains(&start) || self.is_terminal(start)? {
            return Ok(true);
        }
        let mut stops = merges.to_vec();
        stops.sort();
        let facts = self.path_query_facts(&stops);
        self.all_paths_with_facts(start, &stops, active, &facts)
    }

    fn path_query_facts(
        &self,
        sorted_stops: &[crate::canonical_safewir::BlockId],
    ) -> std::rc::Rc<RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>> {
        // Intern only the old exact sorted key, including duplicates. The
        // query borrows this group's node memo throughout recursion; no
        // ancestor-set judgment or success for another stop set is reused.
        let mut groups = self.path_facts.borrow_mut();
        if let Some(facts) = groups.get(sorted_stops) {
            return std::rc::Rc::clone(facts);
        }
        let facts = std::rc::Rc::new(RefCell::new(BTreeMap::new()));
        groups.insert(sorted_stops.to_vec(), std::rc::Rc::clone(&facts));
        facts
    }

    fn all_paths_with_facts(
        &self,
        start: crate::canonical_safewir::BlockId,
        merges: &[crate::canonical_safewir::BlockId],
        active: &mut BTreeSet<crate::canonical_safewir::BlockId>,
        facts: &RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>,
    ) -> Result<bool, EncodeError> {
        if merges.contains(&start) || self.is_terminal(start)? {
            return Ok(true);
        }
        if let Some(result) = facts.borrow().get(&start) {
            return Ok(*result);
        }
        if !active.insert(start) {
            return Ok(false);
        }
        let result = if self.is_loop_header(start)? {
            let exit = self.loop_exit(start)?;
            let mut body_merges = vec![start];
            body_merges.extend(exit);
            body_merges.sort();
            let body_facts = self.path_query_facts(&body_merges);
            // The body walk keeps the caller's active set: body blocks are
            // dominated by the header, so an active ancestor is reachable
            // only by leaving the loop somewhere other than its exit, which
            // must fail rather than walk the enclosing loop again.
            let mut result = true;
            for successor in self.successors(start)? {
                if !self.all_paths_with_facts(successor, &body_merges, active, &body_facts)? {
                    result = false;
                    break;
                }
            }
            result
                && match exit {
                    Some(exit) => self.all_paths_with_facts(exit, merges, active, facts)?,
                    None => true,
                }
        } else {
            let mut result = true;
            for successor in self.successors(start)? {
                if !self.all_paths_with_facts(successor, merges, active, facts)? {
                    result = false;
                    break;
                }
            }
            result
        };
        active.remove(&start);
        facts.borrow_mut().insert(start, result);
        Ok(result)
    }

    fn is_loop_header(
        &self,
        id: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        if let Some(known) = self.loop_headers.borrow().get(&id) {
            return Ok(*known);
        }
        // A loop header is the target of a back edge: a predecessor it
        // dominates. Plain reachability is not enough: an inner loop's exit
        // reaches the inner header again through the enclosing loop's back
        // edge, which is not a cycle of the inner loop.
        let result = !self.back_edge_sources(id)?.is_empty();
        self.loop_headers.borrow_mut().insert(id, result);
        Ok(result)
    }

    /// The block a structured loop continues at: the one target outside the
    /// header's natural loop that the loop's blocks branch to.  `None` when
    /// the loop only leaves by terminating; several distinct exits are not a
    /// shape the structured emitter accepts.
    fn loop_exit(
        &self,
        header: crate::canonical_safewir::BlockId,
    ) -> Result<Option<crate::canonical_safewir::BlockId>, EncodeError> {
        if let Some(known) = self.loop_exit_targets.borrow().get(&header) {
            return Ok(*known);
        }
        let mut members = vec![header];
        for block in &self.target_function.blocks {
            if block.id != header && self.in_natural_loop(header, block.id)? {
                members.push(block.id);
            }
        }
        // The canonical `while` header names an exit directly: its `else`
        // side leaves the loop. Every other edge out of the loop is an exit
        // too unless all its paths terminate (a `return` inside the body,
        // emitted inline). With one exit the loop continues there; with
        // several (drop blocks on the edges out of a byte scan) it continues
        // at the nearest block all of them reach, and the edge blocks are
        // emitted inside the loop ahead of their `br`.
        let classic_exit = match &self.block(header)?.terminator.operation {
            ScalarTerminator::BranchIf { else_target, .. } if !members.contains(else_target) => Some(*else_target),
            _ => None,
        };
        let mut leaving = BTreeSet::new();
        leaving.extend(classic_exit);
        for member in &members {
            for successor in self.successors(*member)? {
                if !members.contains(&successor) {
                    leaving.insert(successor);
                }
            }
        }
        // An exclusive terminal tail is emitted inline; every other leaving
        // edge is a continuation. Shared terminal sinks are harmless, but a
        // shared cleanup block would duplicate an observable operation.
        let mut candidates = BTreeSet::new();
        for block in &leaving {
            if Some(*block) == classic_exit
                || !self.is_exclusive_terminal_loop_tail(*block, header, &members)?
            {
                candidates.insert(*block);
            }
        }
        let exit = if candidates.len() <= 1 {
            candidates.into_iter().next()
        } else {
            let mut common: Option<(usize, crate::canonical_safewir::BlockId)> = None;
            let distances = candidates
                .iter()
                .map(|candidate| self.distances_from(*candidate))
                .collect::<Result<Vec<_>, _>>()?;
            for block in distances[0].keys() {
                let Some(farthest) = distances
                    .iter()
                    .map(|reach| reach.get(block).copied())
                    .collect::<Option<Vec<_>>>()
                    .and_then(|reaches| reaches.into_iter().max())
                else {
                    continue;
                };
                let score = (farthest, *block);
                if common.is_none_or(|previous| score < previous) {
                    common = Some(score);
                }
            }
            let Some((_, block)) = common else {
                return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                    function: self.source_function.id,
                    operation: "loop exits that never converge",
                }));
            };
            Some(block)
        };
        self.loop_exit_targets.borrow_mut().insert(header, exit);
        Ok(exit)
    }

    /// A branch that leaves structured emission with a plain `br`: back to an
    /// enclosing loop's header (its loop label) or out to that loop's exit
    /// (its block label).
    fn escape_label(&self, target: crate::canonical_safewir::BlockId) -> Option<usize> {
        self.loops
            .iter()
            .rev()
            .find(|(header, _)| *header == target)
            .or_else(|| self.loop_exits.iter().rev().find(|(exit, _)| *exit == target))
            .map(|(_, label_index)| *label_index)
    }

    fn predecessors(&self, block: crate::canonical_safewir::BlockId) -> Result<Vec<crate::canonical_safewir::BlockId>, EncodeError> {
        self.predecessors_by_block.get(&block).cloned().ok_or(EncodeError::MissingBlock {
            function: self.source_function.id,
            block,
        })
    }

    fn reachable_until(&self, start: crate::canonical_safewir::BlockId, stops: &[crate::canonical_safewir::BlockId]) -> Result<BTreeSet<crate::canonical_safewir::BlockId>, EncodeError> {
        let mut seen = BTreeSet::new();
        let mut pending = VecDeque::from([start]);
        while let Some(id) = pending.pop_front() {
            if stops.contains(&id) || !seen.insert(id) {
                continue;
            }
            pending.extend(self.successors(id)?);
        }
        Ok(seen)
    }

    /// A branch side that only leaves: its target is an escape, or every
    /// path from it ends in an escape or a terminal block (the drop block on
    /// a loop's exit edge) and it shares no block with the other side.
    fn side_escapes(&self, current: crate::canonical_safewir::BlockId, side: crate::canonical_safewir::BlockId, other: crate::canonical_safewir::BlockId, stop: Option<crate::canonical_safewir::BlockId>) -> Result<bool, EncodeError> {
        if self.escape_label(side).is_some() {
            return Ok(true);
        }
        let mut stops = self.escape_targets();
        if stops.is_empty() || !self.all_paths_reach_or_terminate(side, &stops, &mut BTreeSet::new())? {
            return Ok(false);
        }
        stops.push(current);
        let reach = self.reachable_until(side, &stops)?;
        // A side that reaches the enclosing merge flows into it rather than
        // escaping: emitting it inline here would emit the merge twice.
        if stop.is_some_and(|stop| reach.contains(&stop)) {
            return Ok(false);
        }
        // Every path must end at an escape: a side that can also return is
        // a terminal side for `only_terminates` to judge, not an escape.
        for block in &reach {
            if self.is_terminal(*block)? {
                return Ok(false);
            }
        }
        let other_reach = self.reachable_until(other, &stops)?;
        Ok(reach.is_disjoint(&other_reach))
    }

    fn escape_targets(&self) -> Vec<crate::canonical_safewir::BlockId> {
        self.loops
            .iter()
            .map(|(header, _)| *header)
            .chain(self.loop_exits.iter().map(|(exit, _)| *exit))
            .collect()
    }

    fn reaches_avoiding(
        &self,
        start: crate::canonical_safewir::BlockId,
        goal: crate::canonical_safewir::BlockId,
        avoid: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        let key = (start, goal, avoid);
        if let Some(known) = self.reaches_avoiding_facts.borrow().get(&key) {
            return Ok(*known);
        }
        let mut seen = BTreeSet::new();
        let mut pending = VecDeque::from([start]);
        let mut result = false;
        while let Some(id) = pending.pop_front() {
            if id == avoid || !seen.insert(id) {
                continue;
            }
            if id == goal {
                result = true;
                break;
            }
            pending.extend(self.successors(id)?);
        }
        self.reaches_avoiding_facts.borrow_mut().insert(key, result);
        Ok(result)
    }

    /// `dominator` lies on every path from the entry block to `node`.
    fn dominates(
        &self,
        dominator: crate::canonical_safewir::BlockId,
        node: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        let entry = self.target_function.entry;
        let idom = self
            .immediate_dominators
            .get_or_init(|| immediate_dominators(entry, &self.successors_by_block));
        Ok(dominates_in(idom, entry, dominator, node))
    }

    /// Predecessors of `header` that it dominates: the sources of its back edges.
    fn back_edge_sources(
        &self,
        header: crate::canonical_safewir::BlockId,
    ) -> Result<Vec<crate::canonical_safewir::BlockId>, EncodeError> {
        if let Some(known) = self.back_edge_source_facts.borrow().get(&header) {
            return Ok(known.clone());
        }
        let mut sources = Vec::new();
        for predecessor in self.predecessors(header)? {
            if predecessor != header && self.dominates(header, predecessor)? {
                sources.push(predecessor);
            }
        }
        self.back_edge_source_facts.borrow_mut().insert(header, sources.clone());
        Ok(sources)
    }

    fn natural_loop_members(
        &self,
        header: crate::canonical_safewir::BlockId,
    ) -> Result<BTreeSet<crate::canonical_safewir::BlockId>, EncodeError> {
        if let Some(known) = self.natural_loop_facts.borrow().get(&header) {
            return Ok(known.clone());
        }
        let members = reverse_loop_members(
            header,
            self.back_edge_sources(header)?,
            &self.predecessors_by_block,
        );
        self.natural_loop_facts.borrow_mut().insert(header, members.clone());
        Ok(members)
    }

    /// `block` belongs to the natural loop of `header`: it reaches one of the
    /// header's back-edge sources without passing through the header.
    fn in_natural_loop(
        &self,
        header: crate::canonical_safewir::BlockId,
        block: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        if block == header {
            return Ok(false);
        }
        if let Some(members) = self.natural_loop_facts.borrow().get(&header) {
            return Ok(members.contains(&block));
        }
        Ok(self.natural_loop_members(header)?.contains(&block))
    }

    fn find_join(
        &self,
        current: crate::canonical_safewir::BlockId,
        then_target: crate::canonical_safewir::BlockId,
        else_target: crate::canonical_safewir::BlockId,
        stop: Option<crate::canonical_safewir::BlockId>,
    ) -> Result<Option<crate::canonical_safewir::BlockId>, EncodeError> {
        if then_target == else_target {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "non-distinct structured branches",
            }));
        }
        // A side that escapes to an enclosing loop's header or exit is a plain
        // `br`: it never reaches a merge, and the other side's first block is
        // where emission continues.
        match (
            self.side_escapes(current, then_target, else_target, stop)?,
            self.side_escapes(current, else_target, then_target, stop)?,
        ) {
            (true, true) => return Ok(None),
            (true, false) => return Ok(Some(else_target)),
            (false, true) => return Ok(Some(then_target)),
            (false, false) => {}
        }
        // Candidates come from paths that stay in the current loop region: a
        // block reached only through an enclosing loop's back edge or exit is
        // the next iteration's, not this branch's merge.
        let stops = self.escape_targets();
        let then_distances = self.distances_until(then_target, &stops)?;
        let else_distances = self.distances_until(else_target, &stops)?;
        // A side whose every path terminates (an exhaustive `match` whose
        // fallthrough traps, `if c: a else: return`) never reaches the merge;
        // `all_paths_reach_or_terminate` already permits that bypass, so such
        // a side contributes distance zero instead of vetoing every candidate.
        // The other side's reach is taken without passing back through the
        // branching block: a loop back edge would otherwise make a returning
        // arm look reachable from the continuing side.
        // Reaching an enclosing continuation is not terminal within this
        // region, even if execution eventually returns beyond that boundary.
        let then_reaches_stop = match stop { Some(stop) => self.reaches(then_target, stop)?, None => false };
        let else_reaches_stop = match stop { Some(stop) => self.reaches(else_target, stop)?, None => false };
        let then_terminal = !then_reaches_stop && self.only_terminates_avoiding(then_target, else_target, current)?;
        let else_terminal = !else_reaches_stop && self.only_terminates_avoiding(else_target, then_target, current)?;
        // Rank every candidate by the score the selection below minimizes
        // before running the path check, so the check runs cheapest-first and
        // stops at the first admissible candidate: that is the minimum, and
        // an executor-sized region no longer walks every reachable block per
        // candidate.
        let mut scored = Vec::new();
        for candidate in then_distances.keys().chain(else_distances.keys()).copied() {
            // A side's own first block can be the merge: when the other side
            // only terminates (`if c: return x` then fall through) or flows
            // into it (a length test guarding a scan loop that exits there),
            // the structured form is `if c { ... } else { } end` followed by
            // that block. An enclosing loop's header or exit is never a
            // merge: emission reaches those only through `br`.
            if candidate == current || self.escape_label(candidate).is_some() {
                continue;
            }
            let then_distance = match then_distances.get(&candidate) {
                Some(distance) => *distance,
                None if then_terminal => 0,
                None => continue,
            };
            let else_distance = match else_distances.get(&candidate) {
                Some(distance) => *distance,
                None if else_terminal => 0,
                None => continue,
            };
            scored.push((then_distance + else_distance, candidate));
        }
        scored.sort_unstable();
        scored.dedup();
        let mut best = None;
        for score @ (_, candidate) in scored {
            // Inside a loop a path may also leave through the loop's exit or
            // back edge (a `br`) instead of reaching the merge.
            let mut merges = self.escape_targets();
            merges.push(candidate);
            if self.all_paths_reach_or_terminate(then_target, &merges, &mut BTreeSet::new())?
                && self.all_paths_reach_or_terminate(else_target, &merges, &mut BTreeSet::new())?
            {
                best = Some(score);
                break;
            }
        }
        if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
            eprintln!("find_join current={current:?} then={then_target:?} else={else_target:?} then_terminal={then_terminal} else_terminal={else_terminal} best={best:?}");
        }
        best.map(|(_, block)| Some(block)).ok_or(EncodeError::Lowering(
            LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "irreducible or non-convergent BranchIf",
            },
        ))
    }

    fn label_depth(&self, index: usize) -> u32 {
        (self.labels.len() - 1 - index) as u32
    }

    fn emit_branch_arguments(
        &mut self,
        target: crate::canonical_safewir::BlockId,
        arguments: &[ValueId],
    ) -> Result<(), EncodeError> {
        self.block(target)?;
        let source_block = self
            .source_function
            .blocks
            .iter()
            .find(|block| block.id == target)
            .ok_or(EncodeError::MissingBlock {
                function: self.source_function.id,
                block: target,
            })?;
        if source_block.parameters.len() != arguments.len() {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "branch arity",
            }));
        }
        // Evaluate every edge operand before assigning any destination.  This
        // is the SSA phi transfer rule and also handles a valid parallel swap.
        for argument in arguments {
            emit_get(self.body, self.layout, self.source_function.id, *argument)?;
        }
        for parameter in source_block.parameters.iter().rev() {
            emit_set(
                self.body,
                self.layout,
                self.source_function.id,
                parameter.value,
            )?;
        }
        Ok(())
    }

    fn emit_path(
        &mut self,
        start: crate::canonical_safewir::BlockId,
        stop: Option<crate::canonical_safewir::BlockId>,
    ) -> Result<(), EncodeError> {
        if let Some(label_index) = self.escape_label(start) {
            self.body
                .instruction(&wasm_encoder::Instruction::Br(self.label_depth(label_index)));
            return Ok(());
        }
        if stop == Some(start) {
            return Ok(());
        }
        if self.is_loop_header(start)? {
            return self.emit_loop(start, stop);
        }
        if !self.emitted.insert(start) {
            if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                eprintln!("block emitted twice: {start:?} stop={stop:?} loops={:?} exits={:?}", self.loops, self.loop_exits);
            }
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "block emitted twice",
            }));
        }
        let block = self.block(start)?.clone();
        for instruction in &block.instructions {
            emit_instruction(
                self.body,
                self.source_function,
                self.layout,
                &instruction.operation,
                self.function_indices,
                self.signature_indices,
                self.import_indices,
                self.imports,
                self.static_data,
                self.layouts,
                self.owner_cleanup_function,
            )?;
        }
        self.emit_block_terminator(start, block.terminator.operation, stop)
    }

    fn emit_block_terminator(
        &mut self,
        start: crate::canonical_safewir::BlockId,
        terminator: ScalarTerminator,
        stop: Option<crate::canonical_safewir::BlockId>,
    ) -> Result<(), EncodeError> {
        match terminator {
            ScalarTerminator::Branch { target, arguments } => {
                self.emit_branch_arguments(target, &arguments)?;
                self.emit_path(target, stop)
            }
            ScalarTerminator::BranchIf {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
            } => self.emit_if(
                start,
                condition,
                then_target,
                &then_arguments,
                else_target,
                &else_arguments,
                stop,
            ),
            terminator => emit_terminator(
                self.body,
                self.source_function,
                self.layout,
                &terminator,
                self.function_indices,
                self.signature_indices,
            ),
        }
    }

    fn emit_if(
        &mut self,
        current: crate::canonical_safewir::BlockId,
        condition: ValueId,
        then_target: crate::canonical_safewir::BlockId,
        then_arguments: &[ValueId],
        else_target: crate::canonical_safewir::BlockId,
        else_arguments: &[ValueId],
        stop: Option<crate::canonical_safewir::BlockId>,
    ) -> Result<(), EncodeError> {
        if self.is_loop_header(current)? && !self.loops.iter().any(|(header, _)| *header == current) {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "loop header outside structured loop",
            }));
        }
        let join = self.find_join(current, then_target, else_target, stop)?;
        emit_get(self.body, self.layout, self.source_function.id, condition)?;
        self.body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        self.labels.push(StructuredLabel::If);
        self.emit_branch_arguments(then_target, then_arguments)?;
        self.emit_path(then_target, join)?;
        self.body.instruction(&wasm_encoder::Instruction::Else);
        self.emit_branch_arguments(else_target, else_arguments)?;
        self.emit_path(else_target, join)?;
        self.body.instruction(&wasm_encoder::Instruction::End);
        self.labels.pop();
        match join {
            Some(join) => self.emit_path(join, stop),
            None => Ok(()),
        }
    }

    fn emit_loop(
        &mut self,
        header: crate::canonical_safewir::BlockId,
        stop: Option<crate::canonical_safewir::BlockId>,
    ) -> Result<(), EncodeError> {
        if !self.emitted.insert(header) {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "loop header emitted twice",
            }));
        }
        if !self.is_loop_header(header)? {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "non-canonical loop shape",
            }));
        }
        let block = self.block(header)?.clone();
        let exit = self.loop_exit(header)?;
        if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
            eprintln!("emit_loop header={header:?} exit={exit:?} stop={stop:?}");
        }

        self.body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
        let block_label = self.labels.len();
        self.labels.push(StructuredLabel::Block);
        self.body.instruction(&wasm_encoder::Instruction::Loop(BlockType::Empty));
        let loop_label = self.labels.len();
        self.labels.push(StructuredLabel::Loop);
        for instruction in &block.instructions {
            emit_instruction(
                self.body,
                self.source_function,
                self.layout,
                &instruction.operation,
                self.function_indices,
                self.signature_indices,
                self.import_indices,
                self.imports,
                self.static_data,
                self.layouts,
                self.owner_cleanup_function,
            )?;
        }
        self.loops.push((header, loop_label));
        if let Some(exit) = exit {
            self.loop_exits.push((exit, block_label));
        }
        match block.terminator.operation {
            // The canonical `while` shape (the header tests the condition and
            // its `else` is the loop's exit) keeps its established form: the
            // body under `if`, the exit as `else { br block }`.
            ScalarTerminator::BranchIf {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
            } if Some(else_target) == exit => {
                emit_get(self.body, self.layout, self.source_function.id, condition)?;
                self.body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
                self.labels.push(StructuredLabel::If);
                self.emit_branch_arguments(then_target, &then_arguments)?;
                self.emit_path(then_target, Some(header))?;
                self.body.instruction(&wasm_encoder::Instruction::Else);
                self.emit_branch_arguments(else_target, &else_arguments)?;
                self.body
                    .instruction(&wasm_encoder::Instruction::Br(self.label_depth(block_label)));
                self.body.instruction(&wasm_encoder::Instruction::End);
                self.labels.pop();
            }
            // Any other header (a short-circuit condition that decides the
            // exit in a later block, a header that only leaves by returning)
            // is emitted structurally inside the loop: branches back to the
            // header and out to the exit become `br`.
            terminator => self.emit_block_terminator(header, terminator, Some(header))?,
        }
        if exit.is_some() {
            self.loop_exits.pop();
        }
        self.loops.pop();
        self.body.instruction(&wasm_encoder::Instruction::End);
        self.labels.pop();
        self.body.instruction(&wasm_encoder::Instruction::End);
        self.labels.pop();
        match exit {
            Some(exit) => self.emit_path(exit, stop),
            // A loop that only leaves by terminating never falls out of its
            // block: the code after it is unreachable, and saying so keeps
            // the function's result stack valid.
            None => {
                self.body.instruction(&wasm_encoder::Instruction::Unreachable);
                Ok(())
            }
        }
    }
}

fn emit_terminator(
    body: &mut wasm_encoder::Function,
    source_function: &Function,
    layout: &LocalLayout,
    terminator: &ScalarTerminator,
    function_indices: &BTreeMap<FunctionId, u32>,
    signature_indices: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
) -> Result<(), EncodeError> {
    match terminator {
        ScalarTerminator::Branch { .. } => return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow { function: source_function.id, operation: "Branch" })),
        ScalarTerminator::BranchIf { .. } => return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow { function: source_function.id, operation: "BranchIf" })),
        ScalarTerminator::Return { values } => {
            for value in values { emit_get(body, layout, source_function.id, *value)?; }
            body.instruction(&wasm_encoder::Instruction::Return);
        }
        ScalarTerminator::TailCall { callee, arguments, .. } => {
            emit_call_arguments(body, layout, source_function.id, arguments)?;
            let index = function_indices.get(callee).copied().ok_or(EncodeError::MissingFunction(*callee))?;
            body.instruction(&wasm_encoder::Instruction::ReturnCall(index));
        }
        ScalarTerminator::VarTailCall { callee, signature, arguments, .. } => {
            emit_call_arguments(body, layout, source_function.id, arguments)?;
            emit_get(body, layout, source_function.id, *callee)?;
            let type_index = signature_indices.get(signature).copied().ok_or(
                EncodeError::UnsupportedSignature { signature: *signature },
            )?;
            body.instruction(&wasm_encoder::Instruction::ReturnCallIndirect {
                type_index,
                table_index: 0,
            });
        }
        ScalarTerminator::Trap | ScalarTerminator::Unreachable => { body.instruction(&wasm_encoder::Instruction::Unreachable); }
    }
    Ok(())
}

fn encode_function(
    source_function: &Function,
    signature: &FunctionSignature,
    target_function: &WasmFunction,
    function_indices: &BTreeMap<FunctionId, u32>,
    signature_indices: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    import_indices: &BTreeMap<ImportId, u32>,
    imports: &BTreeMap<ImportId, ImportContract>,
    static_data: &BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    layouts: &LayoutInterner,
    owner_bridges: &[AdmittedOwnerBridge],
    owner_cleanup_function: Option<u32>,
    memory_helpers: &std::sync::Arc<outlined_memory::Helpers>,
) -> Result<WasmFunctionEncoder, EncodeError> {
    let mut layout = local_layout(source_function, signature, layouts)?;
    layout.owner_bridges = owner_bridges.iter().collect();
    layout.memory_helpers = Some(memory_helpers.clone());
    let local_declarations: Vec<_> = layout.extra_types.iter().copied().map(|ty| (1, ty)).collect();
    let mut body = WasmFunctionEncoder::new(local_declarations);
    if target_function.entry != source_function.entry {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
            function: source_function.id,
            operation: "entry mismatch",
        }));
    }
    let entry = target_function.entry;
    let successors_by_block: BTreeMap<_, _> = target_function
        .blocks
        .iter()
        .map(|block| {
            let successors = match &block.terminator.operation {
                ScalarTerminator::Branch { target, .. } => vec![*target],
                ScalarTerminator::BranchIf { then_target, else_target, .. } => {
                    vec![*then_target, *else_target]
                }
                ScalarTerminator::Return { .. }
                | ScalarTerminator::TailCall { .. }
                | ScalarTerminator::VarTailCall { .. }
                | ScalarTerminator::Trap
                | ScalarTerminator::Unreachable => Vec::new(),
            };
            (block.id, successors)
        })
        .collect();
    let mut predecessors_by_block: BTreeMap<_, Vec<_>> = target_function
        .blocks
        .iter()
        .map(|block| (block.id, Vec::new()))
        .collect();
    for (source, successors) in &successors_by_block {
        for successor in successors {
            let predecessors = predecessors_by_block
                .get_mut(successor)
                .ok_or(EncodeError::MissingBlock {
                    function: source_function.id,
                    block: *successor,
                })?;
            if !predecessors.contains(source) {
                predecessors.push(*source);
            }
        }
    }
    let mut emitter = StructuredEmitter {
        body: &mut body,
        source_function,
        target_function,
        blocks_by_id: target_function.blocks.iter().map(|block| (block.id, block)).collect(),
        layout: &layout,
        function_indices,
        signature_indices,
        import_indices,
        imports,
        static_data,
        layouts,
        owner_cleanup_function,
        successors_by_block,
        predecessors_by_block,
        emitted: BTreeSet::new(),
        labels: Vec::new(),
        loops: Vec::new(),
        loop_exits: Vec::new(),
        loop_headers: RefCell::new(BTreeMap::new()),
        loop_exit_targets: RefCell::new(BTreeMap::new()),
        back_edge_source_facts: RefCell::new(BTreeMap::new()),
        natural_loop_facts: RefCell::new(BTreeMap::new()),
        reaches_avoiding_facts: RefCell::new(BTreeMap::new()),
        immediate_dominators: std::cell::OnceCell::new(),
        block_slots: std::cell::OnceCell::new(),
        path_facts: RefCell::new(BTreeMap::new()),
    };
    emitter.emit_path(entry, None)?;
    if emitter.emitted.len() != target_function.blocks.len() {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
            function: source_function.id,
            operation: "unstructured or unreachable block",
        }));
    }
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(body)
}

fn authorize_wasm_ir(
    validated: &ValidatedSafeWir,
    source_witness: &witchy_types::pipeline::CanonicalCheckedSource,
    entry: Option<&CanonicalEntry>,
) -> Result<CheckedWasmIr, EncodeError> {
    authorize_wasm_ir_inner(validated, Some(source_witness), entry.map(CanonicalEntry::encode))
}

#[cfg(test)]
fn authorize_wasm_ir_for_test(validated: ValidatedSafeWir) -> Result<CheckedWasmIr, EncodeError> {
    authorize_wasm_ir_inner(&validated, None, None)
}

fn authorize_wasm_ir_inner(
    validated: &ValidatedSafeWir,
    source_witness: Option<&witchy_types::pipeline::CanonicalCheckedSource>,
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

#[cfg(test)]
fn encode_validated(validated: ValidatedSafeWir) -> Result<Vec<u8>, EncodeError> {
    let checked = authorize_wasm_ir_for_test(validated)?;
    encode_checked(&checked)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PlanToken {
    Drop,
    RefCastNonNull(u32),
    StructGet { struct_type_index: u32, field_index: u32 },
    ArrayLen,
    ArrayGet(u32),
    LocalGet(u32),
    LocalSet(u32),
    RefNull(String),
    RefIsNull,
    RefAsNonNull,
    LocalTee(u32),
    I32Const(i32),
    I64Const(i64),
    I64ExtendI32U,
    F64Sqrt,
    I64TruncSatF64S,
    F64ConvertI64S,
    I64Mul,
    I64Add,
    I64Sub,
    MemorySize,
    MemoryGrow,
    MemoryFill(u32),
    I32WrapI64,
    I64Eq,
    I64LtS,
    I64GtS,
    GlobalGet(u32),
    GlobalSet(u32),
    I32Add,
    I32Mul,
    I32Sub,
    I32LtU,
    I32LeU,
    I32GtU,
    I32Eqz,
    I32Eq,
    I32Load { offset: u64, align: u8, memory: u32 },
    I32Load8U { offset: u64, align: u8, memory: u32 },
    I32Load16U { offset: u64, align: u8, memory: u32 },
    I64Load { offset: u64, align: u8, memory: u32 },
    F64Load { offset: u64, align: u8, memory: u32 },
    I32Store { offset: u64, align: u8, memory: u32 },
    I32Store8 { offset: u64, align: u8, memory: u32 },
    I32Store16 { offset: u64, align: u8, memory: u32 },
    I64Store { offset: u64, align: u8, memory: u32 },
    F64Store { offset: u64, align: u8, memory: u32 },
    MemoryCopy { source: u32, destination: u32 },
    I64GtU,
    I64Ne,
    I64LeS,
    I64GeS,
    I64LtU,
    I64LeU,
    I64GeU,
    I32Ne,
    I32LtS,
    I32LeS,
    I32GtS,
    I32GeS,
    I32GeU,
    F64Eq,
    F64Ne,
    F64Lt,
    F64Le,
    F64Gt,
    F64Ge,
    I64DivS,
    I64DivU,
    I64RemS,
    I64RemU,
    I64And,
    I64Or,
    I64Xor,
    I64Shl,
    I64ShrS,
    I64ShrU,
    I32DivS,
    I32DivU,
    I32RemS,
    I32RemU,
    I32And,
    I32Or,
    I32Xor,
    I32Shl,
    I32ShrS,
    I32ShrU,
    F64Add,
    F64Sub,
    F64Mul,
    F64Div,
    F64Const(u64),
    Block(String),
    Loop(String),
    If(String),
    Else,
    Br(u32),
    BrIf(u32),
    Unreachable,
    Return,
    Call(u32),
    CallIndirect { type_index: u32, table_index: u32 },
    ReturnCall(u32),
    ReturnCallIndirect { type_index: u32, table_index: u32 },
    End,
}

fn decode_plan_token(operator: wasmparser::Operator<'_>) -> Result<PlanToken, CanonicalAdmissionError> {
    Ok(match operator {
        wasmparser::Operator::Drop => PlanToken::Drop,
        wasmparser::Operator::LocalGet { local_index } => PlanToken::LocalGet(local_index),
        wasmparser::Operator::LocalSet { local_index } => PlanToken::LocalSet(local_index),
        wasmparser::Operator::RefNull { hty } => PlanToken::RefNull(format!("{hty:?}")),
wasmparser::Operator::RefCastNonNull { hty: wasmparser::HeapType::Concrete(index) } => PlanToken::RefCastNonNull(index.as_module_index().ok_or(CanonicalAdmissionError::UnsupportedPlan)?),
wasmparser::Operator::StructGet { struct_type_index, field_index } => PlanToken::StructGet { struct_type_index, field_index },
wasmparser::Operator::ArrayLen => PlanToken::ArrayLen,
wasmparser::Operator::ArrayGet { array_type_index } => PlanToken::ArrayGet(array_type_index),
wasmparser::Operator::RefIsNull => PlanToken::RefIsNull,

        wasmparser::Operator::RefAsNonNull => PlanToken::RefAsNonNull,
        wasmparser::Operator::I32Const { value } => PlanToken::I32Const(value),
        wasmparser::Operator::I64Const { value } => PlanToken::I64Const(value),
        wasmparser::Operator::I64ExtendI32U => PlanToken::I64ExtendI32U,
        wasmparser::Operator::F64Sqrt => PlanToken::F64Sqrt,
        wasmparser::Operator::I64TruncSatF64S => PlanToken::I64TruncSatF64S,
        wasmparser::Operator::F64ConvertI64S => PlanToken::F64ConvertI64S,
        wasmparser::Operator::I64Mul => PlanToken::I64Mul,
        wasmparser::Operator::I64Add => PlanToken::I64Add,
        wasmparser::Operator::I64Sub => PlanToken::I64Sub,
        wasmparser::Operator::MemorySize { mem: 0 } => PlanToken::MemorySize,
        wasmparser::Operator::MemoryGrow { mem: 0 } => PlanToken::MemoryGrow,
        wasmparser::Operator::MemoryFill { mem } => PlanToken::MemoryFill(mem),
        wasmparser::Operator::LocalTee { local_index } => PlanToken::LocalTee(local_index),
        wasmparser::Operator::I32WrapI64 => PlanToken::I32WrapI64,
        wasmparser::Operator::I64Eq => PlanToken::I64Eq,
        wasmparser::Operator::I64LtS => PlanToken::I64LtS,
        wasmparser::Operator::I64GtS => PlanToken::I64GtS,
        wasmparser::Operator::GlobalGet { global_index } => PlanToken::GlobalGet(global_index),
        wasmparser::Operator::GlobalSet { global_index } => PlanToken::GlobalSet(global_index),
        wasmparser::Operator::I32Add => PlanToken::I32Add,
        wasmparser::Operator::I32Mul => PlanToken::I32Mul,
        wasmparser::Operator::I32Sub => PlanToken::I32Sub,
        wasmparser::Operator::I32LtU => PlanToken::I32LtU,
        wasmparser::Operator::I32LeU => PlanToken::I32LeU,
        wasmparser::Operator::I32GtU => PlanToken::I32GtU,
        wasmparser::Operator::I32Eqz => PlanToken::I32Eqz,
        wasmparser::Operator::I32Eq => PlanToken::I32Eq,
        wasmparser::Operator::I32Load { memarg } => PlanToken::I32Load { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::I32Load8U { memarg } => PlanToken::I32Load8U { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::I32Load16U { memarg } => PlanToken::I32Load16U { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::I64Load { memarg } => PlanToken::I64Load { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::F64Load { memarg } => PlanToken::F64Load { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::I32Store { memarg } => PlanToken::I32Store {
            offset: memarg.offset,
            align: memarg.align,
            memory: memarg.memory,
        },
        wasmparser::Operator::I32Store8 { memarg } => PlanToken::I32Store8 { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::I32Store16 { memarg } => PlanToken::I32Store16 { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::I64Store { memarg } => PlanToken::I64Store { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::F64Store { memarg } => PlanToken::F64Store { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::MemoryCopy { src_mem, dst_mem } => PlanToken::MemoryCopy {
            source: src_mem,
            destination: dst_mem,
        },
        wasmparser::Operator::I64GtU => PlanToken::I64GtU,
        wasmparser::Operator::I64Ne => PlanToken::I64Ne,
        wasmparser::Operator::I64LeS => PlanToken::I64LeS,
        wasmparser::Operator::I64GeS => PlanToken::I64GeS,
        wasmparser::Operator::I64LtU => PlanToken::I64LtU,
        wasmparser::Operator::I64LeU => PlanToken::I64LeU,
        wasmparser::Operator::I64GeU => PlanToken::I64GeU,
        wasmparser::Operator::I32Ne => PlanToken::I32Ne,
        wasmparser::Operator::I32LtS => PlanToken::I32LtS,
        wasmparser::Operator::I32LeS => PlanToken::I32LeS,
        wasmparser::Operator::I32GtS => PlanToken::I32GtS,
        wasmparser::Operator::I32GeS => PlanToken::I32GeS,
        wasmparser::Operator::I32GeU => PlanToken::I32GeU,
        wasmparser::Operator::F64Eq => PlanToken::F64Eq,
        wasmparser::Operator::F64Ne => PlanToken::F64Ne,
        wasmparser::Operator::F64Lt => PlanToken::F64Lt,
        wasmparser::Operator::F64Le => PlanToken::F64Le,
        wasmparser::Operator::F64Gt => PlanToken::F64Gt,
        wasmparser::Operator::F64Ge => PlanToken::F64Ge,
        wasmparser::Operator::I64DivS => PlanToken::I64DivS,
        wasmparser::Operator::I64DivU => PlanToken::I64DivU,
        wasmparser::Operator::I64RemS => PlanToken::I64RemS,
        wasmparser::Operator::I64RemU => PlanToken::I64RemU,
        wasmparser::Operator::I64And => PlanToken::I64And,
        wasmparser::Operator::I64Or => PlanToken::I64Or,
        wasmparser::Operator::I64Xor => PlanToken::I64Xor,
        wasmparser::Operator::I64Shl => PlanToken::I64Shl,
        wasmparser::Operator::I64ShrS => PlanToken::I64ShrS,
        wasmparser::Operator::I64ShrU => PlanToken::I64ShrU,
        wasmparser::Operator::I32DivS => PlanToken::I32DivS,
        wasmparser::Operator::I32DivU => PlanToken::I32DivU,
        wasmparser::Operator::I32RemS => PlanToken::I32RemS,
        wasmparser::Operator::I32RemU => PlanToken::I32RemU,
        wasmparser::Operator::I32And => PlanToken::I32And,
        wasmparser::Operator::I32Or => PlanToken::I32Or,
        wasmparser::Operator::I32Xor => PlanToken::I32Xor,
        wasmparser::Operator::I32Shl => PlanToken::I32Shl,
        wasmparser::Operator::I32ShrS => PlanToken::I32ShrS,
        wasmparser::Operator::I32ShrU => PlanToken::I32ShrU,
        wasmparser::Operator::F64Add => PlanToken::F64Add,
        wasmparser::Operator::F64Sub => PlanToken::F64Sub,
        wasmparser::Operator::F64Mul => PlanToken::F64Mul,
        wasmparser::Operator::F64Div => PlanToken::F64Div,
        wasmparser::Operator::F64Const { value } => PlanToken::F64Const(value.bits()),
        wasmparser::Operator::Block { blockty } => PlanToken::Block(format!("{blockty:?}")),
        wasmparser::Operator::Loop { blockty } => PlanToken::Loop(format!("{blockty:?}")),
        wasmparser::Operator::If { blockty } => PlanToken::If(format!("{blockty:?}")),
        wasmparser::Operator::Else => PlanToken::Else,
        wasmparser::Operator::Br { relative_depth } => PlanToken::Br(relative_depth),
        wasmparser::Operator::BrIf { relative_depth } => PlanToken::BrIf(relative_depth),
        wasmparser::Operator::Unreachable => PlanToken::Unreachable,
        wasmparser::Operator::Return => PlanToken::Return,
        wasmparser::Operator::Call { function_index } => PlanToken::Call(function_index),
        wasmparser::Operator::CallIndirect { type_index, table_index } => PlanToken::CallIndirect { type_index, table_index },
        wasmparser::Operator::ReturnCall { function_index } => PlanToken::ReturnCall(function_index),
        wasmparser::Operator::ReturnCallIndirect { type_index, table_index } => {
            PlanToken::ReturnCallIndirect { type_index, table_index }
        }
        wasmparser::Operator::End => PlanToken::End,
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    })
}

fn plan_local(layout: &LocalLayout, value: ValueId) -> Result<u32, CanonicalAdmissionError> {
    layout.indices.get(&value).copied().ok_or(CanonicalAdmissionError::UnsupportedPlan)
}

fn push_get(plan: &mut Vec<PlanToken>, layout: &LocalLayout, value: ValueId) -> Result<(), CanonicalAdmissionError> {
    plan.push(PlanToken::LocalGet(plan_local(layout, value)?));
    Ok(())
}

fn push_set(plan: &mut Vec<PlanToken>, layout: &LocalLayout, value: ValueId) -> Result<(), CanonicalAdmissionError> {
    plan.push(PlanToken::LocalSet(plan_local(layout, value)?));
    Ok(())
}

fn plan_call_argument(argument: &CallArgument) -> Result<ValueId, CanonicalAdmissionError> {
    match argument {
        CallArgument::Value(value) | CallArgument::Move(value) => Ok(*value),
    }
}

fn push_store(plan: &mut Vec<PlanToken>) {
    plan.push(PlanToken::I32Store { offset: 0, align: 2, memory: 0 });
}

fn push_u32_trap_if_gt_plan(
    plan: &mut Vec<PlanToken>,
    layout: &LocalLayout,
    value: ValueId,
    limit: u32,
) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, layout, value)?;
    plan.extend([
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(limit)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    Ok(())
}

fn push_projected_address_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    access: MemoryAccess,
) -> Result<(), CanonicalAdmissionError> {
    if let Some(index) = access.index {
        let maximum = (u32::MAX
            .checked_sub(access.offset)
            .and_then(|value| value.checked_sub(access.width))
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?
            ) / access.collection.stride;
        push_u32_trap_if_gt_plan(plan, layout, index, maximum)?;
        push_get(plan, layout, access.object)?;
        plan.push(PlanToken::I64ExtendI32U);
        push_get(plan, layout, index)?;
        plan.extend([
            PlanToken::I64ExtendI32U,
            PlanToken::I64Const(i64::from(access.collection.stride)),
            PlanToken::I64Mul,
            PlanToken::I64Const(i64::from(access.offset)),
            PlanToken::I64Add,
            PlanToken::I64Add,
        ]);
        push_get(plan, layout, access.object)?;
        plan.push(PlanToken::I64ExtendI32U);
        push_get(plan, layout, index)?;
        plan.extend([
            PlanToken::I64ExtendI32U,
            PlanToken::I64Const(i64::from(access.collection.stride)),
            PlanToken::I64Mul,
            PlanToken::I64Const(i64::from(access.offset)),
            PlanToken::I64Add,
            PlanToken::I64Add,
            PlanToken::I64Const(i64::from(u32::MAX - access.width)),
            PlanToken::I64GtU,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Unreachable,
            PlanToken::End,
            PlanToken::I32WrapI64,
        ]);
        return Ok(());
    }
    let maximum = u32::MAX.checked_sub(access.width).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
    if let Some(helpers) = &layout.memory_helpers {
        push_get(plan, layout, access.object)?;
        plan.extend([PlanToken::I32Const(access.offset as i32), PlanToken::I32Const(maximum as i32), PlanToken::Call(helpers.address)]);
        return Ok(());
    }
    push_get(plan, layout, access.object)?;
    plan.extend([
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(access.offset)),
        PlanToken::I64Add,
        PlanToken::I64Const(i64::from(maximum)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    push_get(plan, layout, access.object)?;
    plan.extend([PlanToken::I32Const(access.offset as i32), PlanToken::I32Add]);
    let _ = function;
    Ok(())
}

fn push_memory_load_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layouts: &LayoutInterner,
    layout: &LocalLayout,
    place: &Place,
    destination: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let access = resolve_memory_place_for_values(function.id, &function.value_types, layouts, place)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    push_projected_address_plan(plan, function, layout, access)?;
    let memarg = |offset, align, memory| PlanToken::I32Load { offset, align, memory };
    plan.push(match access.scalar {
        ScalarKind::Bool | ScalarKind::Tag8 | ScalarKind::RawByte => PlanToken::I32Load8U { offset: 0, align: 0, memory: 0 },
        ScalarKind::Tag16 => PlanToken::I32Load16U { offset: 0, align: 1, memory: 0 },
        ScalarKind::Unit | ScalarKind::U32 | ScalarKind::Tag32 | ScalarKind::Callable => memarg(0, 2, 0),
        ScalarKind::Int | ScalarKind::Duration => PlanToken::I64Load { offset: 0, align: 3, memory: 0 },
        ScalarKind::Float => PlanToken::F64Load { offset: 0, align: 3, memory: 0 },
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    });
    if access.scalar == ScalarKind::Unit {
        plan.extend([PlanToken::Drop, PlanToken::I32Const(0)]);
    }
    push_set(plan, layout, destination)
}

fn push_init_nullable_owned_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layouts: &LayoutInterner,
    layout: &LocalLayout,
    place: &Place,
) -> Result<(), CanonicalAdmissionError> {
    let access = resolve_memory_place_for_values(function.id, &function.value_types, layouts, place)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    if access.pointer_slot != Some(PointerSlot::OwnedNullable) || place.access() != AccessMode::Write {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    push_projected_address_plan(plan, function, layout, access)?;
    plan.extend([PlanToken::I32Const(0), PlanToken::I32Store { offset: 0, align: 2, memory: 0 }]);
    Ok(())
}

fn push_require_present_plan(
    plan: &mut Vec<PlanToken>,
    layout: &LocalLayout,
    value: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, layout, value)?;
    plan.extend([
        PlanToken::I32Eqz,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    Ok(())
}

fn push_memory_store_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layouts: &LayoutInterner,
    layout: &LocalLayout,
    place: &Place,
    value: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let access = resolve_memory_place_for_values(function.id, &function.value_types, layouts, place)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    push_projected_address_plan(plan, function, layout, access)?;
    if access.scalar == ScalarKind::Unit {
        plan.push(PlanToken::I32Const(0));
    } else {
        push_get(plan, layout, value)?;
    }
    plan.push(match access.scalar {
        ScalarKind::Bool | ScalarKind::Tag8 | ScalarKind::RawByte => PlanToken::I32Store8 { offset: 0, align: 0, memory: 0 },
        ScalarKind::Tag16 => PlanToken::I32Store16 { offset: 0, align: 1, memory: 0 },
        ScalarKind::Unit | ScalarKind::U32 | ScalarKind::Tag32 | ScalarKind::Callable => PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
        ScalarKind::Int | ScalarKind::Duration => PlanToken::I64Store { offset: 0, align: 3, memory: 0 },
        ScalarKind::Float => PlanToken::F64Store { offset: 0, align: 3, memory: 0 },
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    });
    Ok(())
}

fn push_share_plan(
    plan: &mut Vec<PlanToken>,
    layout: &LocalLayout,
    destination: ValueId,
    object: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, layout, object)?;
    if let Some(helpers) = &layout.memory_helpers {
        plan.push(PlanToken::Call(helpers.retain));
        return push_set(plan, layout, destination);
    }
    plan.extend([
        PlanToken::LocalSet(layout.scratch[1]),
        PlanToken::LocalGet(layout.scratch[1]),
        PlanToken::GlobalGet(2),
        PlanToken::I32LtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Else,
        PlanToken::LocalGet(layout.scratch[1]),
        PlanToken::I32Const(8),
        PlanToken::I32Sub,
        PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
        PlanToken::LocalSet(layout.scratch[2]),
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::I32Eqz,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::I32Const(-1),
        PlanToken::I32Eq,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
        PlanToken::LocalGet(layout.scratch[1]),
        PlanToken::I32Const(8),
        PlanToken::I32Sub,
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::I32Const(1),
        PlanToken::I32Add,
        PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
        PlanToken::End,
        PlanToken::LocalGet(layout.scratch[1]),
    ]);
    push_set(plan, layout, destination)
}

fn push_update_count_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layouts: &LayoutInterner,
    layout: &LocalLayout,
    object: ValueId,
    count: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let collection = resolve_collection_layout_for_values(function.id, &function.value_types, layouts, object)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    if let Some(capacity_offset) = collection.capacity_offset {
        push_get(plan, layout, count)?;
        push_get(plan, layout, object)?;
        plan.extend([
            PlanToken::I32Const(capacity_offset as i32),
            PlanToken::I32Add,
            PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
            PlanToken::I32GtU,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Unreachable,
            PlanToken::End,
        ]);
    }
    push_get(plan, layout, object)?;
    plan.extend([PlanToken::I32Const(collection.length_offset as i32), PlanToken::I32Add]);
    push_get(plan, layout, count)?;
    push_store(plan);
    Ok(())
}

fn push_host_slice_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    argument: &HostCallArgument,
) -> Result<(), CanonicalAdmissionError> {
    let (place, access, with_length) = match argument {
        // A checked slice passes (ptr, len) in either direction; the place's
        // own access mode decides whether the range is bounded by the
        // object's count or by its capacity.
        HostCallArgument::Buffer { place, extent: HostBufferExtent::Slice } => (place, place.access(), true),
        // A write buffer of a declared minimum size passes its address only.
        HostCallArgument::Buffer { place, extent: HostBufferExtent::AtLeast(_) } => (place, AccessMode::Write, false),
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    };
    let Projection::Slice { length, .. } = place.projection() else {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    };
    push_raw_byte_slice_address_plan(plan, function, layout, layouts, place, access, *length)?;
    if with_length {
        push_get(plan, layout, *length)?;
    }
    Ok(())
}

fn push_host_call_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    imports: &BTreeMap<ImportId, ImportContract>,
    import_indices: &BTreeMap<ImportId, u32>,
    layouts: &LayoutInterner,
    destinations: &[ValueId],
    import: ImportId,
    arguments: &[HostCallArgument],
) -> Result<(), CanonicalAdmissionError> {
    let contract = imports.get(&import).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
    validate_host_call_lowering(function.id, &function.value_types, layouts, contract, arguments, destinations)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    for (role, argument) in contract.parameter_roles.iter().zip(arguments) {
        match (role, argument) {
            (HostParameterRole::Value(_), HostCallArgument::Value(value))
            | (HostParameterRole::Capability(_), HostCallArgument::Capability(value)) => push_get(plan, layout, *value)?,
            (HostParameterRole::Buffer { .. }, HostCallArgument::Buffer { .. }) => push_host_slice_plan(plan, function, layout, layouts, argument)?,
            _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
        }
    }
    plan.push(PlanToken::Call(*import_indices.get(&import).ok_or(CanonicalAdmissionError::UnsupportedPlan)?));
    for destination in destinations.iter().rev() {
        push_set(plan, layout, *destination)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn push_raw_byte_slice_address_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    place: &Place,
    access: AccessMode,
    length: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let (object, start, length) = raw_byte_slice_place(
        function.id,
        &function.value_types,
        layouts,
        place,
        access,
        length,
    )
    .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    push_get(plan, layout, object)?;
    plan.extend([PlanToken::I64ExtendI32U, PlanToken::I64Const(4), PlanToken::I64Add]);
    push_get(plan, layout, start)?;
    plan.push(PlanToken::I64ExtendI32U);
    plan.push(PlanToken::I64Add);
    push_get(plan, layout, length)?;
    plan.extend([
        PlanToken::I64ExtendI32U,
        PlanToken::I64Add,
        PlanToken::I64Const(i64::from(u32::MAX)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    push_get(plan, layout, object)?;
    plan.extend([PlanToken::I32Const(4), PlanToken::I32Add]);
    push_get(plan, layout, start)?;
    plan.push(PlanToken::I32Add);
    Ok(())
}

fn push_copy_bytes_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    destination: &Place,
    source: &Place,
    length: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    push_raw_byte_slice_address_plan(
        plan,
        function,
        layout,
        layouts,
        destination,
        AccessMode::Write,
        length,
    )?;
    push_raw_byte_slice_address_plan(
        plan,
        function,
        layout,
        layouts,
        source,
        AccessMode::Read,
        length,
    )?;
    push_get(plan, layout, length)?;
    plan.push(PlanToken::MemoryCopy { source: 0, destination: 0 });
    Ok(())
}

fn push_move_range_base_plan(
    plan: &mut Vec<PlanToken>,
    layout: &LocalLayout,
    object: ValueId,
    start: ValueId,
    collection: CollectionLayout,
    scratch: usize,
) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, layout, object)?;
    plan.push(PlanToken::I64ExtendI32U);
    push_get(plan, layout, start)?;
    plan.extend([
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(collection.stride)),
        PlanToken::I64Mul,
        PlanToken::I64Const(i64::from(collection.data_offset)),
        PlanToken::I64Add,
        PlanToken::I64Add,
        PlanToken::I64Const(i64::from(u32::MAX)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    push_get(plan, layout, object)?;
    plan.push(PlanToken::I64ExtendI32U);
    push_get(plan, layout, start)?;
    plan.extend([
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(collection.stride)),
        PlanToken::I64Mul,
        PlanToken::I64Const(i64::from(collection.data_offset)),
        PlanToken::I64Add,
        PlanToken::I64Add,
        PlanToken::I32WrapI64,
        PlanToken::LocalSet(layout.scratch[scratch]),
    ]);
    Ok(())
}

fn push_move_range_element_address_plan(
    plan: &mut Vec<PlanToken>,
    layout: &LocalLayout,
    base_scratch: usize,
    index_scratch: usize,
    stride: u32,
) {
    plan.extend([
        PlanToken::LocalGet(layout.scratch[base_scratch]),
        PlanToken::I64ExtendI32U,
        PlanToken::LocalGet(layout.scratch[index_scratch]),
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(stride)),
        PlanToken::I64Mul,
        PlanToken::I64Add,
        PlanToken::I64Const(i64::from(u32::MAX)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    plan.extend([
        PlanToken::LocalGet(layout.scratch[base_scratch]),
        PlanToken::I64ExtendI32U,
        PlanToken::LocalGet(layout.scratch[index_scratch]),
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(stride)),
        PlanToken::I64Mul,
        PlanToken::I64Add,
        PlanToken::I32WrapI64,
    ]);
}

fn push_move_range_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    destination: &Place,
    source: &Place,
    count: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let destination_layout = resolve_collection_layout(function, layouts, destination.object())
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let source_layout = resolve_collection_layout(function, layouts, source.object())
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    if destination_layout != source_layout {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    if destination_layout.pointer_layout.is_none() {
        return push_clone_range_plan(
            plan,
            function,
            layout,
            layouts,
            destination,
            source,
            count,
        );
    }
    let (destination_start, source_start) = match (destination.projection(), source.projection()) {
        (Projection::Slice { start: destination_start, length: destination_length }, Projection::Slice { start: source_start, length: source_length })
            if *destination_length == count && *source_length == count => (*destination_start, *source_start),
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    };
    push_get(plan, layout, count)?;
    plan.push(PlanToken::LocalSet(layout.scratch[3]));
    push_move_range_base_plan(plan, layout, source.object(), source_start, source_layout, 1)?;
    push_move_range_base_plan(plan, layout, destination.object(), destination_start, destination_layout, 2)?;
    plan.extend([
        PlanToken::I32Const(0),
        PlanToken::LocalSet(layout.scratch[0]),
        PlanToken::Block("Empty".to_owned()),
        PlanToken::Loop("Empty".to_owned()),
        PlanToken::LocalGet(layout.scratch[0]),
        PlanToken::LocalGet(layout.scratch[3]),
        PlanToken::I32LtU,
        PlanToken::I32Eqz,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Br(2),
        PlanToken::End,
    ]);
    push_move_range_element_address_plan(plan, layout, 2, 0, destination_layout.stride);
    push_move_range_element_address_plan(plan, layout, 1, 0, source_layout.stride);
    plan.push(PlanToken::I32Load { offset: 0, align: 2, memory: 0 });
    plan.push(PlanToken::I32Store { offset: 0, align: 2, memory: 0 });
    push_move_range_element_address_plan(plan, layout, 1, 0, source_layout.stride);
    plan.extend([
        PlanToken::I32Const(0),
        PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
        PlanToken::LocalGet(layout.scratch[0]),
        PlanToken::I32Const(1),
        PlanToken::I32Add,
        PlanToken::LocalSet(layout.scratch[0]),
        PlanToken::Br(0),
        PlanToken::End,
        PlanToken::End,
    ]);
    Ok(())
}

fn push_clone_range_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    destination: &Place,
    source: &Place,
    count: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let destination_layout = resolve_collection_layout(function, layouts, destination.object())
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let source_layout = resolve_collection_layout(function, layouts, source.object())
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    if destination_layout != source_layout {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    let (destination_start, source_start) = match (destination.projection(), source.projection()) {
        (Projection::Slice { start: destination_start, length: destination_length }, Projection::Slice { start: source_start, length: source_length })
            if *destination_length == count && *source_length == count => (*destination_start, *source_start),
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    };
    push_move_range_base_plan(plan, layout, source.object(), source_start, source_layout, 1)?;
    push_move_range_base_plan(plan, layout, destination.object(), destination_start, destination_layout, 2)?;
    push_get(plan, layout, count)?;
    plan.extend([
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(destination_layout.stride)),
        PlanToken::I64Mul,
        PlanToken::I64Const(i64::from(u32::MAX)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    if destination_layout.pointer_layout.is_some() {
        if layout.scratch.len() < 5 {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        let load = PlanToken::I32Load { offset: 0, align: 2, memory: 0 };
        plan.extend([
            PlanToken::I32Const(0),
            PlanToken::LocalSet(layout.scratch[0]),
            PlanToken::Block("Empty".to_owned()),
            PlanToken::Loop("Empty".to_owned()),
            PlanToken::LocalGet(layout.scratch[0]),
        ]);
        push_get(plan, layout, count)?;
        plan.extend([
            PlanToken::I32LtU,
            PlanToken::I32Eqz,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Br(2),
            PlanToken::End,
            PlanToken::LocalGet(layout.scratch[1]),
            PlanToken::LocalGet(layout.scratch[0]),
            PlanToken::I32Const(destination_layout.stride as i32),
            PlanToken::I32Mul,
            PlanToken::I32Add,
            load.clone(),
            PlanToken::LocalSet(layout.scratch[3]),
            PlanToken::LocalGet(layout.scratch[3]),
            PlanToken::GlobalGet(2),
            PlanToken::I32LtU,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Else,
            PlanToken::LocalGet(layout.scratch[3]),
            PlanToken::I32Const(8),
            PlanToken::I32Sub,
            load,
            PlanToken::LocalSet(layout.scratch[4]),
            PlanToken::LocalGet(layout.scratch[4]),
            PlanToken::I32Eqz,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Unreachable,
            PlanToken::End,
            PlanToken::LocalGet(layout.scratch[4]),
            PlanToken::I32Const(-1),
            PlanToken::I32Eq,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Unreachable,
            PlanToken::End,
            PlanToken::LocalGet(layout.scratch[3]),
            PlanToken::I32Const(8),
            PlanToken::I32Sub,
            PlanToken::LocalGet(layout.scratch[4]),
            PlanToken::I32Const(1),
            PlanToken::I32Add,
            PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
            PlanToken::End,
            PlanToken::LocalGet(layout.scratch[0]),
            PlanToken::I32Const(1),
            PlanToken::I32Add,
            PlanToken::LocalSet(layout.scratch[0]),
            PlanToken::Br(0),
            PlanToken::End,
            PlanToken::End,
        ]);
    }
    plan.extend([
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::LocalGet(layout.scratch[1]),
    ]);
    push_get(plan, layout, count)?;
    plan.extend([
        PlanToken::I32Const(destination_layout.stride as i32),
        PlanToken::I32Mul,
        PlanToken::MemoryCopy { source: 0, destination: 0 },
    ]);
    Ok(())
}

fn push_reclaim_plan(plan: &mut Vec<PlanToken>, layout: &LocalLayout, object: ValueId) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, layout, object)?;
    plan.push(PlanToken::LocalSet(layout.scratch[1]));
    plan.extend([
        PlanToken::LocalGet(layout.scratch[1]),
        PlanToken::GlobalGet(1),
        PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
        PlanToken::LocalGet(layout.scratch[1]),
        PlanToken::GlobalSet(1),
    ]);
    Ok(())
}

fn push_reclaim_local_plan(plan: &mut Vec<PlanToken>, layout: &LocalLayout, slot: usize) {
    plan.extend([
        PlanToken::LocalGet(layout.scratch[slot]),
        PlanToken::GlobalGet(1),
        PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
        PlanToken::LocalGet(layout.scratch[slot]),
        PlanToken::GlobalSet(1),
    ]);
}

fn push_drop_ref_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layouts: &LayoutInterner,
    layout: &LocalLayout,
    object: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let Some(ValueType::Ref(object_layout)) = function.value_types.get(&object).copied() else {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    };
    push_get(plan, layout, object)?;
    if let Some(index) = layout.memory_helpers.as_ref().and_then(|helpers| helpers.drops.get(&object_layout)) {
        plan.push(PlanToken::Call(*index));
        return Ok(());
    }
    plan.push(PlanToken::LocalSet(layout.scratch[0]));
    drop_worklist::plan(plan, function.id, layouts, layout, object_layout)
}

fn expected_alloc_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    collection: Option<CollectionLayout>,
    destination: ValueId,
    extent: &Extent,
) -> Result<(), CanonicalAdmissionError> {
    // Mirror of `emit_alloc`: the size in scratch 0 comes from the capacity
    // of a counted collection or is a fixed layout's constant.
    let collection = match extent {
        Extent::Counted { capacity, header_bytes, stride_bytes, .. } => {
            let collection = collection.ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            if *header_bytes != collection.data_offset || *stride_bytes != collection.stride {
                return Err(CanonicalAdmissionError::UnsupportedPlan);
            }
            let maximum = (u32::MAX - collection.data_offset) / collection.stride;
            push_get(plan, layout, *capacity)?;
            plan.extend([
                PlanToken::I64ExtendI32U,
                PlanToken::I64Const(i64::from(maximum)),
                PlanToken::I64GtU,
                PlanToken::If("Empty".to_owned()),
                PlanToken::Unreachable,
                PlanToken::End,
            ]);
            push_get(plan, layout, *capacity)?;
            plan.extend([
                PlanToken::I64ExtendI32U,
                PlanToken::I64Const(i64::from(collection.stride)),
                PlanToken::I64Mul,
                PlanToken::I64Const(i64::from(collection.data_offset)),
                PlanToken::I64Add,
                PlanToken::I32WrapI64,
                PlanToken::LocalSet(layout.scratch[0]),
            ]);
            Some(collection)
        }
        Extent::FixedBytes(bytes) => {
            let bytes = fixed_alloc_bytes(*bytes).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            plan.extend([PlanToken::I32Const(bytes as i32), PlanToken::LocalSet(layout.scratch[0])]);
            None
        }
        Extent::HeaderPrefix { .. } => return Err(CanonicalAdmissionError::UnsupportedPlan),
    };
    plan.extend([
        PlanToken::GlobalGet(1),
        PlanToken::LocalSet(layout.scratch[2]),
        PlanToken::I32Const(0),
        PlanToken::LocalSet(layout.scratch[3]),
        PlanToken::Block("Empty".to_owned()),
        PlanToken::Loop("Empty".to_owned()),
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::I32Eqz,
        PlanToken::If("Empty".to_owned()),
        PlanToken::GlobalGet(0),
        PlanToken::LocalSet(layout.scratch[1]),
    ]);
    let end = [
        PlanToken::LocalGet(layout.scratch[1]),
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(RC_PREFIX_BYTES)),
        PlanToken::I64Add,
        PlanToken::LocalGet(layout.scratch[0]),
        PlanToken::I64ExtendI32U,
        PlanToken::I64Add,
    ];
    plan.extend(end.iter().cloned());
    plan.extend([
        PlanToken::I64Const(i64::from(u32::MAX)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    plan.extend(end.iter().cloned());
    plan.extend([
        PlanToken::MemorySize,
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(LINEAR_MEMORY_BYTES)),
        PlanToken::I64Mul,
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
    ]);
    plan.extend(end.iter().cloned());
    plan.extend([
        PlanToken::I64Const(i64::from(LINEAR_MEMORY_BYTES - 1)),
        PlanToken::I64Add,
        PlanToken::I64Const(i64::from(LINEAR_MEMORY_BYTES)),
        PlanToken::I64DivU,
        PlanToken::MemorySize,
        PlanToken::I64ExtendI32U,
        PlanToken::I64Sub,
        PlanToken::I32WrapI64,
        PlanToken::MemoryGrow,
        PlanToken::I32Const(-1),
        PlanToken::I32Eq,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
        PlanToken::End,
        PlanToken::LocalGet(layout.scratch[1]),
        PlanToken::I32Const(RC_PREFIX_BYTES as i32),
        PlanToken::I32Add,
        PlanToken::LocalSet(layout.scratch[2]),
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::LocalGet(layout.scratch[0]),
        PlanToken::I32Add,
        PlanToken::GlobalSet(0),
        PlanToken::Br(2),
        PlanToken::End,
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::I32Const(4),
        PlanToken::I32Sub,
        PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
        PlanToken::LocalGet(layout.scratch[0]),
        PlanToken::I32Eq,
        PlanToken::If("Empty".to_owned()),
        PlanToken::LocalGet(layout.scratch[3]),
        PlanToken::I32Eqz,
        PlanToken::If("Empty".to_owned()),
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
        PlanToken::GlobalSet(1),
        PlanToken::Else,
        PlanToken::LocalGet(layout.scratch[3]),
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
        PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
        PlanToken::End,
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::I32Const(RC_PREFIX_BYTES as i32),
        PlanToken::I32Sub,
        PlanToken::LocalSet(layout.scratch[1]),
        PlanToken::Br(2),
        PlanToken::End,
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::LocalSet(layout.scratch[3]),
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
        PlanToken::LocalSet(layout.scratch[2]),
        PlanToken::Br(0),
        PlanToken::End,
        PlanToken::End,
        PlanToken::LocalGet(layout.scratch[1]),
        PlanToken::I32Const(1),
    ]);
    push_store(plan);
    plan.extend([
        PlanToken::LocalGet(layout.scratch[1]),
        PlanToken::I32Const(4),
        PlanToken::I32Add,
        PlanToken::LocalGet(layout.scratch[0]),
    ]);
    push_store(plan);
    plan.extend([
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::LocalSet(plan_local(layout, destination)?),
    ]);
    if let (Some(collection), Extent::Counted { count, capacity, .. }) = (collection, extent) {
        plan.extend([
            PlanToken::LocalGet(layout.scratch[2]),
            PlanToken::I32Const(collection.length_offset as i32),
            PlanToken::I32Add,
        ]);
        push_get(plan, layout, *count)?;
        push_store(plan);
        if let Some(capacity_offset) = collection.capacity_offset {
            plan.extend([
                PlanToken::LocalGet(layout.scratch[2]),
                PlanToken::I32Const(capacity_offset as i32),
                PlanToken::I32Add,
            ]);
            push_get(plan, layout, *capacity)?;
            push_store(plan);
        }
    }
    let _ = function;
    Ok(())
}

fn expected_block_operations_plan(
    function: &Function,
    block: &BasicBlock,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    function_indices: &BTreeMap<FunctionId, u32>,
    signature_indices: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    imports: &BTreeMap<ImportId, ImportContract>,
    import_indices: &BTreeMap<ImportId, u32>,
    static_data_offsets: &BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    aggregates: &mut BTreeMap<ValueId, Vec<ValueId>>,
    owner_cleanup_function: Option<u32>,
) -> Result<Vec<PlanToken>, CanonicalAdmissionError> {
    // The independent function planner has already derived and checked this
    // whole-function layout. Borrow it and the original block: rebuilding a
    // synthetic function here cloned every value type and owner recipe once
    // per block. CFG terminators remain the independent PlanEmitter's job.
    let mut plan = Vec::new();
    for operation in &block.operations {
        match operation {
            // Checker-only proof transition: admission records the source
            // correspondence, but the validated Wasm plan intentionally
            // emits no instruction.
            Operation::ReauthenticateRawHeaderExtent { .. } => {}
            Operation::ClosureOwner(operation) => { owner_operation_kernel::plan(&mut plan, function, layouts, &layout, operation, import_indices, owner_cleanup_function)?; }
            Operation::ConstInt { destination, value } => {
                plan.push(PlanToken::I64Const(*value));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::Arithmetic { destination, operator, left, right, overflow } => {
                if matches!(
                    (function.value_types.get(destination), operator, overflow),
                    (
                        Some(ValueType::Int),
                        ArithmeticOperator::Add,
                        crate::canonical_safewir::OverflowBehavior::Trap,
                    )
                ) {
                    push_get(&mut plan, &layout, *left)?;
                    push_get(&mut plan, &layout, *right)?;
                    plan.extend([
                        PlanToken::I64Add,
                        PlanToken::LocalTee(plan_local(&layout, *destination)?),
                    ]);
                    push_get(&mut plan, &layout, *left)?;
                    plan.push(PlanToken::I64Xor);
                    push_get(&mut plan, &layout, *destination)?;
                    push_get(&mut plan, &layout, *right)?;
                    plan.extend([
                        PlanToken::I64Xor,
                        PlanToken::I64And,
                        PlanToken::I64Const(0),
                        PlanToken::I64LtS,
                        PlanToken::If("Empty".to_owned()),
                        PlanToken::Unreachable,
                        PlanToken::End,
                    ]);
                    continue;
                } else if matches!(
                    (function.value_types.get(destination), operator, overflow),
                    (
                        Some(ValueType::Size),
                        ArithmeticOperator::Add,
                        crate::canonical_safewir::OverflowBehavior::Trap,
                    )
                ) {
                    push_get(&mut plan, &layout, *left)?;
                    plan.push(PlanToken::I64ExtendI32U);
                    push_get(&mut plan, &layout, *right)?;
                    plan.extend([
                        PlanToken::I64ExtendI32U,
                        PlanToken::I64Add,
                        PlanToken::I64Const(i64::from(u32::MAX)),
                        PlanToken::I64GtU,
                        PlanToken::If("Empty".to_owned()),
                        PlanToken::Unreachable,
                        PlanToken::End,
                    ]);
                } else if !matches!(
                    (function.value_types.get(destination), operator, overflow),
                    (
                        Some(ValueType::Int | ValueType::Size),
                        ArithmeticOperator::DivideSigned
                            | ArithmeticOperator::DivideUnsigned
                            | ArithmeticOperator::RemainderSigned
                            | ArithmeticOperator::RemainderUnsigned,
                        crate::canonical_safewir::OverflowBehavior::Trap,
                    ) | (_, _, crate::canonical_safewir::OverflowBehavior::Wrap)
                ) {
                    // Mirror of `emit_checked_arithmetic`: integer division
                    // and remainder trap natively; everything else must wrap.
                    return Err(CanonicalAdmissionError::UnsupportedPlan);
                }
                push_get(&mut plan, &layout, *left)?;
                push_get(&mut plan, &layout, *right)?;
                let value_type = function
                    .value_types
                    .get(destination)
                    .copied()
                    .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
                plan.push(planned_arithmetic_token(value_type, *operator)?);
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::ConstUnit { destination } => {
                plan.push(PlanToken::I32Const(0));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::ConstBool { destination, value } => {
                plan.push(PlanToken::I32Const(i32::from(*value)));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::ConstFloat { destination, bits } => {
                plan.push(PlanToken::F64Const(*bits));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::ConstSize { destination, value } => {
                let value = u32::try_from(*value)
                    .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
                plan.push(PlanToken::I32Const(value as i32));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::CapabilityNone { destination, .. } => {
                plan.push(PlanToken::RefNull(format!("{:?}", wasmparser::HeapType::EXTERN)));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::CapabilitySome { destination, source } => {
                push_get(&mut plan, &layout, *source)?;
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::CapabilityIsPresent { destination, source } => {
                push_get(&mut plan, &layout, *source)?;
                plan.extend([PlanToken::RefIsNull, PlanToken::I32Eqz]);
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::CapabilityUnwrap { destination, source } => {
                push_get(&mut plan, &layout, *source)?;
                plan.push(PlanToken::RefAsNonNull);
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::StaticData { destination, data, .. } => {
                let offset = static_data_offsets
                    .get(data)
                    .copied()
                    .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
                plan.push(PlanToken::I32Const(offset as i32));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::InitNullableOwned { place } => {
                push_init_nullable_owned_plan(&mut plan, function, layouts, &layout, place)?;
            }
            Operation::NarrowSize { destination, value } => {
                push_get(&mut plan, &layout, *value)?;
                plan.extend([
                    PlanToken::I64Const(0),
                    PlanToken::I64LtS,
                    PlanToken::If("Empty".to_owned()),
                    PlanToken::Unreachable,
                    PlanToken::End,
                ]);
                push_get(&mut plan, &layout, *value)?;
                plan.extend([
                    PlanToken::I64Const(i64::from(u32::MAX)),
                    PlanToken::I64GtS,
                    PlanToken::If("Empty".to_owned()),
                    PlanToken::Unreachable,
                    PlanToken::End,
                ]);
                push_get(&mut plan, &layout, *value)?;
                plan.extend([PlanToken::I32WrapI64, PlanToken::LocalSet(plan_local(&layout, *destination)?)])
            }
            Operation::WidenSize { destination, value } => {
                push_get(&mut plan, &layout, *value)?;
                plan.extend([
                    PlanToken::I64ExtendI32U,
                    PlanToken::LocalSet(plan_local(&layout, *destination)?),
                ]);
            }
            Operation::FloatUnary { destination, operator, value } => {
                push_get(&mut plan, &layout, *value)?;
                plan.extend([
                    match operator {
                        crate::canonical_safewir::FloatUnaryOperator::Sqrt => PlanToken::F64Sqrt,
                        crate::canonical_safewir::FloatUnaryOperator::TruncateToInt => PlanToken::I64TruncSatF64S,
                        crate::canonical_safewir::FloatUnaryOperator::FromInt => PlanToken::F64ConvertI64S,
                    },
                    PlanToken::LocalSet(plan_local(&layout, *destination)?),
                ]);
            }
            Operation::MoveValue { destination, source } => {
                push_get(&mut plan, &layout, *source)?;
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::MakeInlineAggregate { destination, fields, .. } => {
                aggregates.insert(*destination, fields.clone());
            }
            Operation::ExtractInlineField { destination, aggregate, index } => {
                let fields = aggregates.get(aggregate).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
                let field = *fields.get(*index as usize).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
                push_get(&mut plan, &layout, field)?;
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::FunctionValue { destination, function: target } => {
                let table_index = checked_table_index(function_indices, *target)?;
                plan.push(PlanToken::I32Const(table_index as i32));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::Compare { destination, operator, left, right } => {
                let value_type = function
                    .value_types
                    .get(left)
                    .copied()
                    .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
                push_get(&mut plan, &layout, *left)?;
                push_get(&mut plan, &layout, *right)?;
                plan.push(planned_compare_token(value_type, *operator)?);
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::Guard { condition, .. } => {
                push_get(&mut plan, &layout, *condition)?;
                plan.extend([
                    PlanToken::I32Eqz,
                    PlanToken::If("Empty".to_owned()),
                    PlanToken::Unreachable,
                    PlanToken::End,
                ]);
            }
            Operation::Alloc { destination, layout: layout_id, extent, class } => {
                if *class != crate::canonical_safewir::AllocationClass::ReferenceCounted {
                    return Err(CanonicalAdmissionError::UnsupportedPlan);
                }
                let collection = collection_layout(function.id, layouts, *layout_id).ok();
                expected_alloc_plan(&mut plan, function, &layout, collection, *destination, extent)?;
            }
            Operation::Load { destination, place, transfer }
                if matches!(
                    transfer,
                    LoadTransfer::CopyScalar
                        | LoadTransfer::CloneShared
                        | LoadTransfer::ClonePresent
                        | LoadTransfer::MoveUnique
                        | LoadTransfer::TakeLast
                ) =>
            {
                push_memory_load_plan(&mut plan, function, layouts, &layout, place, *destination)?;
                match transfer {
                    LoadTransfer::CloneShared => push_share_plan(&mut plan, &layout, *destination, *destination)?,
                    LoadTransfer::ClonePresent => {
                        push_require_present_plan(&mut plan, &layout, *destination)?;
                        push_share_plan(&mut plan, &layout, *destination, *destination)?;
                    }
                    LoadTransfer::TakeLast => {
                        let Projection::Element { index } = place.projection() else { return Err(CanonicalAdmissionError::UnsupportedPlan); };
                        push_update_count_plan(&mut plan, function, layouts, &layout, place.object(), *index)?;
                    }
                    LoadTransfer::CopyScalar | LoadTransfer::MoveUnique => {}
                }
            }
            Operation::Store { place, value, transfer }
                if matches!(transfer, StoreTransfer::CopyScalar | StoreTransfer::MoveUnique | StoreTransfer::CloneShared) =>
            {
                if *transfer == StoreTransfer::CloneShared {
                    push_share_plan(&mut plan, &layout, *value, *value)?;
                }
                push_memory_store_plan(&mut plan, function, layouts, &layout, place, *value)?;
            }
            Operation::CopyBytes { destination, source, length } => {
                push_copy_bytes_plan(
                    &mut plan,
                    function,
                    &layout,
                    layouts,
                    destination,
                    source,
                    *length,
                )?;
            }
            Operation::MoveRange { destination, source, count } => {
                push_move_range_plan(&mut plan, function, &layout, layouts, destination, source, *count)?;
            }
            Operation::CloneRange { destination, source, count } => {
                push_clone_range_plan(&mut plan, function, &layout, layouts, destination, source, *count)?;
            }
            Operation::UpdateCount { object, count } => {
                push_update_count_plan(&mut plan, function, layouts, &layout, *object, *count)?;
            }
            Operation::CloneRef { destination, source } => {
                push_share_plan(&mut plan, &layout, *destination, *source)?;
            }
            Operation::Reclaim { object } => {
                push_reclaim_plan(&mut plan, &layout, *object)?;
            }
            Operation::Release { object } => {
                push_drop_ref_plan(&mut plan, function, layouts, &layout, *object)?;
            }
            Operation::DropRef { object } => {
                push_drop_ref_plan(&mut plan, function, layouts, &layout, *object)?;
            }
            Operation::Call { destinations, callee, arguments } => {
                for argument in arguments {
                    push_get(&mut plan, &layout, plan_call_argument(argument)?)?;
                }
                let index = function_indices
                    .get(callee)
                    .copied()
                    .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
                plan.push(PlanToken::Call(index));
                for destination in destinations.iter().rev() {
                    push_set(&mut plan, &layout, *destination)?;
                }
            }
            Operation::ExternalCall { destinations, import, arguments } => {
                for argument in arguments {
                    let value = plan_call_argument(argument)?;
                    if function.value_types.get(&value) != Some(&ValueType::Unit) {
                        push_get(&mut plan, &layout, value)?;
                    }
                }
                plan.push(PlanToken::Call(*import_indices.get(import).ok_or(CanonicalAdmissionError::UnsupportedPlan)?));
                for destination in destinations.iter().rev() {
                    if function.value_types.get(destination) != Some(&ValueType::Unit) {
                        push_set(&mut plan, &layout, *destination)?;
                    }
                }
                for destination in destinations {
                    if function.value_types.get(destination) == Some(&ValueType::Unit) {
                        plan.push(PlanToken::I32Const(0));
                        push_set(&mut plan, &layout, *destination)?;
                    }
                }
            }
            Operation::VarCall { destinations, callee, signature: call_signature, arguments } => {
                if function.value_types.get(callee) != Some(&ValueType::Callable(*call_signature)) {
                    return Err(CanonicalAdmissionError::UnsupportedPlan);
                }
                for argument in arguments {
                    push_get(&mut plan, &layout, plan_call_argument(argument)?)?;
                }
                push_get(&mut plan, &layout, *callee)?;
                plan.push(PlanToken::CallIndirect {
                    type_index: *signature_indices.get(call_signature).ok_or(CanonicalAdmissionError::UnsupportedPlan)?,
                    table_index: 0,
                });
                for destination in destinations.iter().rev() {
                    push_set(&mut plan, &layout, *destination)?;
                }
            }
            Operation::HostCall { destinations, import, arguments } => {
                push_host_call_plan(&mut plan, function, &layout, imports, import_indices, layouts, destinations, *import, arguments)?;
            }
            _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
        }
    }
    Ok(plan)
}

struct PlanEmitter<'a> {
    source_function: &'a Function,
    /// Blocks by id: the structured walk looks blocks up on every path,
    /// join, and reachability step, so the lookup must not scan the body.
    blocks_by_id: BTreeMap<crate::canonical_safewir::BlockId, &'a BasicBlock>,
    layout: &'a LocalLayout<'a>,
    layouts: &'a LayoutInterner,
    function_indices: &'a BTreeMap<FunctionId, u32>,
    signature_indices: &'a BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    imports: &'a BTreeMap<ImportId, ImportContract>,
    import_indices: &'a BTreeMap<ImportId, u32>,
    static_data_offsets: &'a BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    owner_cleanup_function: Option<u32>,
    successors_by_block: BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
    predecessors_by_block: BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
    plan: Vec<PlanToken>,
    emitted: BTreeSet<crate::canonical_safewir::BlockId>,
    labels: Vec<StructuredLabel>,
    loops: Vec<(crate::canonical_safewir::BlockId, usize)>,
    /// Inline aggregates are SSA markers over field locals, so a field read
    /// is valid anywhere the aggregate dominates: the map is function-wide,
    /// as in the encoder.
    aggregates: BTreeMap<ValueId, Vec<ValueId>>,
    loop_exits: Vec<(crate::canonical_safewir::BlockId, usize)>,
    loop_headers: RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>,
    loop_exit_targets: RefCell<BTreeMap<crate::canonical_safewir::BlockId, Option<crate::canonical_safewir::BlockId>>>,
    back_edge_source_facts: RefCell<BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>>,
    natural_loop_facts: RefCell<BTreeMap<crate::canonical_safewir::BlockId, BTreeSet<crate::canonical_safewir::BlockId>>>,
    reaches_avoiding_facts: RefCell<BTreeMap<(crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId), bool>>,
    immediate_dominators: std::cell::OnceCell<BTreeMap<crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId>>,
    block_slots: std::cell::OnceCell<usize>,
    /// Memoized path judgments keyed by the exact normalized merge set. This
    /// mirrors the encoder's bounded graph decision, including cached cyclic
    /// refusals, without weakening the independent plan comparison.
    path_facts: RefCell<BTreeMap<Vec<crate::canonical_safewir::BlockId>, std::rc::Rc<RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>>>>,
}

impl<'a> PlanEmitter<'a> {
    fn block(&self, id: crate::canonical_safewir::BlockId) -> Result<&'a BasicBlock, CanonicalAdmissionError> {
        self.blocks_by_id.get(&id).copied().ok_or(CanonicalAdmissionError::UnsupportedPlan)
    }

    fn successors(&self, id: crate::canonical_safewir::BlockId) -> Result<Vec<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
        self.successors_by_block.get(&id).cloned().ok_or(CanonicalAdmissionError::UnsupportedPlan)
    }

    /// `distances_from` stopping at `stops` (which are never entered).
    fn distances_until(
        &self,
        start: crate::canonical_safewir::BlockId,
        stops: &[crate::canonical_safewir::BlockId],
    ) -> Result<BTreeMap<crate::canonical_safewir::BlockId, usize>, CanonicalAdmissionError> {
        let mut distances = BTreeMap::new();
        let mut pending = VecDeque::from([(start, 0)]);
        while let Some((id, distance)) = pending.pop_front() {
            if stops.contains(&id) || distances.contains_key(&id) {
                continue;
            }
            self.block(id)?;
            distances.insert(id, distance);
            pending.extend(self.successors(id)?.into_iter().map(|successor| (successor, distance + 1)));
        }
        Ok(distances)
    }

    fn distances_from(&self, start: crate::canonical_safewir::BlockId) -> Result<BTreeMap<crate::canonical_safewir::BlockId, usize>, CanonicalAdmissionError> {
        let mut distances = BTreeMap::new();
        let mut pending = VecDeque::from([(start, 0)]);
        while let Some((id, distance)) = pending.pop_front() {
            if distances.contains_key(&id) {
                continue;
            }
            self.block(id)?;
            distances.insert(id, distance);
            pending.extend(self.successors(id)?.into_iter().map(|successor| (successor, distance + 1)));
        }
        Ok(distances)
    }

    fn reaches(&self, start: crate::canonical_safewir::BlockId, goal: crate::canonical_safewir::BlockId) -> Result<bool, CanonicalAdmissionError> {
        // Independently walk the checked-source graph. The complete edge
        // census in expected_function_plan rejects missing successors before
        // traversal; finding the goal cannot hide a malformed sibling edge.
        let mut pending = vec![start];
        let mut seen = BTreeSet::new();
        while let Some(current) = pending.pop() {
            if !seen.insert(current) { continue; }
            self.block(current)?;
            if current == goal { return Ok(true); }
            let successors = self.successors_by_block.get(&current)
                .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            pending.extend(successors.iter().rev().copied());
        }
        Ok(false)
    }

    /// One slot per block id, for dense visited marks in the walks below.
    fn block_slots(&self) -> usize {
        *self.block_slots.get_or_init(|| block_slot_count(&self.successors_by_block))
    }

    /// A side only terminates when every path from it ends in a terminal
    /// block (structured loops included: their bodies return to their
    /// headers or leave through their exits, and the exits terminate) and it
    /// shares no block with the other side, where the other side is what
    /// `other` reaches without passing back through `avoid` (the branching
    /// block: a loop back edge would otherwise make a returning arm look
    /// reachable from the continuing side). Such a side never reaches a
    /// merge and needs none.
    fn only_terminates_avoiding(
        &self,
        start: crate::canonical_safewir::BlockId,
        other: crate::canonical_safewir::BlockId,
        avoid: crate::canonical_safewir::BlockId,
    ) -> Result<bool, CanonicalAdmissionError> {
        let slots = self.block_slots();
        let mut visited = vec![false; slots];
        let mut side = Vec::new();
        let mut pending = VecDeque::from([start]);
        while let Some(id) = pending.pop_front() {
            if std::mem::replace(&mut visited[id.get() as usize], true) {
                continue;
            }
            self.block(id)?;
            side.push(id);
            pending.extend(self.successors(id)?);
        }
        // The sides share a block exactly when `other` reaches one of this
        // side's blocks without passing through `avoid`: walk that path
        // backwards from the side. The side is small (a terminating arm);
        // the forward sweep from `other` this replaces covered the rest of
        // the function on every call -- 13.5M block visits in one async
        // compile, against 47k for the side itself.
        if other != avoid {
            let mut reached = vec![false; slots];
            let mut backward = VecDeque::from(side);
            while let Some(id) = backward.pop_front() {
                if id == avoid || std::mem::replace(&mut reached[id.get() as usize], true) {
                    continue;
                }
                if id == other {
                    return Ok(false);
                }
                backward.extend(self.predecessors_by_block.get(&id).into_iter().flatten().copied());
            }
        }
        self.all_paths_reach_or_terminate(start, &[], &mut BTreeSet::new())
    }

    fn is_terminal(&self, id: crate::canonical_safewir::BlockId) -> Result<bool, CanonicalAdmissionError> {
        Ok(matches!(
            self.block(id)?.terminator,
            Terminator::Return { .. }
                | Terminator::TailCall { .. }
                | Terminator::VarTailCall { .. }
                | Terminator::Trap
                | Terminator::Unreachable
        ))
    }

    /// Mirror of the encoder's exclusive terminal-tail judgment.
    fn is_exclusive_terminal_loop_tail(
        &self,
        start: crate::canonical_safewir::BlockId,
        header: crate::canonical_safewir::BlockId,
        members: &[crate::canonical_safewir::BlockId],
    ) -> Result<bool, CanonicalAdmissionError> {
        if self.reaches(start, header)? {
            return Ok(false);
        }
        let reach = self.reachable_until(start, &[header])?;
        if !self.all_paths_reach_or_terminate(start, &[], &mut BTreeSet::new())? {
            return Ok(false);
        }
        for reached in &reach {
            // Match the encoder's exclusivity rule for terminal sinks too.
            let predecessors = self.predecessors(*reached)?;
            let mut external = predecessors.iter().filter(|predecessor| !reach.contains(predecessor));
            if *reached == start {
                let Some(entry) = external.next() else { return Ok(false) };
                if !members.contains(entry) || external.next().is_some() {
                    return Ok(false);
                }
            } else if external.next().is_some() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn all_paths_reach_or_terminate(
        &self,
        start: crate::canonical_safewir::BlockId,
        merges: &[crate::canonical_safewir::BlockId],
        active: &mut BTreeSet<crate::canonical_safewir::BlockId>,
    ) -> Result<bool, CanonicalAdmissionError> {
        if merges.contains(&start) || self.is_terminal(start)? {
            return Ok(true);
        }
        let mut region = merges.to_vec();
        region.sort_unstable();
        let facts = self.path_query_facts(&region);
        self.all_paths_with_facts(start, &region, active, &facts)
    }

    fn path_query_facts(
        &self,
        sorted_stops: &[crate::canonical_safewir::BlockId],
    ) -> std::rc::Rc<RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>> {
        // Independent plan-side storage for the same exact query keys. A
        // group stays live across nested queries without holding a borrow of
        // the outer table, and its results never come from encoder decisions.
        let mut table = self.path_facts.borrow_mut();
        if let Some(existing) = table.get(sorted_stops) {
            return std::rc::Rc::clone(existing);
        }
        let nodes = std::rc::Rc::new(RefCell::new(BTreeMap::new()));
        table.insert(sorted_stops.to_vec(), std::rc::Rc::clone(&nodes));
        nodes
    }

    fn all_paths_with_facts(
        &self,
        start: crate::canonical_safewir::BlockId,
        merges: &[crate::canonical_safewir::BlockId],
        active: &mut BTreeSet<crate::canonical_safewir::BlockId>,
        facts: &RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>,
    ) -> Result<bool, CanonicalAdmissionError> {
        if merges.contains(&start) || self.is_terminal(start)? {
            return Ok(true);
        }
        if let Some(result) = facts.borrow().get(&start) {
            return Ok(*result);
        }
        if !active.insert(start) {
            return Ok(false);
        }
        let result = if self.is_loop_header(start)? {
            let exit = self.loop_exit(start)?;
            let mut body_merges = vec![start];
            body_merges.extend(exit);
            body_merges.sort_unstable();
            let body_facts = self.path_query_facts(&body_merges);
            // The body walk keeps the caller's active set: body blocks are
            // dominated by the header, so an active ancestor is reachable
            // only by leaving the loop somewhere other than its exit, which
            // must fail rather than walk the enclosing loop again.
            let mut result = true;
            for successor in self.successors(start)? {
                if !self.all_paths_with_facts(successor, &body_merges, active, &body_facts)? {
                    result = false;
                    break;
                }
            }
            result
                && match exit {
                    Some(exit) => self.all_paths_with_facts(exit, merges, active, facts)?,
                    None => true,
                }
        } else {
            let mut result = true;
            for successor in self.successors(start)? {
                if !self.all_paths_with_facts(successor, merges, active, facts)? {
                    result = false;
                    break;
                }
            }
            result
        };
        active.remove(&start);
        facts.borrow_mut().insert(start, result);
        Ok(result)
    }

    fn is_loop_header(&self, id: crate::canonical_safewir::BlockId) -> Result<bool, CanonicalAdmissionError> {
        if let Some(known) = self.loop_headers.borrow().get(&id) {
            return Ok(*known);
        }
        // Mirror of the encoder's rule: a loop header is the target of a back
        // edge, a predecessor it dominates.
        let result = !self.back_edge_sources(id)?.is_empty();
        self.loop_headers.borrow_mut().insert(id, result);
        Ok(result)
    }

    /// Mirror of the encoder's `loop_exit`: the one target outside the natural
    /// loop, `None` for a loop that only leaves by terminating.
    fn loop_exit(&self, header: crate::canonical_safewir::BlockId) -> Result<Option<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
        if let Some(known) = self.loop_exit_targets.borrow().get(&header) {
            return Ok(*known);
        }
        let mut members = vec![header];
        for block in &self.source_function.blocks {
            if block.id != header && self.in_natural_loop(header, block.id)? {
                members.push(block.id);
            }
        }
        // Mirror of the encoder's rule: the `while` header's `else` side and
        // every non-terminating edge out of the loop are exits; one exit is
        // the continuation, several continue at the nearest block they all
        // reach.
        let classic_exit = match &self.block(header)?.terminator {
            Terminator::BranchIf { else_target, .. } if !members.contains(else_target) => Some(*else_target),
            _ => None,
        };
        let mut leaving = BTreeSet::new();
        leaving.extend(classic_exit);
        for member in &members {
            for successor in self.successors(*member)? {
                if !members.contains(&successor) {
                    leaving.insert(successor);
                }
            }
        }
        // Mirror of the encoder: only an exclusive terminal tail is emitted
        // inline; shared terminal sinks need no duplicated cleanup.
        let mut candidates = BTreeSet::new();
        for block in &leaving {
            if Some(*block) == classic_exit
                || !self.is_exclusive_terminal_loop_tail(*block, header, &members)?
            {
                candidates.insert(*block);
            }
        }
        let exit = if candidates.len() <= 1 {
            candidates.into_iter().next()
        } else {
            let mut common: Option<(usize, crate::canonical_safewir::BlockId)> = None;
            let distances = candidates
                .iter()
                .map(|candidate| self.distances_from(*candidate))
                .collect::<Result<Vec<_>, _>>()?;
            for block in distances[0].keys() {
                let Some(farthest) = distances
                    .iter()
                    .map(|reach| reach.get(block).copied())
                    .collect::<Option<Vec<_>>>()
                    .and_then(|reaches| reaches.into_iter().max())
                else {
                    continue;
                };
                let score = (farthest, *block);
                if common.is_none_or(|previous| score < previous) {
                    common = Some(score);
                }
            }
            let Some((_, block)) = common else {
                return Err(CanonicalAdmissionError::UnsupportedPlan);
            };
            Some(block)
        };
        self.loop_exit_targets.borrow_mut().insert(header, exit);
        Ok(exit)
    }

    fn escape_label(&self, target: crate::canonical_safewir::BlockId) -> Option<usize> {
        self.loops
            .iter()
            .rev()
            .find(|(header, _)| *header == target)
            .or_else(|| self.loop_exits.iter().rev().find(|(exit, _)| *exit == target))
            .map(|(_, label_index)| *label_index)
    }

    fn predecessors(&self, block: crate::canonical_safewir::BlockId) -> Result<Vec<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
        self.predecessors_by_block.get(&block).cloned().ok_or(CanonicalAdmissionError::UnsupportedPlan)
    }

    fn reachable_until(&self, start: crate::canonical_safewir::BlockId, stops: &[crate::canonical_safewir::BlockId]) -> Result<BTreeSet<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
        let mut seen = BTreeSet::new();
        let mut pending = VecDeque::from([start]);
        while let Some(id) = pending.pop_front() {
            if stops.contains(&id) || !seen.insert(id) {
                continue;
            }
            pending.extend(self.successors(id)?);
        }
        Ok(seen)
    }

    /// A branch side that only leaves: its target is an escape, or every
    /// path from it ends in an escape or a terminal block (the drop block on
    /// a loop's exit edge) and it shares no block with the other side.
    fn side_escapes(&self, current: crate::canonical_safewir::BlockId, side: crate::canonical_safewir::BlockId, other: crate::canonical_safewir::BlockId, stop: Option<crate::canonical_safewir::BlockId>) -> Result<bool, CanonicalAdmissionError> {
        if self.escape_label(side).is_some() {
            return Ok(true);
        }
        let mut stops = self.escape_targets();
        if stops.is_empty() || !self.all_paths_reach_or_terminate(side, &stops, &mut BTreeSet::new())? {
            return Ok(false);
        }
        stops.push(current);
        let reach = self.reachable_until(side, &stops)?;
        // A side that reaches the enclosing merge flows into it rather than
        // escaping: emitting it inline here would emit the merge twice.
        if stop.is_some_and(|stop| reach.contains(&stop)) {
            return Ok(false);
        }
        // Every path must end at an escape: a side that can also return is
        // a terminal side for `only_terminates` to judge, not an escape.
        for block in &reach {
            if self.is_terminal(*block)? {
                return Ok(false);
            }
        }
        let other_reach = self.reachable_until(other, &stops)?;
        Ok(reach.is_disjoint(&other_reach))
    }

    fn escape_targets(&self) -> Vec<crate::canonical_safewir::BlockId> {
        self.loops
            .iter()
            .map(|(header, _)| *header)
            .chain(self.loop_exits.iter().map(|(exit, _)| *exit))
            .collect()
    }

    fn reaches_avoiding(&self, start: crate::canonical_safewir::BlockId, goal: crate::canonical_safewir::BlockId, avoid: crate::canonical_safewir::BlockId) -> Result<bool, CanonicalAdmissionError> {
        let key = (start, goal, avoid);
        if let Some(known) = self.reaches_avoiding_facts.borrow().get(&key) {
            return Ok(*known);
        }
        let mut seen = BTreeSet::new();
        let mut pending = VecDeque::from([start]);
        let mut result = false;
        while let Some(id) = pending.pop_front() {
            if id == avoid || !seen.insert(id) {
                continue;
            }
            if id == goal {
                result = true;
                break;
            }
            pending.extend(self.successors(id)?);
        }
        self.reaches_avoiding_facts.borrow_mut().insert(key, result);
        Ok(result)
    }

    fn dominates(&self, dominator: crate::canonical_safewir::BlockId, node: crate::canonical_safewir::BlockId) -> Result<bool, CanonicalAdmissionError> {
        let entry = self.source_function.entry;
        let idom = self
            .immediate_dominators
            .get_or_init(|| immediate_dominators(entry, &self.successors_by_block));
        Ok(dominates_in(idom, entry, dominator, node))
    }

    fn back_edge_sources(&self, header: crate::canonical_safewir::BlockId) -> Result<Vec<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
        if let Some(known) = self.back_edge_source_facts.borrow().get(&header) {
            return Ok(known.clone());
        }
        let mut sources = Vec::new();
        for predecessor in self.predecessors(header)? {
            if predecessor != header && self.dominates(header, predecessor)? {
                sources.push(predecessor);
            }
        }
        self.back_edge_source_facts.borrow_mut().insert(header, sources.clone());
        Ok(sources)
    }

    fn natural_loop_members(&self, header: crate::canonical_safewir::BlockId) -> Result<BTreeSet<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
        if let Some(known) = self.natural_loop_facts.borrow().get(&header) {
            return Ok(known.clone());
        }
        let members = reverse_loop_members(
            header,
            self.back_edge_sources(header)?,
            &self.predecessors_by_block,
        );
        self.natural_loop_facts.borrow_mut().insert(header, members.clone());
        Ok(members)
    }

    fn in_natural_loop(&self, header: crate::canonical_safewir::BlockId, block: crate::canonical_safewir::BlockId) -> Result<bool, CanonicalAdmissionError> {
        if block == header {
            return Ok(false);
        }
        if let Some(members) = self.natural_loop_facts.borrow().get(&header) {
            return Ok(members.contains(&block));
        }
        Ok(self.natural_loop_members(header)?.contains(&block))
    }

    fn find_join(
        &self,
        current: crate::canonical_safewir::BlockId,
        then_target: crate::canonical_safewir::BlockId,
        else_target: crate::canonical_safewir::BlockId,
        stop: Option<crate::canonical_safewir::BlockId>,
    ) -> Result<Option<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
        if then_target == else_target {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        // Mirror of the encoder's rule: an escaping side is a plain `br` and
        // the other side's first block is where emission continues.
        match (
            self.side_escapes(current, then_target, else_target, stop)?,
            self.side_escapes(current, else_target, then_target, stop)?,
        ) {
            (true, true) => return Ok(None),
            (true, false) => return Ok(Some(else_target)),
            (false, true) => return Ok(Some(then_target)),
            (false, false) => {}
        }
        // Candidates come from paths that stay in the current loop region: a
        // block reached only through an enclosing loop's back edge or exit is
        // the next iteration's, not this branch's merge.
        let stops = self.escape_targets();
        let then_distances = self.distances_until(then_target, &stops)?;
        let else_distances = self.distances_until(else_target, &stops)?;
        // Mirror of the encoder's rule: a terminal side bypasses the merge and
        // contributes distance zero.
        // The other side's reach is taken without passing back through the
        // branching block: a loop back edge would otherwise make a returning
        // arm look reachable from the continuing side.
        // Mirror the enclosing-continuation boundary used by the encoder.
        let then_reaches_stop = match stop { Some(stop) => self.reaches(then_target, stop)?, None => false };
        let else_reaches_stop = match stop { Some(stop) => self.reaches(else_target, stop)?, None => false };
        let then_terminal = !then_reaches_stop && self.only_terminates_avoiding(then_target, else_target, current)?;
        let else_terminal = !else_reaches_stop && self.only_terminates_avoiding(else_target, then_target, current)?;
        // Rank every candidate by the score the selection below minimizes
        // before running the path check, so the check runs cheapest-first and
        // stops at the first admissible candidate: that is the minimum, and
        // an executor-sized region no longer walks every reachable block per
        // candidate.
        let mut scored = Vec::new();
        for candidate in then_distances.keys().chain(else_distances.keys()).copied() {
            // Mirror of the encoder's rule: a side's own first block can be
            // the merge; loop headers and exits never are.
            if candidate == current || self.escape_label(candidate).is_some() {
                continue;
            }
            let then_distance = match then_distances.get(&candidate) {
                Some(distance) => *distance,
                None if then_terminal => 0,
                None => continue,
            };
            let else_distance = match else_distances.get(&candidate) {
                Some(distance) => *distance,
                None if else_terminal => 0,
                None => continue,
            };
            scored.push((then_distance + else_distance, candidate));
        }
        scored.sort_unstable();
        scored.dedup();
        let mut best = None;
        for score @ (_, candidate) in scored {
            // Inside a loop a path may also leave through the loop's exit or
            // back edge (a `br`) instead of reaching the merge.
            let mut merges = self.escape_targets();
            merges.push(candidate);
            if self.all_paths_reach_or_terminate(then_target, &merges, &mut BTreeSet::new())?
                && self.all_paths_reach_or_terminate(else_target, &merges, &mut BTreeSet::new())?
            {
                best = Some(score);
                break;
            }
        }
        best.map(|(_, block)| Some(block)).ok_or(CanonicalAdmissionError::UnsupportedPlan)
    }

    fn label_depth(&self, index: usize) -> u32 {
        (self.labels.len() - 1 - index) as u32
    }

    fn push_branch_arguments(&mut self, target: crate::canonical_safewir::BlockId, arguments: &[ValueId]) -> Result<(), CanonicalAdmissionError> {
        let parameters = self.block(target)?.parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
        if parameters.len() != arguments.len() {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        for argument in arguments {
            push_get(&mut self.plan, self.layout, *argument)?;
        }
        for parameter in parameters.into_iter().rev() {
            push_set(&mut self.plan, self.layout, parameter)?;
        }
        Ok(())
    }

    fn push_terminator(&mut self, terminator: &Terminator) -> Result<(), CanonicalAdmissionError> {
        match terminator {
            Terminator::Return { values } => {
                for value in values {
                    push_get(&mut self.plan, self.layout, *value)?;
                }
                self.plan.push(PlanToken::Return);
            }
            Terminator::TailCall { callee, arguments, .. } => {
                for argument in arguments {
                    push_get(&mut self.plan, self.layout, plan_call_argument(argument)?)?;
                }
                self.plan.push(PlanToken::ReturnCall(self.function_indices.get(callee).copied().ok_or(CanonicalAdmissionError::UnsupportedPlan)?));
            }
            Terminator::VarTailCall { callee, signature, arguments, .. } => {
                for argument in arguments {
                    push_get(&mut self.plan, self.layout, plan_call_argument(argument)?)?;
                }
                push_get(&mut self.plan, self.layout, *callee)?;
                self.plan.push(PlanToken::ReturnCallIndirect {
                    type_index: *self
                        .signature_indices
                        .get(signature)
                        .ok_or(CanonicalAdmissionError::UnsupportedPlan)?,
                    table_index: 0,
                });
            }
            Terminator::Trap | Terminator::Unreachable => self.plan.push(PlanToken::Unreachable),
            Terminator::Branch { .. } | Terminator::BranchIf { .. } => {
                return Err(CanonicalAdmissionError::UnsupportedPlan);
            }
        }
        Ok(())
    }

    fn emit_path(&mut self, start: crate::canonical_safewir::BlockId, stop: Option<crate::canonical_safewir::BlockId>) -> Result<(), CanonicalAdmissionError> {
        if let Some(label_index) = self.escape_label(start) {
            self.plan.push(PlanToken::Br(self.label_depth(label_index)));
            return Ok(());
        }
        if stop == Some(start) {
            return Ok(());
        }
        if self.is_loop_header(start)? {
            return self.emit_loop(start, stop);
        }
        if !self.emitted.insert(start) {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        let block = self.block(start)?;
        self.plan.extend(expected_block_operations_plan(
            self.source_function,
            block,
            self.layout,
            self.layouts,
        self.function_indices,
        self.signature_indices,
            self.imports,
            self.import_indices,
            self.static_data_offsets,
            &mut self.aggregates,
            self.owner_cleanup_function,
        )?);
        self.emit_block_terminator(start, &block.terminator, stop)
    }

    fn emit_block_terminator(&mut self, start: crate::canonical_safewir::BlockId, terminator: &Terminator, stop: Option<crate::canonical_safewir::BlockId>) -> Result<(), CanonicalAdmissionError> {
        match terminator {
            Terminator::Branch { target, arguments } => {
                self.push_branch_arguments(*target, arguments)?;
                self.emit_path(*target, stop)
            }
            Terminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments } => {
                self.emit_if(start, *condition, *then_target, then_arguments, *else_target, else_arguments, stop)
            }
            terminator => self.push_terminator(terminator),
        }
    }

    fn emit_if(
        &mut self,
        current: crate::canonical_safewir::BlockId,
        condition: ValueId,
        then_target: crate::canonical_safewir::BlockId,
        then_arguments: &[ValueId],
        else_target: crate::canonical_safewir::BlockId,
        else_arguments: &[ValueId],
        stop: Option<crate::canonical_safewir::BlockId>,
    ) -> Result<(), CanonicalAdmissionError> {
        if self.is_loop_header(current)? && !self.loops.iter().any(|(header, _)| *header == current) {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        let join = self.find_join(current, then_target, else_target, stop)?;
        push_get(&mut self.plan, self.layout, condition)?;
        self.plan.push(PlanToken::If("Empty".to_owned()));
        self.labels.push(StructuredLabel::If);
        self.push_branch_arguments(then_target, then_arguments)?;
        self.emit_path(then_target, join)?;
        self.plan.push(PlanToken::Else);
        self.push_branch_arguments(else_target, else_arguments)?;
        self.emit_path(else_target, join)?;
        self.plan.push(PlanToken::End);
        self.labels.pop();
        match join {
            Some(join) => self.emit_path(join, stop),
            None => Ok(()),
        }
    }

    fn emit_loop(&mut self, header: crate::canonical_safewir::BlockId, stop: Option<crate::canonical_safewir::BlockId>) -> Result<(), CanonicalAdmissionError> {
        if !self.emitted.insert(header) {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        if !self.is_loop_header(header)? {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        let block = self.block(header)?;
        let exit = self.loop_exit(header)?;
        self.plan.push(PlanToken::Block("Empty".to_owned()));
        let block_label = self.labels.len();
        self.labels.push(StructuredLabel::Block);
        self.plan.push(PlanToken::Loop("Empty".to_owned()));
        let loop_label = self.labels.len();
        self.labels.push(StructuredLabel::Loop);
        self.plan.extend(expected_block_operations_plan(self.source_function, block, self.layout, self.layouts, self.function_indices, self.signature_indices, self.imports, self.import_indices, self.static_data_offsets, &mut self.aggregates, self.owner_cleanup_function)?);
        self.loops.push((header, loop_label));
        if let Some(exit) = exit {
            self.loop_exits.push((exit, block_label));
        }
        match &block.terminator {
            // Mirror of the encoder's canonical `while` form.
            Terminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments } if Some(*else_target) == exit => {
                push_get(&mut self.plan, self.layout, *condition)?;
                self.plan.push(PlanToken::If("Empty".to_owned()));
                self.labels.push(StructuredLabel::If);
                self.push_branch_arguments(*then_target, then_arguments)?;
                self.emit_path(*then_target, Some(header))?;
                self.plan.push(PlanToken::Else);
                self.push_branch_arguments(*else_target, else_arguments)?;
                self.plan.push(PlanToken::Br(self.label_depth(block_label)));
                self.plan.push(PlanToken::End);
                self.labels.pop();
            }
            terminator => self.emit_block_terminator(header, terminator, Some(header))?,
        }
        if exit.is_some() {
            self.loop_exits.pop();
        }
        self.loops.pop();
        self.plan.push(PlanToken::End);
        self.labels.pop();
        self.plan.push(PlanToken::End);
        self.labels.pop();
        match exit {
            Some(exit) => self.emit_path(exit, stop),
            None => {
                self.plan.push(PlanToken::Unreachable);
                Ok(())
            }
        }
    }
}

/// One canonical spelling for a local's value type so the independently
/// decoded body and the planned body compare kinds, not two libraries' `Debug`
/// renderings (which agree for numerics and differ for reference types).
/// The plan's comparison table. It must be the exact mirror of `emit_compare`:
/// `Int` compares as i64, `Size` and `Bool` as i32, `Float` as f64; every
/// operator is covered so the plan never refuses a comparison the encoder
/// emits.
/// The plan's arithmetic table, the exact mirror of `emit_arithmetic`.
fn planned_arithmetic_token(
    value_type: crate::canonical_safewir::ValueType,
    operator: ArithmeticOperator,
) -> Result<PlanToken, CanonicalAdmissionError> {
    use crate::canonical_safewir::ValueType as V;
    use ArithmeticOperator as A;
    Ok(match value_type {
        V::Int => match operator {
            A::Add => PlanToken::I64Add,
            A::Subtract => PlanToken::I64Sub,
            A::Multiply => PlanToken::I64Mul,
            A::DivideSigned => PlanToken::I64DivS,
            A::DivideUnsigned => PlanToken::I64DivU,
            A::RemainderSigned => PlanToken::I64RemS,
            A::RemainderUnsigned => PlanToken::I64RemU,
            A::BitAnd => PlanToken::I64And,
            A::BitOr => PlanToken::I64Or,
            A::BitXor => PlanToken::I64Xor,
            A::ShiftLeft => PlanToken::I64Shl,
            A::ShiftRightSigned => PlanToken::I64ShrS,
            A::ShiftRightUnsigned => PlanToken::I64ShrU,
        },
        V::Size => match operator {
            A::Add => PlanToken::I32Add,
            A::Subtract => PlanToken::I32Sub,
            A::Multiply => PlanToken::I32Mul,
            A::DivideSigned => PlanToken::I32DivS,
            A::DivideUnsigned => PlanToken::I32DivU,
            A::RemainderSigned => PlanToken::I32RemS,
            A::RemainderUnsigned => PlanToken::I32RemU,
            A::BitAnd => PlanToken::I32And,
            A::BitOr => PlanToken::I32Or,
            A::BitXor => PlanToken::I32Xor,
            A::ShiftLeft => PlanToken::I32Shl,
            A::ShiftRightSigned => PlanToken::I32ShrS,
            A::ShiftRightUnsigned => PlanToken::I32ShrU,
        },
        V::Float => match operator {
            A::Add => PlanToken::F64Add,
            A::Subtract => PlanToken::F64Sub,
            A::Multiply => PlanToken::F64Mul,
            A::DivideSigned | A::DivideUnsigned => PlanToken::F64Div,
            _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
        },
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    })
}

fn planned_compare_token(
    value_type: crate::canonical_safewir::ValueType,
    operator: ComparisonOperator,
) -> Result<PlanToken, CanonicalAdmissionError> {
    use crate::canonical_safewir::ValueType as V;
    Ok(match value_type {
        V::Int => match operator {
            ComparisonOperator::Equal => PlanToken::I64Eq,
            ComparisonOperator::NotEqual => PlanToken::I64Ne,
            ComparisonOperator::LessSigned => PlanToken::I64LtS,
            ComparisonOperator::LessUnsigned => PlanToken::I64LtU,
            ComparisonOperator::LessEqualSigned => PlanToken::I64LeS,
            ComparisonOperator::LessEqualUnsigned => PlanToken::I64LeU,
            ComparisonOperator::GreaterSigned => PlanToken::I64GtS,
            ComparisonOperator::GreaterUnsigned => PlanToken::I64GtU,
            ComparisonOperator::GreaterEqualSigned => PlanToken::I64GeS,
            ComparisonOperator::GreaterEqualUnsigned => PlanToken::I64GeU,
        },
        V::Size | V::Bool => match operator {
            ComparisonOperator::Equal => PlanToken::I32Eq,
            ComparisonOperator::NotEqual => PlanToken::I32Ne,
            ComparisonOperator::LessSigned => PlanToken::I32LtS,
            ComparisonOperator::LessUnsigned => PlanToken::I32LtU,
            ComparisonOperator::LessEqualSigned => PlanToken::I32LeS,
            ComparisonOperator::LessEqualUnsigned => PlanToken::I32LeU,
            ComparisonOperator::GreaterSigned => PlanToken::I32GtS,
            ComparisonOperator::GreaterUnsigned => PlanToken::I32GtU,
            ComparisonOperator::GreaterEqualSigned => PlanToken::I32GeS,
            ComparisonOperator::GreaterEqualUnsigned => PlanToken::I32GeU,
        },
        V::Float => match operator {
            ComparisonOperator::Equal => PlanToken::F64Eq,
            ComparisonOperator::NotEqual => PlanToken::F64Ne,
            ComparisonOperator::LessSigned | ComparisonOperator::LessUnsigned => PlanToken::F64Lt,
            ComparisonOperator::LessEqualSigned | ComparisonOperator::LessEqualUnsigned => PlanToken::F64Le,
            ComparisonOperator::GreaterSigned | ComparisonOperator::GreaterUnsigned => PlanToken::F64Gt,
            ComparisonOperator::GreaterEqualSigned | ComparisonOperator::GreaterEqualUnsigned => PlanToken::F64Ge,
        },
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    })
}

fn planned_local_kind(ty: ValType) -> String {
    match ty {
        ValType::I32 => "i32".to_owned(),
        ValType::I64 => "i64".to_owned(),
        ValType::F32 => "f32".to_owned(),
        ValType::F64 => "f64".to_owned(),
        ValType::V128 => "v128".to_owned(),
        ValType::Ref(reference) if reference == wasm_encoder::RefType::EXTERNREF => "externref".to_owned(),
        ValType::Ref(reference) if reference == wasm_encoder::RefType::FUNCREF => "funcref".to_owned(),
        ValType::Ref(reference) if reference == wasm_encoder::RefType::ANYREF => "anyref".to_owned(),
        other => format!("{other:?}"),
    }
}

fn decoded_local_kind(ty: wasmparser::ValType) -> String {
    match ty {
        wasmparser::ValType::I32 => "i32".to_owned(),
        wasmparser::ValType::I64 => "i64".to_owned(),
        wasmparser::ValType::F32 => "f32".to_owned(),
        wasmparser::ValType::F64 => "f64".to_owned(),
        wasmparser::ValType::V128 => "v128".to_owned(),
        wasmparser::ValType::Ref(reference) if reference.is_extern_ref() => "externref".to_owned(),
        wasmparser::ValType::Ref(reference) if reference.is_func_ref() => "funcref".to_owned(),
        wasmparser::ValType::Ref(reference) if reference == wasmparser::RefType::ANYREF => "anyref".to_owned(),
        other => format!("{other:?}"),
    }
}

fn expected_function_plan(
    function: &Function,
    signature: &FunctionSignature,
    layouts: &LayoutInterner,
    function_indices: &BTreeMap<FunctionId, u32>,
    signature_indices: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    imports: &BTreeMap<ImportId, ImportContract>,
    import_indices: &BTreeMap<ImportId, u32>,
    static_data_offsets: &BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    owner_cleanup_function: Option<u32>,
    owner_bridges: &[AdmittedOwnerBridge],
    memory_helpers: &std::sync::Arc<outlined_memory::Helpers>,
) -> Result<Vec<PlanToken>, CanonicalAdmissionError> {
    let mut layout = local_layout(function, signature, layouts).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    layout.owner_bridges = owner_bridges.iter().collect();
    layout.memory_helpers = Some(memory_helpers.clone());
    let successors_by_block: BTreeMap<_, _> = function
        .blocks
        .iter()
        .map(|block| {
            let successors = match &block.terminator {
                Terminator::Branch { target, .. } => vec![*target],
                Terminator::BranchIf { then_target, else_target, .. } => {
                    vec![*then_target, *else_target]
                }
                Terminator::Return { .. }
                | Terminator::TailCall { .. }
                | Terminator::VarTailCall { .. }
                | Terminator::Trap
                | Terminator::Unreachable => Vec::new(),
            };
            (block.id, successors)
        })
        .collect();
    let mut predecessors_by_block: BTreeMap<_, Vec<_>> = function
        .blocks
        .iter()
        .map(|block| (block.id, Vec::new()))
        .collect();
    for (source, successors) in &successors_by_block {
        for successor in successors {
            let predecessors = predecessors_by_block
                .get_mut(successor)
                .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            if !predecessors.contains(source) {
                predecessors.push(*source);
            }
        }
    }
    let mut emitter = PlanEmitter {
        source_function: function,
        blocks_by_id: function.blocks.iter().map(|block| (block.id, block)).collect(),
        layout: &layout,
        layouts,
        function_indices,
        signature_indices,
        imports,
        import_indices,
        static_data_offsets,
        owner_cleanup_function,
        successors_by_block,
        predecessors_by_block,
        plan: Vec::new(),
        emitted: BTreeSet::new(),
        labels: Vec::new(),
        loops: Vec::new(),
        loop_exits: Vec::new(),
        aggregates: BTreeMap::new(),
        loop_headers: RefCell::new(BTreeMap::new()),
        loop_exit_targets: RefCell::new(BTreeMap::new()),
        back_edge_source_facts: RefCell::new(BTreeMap::new()),
        natural_loop_facts: RefCell::new(BTreeMap::new()),
        reaches_avoiding_facts: RefCell::new(BTreeMap::new()),
        immediate_dominators: std::cell::OnceCell::new(),
        block_slots: std::cell::OnceCell::new(),
        path_facts: RefCell::new(BTreeMap::new()),
    };
    emitter.emit_path(function.entry, None)?;
    if emitter.emitted.len() != function.blocks.len() {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    emitter.plan.push(PlanToken::End);
    Ok(emitter.plan)
}

fn expected_metadata_prefix(
    source: &CanonicalModule<InitialValid>,
    target: &WasmIr,
) -> Result<Vec<u8>, CanonicalAdmissionError> {
    let mut types = TypeSection::new();
    for signature in source.signatures() {
        let parameters = signature.parameters.iter().enumerate()
            .map(|(index, value_type)| wasm_value_type(FunctionId::new(u32::MAX), ValueId::new(index as u32), *value_type))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
        let results = signature.results.iter().enumerate()
            .map(|(index, value_type)| wasm_value_type(FunctionId::new(u32::MAX), ValueId::new(index as u32), *value_type))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
        types.ty().function(parameters, results);
    }
    let signature_indices: BTreeMap<_, _> = source.signatures().iter().enumerate()
        .map(|(index, signature)| (signature.id, index as u32)).collect();
    // Both maps are functions of the module. Built inside the loop they made
    // this quadratic in the module's imports and signatures: every signature
    // was cloned once per import, and each contract came from a linear scan.
    let signatures_by_id: BTreeMap<_, _> = source.signatures().iter()
        .map(|signature| (signature.id, signature.clone())).collect();
    let contracts_by_id: BTreeMap<_, _> = source.imports().iter()
        .map(|contract| (contract.id, contract)).collect();
    let mut import_type_indices = BTreeMap::new();
    for (index, import) in target.imports.iter().enumerate() {
        let contract = *contracts_by_id.get(&import.id)
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        let (parameters, results) = import_wasm_signature(FunctionId::new(u32::MAX), contract, &signatures_by_id)
            .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
        let type_index = u32::try_from(source.signatures().len() + index)
            .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
        types.ty().function(parameters, results);
        import_type_indices.insert(import.id, type_index);
    }
    owner_bridges::append_types(&mut types, &target.owner_bridges).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let owner_cleanup_bridge = owner_cleanup_bridge(&target.owner_bridges);
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
    let memory_helpers = outlined_memory::Helpers::new(source,
        (target.imports.len() + target.owner_bridges.len() + target.functions.len()) as u32 + u32::from(owner_cleanup_bridge.is_some()))
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let mut imports = ImportSection::new();
    for import in &target.imports {
        let type_index = import_type_indices.get(&import.id).copied()
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        // `contracts_by_id` above already indexes exactly this; scanning
        // here was the O(imports^2) half of the same lookup.
        let contract = *contracts_by_id.get(&import.id)
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        imports.import(&contract.module, &contract.name, EntityType::Function(type_index));
    }
    owner_bridges::append_imports(&mut imports, &target.owner_bridges, (source.signatures().len() + source.imports().len()) as u32);
    let mut functions = FunctionSection::new();
    let mut tables = TableSection::new();
    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    // The allocator's three globals are exported read-only for the host's
    // heap audit (a launcher may check that an admitted program released
    // every object); they grant no authority the module does not already
    // exercise on its own memory.
    exports.export(HEAP_TOP_EXPORT, ExportKind::Global, 0);
    exports.export(HEAP_FREE_EXPORT, ExportKind::Global, 1);
    exports.export(HEAP_BASE_EXPORT, ExportKind::Global, 2);
    let import_count = u32::try_from(target.imports.len() + target.owner_bridges.len()).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    for (index, function) in target.functions.iter().enumerate() {
        let type_index = signature_indices.get(&function.signature).copied()
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        functions.function(type_index);
        exports.export(&format!("f{}", function.id.get()), ExportKind::Func, import_count + index as u32);
    }
    if let Some(type_index) = owner_cleanup_type {
        functions.function(type_index);
    }
    functions.function(address_type);
    for _ in memory_helpers.drops.keys() { functions.function(drop_type); }
    functions.function(retain_type);
    if let Some(table) = &target.table {
        tables.table(TableType {
            element_type: RefType::FUNCREF,
            table64: false,
            minimum: table.functions.len() as u64,
            maximum: None,
            shared: false,
        });
    }
    let mut memory = MemorySection::new();
    memory.memory(MemoryType { minimum: initial_memory_pages(target.heap_start), maximum: None, memory64: false, shared: false, page_size_log2: None });
    let mut globals = GlobalSection::new();
    globals.global(GlobalType { val_type: ValType::I32, mutable: true, shared: false }, &ConstExpr::i32_const(target.heap_start as i32));
    globals.global(GlobalType { val_type: ValType::I32, mutable: true, shared: false }, &ConstExpr::i32_const(0));
    globals.global(GlobalType { val_type: ValType::I32, mutable: false, shared: false }, &ConstExpr::i32_const(target.heap_start as i32));
    let mut module = WasmModule::new();
    module.section(&types).section(&imports).section(&functions);
    if target.table.is_some() {
        module.section(&tables);
    }
    module.section(&memory).section(&globals).section(&exports);
    Ok(module.finish())
}

fn decode_and_match_plan(
    source: &CanonicalModule<InitialValid>,
    target: &WasmIr,
    bytes: &[u8],
) -> Result<TargetWasmWitness, CanonicalAdmissionError> {
    wasmparser::Validator::new_with_features(
        wasmparser::WasmFeatures::default() | wasmparser::WasmFeatures::TAIL_CALL,
    )
    .validate_all(bytes)
    .map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
    let metadata = expected_metadata_prefix(source, target)?;
    if bytes.len() < metadata.len() || bytes[..metadata.len()] != metadata[..] {
        return Err(CanonicalAdmissionError::PlanMismatch("module metadata"));
    }
    let (expected_static_data, expected_heap_start) = static_data_plan(source.static_data_segments())
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    if target.heap_start != expected_heap_start || target.static_data != expected_static_data {
        return Err(CanonicalAdmissionError::PlanMismatch("static-data lowering"));
    }
    let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&source.layouts().canonical_bytes())
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let import_count = u32::try_from(target.imports.len() + target.owner_bridges.len()).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let owner_cleanup_bridge = owner_cleanup_bridge(&target.owner_bridges);
    let owner_cleanup_function = owner_cleanup_bridge
        .map(|_| import_count + target.functions.len() as u32);
    let memory_helpers = outlined_memory::Helpers::new(source, import_count + target.functions.len() as u32 + u32::from(owner_cleanup_bridge.is_some()))
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let function_indices: BTreeMap<_, _> = target.functions.iter().enumerate()
        .map(|(index, function)| (function.id, import_count + index as u32))
        .collect();
    let signature_indices: BTreeMap<_, _> = source.signatures().iter().enumerate()
        .map(|(index, signature)| (signature.id, index as u32))
        .collect();
    let mut import_indices: BTreeMap<_, _> = target.imports.iter().enumerate()
        .map(|(index, import)| (import.id, index as u32)).collect();
    for (index, bridge) in target.owner_bridges.iter().enumerate() { import_indices.insert(bridge.id(), target.imports.len() as u32 + index as u32); }
    let static_data_offsets: BTreeMap<_, _> = target.static_data.iter()
        .map(|segment| (segment.id, segment.offset))
        .collect();
    // Both of these are functions of the module, not of one function in it.
    // Built inside the per-function closure they were quadratic: the import
    // map cloned every ImportContract (two Strings and two Vecs each) once per
    // function, and the signature came from a linear scan of every signature.
    let imports_by_id: BTreeMap<_, _> = source.imports().iter()
        .map(|import| (import.id, import.clone())).collect();
    let signatures_by_id: BTreeMap<_, _> = source.signatures().iter()
        .map(|signature| (signature.id, signature)).collect();
    let mut expected = source.functions().iter()
        .map(|function| {
            let signature = *signatures_by_id.get(&function.signature)
                .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            let local = local_layout(function, signature, &layouts).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            let locals = local.extra_types.iter().map(|ty| (1, planned_local_kind(*ty))).collect();
            Ok((locals, expected_function_plan(function, signature, &layouts, &function_indices, &signature_indices, &imports_by_id, &import_indices, &static_data_offsets, owner_cleanup_function, &target.owner_bridges, &memory_helpers)?))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(bridge) = owner_cleanup_bridge {
        expected.push(owner_operation_kernel::plan_cleanup_helper(&layouts, bridge)?);
    }
    expected.push(outlined_memory::plan_address());
    for root in memory_helpers.drops.keys() {
        expected.push(outlined_memory::plan_drop(&layouts, *root)?);
    }
    expected.push(outlined_memory::plan_retain());
    let mut actual = Vec::new();
    let mut code_count = None;
    let mut section_order = Vec::new();
    let mut code_started = false;
    let mut actual_data = Vec::new();
    let mut actual_table = None;
    let mut actual_elements = None;
    let mut actual_launch_contract = None;
    let mut actual_entry: Option<Vec<u8>> = None;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        match payload.map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))? {
            wasmparser::Payload::Version { .. } => {}
            wasmparser::Payload::TypeSection(_) => section_order.push(0),
            wasmparser::Payload::ImportSection(_) => section_order.push(1),
            wasmparser::Payload::FunctionSection(_) => section_order.push(2),
            wasmparser::Payload::TableSection(reader) => {
                if actual_table.is_some() {
                    return Err(CanonicalAdmissionError::PlanMismatch("duplicate table section"));
                }
                let tables = reader.into_iter().collect::<Result<Vec<_>, _>>().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                if tables.len() != 1 {
                    return Err(CanonicalAdmissionError::PlanMismatch("table count"));
                }
                let table = &tables[0];
                actual_table = Some((
                    table.ty.element_type == wasmparser::RefType::FUNCREF,
                    table.ty.table64,
                    table.ty.initial,
                    table.ty.maximum,
                    table.ty.shared,
                ));
                section_order.push(3);
            }
            wasmparser::Payload::MemorySection(_) => section_order.push(4),
            wasmparser::Payload::GlobalSection(_) => section_order.push(5),
            wasmparser::Payload::ExportSection(_) => section_order.push(6),
            wasmparser::Payload::ElementSection(reader) => {
                if actual_elements.is_some() {
                    return Err(CanonicalAdmissionError::PlanMismatch("duplicate element section"));
                }
                let elements = reader.into_iter().collect::<Result<Vec<_>, _>>().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                if elements.len() != 1 {
                    return Err(CanonicalAdmissionError::PlanMismatch("element count"));
                }
                let element = &elements[0];
                let wasmparser::ElementKind::Active { table_index, offset_expr } = &element.kind else {
                    return Err(CanonicalAdmissionError::PlanMismatch("element kind"));
                };
                if *table_index != None {
                    return Err(CanonicalAdmissionError::PlanMismatch("element table"));
                }
                let mut offset = offset_expr.get_operators_reader();
                if !matches!(offset.read().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?, wasmparser::Operator::I32Const { value: 0 })
                    || !matches!(offset.read().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?, wasmparser::Operator::End)
                {
                    return Err(CanonicalAdmissionError::PlanMismatch("element offset"));
                }
                let wasmparser::ElementItems::Functions(reader) = &element.items else {
                    return Err(CanonicalAdmissionError::PlanMismatch("element expressions"));
                };
                actual_elements = Some(reader.clone().into_iter().collect::<Result<Vec<_>, _>>().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?);
                section_order.push(7);
            }
            wasmparser::Payload::CodeSectionStart { count, .. } => {
                if code_count.replace(count).is_some() {
                    return Err(CanonicalAdmissionError::PlanMismatch("duplicate code section"));
                }
                section_order.push(8);
                code_started = true;
            }
            wasmparser::Payload::CodeSectionEntry(body) => {
                if !code_started {
                    return Err(CanonicalAdmissionError::PlanMismatch("code body before section"));
                }
                let mut locals_reader = body.get_locals_reader().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                let mut locals = Vec::new();
                for _ in 0..locals_reader.get_count() {
                    let (count, ty) = locals_reader.read().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                    locals.push((count, decoded_local_kind(ty)));
                }
                let mut operators = body.get_operators_reader().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                let mut plan = Vec::new();
                while !operators.eof() {
                    plan.push(decode_plan_token(operators.read().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?)?);
                }
                actual.push((locals, plan));
            }
            wasmparser::Payload::DataSection(reader) => {
                section_order.push(9);
                for data in reader {
                    let data = data.map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                    let wasmparser::DataKind::Active { memory_index, offset_expr } = data.kind else {
                        return Err(CanonicalAdmissionError::PlanMismatch("passive data segment"));
                    };
                    if memory_index != 0 {
                        return Err(CanonicalAdmissionError::PlanMismatch("nonzero data memory"));
                    }
                    let mut operators = offset_expr.get_operators_reader();
                    let wasmparser::Operator::I32Const { value } = operators
                        .read()
                        .map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?
                    else {
                        return Err(CanonicalAdmissionError::PlanMismatch("dynamic data offset"));
                    };
                    let offset = u32::try_from(value)
                        .map_err(|_| CanonicalAdmissionError::PlanMismatch("negative data offset"))?;
                    actual_data.push((offset, data.data.to_vec()));
                }
            }
            wasmparser::Payload::CustomSection(section) => match section.name() {
                "witchy.launch" if actual_launch_contract.replace(section.data().to_vec()).is_none() => {
                    section_order.push(10);
                }
                ENTRY_SECTION if actual_entry.replace(section.data().to_vec()).is_none() => {
                    section_order.push(11);
                }
                _ => return Err(CanonicalAdmissionError::PlanMismatch("launch contract section")),
            },
            wasmparser::Payload::End(_) => {}
            _ => return Err(CanonicalAdmissionError::PlanMismatch("unexpected Wasm section")),
        }
    }
    let expected_count = u32::try_from(expected.len()).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    // The canonical encoder deliberately emits every fixed metadata section,
    // including an empty import section. Match that exact byte policy rather
    // than inferring section presence from the logical import count.
    let mut expected_sections = vec![0, 1, 2];
    if target.table.is_some() {
        expected_sections.push(3);
    }
    expected_sections.extend([4, 5, 6]);
    if target.table.is_some() {
        expected_sections.push(7);
    }
    expected_sections.push(8);
    if !target.static_data.is_empty() {
        expected_sections.push(9);
    }
    expected_sections.push(10);
    if target.entry_payload.is_some() {
        expected_sections.push(11);
    }
    let expected_data = expected_static_data.iter().map(|segment| (segment.offset, segment.bytes.clone())).collect::<Vec<_>>();
    if section_order != expected_sections {
        return Err(CanonicalAdmissionError::SectionOrderMismatch {
            expected: expected_sections,
            actual: section_order,
        });
    }
    if code_count != Some(expected_count) {
        return Err(CanonicalAdmissionError::PlanMismatch("function count"));
    }
    if actual != expected {
        // Development diagnostic only: name the first token where the emitted
        // body and the independently derived plan diverge. Nothing is admitted
        // on this path; the refusal below is unchanged.
        if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
            for (index, (emitted, planned)) in actual.iter().zip(&expected).enumerate() {
                if emitted == planned {
                    continue;
                }
                if emitted.0 != planned.0 {
                    eprintln!("canonical plan mismatch: function {index} locals differ");
                    eprintln!("  actual:   {:?}", emitted.0);
                    eprintln!("  expected: {:?}", planned.0);
                }
                let divergence = emitted.1.iter().zip(&planned.1).position(|(a, e)| a != e).unwrap_or(emitted.1.len().min(planned.1.len()));
                let window = |tokens: &[PlanToken]| tokens.iter().skip(divergence.saturating_sub(6)).take(14).map(|t| format!("{t:?}")).collect::<Vec<_>>().join(" ");
                eprintln!("canonical plan mismatch: function {index} token {divergence} (actual {} / expected {})", emitted.1.len(), planned.1.len());
                eprintln!("  actual:   {}", window(emitted.1.as_slice()));
                eprintln!("  expected: {}", window(planned.1.as_slice()));
                break;
            }
            if actual.len() != expected.len() {
                eprintln!("canonical plan mismatch: {} functions emitted, {} planned", actual.len(), expected.len());
            }
        }
        return Err(CanonicalAdmissionError::PlanMismatch("function body"));
    }
    if actual_data != expected_data {
        return Err(CanonicalAdmissionError::PlanMismatch("data contents"));
    }
    if actual_launch_contract.as_deref() != Some(target.launch_contract_payload.as_slice()) {
        return Err(CanonicalAdmissionError::PlanMismatch("launch contract payload"));
    }
    if actual_entry != target.entry_payload {
        return Err(CanonicalAdmissionError::PlanMismatch("entry section payload"));
    }
    match &target.table {
        Some(table) => {
            let expected_table = (true, false, table.functions.len() as u64, None, false);
            if actual_table != Some(expected_table) {
                return Err(CanonicalAdmissionError::PlanMismatch("table definition"));
            }
            let expected_elements = table.functions.iter().map(|function| function_indices.get(function).copied().ok_or(CanonicalAdmissionError::UnsupportedPlan)).collect::<Result<Vec<_>, _>>()?;
            if actual_elements != Some(expected_elements) {
                return Err(CanonicalAdmissionError::PlanMismatch("table elements"));
            }
        }
        None => {
            if actual_table.is_some() || actual_elements.is_some() {
                return Err(CanonicalAdmissionError::PlanMismatch("unexpected table"));
            }
        }
    }
    Ok(TargetWasmWitness::from_decoded(
        bytes,
        &section_order,
        &actual,
        &actual_data,
    ))
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
#[cfg(not(test))]
fn derive_entry(
    validated: &ValidatedSafeWir,
    source: &witchy_types::pipeline::CanonicalCheckedSource,
) -> Result<Option<CanonicalEntry>, CanonicalAdmissionError> {
    use crate::canonical_safewir::{CapabilityKindId, ValueType};
    use witchy_types::canonical_abi::CanonicalTargetPolicy;

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

#[cfg(not(test))]
pub fn admit_canonical(
    validated: ValidatedSafeWir,
    source: &witchy_types::pipeline::CanonicalCheckedSource,
) -> Result<AdmittedCanonicalArtifact, CanonicalAdmissionError> {
    admit_canonical_with_observer(validated, source, |_, _| ())
        .map(|(artifact, ())| artifact)
}

/// Observe the original immutable validated graph only after full production
/// admission succeeds. The graph is neither cloned nor retained by the artifact.
#[cfg(not(test))]
pub fn admit_canonical_with_observer<R>(
    validated: ValidatedSafeWir,
    source: &witchy_types::pipeline::CanonicalCheckedSource,
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

#[cfg(test)]
pub fn admit_canonical(
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

#[cfg(test)]
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
#[cfg(test)]
mod tests {
    mod unique_pop;
    mod boolean_reaches;
    /// Every entry capability tag survives the custom section, so a
    /// launcher reading a `.wasm` back mints exactly the tokens the checked
    /// signature declared.
    #[test]
    fn entry_capability_tags_round_trip() {
        let entry = super::CanonicalEntry {
            export: "f0".to_string(),
            argument_positions: Vec::new(),
            capabilities: vec![
                super::CanonicalEntryCapability::Console,
                super::CanonicalEntryCapability::Dir,
                super::CanonicalEntryCapability::Env,
                super::CanonicalEntryCapability::Net,
            ],
            result: super::CanonicalEntryResult::Int,
        };
        let encoded = entry.encode();
        assert_eq!(encoded[encoded.len() - 5..encoded.len() - 1], [0, 1, 2, 4]);
        assert_eq!(super::CanonicalEntry::decode(&encoded), Ok(entry));
    }

    #[test]
    fn registry_hosts_entry_tags_preserve_separate_roots() {
        use super::{CanonicalEntry, CanonicalEntryCapability as C, CanonicalEntryResult};
        let entry = CanonicalEntry { export: "f0".into(),
            capabilities: vec![C::Clock, C::SecretStore, C::Secret, C::Dir],
            argument_positions: Vec::new(), result: CanonicalEntryResult::Unit };
        let bytes = entry.encode();
        assert_eq!(&bytes[bytes.len() - 5..bytes.len() - 1], &[19, 20, 21, 1]);
        assert_eq!(CanonicalEntry::decode(&bytes), Ok(entry));
    }

    #[test]
    fn entry_argument_positions_round_trip_and_refuse_malformed_order() {
        let entry = super::CanonicalEntry {
            export: "f0".into(),
            capabilities: vec![super::CanonicalEntryCapability::Console],
            argument_positions: vec![0, 2],
            result: super::CanonicalEntryResult::Unit,
        };
        let bytes = entry.encode();
        assert_eq!(bytes[0], 2);
        assert_eq!(super::CanonicalEntry::decode(&bytes), Ok(entry));
        let last = bytes.len() - 1;
        for bad_position in [0, 3, u8::MAX] {
            let mut bad = bytes.clone();
            bad[last] = bad_position;
            assert!(super::CanonicalEntry::decode(&bad).is_err());
        }
        assert!(super::CanonicalEntry::decode(&bytes[..last]).is_err());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(super::CanonicalEntry::decode(&trailing).is_err());
    }

    use super::*;
    use crate::canonical_safewir::{
        BasicBlock, BlockId, BlockParameter, CallableContract, CheckedSourceIdentity,
        FunctionSignature, FunctionSignatureId, GcTypeId,
        GuardKind,
        LoweredCandidate, LoweredModuleParts, OverflowBehavior, ParameterContract,
        ParameterOwnership, Revision, ResultContract, check_initial_with_test_witness,
        validate_final_for_test,
    };

    #[test]
    fn external_unit_signature_has_zero_physical_wasm_lanes() {
        let signature = FunctionSignature {
            id: FunctionSignatureId::new(91),
            parameters: vec![ValueType::Unit],
            results: vec![ValueType::Unit],
            contract: CallableContract {
                parameters: vec![ParameterContract {
                    value_type: ValueType::Unit,
                    ownership: ParameterOwnership::Value,
                }],
                results: vec![ResultContract::Value(ValueType::Unit)],
                parameter_effects: BTreeMap::new(),
            },
        };
        assert_eq!(
            super::external_import_wasm_signature(FunctionId::new(91), &signature),
            Ok((Vec::new(), Vec::new())),
        );
    }
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

    #[test]
    fn scalar_lowering_has_one_mapping_per_operation_and_terminator() {
        let source = checked_scalar();
        let target = lower_validated(&source).unwrap();
        let receipt = check_correspondence(source.module(), &target).unwrap();
        assert_eq!(receipt.mappings.len(), 2);
    }

    #[test]
    fn zero_pass_scalar_module_encodes_and_validates_real_wasm() {
        let wasm = encode_validated(checked_scalar()).expect("validated scalar authority encodes");
        wasmparser::validate(&wasm).expect("encoded scalar module is structurally valid");
        assert!(wasm.windows(2).any(|window| window == [0x42, 9]));
    }

    #[test]
    fn int_subtraction_matches_the_exact_encoded_plan() {
        let artifact = admit_canonical(checked_int_subtraction())
            .expect("checked subtraction admits through exact-byte matching");
        wasmparser::validate(artifact.wasm()).expect("subtraction artifact is valid Wasm");
    }

    #[test]
    fn a3_zero_pass_candidate_admits_with_all_witnesses() {
        let validated = checked_scalar();
        let artifact = admit_canonical(validated).expect("zero-pass A3 witness chain admits");
        wasmparser::validate(artifact.wasm()).expect("admitted bytes remain valid Wasm");
    }

    #[test]
    fn a3_target_witness_cannot_substitute_for_other_bytes() {
        let validated = checked_scalar();
        let target = lower_validated(&validated).unwrap();
        let bytes = encode_validated(checked_scalar()).expect("validated scalar encodes");
        let witness = decode_and_match_plan(validated.module(), &target, &bytes)
            .expect("exact target witness");
        let mut substituted = bytes;
        substituted.push(0);

        assert!(!witness.matches_bytes(&substituted));
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

    #[test]
    fn owner_gc_scratch_uses_exact_externref_type() {
        let interner = LayoutInterner::new();
        let owner = ValueId::new(1);
        let signature = scalar_signature_with(FunctionSignatureId::new(991), Vec::new(), Vec::new());
        let function = Function {
            id: FunctionId::new(991),
            signature: signature.id,
            entry: BlockId::new(991),
            value_types: [(owner, ValueType::Gc(GcTypeId::new(991)))].into_iter().collect(),
            blocks: vec![BasicBlock {
                id: BlockId::new(991),
                parameters: Vec::new(),
                operations: Vec::new(),
                terminator: Terminator::Return { values: Vec::new() },
            }],
        };
        let layout = local_layout(&function, &signature, &interner).expect("Gc local layout");
        let scratch = layout.owner_scratch.expect("synthetic Gc scratch");
        assert_eq!(layout.extra_types[scratch[0] as usize], ValType::I32);
        assert_eq!(layout.extra_types[scratch[1] as usize], ValType::I32);
        assert_eq!(layout.extra_types[scratch[2] as usize], ValType::Ref(RefType::EXTERNREF));
        assert_eq!(layout.extra_types[scratch[3] as usize], ValType::I32);
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
        let artifact = admit_canonical(module).expect("validated int addition admits");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())?;
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
        let run = instance.get_typed_func::<(), i64>(&mut store, "f71")?;
        run.call(&mut store, ())
    }

    #[cfg(feature = "native")]
    #[test]
    fn trapping_int_add_accepts_signed_boundaries() {
        for (left, right, expected) in [
            (i64::MAX - 1, 1, i64::MAX),
            (i64::MIN + 1, -1, i64::MIN),
            (-20, 62, 42),
        ] {
            let validated = checked_int_add(left, right);
            assert_eq!(
                crate::canonical_interpreter::interpret(
                    &validated,
                    FunctionId::new(71),
                    Vec::new(),
                ),
                Ok(vec![crate::canonical_interpreter::InterpreterValue::Int(
                    expected,
                )]),
            );
            assert_eq!(run_int_add(validated).unwrap(), expected);
        }
    }

    #[cfg(feature = "native")]
    #[test]
    fn trapping_int_add_overflow_matches_interpreter() {
        for (left, right) in [(i64::MAX, 1), (i64::MIN, -1)] {
            let validated = checked_int_add(left, right);
            assert_eq!(
                crate::canonical_interpreter::interpret(
                    &validated,
                    FunctionId::new(71),
                    Vec::new(),
                ),
                Err(crate::canonical_interpreter::InterpretError::ArithmeticTrap),
            );
            assert!(run_int_add(validated).is_err());
        }
    }

    #[cfg(feature = "native")]
    #[test]
    fn trapping_size_add_accepts_the_unsigned_boundary() {
        assert_eq!(
            run_size_add(checked_size_add(
                u32::MAX - 1,
                1,
                crate::canonical_safewir::OverflowBehavior::Trap,
            ))
            .unwrap(),
            u32::MAX,
        );
    }

    #[cfg(feature = "native")]
    #[test]
    fn size_add_overflow_traps_or_wraps_exactly_as_declared() {
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
    #[test]
    fn encoded_structured_branch_executes_both_arms() {
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
    #[test]
    fn encoded_structured_loop_executes_carried_arguments() {
        let artifact = admit_canonical(checked_loop()).expect("structured loop artifact admits");
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

    #[test]
    fn structured_loop_plan_rejects_control_opcode_mutation() {
        let mut bytes = encode_validated(checked_loop()).expect("structured loop encodes");
        let block = bytes
            .windows(2)
            .position(|window| window == [0x02, 0x40])
            .expect("structured loop has an outer empty block");
        bytes[block] = 0x03;
        assert!(matches!(
            admit_canonical_with_bytes_for_test(checked_loop(), bytes),
            Err(CanonicalAdmissionError::PlanMismatch(_) | CanonicalAdmissionError::WasmValidation(_))
        ));
    }

    #[cfg(feature = "native")]
    #[test]
    fn admitted_int_list_loop_executes_with_bounds_checks() {
        let artifact = admit_canonical(checked_int_list_loop()).expect("validated List(Int) loop admits");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let function = instance.get_typed_func::<(), i64>(&mut store, "f37").unwrap();
        assert_eq!(function.call(&mut store, ()).unwrap(), 42);
    }

    #[cfg(feature = "native")]
    #[test]
    fn encoded_trap_terminator_remains_terminal() {
        let wasm = encode_validated(checked_trap()).expect("trap terminator encodes");
        wasmparser::validate(&wasm).expect("trap module is valid Wasm");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let function = instance.get_typed_func::<(), ()>(&mut store, "f9").unwrap();
        assert!(function.call(&mut store, ()).is_err());
    }

    #[test]
    fn irreducible_cfg_is_refused_without_dispatcher_lowering() {
        assert!(matches!(
            encode_validated(checked_irreducible()),
            Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow { .. }))
        ));
    }

    #[test]
    fn common_reachable_join_with_non_terminating_bypass_is_refused() {
        assert!(matches!(
            encode_validated(checked_join_bypass()),
            Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow { .. }))
        ));
    }

    #[test]
    fn structured_diamond_chain_admits_without_enumerating_paths() {
        admit_canonical(checked_diamond_chain(96, false)).expect("bounded diamond graph admits");
    }

    #[test]
    fn structured_diamond_chain_with_cyclic_suffix_refuses_fail_closed() {
        assert!(matches!(
            encode_validated(checked_diamond_chain(96, true)),
            Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow { .. }))
        ));
    }

    #[test]
    fn cached_natural_loop_members_match_forward_predicate_for_multi_exit_loop() {
        use crate::canonical_safewir::BlockId;

        let header = BlockId::new(1);
        let body = BlockId::new(2);
        let back_edge = BlockId::new(3);
        let first_exit = BlockId::new(4);
        let second_exit = BlockId::new(5);
        let join = BlockId::new(6);
        let outside = BlockId::new(7);
        let successors = BTreeMap::from([
            (header, vec![body, first_exit]),
            (body, vec![back_edge, second_exit]),
            (back_edge, vec![header]),
            (first_exit, vec![join]),
            (second_exit, vec![join]),
            (join, Vec::new()),
            (outside, vec![join]),
        ]);
        let mut predecessors = successors
            .keys()
            .copied()
            .map(|block| (block, Vec::new()))
            .collect::<BTreeMap<_, _>>();
        for (source, targets) in &successors {
            for target in targets {
                let sources = predecessors.get_mut(target).unwrap();
                if !sources.contains(source) {
                    sources.push(*source);
                }
            }
        }

        let cached = reverse_loop_members(header, vec![back_edge], &predecessors);
        let old_predicate = |start| {
            let mut seen = BTreeSet::new();
            let mut pending = VecDeque::from([start]);
            while let Some(block) = pending.pop_front() {
                if block == header || !seen.insert(block) {
                    continue;
                }
                if block == back_edge {
                    return true;
                }
                pending.extend(successors.get(&block).unwrap().iter().copied());
            }
            false
        };

        for block in [header, body, back_edge, first_exit, second_exit, join, outside] {
            assert_eq!(
                block != header && cached.contains(&block),
                old_predicate(block),
                "natural-loop membership changed for block {}",
                block.get(),
            );
        }
        assert!(!cached.contains(&header));
        assert!(cached.contains(&back_edge));
        assert!(!cached.contains(&outside));
        assert!(!cached.contains(&first_exit));
        assert!(!cached.contains(&second_exit));
    }

    #[test]
    fn variable_call_is_refused_fail_closed() {
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
    #[test]
    fn admitted_scalar_var_call_uses_authenticated_table_and_executes() {
        let artifact = admit_canonical(checked_var_call()).expect("scalar indirect call artifact admits");
        wasmparser::validate(&artifact.wasm()).expect("scalar indirect call is structurally valid");
        assert!(artifact.wasm().contains(&0x11), "encoded artifact contains call_indirect");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let function = instance.get_typed_func::<(), i64>(&mut store, "f20").unwrap();
        assert_eq!(function.call(&mut store, ()).unwrap(), 42);
    }

    #[cfg(feature = "native")]
    #[test]
    fn admitted_scalar_var_tail_call_executes_with_return_call_indirect() {
        let artifact = admit_canonical(checked_var_tail_call())
            .expect("scalar indirect proper-tail call artifact admits");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let function = instance.get_typed_func::<(), i64>(&mut store, "f22").unwrap();
        assert_eq!(function.call(&mut store, ()).unwrap(), 42);
    }

    #[test]
    fn mutated_scalar_var_call_signature_fails_exact_admission() {
        let mut bytes = encode_validated(checked_var_call()).expect("scalar indirect call encodes");
        let call = bytes.iter().position(|byte| *byte == 0x11).expect("call_indirect opcode exists");
        bytes[call + 1] = 0;
        assert!(matches!(
            admit_canonical_with_bytes_for_test(checked_var_call(), bytes),
            Err(CanonicalAdmissionError::PlanMismatch("function body"))
                | Err(CanonicalAdmissionError::WasmValidation(_))
        ));
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

    #[test]
    fn admitted_capability_var_call_preserves_kind_and_plan() {
        for value_type in indirect_capability_types() {
            admit_canonical(finalize(capability_indirect_candidate(false, value_type)))
                .expect("checked capability indirect call matches the independent byte plan");
        }
    }

    #[test]
    fn admitted_capability_var_tail_call_preserves_proper_tail_and_plan() {
        for value_type in indirect_capability_types() {
            admit_canonical(finalize(capability_indirect_candidate(true, value_type)))
                .expect("checked capability proper tail matches the independent byte plan");
        }
    }

    #[cfg(feature = "native")]
    #[test]
    fn admitted_capability_indirect_calls_preserve_externrefs() {
        for tail in [false, true] {
            for value_type in indirect_capability_types() {
                let artifact = admit_canonical(finalize(capability_indirect_candidate(tail, value_type)))
                    .expect("capability identity call admits");
                let engine = wasmtime::Engine::default();
                let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
                let mut store = wasmtime::Store::new(&engine, ());
                let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
                let function = instance.get_typed_func::<
                    Option<wasmtime::Rooted<wasmtime::ExternRef>>,
                    Option<wasmtime::Rooted<wasmtime::ExternRef>>,
                >(&mut store, "f20").unwrap();
                // This is a transport sentinel, not a host-minted authority:
                // the fixture performs no host calls or capability effects.
                let token = wasmtime::ExternRef::new(&mut store, 73_u32).unwrap();
                let returned = function.call(&mut store, Some(token)).unwrap().unwrap();
                assert_eq!(returned.data(&store).unwrap().unwrap().downcast_ref::<u32>(), Some(&73));
                if matches!(value_type, ValueType::NullableCapability(_)) {
                    assert!(function.call(&mut store, None).unwrap().is_none());
                }
            }
        }
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

    #[test]
    fn capability_indirect_calls_reject_wrong_kind_and_signature() {
        for tail in [false, true] {
            for value_type in indirect_capability_types() {
                for (value, wrong_type) in [
                    (ValueId::new(0), ValueType::Capability(crate::canonical_safewir::CapabilityKindId::new(0))),
                    (ValueId::new(0), ValueType::Int),
                    (ValueId::new(1), ValueType::Callable(FunctionSignatureId::new(20))),
                ] {
                    let mut parts = capability_indirect_parts(tail, value_type);
                    parts.functions[0].value_types.insert(value, wrong_type);
                    assert!(lower_capability_indirect_for_test(&parts).is_err());
                    assert!(check_initial_with_test_witness(CanonicalModule::from_lowering(parts)).is_err());
                }
                let mut parts = capability_indirect_parts(tail, value_type);
                if tail {
                    // A proper tail cannot change the caller's result kind.
                    parts.signatures[0].results[0] = ValueType::Int;
                    parts.signatures[0].contract.results[0] = ResultContract::Value(ValueType::Int);
                } else {
                    parts.functions[0].value_types.insert(ValueId::new(2), ValueType::Int);
                    assert!(lower_capability_indirect_for_test(&parts).is_err());
                }
                assert!(check_initial_with_test_witness(CanonicalModule::from_lowering(parts)).is_err());
            }
        }
    }

    #[test]
    fn capability_indirect_parameters_require_value_without_memory_effects() {
        for tail in [false, true] {
            for value_type in indirect_capability_types() {
                let mut parts = capability_indirect_parts(tail, value_type);
                assert!(lower_capability_indirect_for_test(&parts).is_ok());
                parts.signatures[1].contract.parameter_effects.insert(0, Vec::new());
                assert!(lower_capability_indirect_for_test(&parts).is_ok());
                for ownership in [ParameterOwnership::Unique, ParameterOwnership::Shared,
                    ParameterOwnership::WriteBack, ParameterOwnership::UniqueWriteBack]
                {
                    parts.signatures[1].contract.parameters[0].ownership = ownership;
                    assert!(lower_capability_indirect_for_test(&parts).is_err());
                }
                parts.signatures[1].contract.parameters[0].ownership = ParameterOwnership::Value;
                for effect in [MemoryEffect::Read, MemoryEffect::Retain, MemoryEffect::Release] {
                    parts.signatures[1].contract.parameter_effects.insert(0, vec![effect]);
                    assert!(lower_capability_indirect_for_test(&parts).is_err());
                }
                parts.signatures[1].contract.parameter_effects.clear();
                parts.signatures[1].contract.parameters[0].value_type = ValueType::Int;
                assert!(lower_capability_indirect_for_test(&parts).is_err());
                parts.signatures[1].contract.parameters.clear();
                assert!(lower_capability_indirect_for_test(&parts).is_err());
            }
        }
    }

    #[test]
    fn indirect_callable_writeback_requires_the_exact_hidden_lane() {
        let layout = crate::layout::LayoutId::from_bytes([0x67; 32]);
        let signature = FunctionSignature {
            id: FunctionSignatureId::new(67),
            parameters: vec![ValueType::Ref(layout), ValueType::Ref(layout)],
            results: vec![ValueType::Ref(layout), ValueType::Ref(layout)],
            contract: CallableContract {
                parameters: vec![
                    ParameterContract { value_type: ValueType::Ref(layout), ownership: ParameterOwnership::Shared },
                    ParameterContract { value_type: ValueType::Ref(layout), ownership: ParameterOwnership::WriteBack },
                ],
                results: vec![
                    ResultContract::Shared { layout },
                    ResultContract::Shared { layout },
                ],
                parameter_effects: BTreeMap::from([
                    (0, vec![MemoryEffect::Read, MemoryEffect::Retain, MemoryEffect::Release]),
                    (1, vec![MemoryEffect::Read, MemoryEffect::Retain, MemoryEffect::Release, MemoryEffect::Write]),
                ]),
            },
        };
        assert!(is_authenticated_indirect_callable_writeback(&signature));

        let mut absent = signature.clone();
        absent.results.pop();
        absent.contract.results.pop();
        assert!(!is_authenticated_indirect_callable_writeback(&absent));

        let mut mismatched = signature.clone();
        mismatched.contract.parameter_effects.get_mut(&1).unwrap().pop();
        assert!(!is_authenticated_indirect_callable_writeback(&mismatched));

        let mut widened = signature;
        widened.parameters.push(ValueType::Ref(layout));
        widened.contract.parameters.push(ParameterContract {
            value_type: ValueType::Ref(layout), ownership: ParameterOwnership::Shared,
        });
        assert!(!is_authenticated_indirect_callable_writeback(&widened));
    }

    #[test]
    fn unique_reference_indirect_parameter_accepts_only_non_mutating_effects() {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::U32).unwrap();
        let layout = interner.intern_packed_list(element, RcHeader::Required).unwrap();
        let value_type = ValueType::Ref(layout);
        let mut signature = FunctionSignature {
            id: FunctionSignatureId::new(22),
            parameters: vec![value_type],
            results: vec![value_type],
            contract: CallableContract {
                parameters: vec![ParameterContract {
                    value_type,
                    ownership: ParameterOwnership::Unique,
                }],
                results: vec![ResultContract::Unique { layout }],
                parameter_effects: BTreeMap::new(),
            },
        };

        assert!(is_indirect_parameter(&signature, 0, value_type));
        for effect in [MemoryEffect::Read, MemoryEffect::Retain, MemoryEffect::Release] {
            signature.contract.parameter_effects.insert(0, vec![effect]);
            assert!(is_indirect_parameter(&signature, 0, value_type));
        }
        for effect in [MemoryEffect::Write, MemoryEffect::Reclaim] {
            signature.contract.parameter_effects.insert(0, vec![effect]);
            assert!(!is_indirect_parameter(&signature, 0, value_type));
        }
        signature.contract.parameters[0].ownership = ParameterOwnership::Value;
        signature.contract.parameter_effects.clear();
        assert!(!is_indirect_parameter(&signature, 0, value_type));
    }

    #[test]
    fn capability_indirect_tail_cannot_drop_required_site() {
        let mut parts = capability_indirect_parts(true, indirect_capability_types()[0]);
        parts.functions[0].blocks[0].terminator = Terminator::Return {
            values: vec![ValueId::new(0)],
        };
        assert!(check_initial_with_test_witness(CanonicalModule::from_lowering(parts)).is_err());
    }

    #[test]
    fn capability_indirect_contract_forgery_fails_initial_admission() {
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

    #[cfg(feature = "native")]
    #[test]
    fn direct_call_encodes_as_wasm_call() {
        let artifact = admit_canonical(checked_direct_call(false)).expect("direct call artifact admits");
        let wasm = artifact.wasm();
        wasmparser::validate(&wasm).expect("direct call module is structurally valid");
        assert!(wasm.windows(2).any(|window| window == [0x10, 0x01]));
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let function = instance.get_typed_func::<(), i64>(&mut store, "f1").unwrap();
        assert_eq!(function.call(&mut store, ()).unwrap(), 17);
    }

    #[cfg(feature = "native")]
    #[test]
    fn proper_tail_call_encodes_as_wasm_return_call() {
        let artifact = admit_canonical(checked_direct_call(true)).expect("proper tail call artifact admits");
        let wasm = artifact.wasm();
        wasmparser::Validator::new_with_features(
            wasmparser::WasmFeatures::default() | wasmparser::WasmFeatures::TAIL_CALL,
        )
        .validate_all(wasm)
        .expect("tail-call module is structurally valid");
        assert!(wasm.windows(2).any(|window| window == [0x12, 0x01]));
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let function = instance.get_typed_func::<(), i64>(&mut store, "f1").unwrap();
        assert_eq!(function.call(&mut store, ()).unwrap(), 17);
    }

    #[test]
    fn mutated_direct_call_callee_index_fails_admission() {
        let mut bytes = encode_validated(checked_direct_call(false)).unwrap();
        let call = bytes
            .windows(2)
            .position(|window| window == [0x10, 0x01])
            .expect("direct call opcode and index exist");
        bytes[call + 1] = 0;
        assert!(matches!(
            admit_canonical_with_bytes_for_test(checked_direct_call(false), bytes),
            Err(CanonicalAdmissionError::PlanMismatch(_))
        ));
    }

    #[cfg(feature = "native")]
    #[test]
    fn admitted_owned_reference_call_executes_and_preserves_ownership() {
        let artifact = admit_canonical(checked_object_call_move()).expect("owned reference call artifact admits");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let function = instance.get_typed_func::<(), ()>(&mut store, "f40").unwrap();
        function.call(&mut store, ()).unwrap();
    }

    #[cfg(feature = "native")]
    #[test]
    fn parameterized_scalar_call_artifact_executes() {
        let artifact = admit_canonical(checked_parameterized_scalar_call())
            .expect("parameterized scalar call artifact admits");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let function = instance
            .get_typed_func::<(), i64>(&mut store, "f51")
            .unwrap();
        assert_eq!(function.call(&mut store, ()).unwrap(), 42);
    }

    #[test]
    fn parameterized_call_signature_mutation_fails_exact_admission() {
        let mut bytes = encode_validated(checked_parameterized_scalar_call()).unwrap();
        let signature = bytes
            .windows(5)
            .position(|window| window == [0x60, 0x01, 0x7e, 0x01, 0x7e])
            .expect("increment signature exists");
        bytes[signature + 2] = 0x7f;
        assert!(matches!(
            admit_canonical_with_bytes_for_test(checked_parameterized_scalar_call(), bytes),
            Err(CanonicalAdmissionError::PlanMismatch(_))
                | Err(CanonicalAdmissionError::WasmValidation(_))
        ));
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

    #[test]
    fn comparison_encodes_and_validates() {
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

    #[test]
    fn narrow_size_negative_traps_in_wasmtime() {
        let input = ValueId::new(1);
        let result = ValueId::new(2);
        let validated = checked_size_operation(Operation::NarrowSize { destination: result, value: input }, input, result);
        let wasm = encode_validated(validated).expect("negative narrow encodes with a trap");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let result = instance.get_typed_func::<(), i32>(&mut store, "f4").unwrap().call(&mut store, ());
        assert!(result.is_err());
    }

    #[test]
    fn narrow_size_u32_overflow_traps_in_wasmtime() {
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

    #[test]
    fn narrow_size_valid_value_executes() {
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

    #[cfg(feature = "native")]
    #[test]
    fn outlined_memory_bounds_body_size_and_executes_shared_nullable_drops() {
        let checked = checked_outlined_memory_fixture();
        let oracle = crate::canonical_interpreter::interpret_program(
            &checked, FunctionId::new(141), Vec::new(), Default::default(),
        ).expect("independent oracle executes the checked operations");
        assert_eq!(oracle.results, vec![crate::canonical_interpreter::InterpreterValue::Size(7)]);
        assert_eq!(oracle.leaked_objects, 0);
        let artifact = admit_canonical(checked).expect("wide repeated memory operations admit");
        let mut sizes = Vec::new();
        let mut calls = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(artifact.wasm()) {
            if let wasmparser::Payload::CodeSectionEntry(body) = payload.unwrap() {
                if sizes.is_empty() {
                    let mut operators = body.get_operators_reader().unwrap();
                    while !operators.eof() {
                        if let wasmparser::Operator::Call { function_index } = operators.read().unwrap() {
                            calls.push(function_index);
                        }
                    }
                }
                sizes.push(body.range().len());
            }
        }
        assert_eq!(sizes.len(), 4, "one source body and address, drop, retain helpers");
        assert!(sizes[0] < 64 * (128 * 16 + 1024), "repeated kernels expanded into the source body: {sizes:?}");
        assert_eq!(calls.iter().filter(|index| **index == 1).count(), 65 * 128);
        assert_eq!(calls.iter().filter(|index| **index == 2).count(), 64 * 2 + 1);
        assert_eq!(calls.iter().filter(|index| **index == 3).count(), 64);
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        assert_eq!(instance.get_typed_func::<(), i32>(&mut store, "f141").unwrap().call(&mut store, ()).unwrap(), 7);
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        let mut free = instance.get_global(&mut store, HEAP_FREE_EXPORT).unwrap().get(&mut store).i32().unwrap() as usize;
        let mut seen = BTreeSet::new();
        while free != 0 {
            assert!(seen.insert(free), "free-list cycle or double release");
            free = u32::from_le_bytes(memory.data(&store)[free..free + 4].try_into().unwrap()) as usize;
        }
        assert_eq!(seen.len(), 64, "every allocated container must be reclaimed exactly once");
    }

    #[test]
    fn outlined_address_predicate_mutation_fails_independent_admission() {
        let mut bytes = encode_validated(checked_outlined_memory_fixture()).unwrap();
        let mut body_index = 0;
        let mut comparison = None;
        for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
            if let wasmparser::Payload::CodeSectionEntry(body) = payload.unwrap() {
                if body_index == 1 {
                    let mut reader = body.get_operators_reader().unwrap();
                    while !reader.eof() {
                        let offset = reader.original_position();
                        if matches!(reader.read().unwrap(), wasmparser::Operator::I64GtU) {
                            comparison = Some(offset);
                        }
                    }
                }
                body_index += 1;
            }
        }
        // i64.lt_u is structurally valid with the same stack signature, but
        // reverses the address authority predicate. Only exact plan matching
        // may authorize the helper, just as for an inlined memory operation.
        bytes[comparison.expect("outlined unsigned overflow comparison")] = 0x54;
        assert!(matches!(
            admit_canonical_with_bytes_for_test(checked_outlined_memory_fixture(), bytes),
            Err(CanonicalAdmissionError::PlanMismatch(_))
        ));
    }

    #[cfg(feature = "native")]
    #[test]
    fn scalar_local_storage_reuse_executes_above_fifty_thousand_ssa_values() {
        use crate::canonical_interpreter::{interpret_program, InterpreterValue};
        let mut interner = LayoutInterner::new();
        let int = interner.intern_scalar(ScalarKind::Int).unwrap();
        let record = interner.intern_packed_record(vec![int]).unwrap();
        let crate::layout::LayoutSize::Fixed(bytes) = interner.get(record).unwrap().size() else { panic!("fixed scalar record"); };
        let captured = ValueId::new(0);
        let object = ValueId::new(1);
        let aggregate = ValueId::new(2);
        let loaded = ValueId::new(55_003);
        let extracted = ValueId::new(55_004);
        let mut value_types = BTreeMap::from([
            (captured, ValueType::Int), (object, ValueType::Ref(record)),
            (aggregate, ValueType::InlineAggregate(record)),
            (loaded, ValueType::Int), (extracted, ValueType::Int),
        ]);
        let mut operations = vec![
            Operation::ConstInt { destination: captured, value: 42 },
            Operation::MakeInlineAggregate { destination: aggregate, layout: record, fields: vec![captured] },
            Operation::Alloc { destination: object, layout: record, extent: Extent::FixedBytes(u64::from(bytes)), class: crate::canonical_safewir::AllocationClass::ReferenceCounted },
        ];
        for index in 0..55_000 {
            let value = ValueId::new(index + 3);
            value_types.insert(value, ValueType::Int);
            operations.push(Operation::ConstInt { destination: value, value: i64::from(index) });
            operations.push(Operation::Store {
                place: Place::projected(object, Projection::Field { index: 0 }, ValueType::Int, AccessMode::Write),
                value, transfer: StoreTransfer::CopyScalar,
            });
        }
        operations.extend([
            Operation::Load { destination: loaded, place: Place::projected(object, Projection::Field { index: 0 }, ValueType::Int, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
            Operation::ExtractInlineField { destination: extracted, aggregate, index: 0 },
            Operation::DropRef { object },
        ]);
        let checked = finalize(CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(142), checked_source: CheckedSourceIdentity::from_digest([142; 32]),
            layouts: LayoutBundle::from_interner(&interner, [record]).unwrap(),
            signatures: vec![scalar_signature_with(FunctionSignatureId::new(142), Vec::new(), vec![ValueType::Int, ValueType::Int])],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(142), signature: FunctionSignatureId::new(142),
                entry: crate::canonical_safewir::BlockId::new(0), value_types,
                blocks: vec![BasicBlock { id: crate::canonical_safewir::BlockId::new(0), parameters: Vec::new(), operations, terminator: Terminator::Return { values: vec![loaded, extracted] } }],
            }], proper_tail_sites: Vec::new(),
        }));
        let oracle = interpret_program(&checked, FunctionId::new(142), Vec::new(), Default::default()).unwrap();
        assert_eq!(oracle.results, vec![InterpreterValue::Int(54_999), InterpreterValue::Int(42)]);
        assert_eq!(oracle.leaked_objects, 0);
        let artifact = admit_canonical(checked).expect("SSA value count is not Wasm local count");
        let source_body = wasmparser::Parser::new(0).parse_all(artifact.wasm()).find_map(|payload| {
            if let wasmparser::Payload::CodeSectionEntry(body) = payload.unwrap() { Some(body) } else { None }
        }).unwrap();
        let locals: u32 = source_body.get_locals_reader().unwrap().into_iter().map(|entry| entry.unwrap().0).sum();
        assert!(locals < 64, "nonescaping scalars consumed {locals} Wasm locals");
        assert!(source_body.range().len() < 2_000_000);
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let result = instance.get_typed_func::<(), (i64, i64)>(&mut store, "f142").unwrap().call(&mut store, ()).unwrap();
        assert_eq!(result, (54_999, 42), "deferred inline field must retain its original scalar");
    }

    #[test]
    fn outlined_retain_saturation_predicate_mutation_fails_admission() {
        let mut bytes = encode_validated(checked_outlined_memory_fixture()).unwrap();
        let mut body_index = 0;
        let mut limit = None;
        for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
            if let wasmparser::Payload::CodeSectionEntry(body) = payload.unwrap() {
                if body_index == 3 {
                    let mut reader = body.get_operators_reader().unwrap();
                    while !reader.eof() {
                        let offset = reader.original_position();
                        if matches!(reader.read().unwrap(), wasmparser::Operator::I32Const { value: -1 }) { limit = Some(offset + 1); }
                    }
                }
                body_index += 1;
            }
        }
        bytes[limit.expect("retain saturation limit")] = 0;
        assert!(matches!(admit_canonical_with_bytes_for_test(checked_outlined_memory_fixture(), bytes), Err(CanonicalAdmissionError::PlanMismatch(_))));
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

    #[cfg(feature = "native")]
    #[test]
    fn admitted_pointer_list_move_and_clone_execute_with_retained_child() {
        let artifact = admit_canonical(checked_pointer_list_transfer_artifact())
            .expect("pointer-list transfers match exact admission plan");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance.get_typed_func::<(), i32>(&mut store, "f36").unwrap();
        let retained = run.call(&mut store, ()).unwrap();
        // The child is the first heap object: its reference count header sits
        // at the heap start and its pointer eight bytes past it.
        assert_eq!(retained, 24, "the returned clone is the child object");
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        // Move into the container, clone out, drop the container: the child
        // keeps exactly the clone's reference.
        let rc = u32::from_le_bytes(memory.data(&store)[16..20].try_into().unwrap());
        assert_eq!(rc, 1);
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

    #[test]
    fn valid_scalar_and_packed_list_artifacts_admit() {
        let scalar = admit_canonical(checked_scalar()).expect("scalar artifact admits");
        wasmparser::validate(scalar.wasm()).expect("admitted scalar bytes validate");
        let list = admit_canonical(checked_packed_list_allocation_artifact()).expect("list allocation artifact admits");
        wasmparser::validate(list.wasm()).expect("admitted list bytes validate");
    }

    #[test]
    fn admitted_int_list_load_store_executes() {
        let artifact = admit_canonical(checked_int_list_fixture()).expect("int list literal/index artifact admits");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let result = instance.get_typed_func::<(), i64>(&mut store, "f36").unwrap().call(&mut store, ()).unwrap();
        assert_eq!(result, 19);
    }

    #[path = "nullable_admission.rs"]
    mod nullable_admission;

    #[cfg(feature = "native")]
    #[path = "unit_storage.rs"]
    mod unit_storage;

    #[test]
    fn foreign_entry_section_fails_admission() {
        // The test admission path derives no entry, so an entry section in the
        // bytes is not the encoder's and must be refused before launch.
        let mut bytes = encode_validated(checked_scalar()).unwrap();
        wasm_encoder::Section::append_to(
            &CustomSection { name: Cow::Borrowed(ENTRY_SECTION), data: Cow::Owned(vec![1, 2, b'f', b'0', 0, 1]) },
            &mut bytes,
        );
        assert!(admit_canonical_with_bytes_for_test(checked_scalar(), bytes).is_err());
    }

    #[test]
    fn canonical_entry_round_trips_through_its_section_encoding() {
        let entry = CanonicalEntry {
            export: "f3".to_owned(),
            argument_positions: Vec::new(),
            capabilities: vec![CanonicalEntryCapability::Console],
            result: CanonicalEntryResult::Int,
        };
        assert_eq!(CanonicalEntry::decode(&entry.encode()).unwrap(), entry);
        assert!(CanonicalEntry::decode(&[2]).is_err());
        assert!(CanonicalEntry::decode(&[1, 2, b'f', b'0', 1, 0]).is_err(), "count/payload mismatch");
    }

    #[test]
    fn mutated_int_list_store_offset_fails_admission() {
        let mut bytes = encode_validated(checked_int_list_fixture()).unwrap();
        let opcode = bytes.iter().position(|byte| *byte == 0x37).expect("i64.store opcode exists");
        assert_eq!(bytes[opcode + 1], 3);
        assert_eq!(bytes[opcode + 2], 0);
        bytes[opcode + 2] = 1;
        assert!(matches!(
            admit_canonical_with_bytes_for_test(checked_int_list_fixture(), bytes),
            Err(CanonicalAdmissionError::PlanMismatch(_))
        ));
    }

    #[test]
    fn mutated_emitted_bytes_fail_independent_admission_plan() {
        let mut bytes = encode_validated(checked_scalar()).unwrap();
        let immediate = bytes
            .windows(2)
            .position(|window| window == [0x42, 9])
            .expect("scalar i64.const immediate exists")
            + 1;
        bytes[immediate] = 10;
        assert!(matches!(
            admit_canonical_with_bytes_for_test(checked_scalar(), bytes),
            Err(CanonicalAdmissionError::PlanMismatch(_))
        ));
    }

    #[test]
    fn mismatched_source_plan_fails_admission() {
        let bytes = encode_validated(checked_scalar()).unwrap();
        assert!(matches!(
            admit_canonical_with_bytes_for_test(checked_packed_list_allocation_artifact(), bytes),
            Err(CanonicalAdmissionError::PlanMismatch(_))
        ));
    }

    #[test]
    fn mutated_local_declaration_fails_admission() {
        let mut bytes = encode_validated(checked_scalar()).unwrap();
        let body_start = bytes
            .windows(2)
            .rposition(|window| window == [1, 0x7f])
            .expect("scalar body has an i32 local declaration");
        bytes[body_start + 1] = 0x7e;
        assert!(matches!(
            admit_canonical_with_bytes_for_test(checked_scalar(), bytes),
            Err(CanonicalAdmissionError::WasmValidation(_) | CanonicalAdmissionError::PlanMismatch(_))
        ));
    }

    #[test]
    fn mutated_if_block_type_fails_admission() {
        let mut bytes = encode_validated(checked_guard_artifact()).unwrap();
        let block_type = bytes
            .windows(2)
            .position(|window| window == [0x04, 0x40])
            .expect("guard body has an empty if block")
            + 1;
        bytes[block_type] = 0x00;
        assert!(matches!(
            admit_canonical_with_bytes_for_test(checked_guard_artifact(), bytes),
            Err(CanonicalAdmissionError::PlanMismatch(_))
        ));
    }
    #[cfg(feature = "native")]
    #[test]
    fn packed_scalar_collection_access_mutates_and_preserves_headers() {
        let source = checked_packed_list_fixture(false);
        let target = lower_validated(&source).unwrap();
        let receipt = check_correspondence(source.module(), &target).unwrap();
        assert_eq!(receipt.mappings.len(), 24);
        assert!(receipt
            .mappings
            .iter()
            .any(|mapping| mapping.rule == CorrespondenceRule::MemoryStore));
        assert!(receipt
            .mappings
            .iter()
            .any(|mapping| mapping.rule == CorrespondenceRule::MemoryUpdateCount));
        assert_eq!(
            crate::canonical_interpreter::interpret(
                &source,
                FunctionId::new(30),
                Vec::new(),
            )
            .unwrap(),
            vec![
                crate::canonical_interpreter::InterpreterValue::Size(2),
                crate::canonical_interpreter::InterpreterValue::Size(2),
                crate::canonical_interpreter::InterpreterValue::Size(42),
            ],
        );
        let wasm = encode_validated(source).expect("packed scalar collection encodes");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance
            .get_typed_func::<(), (i32, i32, i32)>(&mut store, "f30")
            .unwrap();
        assert_eq!(run.call(&mut store, ()).unwrap(), (2, 2, 42));
    }

    #[cfg(feature = "native")]
    #[test]
    fn packed_scalar_collection_oob_guard_traps_in_wasmtime() {
        let wasm = encode_validated(checked_packed_list_fixture(true)).expect("OOB fixture encodes");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance.get_typed_func::<(), ()>(&mut store, "f30").unwrap();
        assert!(run.call(&mut store, ()).is_err());
    }

    #[cfg(feature = "native")]
    #[test]
    fn shared_release_preserves_surviving_original_for_use() {
        let source = checked_share_fixture();
        let target = lower_validated(&source).unwrap();
        let receipt = check_correspondence(source.module(), &target).unwrap();
        assert_eq!(receipt.mappings.len(), 8);
        let wasm = encode_validated(source).expect("shared scalar collection encodes");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance.get_typed_func::<(), i32>(&mut store, "f33").unwrap();
        assert_eq!(run.call(&mut store, ()).unwrap(), 1);
    }

    #[cfg(feature = "native")]
    #[test]
    fn consecutive_rc_allocations_reserve_non_overlapping_prefixes() {
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
    #[test]
    fn allocation_past_linear_memory_grows_or_traps_when_growth_is_refused() {
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

    #[test]
    fn allocation_exclusive_end_boundary_matches_emission_and_plan() {
        fn module_with_body(body: &WasmFunctionEncoder) -> Vec<u8> {
            let mut types = TypeSection::new();
            types.ty().function([], []);
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
            for value in [16, 0, 16] {
                globals.global(
                    GlobalType {
                        val_type: ValType::I32,
                        mutable: true,
                        shared: false,
                    },
                    &ConstExpr::i32_const(value),
                );
            }
            let mut code = CodeSection::new();
            code.function(body);
            let mut module = WasmModule::new();
            module
                .section(&types)
                .section(&functions)
                .section(&memories)
                .section(&globals)
                .section(&code);
            module.finish()
        }

        fn operator_positions(bytes: &[u8]) -> (Option<usize>, Option<usize>) {
            let mut boundary = None;
            let mut mutation = None;
            let mut index = 0;
            for payload in wasmparser::Parser::new(0).parse_all(bytes) {
                if let wasmparser::Payload::CodeSectionEntry(body) = payload.unwrap() {
                    let mut operators = body.get_operators_reader().unwrap();
                    while !operators.eof() {
                        match operators.read().unwrap() {
                            wasmparser::Operator::I64Const { value }
                                if value == i64::from(u32::MAX) && boundary.is_none() =>
                            {
                                boundary = Some(index);
                            }
                            wasmparser::Operator::MemoryGrow { .. }
                            | wasmparser::Operator::GlobalSet { .. }
                            | wasmparser::Operator::I32Store { .. }
                                if mutation.is_none() =>
                            {
                                mutation = Some(index);
                            }
                            _ => {}
                        }
                        index += 1;
                    }
                }
            }
            (boundary, mutation)
        }

        let mut interner = LayoutInterner::new();
        let scalar = interner.intern_scalar(ScalarKind::U32).unwrap();
        let destination = ValueId::new(1);
        let signature = scalar_signature_with(
            FunctionSignatureId::new(168),
            Vec::new(),
            Vec::new(),
        );
        let function = Function {
            id: FunctionId::new(168),
            signature: signature.id,
            entry: BlockId::new(168),
            value_types: [(destination, ValueType::Ref(scalar))].into_iter().collect(),
            blocks: vec![BasicBlock {
                id: BlockId::new(168),
                parameters: Vec::new(),
                operations: Vec::new(),
                terminator: Terminator::Return { values: Vec::new() },
            }],
        };
        let layout = local_layout(&function, &signature, &interner).unwrap();
        let representable_bytes = u64::from(u32::MAX - RC_PREFIX_BYTES);
        let representable = Extent::FixedBytes(representable_bytes);
        let exclusive_4gib = Extent::FixedBytes(representable_bytes + 1);

        assert_eq!(fixed_alloc_bytes(representable_bytes), Some(u32::MAX - RC_PREFIX_BYTES));
        assert_eq!(fixed_alloc_bytes(representable_bytes + 1), None);

        let declarations = || layout.extra_types.iter().copied().map(|value_type| (1, value_type));
        let mut emitted = WasmFunctionEncoder::new(declarations());
        emit_alloc(
            &mut emitted,
            &function,
            &layout,
            &interner,
            destination,
            scalar,
            &representable,
        )
        .expect("an exclusive end of u32::MAX remains structurally representable");
        emitted.instruction(&wasm_encoder::Instruction::End);
        let emitted = module_with_body(&emitted);
        wasmparser::validate(&emitted).expect("representable boundary emission is valid Wasm");
        let (emitted_boundary, emitted_mutation) = operator_positions(&emitted);
        assert!(
            emitted_boundary.unwrap() < emitted_mutation.unwrap(),
            "the exclusive-end guard must precede memory growth and allocator mutation",
        );

        let mut planned = Vec::new();
        expected_alloc_plan(
            &mut planned,
            &function,
            &layout,
            None,
            destination,
            &representable,
        )
        .expect("the independent plan accepts the representable predecessor");
        let planned_boundary = planned
            .iter()
            .position(|token| *token == PlanToken::I64Const(i64::from(u32::MAX)))
            .unwrap();
        let planned_mutation = planned
            .iter()
            .position(|token| matches!(
                token,
                PlanToken::MemoryGrow
                    | PlanToken::GlobalSet(_)
                    | PlanToken::I32Store { .. }
            ))
            .unwrap();
        assert!(planned_boundary < planned_mutation);

        let mut rejected = WasmFunctionEncoder::new(declarations());
        assert!(matches!(
            emit_alloc(
                &mut rejected,
                &function,
                &layout,
                &interner,
                destination,
                scalar,
                &exclusive_4gib,
            ),
            Err(EncodeError::UnsupportedLayout { operation: "Alloc extent", .. })
        ));
        rejected.instruction(&wasm_encoder::Instruction::End);
        let rejected = module_with_body(&rejected);
        assert_eq!(operator_positions(&rejected), (None, None));

        let mut rejected_plan = Vec::new();
        assert!(matches!(
            expected_alloc_plan(
                &mut rejected_plan,
                &function,
                &layout,
                None,
                destination,
                &exclusive_4gib,
            ),
            Err(CanonicalAdmissionError::UnsupportedPlan)
        ));
        assert!(rejected_plan.is_empty(), "rejection must precede planned allocator mutation");
    }

    #[cfg(feature = "native")]
    #[test]
    fn share_at_max_refcount_traps_before_destination_or_refcount_write() {
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
    #[test]
    fn release_at_zero_refcount_traps_before_decrement() {
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

    #[test]
    fn bump_allocator_class_is_refused_by_initial_check() {
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

    #[test]
    fn pointer_bearing_catalog_layout_is_refused_before_the_canonical_backend() {
        // A pointer-bearing catalog buffer never reaches the encoder: under
        // callee-drops its owned token must be released, and the initial
        // checker has no recursive destructor for that layout, so the module
        // fails closed one stage earlier than this test once pinned.
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::U32).unwrap();
        let layout = interner.intern_runtime_pointer_buffer(element, PointerSlot::Owned).unwrap();
        let bundle = LayoutBundle::from_interner(&interner, [layout]).unwrap();
        let candidate = alloc_only_candidate(layout, bundle, crate::canonical_safewir::AllocationClass::ReferenceCounted, 4, 8, 1);
        let Err(refusal) = check_initial_with_test_witness(candidate) else {
            panic!("pointer-bearing catalog buffer must be refused");
        };
        assert!(format!("{refusal:?}").contains("pointer-bearing recursive destructor"), "{refusal:?}");
    }

    #[cfg(feature = "native")]
    fn run_scalar_range(
        range_count: u64,
        source_start: u64,
        destination_start: u64,
        move_cells: bool,
    ) -> Result<i64, wasmtime::Error> {
        let artifact = admit_canonical(checked_scalar_range(
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

    #[cfg(feature = "native")]
    #[test]
    fn scalar_clone_range_admits_emits_and_matches_exact_correspondence() {
        let validated = checked_scalar_range(1, 0, 0, false);
        let target = lower_validated(&validated).expect("scalar CloneRange lowers");
        assert!(target.functions.iter().flat_map(|function| function.blocks.iter()).flat_map(|block| block.instructions.iter()).any(
            |instruction| matches!(instruction.operation, ScalarInstruction::CloneRange { .. })
        ));
        let receipt = check_correspondence(validated.module(), &target).expect("CloneRange correspondence is exact");
        assert!(receipt.mappings.iter().any(|mapping| mapping.rule == CorrespondenceRule::MemoryCopy));

        let artifact = admit_canonical(validated).expect("scalar CloneRange admits with its exact plan");
        wasmparser::validate(artifact.wasm()).expect("admitted scalar CloneRange is valid Wasm");
        assert!(artifact.wasm().windows(2).any(|window| window == [0xfc, 0x0a]));
        assert_eq!(run_scalar_range(1, 0, 0, false).unwrap(), 42);
    }

    #[cfg(feature = "native")]
    #[test]
    fn scalar_move_range_admits_and_executes_destination_value() {
        let validated = checked_scalar_range(1, 0, 0, true);
        let target = lower_validated(&validated).expect("scalar MoveRange lowers");
        assert!(target
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .any(|instruction| matches!(instruction.operation, ScalarInstruction::MoveRange { .. })));
        let receipt = check_correspondence(validated.module(), &target)
            .expect("MoveRange correspondence is exact");
        assert!(receipt
            .mappings
            .iter()
            .any(|mapping| mapping.rule == CorrespondenceRule::MemoryOwnership));

        let artifact = admit_canonical(validated).expect("scalar MoveRange admits with its exact plan");
        wasmparser::validate(artifact.wasm()).expect("admitted scalar MoveRange is valid Wasm");
        assert!(artifact.wasm().windows(2).any(|window| window == [0xfc, 0x0a]));
        assert_eq!(run_scalar_range(1, 0, 0, true).unwrap(), 42);
    }

    #[test]
    fn pointer_bearing_clone_range_lowers_in_canonical_backend() {
        let mut interner = LayoutInterner::new();
        let byte = interner.intern_scalar(ScalarKind::RawByte).unwrap();
        let string = interner.intern_runtime_buffer(byte).unwrap();
        let slot = interner
            .intern_runtime_pointer_value_slot(string, PointerSlot::Owned)
            .unwrap();
        let list = interner.intern_packed_list(slot, RcHeader::Required).unwrap();
        let count = ValueId::new(1);
        let source_start = ValueId::new(2);
        let destination_start = ValueId::new(3);
        let source = ValueId::new(4);
        let destination = ValueId::new(5);
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
        let operation = Operation::CloneRange { destination: destination_place, source: source_place, count };
        let value_types = [
            (count, ValueType::Size),
            (source_start, ValueType::Size),
            (destination_start, ValueType::Size),
            (source, ValueType::Ref(list)),
            (destination, ValueType::Ref(list)),
        ]
        .into_iter()
        .collect();
        assert!(
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
            )
            .is_ok()
        );
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
    #[test]
    fn pointer_clone_range_retains_executes_and_recursively_releases() {
        let (validated, expected_reclaimed) = checked_pointer_clone_range_execution();
        let interpreted = crate::canonical_interpreter::interpret_program(
            &validated,
            FunctionId::new(166),
            Vec::new(),
            Default::default(),
        )
        .expect("pointer CloneRange executes in the independent interpreter");
        assert_eq!(
            interpreted.results,
            vec![crate::canonical_interpreter::InterpreterValue::Size(1)]
        );
        assert_eq!(interpreted.leaked_objects, 0);

        let artifact = admit_canonical(validated)
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

    #[test]
    fn malformed_clone_range_places_are_refused_by_canonical_backend() {
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
    #[test]
    fn scalar_clone_range_count_stride_overflow_traps_before_copy() {
        assert!(run_raw_scalar_clone_range_probe([0, 0, 0, 0, u32::MAX as i32]).is_err());
    }

    #[cfg(feature = "native")]
    #[test]
    fn scalar_clone_range_start_stride_overflow_traps_before_copy() {
        assert!(run_raw_scalar_clone_range_probe([0, 0, (u32::MAX - 1) as i32, 0, 0]).is_err());
    }

    #[cfg(feature = "native")]
    #[test]
    fn pointer_clone_range_second_saturated_child_traps_before_destination_publish() {
        let (wasm, data_offset, stride) = raw_pointer_clone_range_probe();
        wasmparser::validate(&wasm).expect("raw pointer CloneRange probe is valid Wasm");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        let source = 32u32;
        let destination = 128u32;
        let first_child = 256u32;
        let saturated_child = 288u32;
        let first_destination = 0x1122_3344u32;
        let second_destination = 0x5566_7788u32;
        let mut contents = vec![0; 320];
        let mut put = |at: u32, value: u32| {
            let at = usize::try_from(at).unwrap();
            contents[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        put(source + data_offset, first_child);
        put(source + data_offset + stride, saturated_child);
        put(destination + data_offset, first_destination);
        put(destination + data_offset + stride, second_destination);
        put(first_child - RC_PREFIX_BYTES, 1);
        put(saturated_child - RC_PREFIX_BYTES, u32::MAX);
        memory.write(&mut store, 0, &contents).unwrap();

        let probe = instance
            .get_typed_func::<(i32, i32, i32, i32, i32), ()>(&mut store, "probe")
            .unwrap();
        assert!(
            probe
                .call(
                    &mut store,
                    (source as i32, destination as i32, 0, 0, 2),
                )
                .is_err(),
            "the second saturated source child must trap",
        );

        let data = memory.data(&store);
        let word = |at: u32| {
            let at = usize::try_from(at).unwrap();
            u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
        };
        assert_eq!(word(destination + data_offset), first_destination);
        assert_eq!(word(destination + data_offset + stride), second_destination);
        assert_eq!(word(source + data_offset), first_child);
        assert_eq!(word(source + data_offset + stride), saturated_child);
        assert_eq!(word(first_child - RC_PREFIX_BYTES), 2);
        assert_eq!(word(saturated_child - RC_PREFIX_BYTES), u32::MAX);
    }

    #[test]
    fn headerless_runtime_buffer_is_refused_by_canonical_backend() {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::U32).unwrap();
        let layout = interner.intern_runtime_buffer(element).unwrap();
        let bundle = LayoutBundle::from_interner(&interner, [layout]).unwrap();
        assert!(matches!(
            encode_validated(checked_alloc_only(layout, bundle, crate::canonical_safewir::AllocationClass::ReferenceCounted, 4, 4, 1)),
            Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { .. }))
        ));
    }

    #[test]
    fn full_u32_size_constant_encodes_and_validates() {
        let wasm = encode_validated(checked_full_u32_size()).expect("full u32 size encodes");
        wasmparser::validate(&wasm).expect("full u32 size module is structurally valid");
    }

    #[test]
    fn full_u32_size_constant_matches_exact_admission_plan() {
        let artifact = admit_canonical(checked_full_u32_size())
            .expect("full u32 size matches its independently decoded plan");
        wasmparser::validate(artifact.wasm()).expect("admitted full u32 size remains valid Wasm");
    }

    #[cfg(feature = "native")]
    #[test]
    fn encoded_comparison_and_full_u32_size_execute() {
        let engine = wasmtime::Engine::default();
        for (wasm, export, expected) in [
            (encode_validated(checked_comparison()).unwrap(), "f3", 1),
            (encode_validated(checked_full_u32_size()).unwrap(), "f5", -1),
        ] {
            let module = wasmtime::Module::new(&engine, &wasm).unwrap();
            let mut store = wasmtime::Store::new(&engine, ());
            let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
            let function = instance.get_typed_func::<(), i32>(&mut store, export).unwrap();
            assert_eq!(function.call(&mut store, ()).unwrap(), expected);
        }
    }

    #[cfg(feature = "native")]
    #[test]
    fn encoded_scalar_module_executes() {
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

    #[cfg(feature = "native")]
    #[test]
    fn fixed_static_aggregate_and_nominal_reuse_match_both_engines() {
        use crate::canonical_interpreter::{InterpreterValue, interpret};

        let mut interner = LayoutInterner::new();
        let int = interner.intern_scalar(ScalarKind::Int).unwrap();
        let boolean = interner.intern_scalar(ScalarKind::Bool).unwrap();
        let physical = interner.intern_packed_record(vec![int, boolean]).unwrap();
        let nominal = interner.intern_opaque_nominal([0x5a; 32]).unwrap();
        interner.bind_nominal(nominal, physical).unwrap();
        let descriptor = interner.get(physical).unwrap();
        let crate::layout::LayoutSize::Fixed(size) = descriptor.size() else { panic!("fixed aggregate") };
        let int_offset = descriptor.fields()[0].offset() as usize;
        let bool_offset = descriptor.fields()[1].offset() as usize;
        let mut bytes = vec![0; size as usize];
        bytes[int_offset..int_offset + 8].copy_from_slice(&42_i64.to_le_bytes());
        bytes[bool_offset] = 1;

        let first = ValueId::new(1);
        let second = ValueId::new(2);
        let loaded_int = ValueId::new(3);
        let loaded_bool = ValueId::new(4);
        let data = crate::canonical_safewir::StaticDataId::new(0);
        let signature = FunctionSignature {
            id: FunctionSignatureId::new(24),
            parameters: Vec::new(),
            results: vec![ValueType::Int, ValueType::Bool],
            contract: CallableContract {
                parameters: Vec::new(),
                results: vec![ResultContract::Value(ValueType::Int), ResultContract::Value(ValueType::Bool)],
                parameter_effects: BTreeMap::new(),
            },
        };
        let candidate = CanonicalModule::from_lowering_with_static_data(
            LoweredModuleParts {
                revision: Revision::new(24),
                checked_source: CheckedSourceIdentity::from_digest([24; 32]),
                layouts: LayoutBundle::from_interner(&interner, [nominal]).unwrap(),
                signatures: vec![signature],
                imports: Vec::new(),
                functions: vec![Function {
                    id: FunctionId::new(24),
                    signature: FunctionSignatureId::new(24),
                    entry: BlockId::new(0),
                    value_types: [
                        (first, ValueType::Ref(nominal)),
                        (second, ValueType::Ref(physical)),
                        (loaded_int, ValueType::Int),
                        (loaded_bool, ValueType::Bool),
                    ]
                    .into_iter()
                    .collect(),
                    blocks: vec![BasicBlock {
                        id: BlockId::new(0),
                        parameters: Vec::new(),
                        operations: vec![
                            Operation::StaticData { destination: first, layout: nominal, extent: Extent::FixedBytes(u64::from(size)), data },
                            Operation::StaticData { destination: second, layout: physical, extent: Extent::FixedBytes(u64::from(size)), data },
                            Operation::Load {
                                destination: loaded_int,
                                place: Place::projected(first, Projection::Field { index: 0 }, ValueType::Int, AccessMode::Read),
                                transfer: LoadTransfer::CopyScalar,
                            },
                            Operation::Load {
                                destination: loaded_bool,
                                place: Place::projected(second, Projection::Field { index: 1 }, ValueType::Bool, AccessMode::Read),
                                transfer: LoadTransfer::CopyScalar,
                            },
                        ],
                        terminator: Terminator::Return { values: vec![loaded_int, loaded_bool] },
                    }],
                }],
                proper_tail_sites: Vec::new(),
            },
            vec![crate::canonical_safewir::StaticDataSegment::new(data, bytes)],
        );
        let checked = check_initial_with_test_witness(candidate).unwrap();
        let validated = validate_final_for_test(checked).unwrap();
        assert_eq!(
            interpret(&validated, FunctionId::new(24), Vec::new()).unwrap(),
            vec![InterpreterValue::Int(42), InterpreterValue::Bool(true)]
        );

        let wasm = encode_validated(validated).unwrap();
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        assert_eq!(
            instance
                .get_typed_func::<(), (i64, i32)>(&mut store, "f24")
                .unwrap()
                .call(&mut store, ())
                .unwrap(),
            (42, 1)
        );
    }

    #[test]
    fn static_data_boundary_selects_enough_initial_memory_pages() {
        for (end, expected_heap_start, expected_pages) in [
            (65_536_usize, 65_536_u32, 1_u64),
            (65_537_usize, 65_544_u32, 2_u64),
        ] {
            let segment = crate::canonical_safewir::StaticDataSegment::new(
                crate::canonical_safewir::StaticDataId::new(0),
                vec![0; end - STATIC_DATA_BASE as usize],
            );
            let (plan, heap_start) = static_data_plan(&[segment]).unwrap();
            assert_eq!(plan[0].offset as usize + plan[0].bytes.len(), end);
            assert_eq!(heap_start, expected_heap_start);
            assert_eq!(initial_memory_pages(heap_start), expected_pages);
        }
    }

    #[cfg(feature = "native")]
    #[test]
    fn backend_total_scalar_families_match_the_interpreter() {
        use crate::canonical_interpreter::{InterpreterValue, interpret};

        let specs = [
            (FunctionId::new(20), ValueType::Int, Operation::ConstInt { destination: ValueId::new(1), value: 40 }, Operation::ConstInt { destination: ValueId::new(2), value: 2 }, ArithmeticOperator::Add, OverflowBehavior::Wrap),
            (FunctionId::new(21), ValueType::Size, Operation::ConstSize { destination: ValueId::new(1), value: 40 }, Operation::ConstSize { destination: ValueId::new(2), value: 2 }, ArithmeticOperator::Add, OverflowBehavior::Trap),
            (FunctionId::new(22), ValueType::Float, Operation::ConstFloat { destination: ValueId::new(1), bits: 84.0_f64.to_bits() }, Operation::ConstFloat { destination: ValueId::new(2), bits: 2.0_f64.to_bits() }, ArithmeticOperator::DivideSigned, OverflowBehavior::Wrap),
        ];
        let mut signatures = Vec::new();
        let mut functions = Vec::new();
        for (function_id, value_type, left, right, operator, overflow) in specs {
            let signature_id = FunctionSignatureId::new(function_id.get());
            signatures.push(FunctionSignature {
                id: signature_id,
                parameters: Vec::new(),
                results: vec![value_type],
                contract: CallableContract { parameters: Vec::new(), results: vec![ResultContract::Value(value_type)], parameter_effects: BTreeMap::new() },
            });
            functions.push(Function {
                id: function_id,
                signature: signature_id,
                entry: BlockId::new(0),
                value_types: [(ValueId::new(1), value_type), (ValueId::new(2), value_type), (ValueId::new(3), value_type)].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: BlockId::new(0),
                    parameters: Vec::new(),
                    operations: vec![left, right, Operation::Arithmetic { destination: ValueId::new(3), operator, left: ValueId::new(1), right: ValueId::new(2), overflow }],
                    terminator: Terminator::Return { values: vec![ValueId::new(3)] },
                }],
            });
        }
        signatures.push(FunctionSignature {
            id: FunctionSignatureId::new(23), parameters: Vec::new(), results: vec![ValueType::Bool],
            contract: CallableContract { parameters: Vec::new(), results: vec![ResultContract::Value(ValueType::Bool)], parameter_effects: BTreeMap::new() },
        });
        functions.push(Function {
            id: FunctionId::new(23), signature: FunctionSignatureId::new(23), entry: BlockId::new(0),
            value_types: [(ValueId::new(1), ValueType::Bool), (ValueId::new(2), ValueType::Bool), (ValueId::new(3), ValueType::Bool)].into_iter().collect(),
            blocks: vec![BasicBlock { id: BlockId::new(0), parameters: Vec::new(), operations: vec![
                Operation::ConstBool { destination: ValueId::new(1), value: true },
                Operation::ConstBool { destination: ValueId::new(2), value: true },
                Operation::Compare { destination: ValueId::new(3), operator: ComparisonOperator::Equal, left: ValueId::new(1), right: ValueId::new(2) },
            ], terminator: Terminator::Return { values: vec![ValueId::new(3)] } }],
        });
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(20), checked_source: CheckedSourceIdentity::from_digest([20; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(), signatures, imports: Vec::new(), functions,
            proper_tail_sites: Vec::new(),
        });
        let checked = check_initial_with_test_witness(candidate).unwrap();
        let validated = validate_final_for_test(checked).unwrap();
        assert_eq!(interpret(&validated, FunctionId::new(20), Vec::new()).unwrap(), vec![InterpreterValue::Int(42)]);
        assert_eq!(interpret(&validated, FunctionId::new(21), Vec::new()).unwrap(), vec![InterpreterValue::Size(42)]);
        assert_eq!(interpret(&validated, FunctionId::new(22), Vec::new()).unwrap(), vec![InterpreterValue::FloatBits(42.0_f64.to_bits())]);
        assert_eq!(interpret(&validated, FunctionId::new(23), Vec::new()).unwrap(), vec![InterpreterValue::Bool(true)]);

        let wasm = encode_validated(validated).unwrap();
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        assert_eq!(instance.get_typed_func::<(), i64>(&mut store, "f20").unwrap().call(&mut store, ()).unwrap(), 42);
        assert_eq!(instance.get_typed_func::<(), i32>(&mut store, "f21").unwrap().call(&mut store, ()).unwrap(), 42);
        assert_eq!(instance.get_typed_func::<(), f64>(&mut store, "f22").unwrap().call(&mut store, ()).unwrap(), 42.0);
        assert_eq!(instance.get_typed_func::<(), i32>(&mut store, "f23").unwrap().call(&mut store, ()).unwrap(), 1);
    }

    #[test]
    fn correspondence_rejects_semantic_mismatch() {
        let source = checked_scalar();
        let mut target = lower_validated(&source).unwrap();
        target.functions[0].blocks[0].instructions[0].operation = ScalarInstruction::IntConst { destination: ValueId::new(1), value: 10 };
        assert!(matches!(check_correspondence(source.module(), &target), Err(CorrespondenceError::InstructionMismatch(_))));
    }

    #[test]
    fn correspondence_rejects_omission() {
        let source = checked_scalar();
        let mut target = lower_validated(&source).unwrap();
        target.functions[0].blocks[0].instructions.clear();
        assert!(matches!(check_correspondence(source.module(), &target), Err(CorrespondenceError::MissingSourceItem(_))));
    }

    #[test]
    fn correspondence_rejects_duplication() {
        let source = checked_scalar();
        let mut target = lower_validated(&source).unwrap();
        let duplicate = target.functions[0].blocks[0].instructions[0].clone();
        target.functions[0].blocks[0].instructions.push(duplicate);
        assert!(matches!(check_correspondence(source.module(), &target), Err(CorrespondenceError::DuplicateSourceItem(_))));
    }
}

#[cfg(test)]
mod dominance_tests {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    use super::{dominates_in, immediate_dominators};
    use crate::canonical_safewir::BlockId;

    /// The definition both emitters used before the dominator tree: a
    /// breadth-first search from the entry that never enters `avoid`.
    fn dominates_by_search(
        successors: &BTreeMap<BlockId, Vec<BlockId>>,
        entry: BlockId,
        dominator: BlockId,
        node: BlockId,
    ) -> bool {
        if node == dominator {
            return true;
        }
        let mut seen = BTreeSet::new();
        let mut pending = VecDeque::from([entry]);
        while let Some(id) = pending.pop_front() {
            if id == dominator || !seen.insert(id) {
                continue;
            }
            if id == node {
                return false;
            }
            pending.extend(successors.get(&id).into_iter().flatten().copied());
        }
        true
    }

    fn graph(edges: &[(u32, &[u32])]) -> BTreeMap<BlockId, Vec<BlockId>> {
        edges
            .iter()
            .map(|(from, to)| (BlockId::new(*from), to.iter().map(|id| BlockId::new(*id)).collect()))
            .collect()
    }

    #[test]
    fn dominator_tree_matches_the_path_definition() {
        // A diamond feeding a loop with a nested loop and two exits, a
        // self-loop, a terminal sink shared by both arms, and an unreachable
        // island (7 -> 8) that nothing enters.
        let successors = graph(&[
            (0, &[1, 2]),
            (1, &[3]),
            (2, &[3]),
            (3, &[4, 9]),
            (4, &[5, 6]),
            (5, &[5, 4]),
            (6, &[3, 9]),
            (7, &[8]),
            (8, &[7, 9]),
            (9, &[]),
        ]);
        let entry = BlockId::new(0);
        let idom = immediate_dominators(entry, &successors);
        assert_eq!(idom.get(&BlockId::new(3)), Some(&entry));
        assert_eq!(idom.get(&BlockId::new(5)), Some(&BlockId::new(4)));
        assert_eq!(idom.get(&BlockId::new(9)), Some(&BlockId::new(3)));
        assert!(!idom.contains_key(&BlockId::new(7)));
        for dominator in 0..10 {
            for node in 0..10 {
                let (dominator, node) = (BlockId::new(dominator), BlockId::new(node));
                assert_eq!(
                    dominates_in(&idom, entry, dominator, node),
                    dominates_by_search(&successors, entry, dominator, node),
                    "dominates({dominator:?}, {node:?})"
                );
            }
        }
    }
}
