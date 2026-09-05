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
    Operation, InitialValid, ParameterOwnership, Place, Projection, StoreTransfer, Terminator,
    ValidatedSafeWir, ValueId, ValueType,
};
use crate::layout::{
    HeaderLayout, LayoutInterner, LayoutKind, LayoutSize, PointerSlot, RcHeader, ScalarKind,
};

// Ref values point at the canonical layout root.  RC metadata occupies the
// eight bytes immediately before that root; the private module keeps one
// fixed linear-memory page and refuses an allocation before returning a
// pointer when the checked extent does not fit.
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
    IntConst { destination: ValueId, value: i64 },
    FloatConst { destination: ValueId, bits: u64 },
    BoolConst { destination: ValueId, value: bool },
    SizeConst { destination: ValueId, value: u64 },
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemoryAccess {
    object: ValueId,
    collection: CollectionLayout,
    offset: u32,
    width: u32,
    scalar: ScalarKind,
    pointer_layout: Option<crate::layout::LayoutId>,
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
    revision: crate::canonical_safewir::Revision,
    checked_source: crate::canonical_safewir::CheckedSourceIdentity,
    launch_contract_payload: Vec<u8>,
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
enum CorrespondenceRule {
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
    HostCall,
    Control,
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
    let descriptor = layouts.get(layout).ok_or(LoweringError::UnsupportedLayout { function, operation: "CopyBytes layout" })?;
    let LayoutKind::RuntimeBuffer { element } = descriptor.kind() else {
        return Err(LoweringError::UnsupportedLayout { function, operation: "CopyBytes layout" });
    };
    let element = layouts.get(*element).ok_or(LoweringError::UnsupportedLayout { function, operation: "CopyBytes element" })?;
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
        || import.mutation != HostMutation::ReadOnly
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
            _ => return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall argument" }),
        }
    }
    for (role, destination) in import.result_roles.iter().zip(destinations) {
        let expected = match role {
            HostResultRole::Value(value_type) => *value_type,
            HostResultRole::Capability(kind) => ValueType::Capability(*kind),
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

fn host_import_wasm_signature(
    function: FunctionId,
    import: &ImportContract,
) -> Result<(Vec<ValType>, Vec<ValType>), LoweringError> {
    let mut parameters = Vec::new();
    for role in &import.parameter_roles {
        match role {
            HostParameterRole::Value(value_type) => parameters.push(host_scalar_wasm_type(function, ValueId::new(u32::MAX), *value_type)?),
            HostParameterRole::Capability(_) => parameters.push(ValType::EXTERNREF),
            HostParameterRole::Buffer { value_type: ValueType::Ref(_), access: AccessMode::Read, extent: HostBufferExtent::Slice } => {
                parameters.extend([ValType::I32, ValType::I32]);
            }
            _ => return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall import signature" }),
        }
    }
    let mut results = Vec::new();
    for role in &import.result_roles {
        match role {
            HostResultRole::Value(value_type) => results.push(host_scalar_wasm_type(function, ValueId::new(u32::MAX), *value_type)?),
            HostResultRole::Capability(_) => results.push(ValType::EXTERNREF),
        }
    }
    Ok((parameters, results))
}

fn collection_layout(
    function: FunctionId,
    layouts: &LayoutInterner,
    layout: crate::layout::LayoutId,
) -> Result<CollectionLayout, LoweringError> {
    let descriptor = layouts
        .get(layout)
        .ok_or(LoweringError::UnsupportedLayout { function, operation: "layout" })?;
    let (element, raw_runtime_buffer) = match descriptor.kind() {
        LayoutKind::PackedList { element, .. } => (*element, false),
        LayoutKind::RuntimeBuffer { element } => (*element, true),
        _ => return Err(LoweringError::UnsupportedLayout { function, operation: "layout" }),
    };
    let element_descriptor = layouts
        .get(element)
        .ok_or(LoweringError::UnsupportedLayout { function, operation: "layout" })?;
    let (scalar, pointer_layout) = match element_descriptor.kind() {
        LayoutKind::Scalar(scalar) if scalar.is_ordinary_lowering_target() => (*scalar, None),
        LayoutKind::Scalar(ScalarKind::RawByte) if raw_runtime_buffer => {
            (ScalarKind::RawByte, None)
        }
        LayoutKind::RuntimePointerValueSlot {
            pointee,
            semantics: PointerSlot::Owned,
        } => (ScalarKind::U32, Some(*pointee)),
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
    })
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
            Ok(MemoryAccess { object: place.object(), collection, offset, width: 4, scalar: ScalarKind::U32, pointer_layout: None, index: None })
        }
        Projection::Element { index } => Ok(MemoryAccess {
            object: place.object(),
            collection,
            offset: collection.data_offset,
            width: collection.scalar.size(),
            scalar: collection.scalar,
            pointer_layout: collection.pointer_layout,
            index: Some(*index),
        }),
        Projection::Field { .. } | Projection::Slice { .. } => Err(LoweringError::UnsupportedLayout {
            function: function.id,
            operation: "projection",
        }),
    }
}

fn resolve_memory_place_for_values(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    place: &Place,
) -> Result<MemoryAccess, LoweringError> {
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
            Ok(MemoryAccess { object: place.object(), collection, offset, width: 4, scalar: ScalarKind::U32, pointer_layout: None, index: None })
        }
        Projection::Element { index } => Ok(MemoryAccess {
            object: place.object(),
            collection,
            offset: collection.data_offset,
            width: collection.scalar.size(),
            scalar: collection.scalar,
            pointer_layout: collection.pointer_layout,
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
        ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Size | ValueType::V128
    )
}

fn is_indirect_result(value_type: ValueType) -> bool {
    is_indirect_scalar(value_type) || matches!(value_type, ValueType::Ref(_))
}

fn is_indirect_parameter(signature: &FunctionSignature, index: usize, value_type: ValueType) -> bool {
    if is_indirect_scalar(value_type) {
        return true;
    }
    matches!(value_type, ValueType::Ref(_))
        && matches!(signature.contract.parameters.get(index).map(|parameter| parameter.ownership), Some(ParameterOwnership::Shared | ParameterOwnership::WriteBack))
        && (matches!(signature.contract.parameters.get(index).map(|parameter| parameter.ownership), Some(ParameterOwnership::WriteBack))
            || signature.contract.parameter_effects.get(&(index as u32)).is_none_or(|effects| {
                effects.iter().all(|effect| matches!(
                    effect,
                    MemoryEffect::Read | MemoryEffect::Retain | MemoryEffect::Release
                ))
            }))
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
        Operation::ConstInt { destination, value } => ScalarInstruction::IntConst { destination: *destination, value: *value },
        Operation::ConstFloat { destination, bits } => ScalarInstruction::FloatConst { destination: *destination, bits: *bits },
        Operation::ConstBool { destination, value } => ScalarInstruction::BoolConst { destination: *destination, value: *value },
        Operation::ConstSize { destination, value } => ScalarInstruction::SizeConst { destination: *destination, value: *value },
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
            if *class != crate::canonical_safewir::AllocationClass::ReferenceCounted
                || collection_layout(function, layouts, *layout).is_err()
                || !matches!(extent, Extent::Counted { .. })
            {
                return Err(LoweringError::UnsupportedLayout { function, operation: "Alloc" });
            }
            ScalarInstruction::Alloc { destination: *destination, layout: *layout, extent: extent.clone(), class: *class }
        }
        Operation::StaticData { destination, data, .. } => {
            ScalarInstruction::StaticData { destination: *destination, data: *data }
        }
        Operation::Load { destination, place, transfer } => {
            let access = resolve_memory_place_for_values(function, value_types, layouts, place)?;
            let valid_transfer = match access.pointer_layout {
                Some(pointee) => {
                    matches!(transfer, LoadTransfer::CloneShared | LoadTransfer::MoveUnique)
                        && place.value_type() == ValueType::Ref(pointee)
                }
                None => *transfer == LoadTransfer::CopyScalar,
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
                    *transfer == StoreTransfer::MoveUnique
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
            if value_types.get(callee) != Some(&ValueType::Callable(*signature))
                || signature_def.parameters.len() != arguments.len()
                || signature_def.results.len() != destinations.len()
                || signature_def.parameters.iter().copied().enumerate().any(|(index, value_type)| !is_indirect_parameter(signature_def, index, value_type))
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
            if destination_layout.pointer_layout.is_none()
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
            if destination_layout.pointer_layout.is_some()
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
        Operation::ConstInt { .. } => "ConstInt",
        Operation::ConstFloat { .. } => "ConstFloat",
        Operation::ConstBool { .. } => "ConstBool",
        Operation::ConstSize { .. } => "ConstSize",
        Operation::Arithmetic { .. } => "Arithmetic",
        Operation::Compare { .. } => "Compare",
        Operation::NarrowSize { .. } => "NarrowSize",
        Operation::WidenSize { .. } => "WidenSize",
        Operation::Guard { .. } => "Guard",
        Operation::Alloc { .. } => "Alloc",
        Operation::StaticData { .. } => "StaticData",
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
        block.operations.iter().any(|operation| matches!(operation, Operation::VarCall { .. } | Operation::FunctionValue { .. }))
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
        revision: module.revision(),
        checked_source: module.checked_source(),
        launch_contract_payload: module.launch_contract_payload().to_vec(),
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

fn operation_rule(operation: &Operation) -> Option<CorrespondenceRule> {
    match operation {
        Operation::ConstInt { .. }
        | Operation::ConstFloat { .. }
        | Operation::ConstBool { .. }
        | Operation::ConstSize { .. } => Some(CorrespondenceRule::ScalarConstant),
        Operation::Arithmetic { .. } => Some(CorrespondenceRule::ScalarArithmetic),
        Operation::Compare { .. } => Some(CorrespondenceRule::ScalarComparison),
        Operation::NarrowSize { .. } | Operation::WidenSize { .. } => Some(CorrespondenceRule::ScalarConversion),
        Operation::Guard { .. } => Some(CorrespondenceRule::ScalarGuard),
        Operation::MoveValue { .. } => Some(CorrespondenceRule::ScalarMove),
        Operation::MakeInlineAggregate { .. } | Operation::ExtractInlineField { .. } => Some(CorrespondenceRule::ScalarMove),
        Operation::FunctionValue { .. } => Some(CorrespondenceRule::ScalarConstant),
        Operation::Alloc { .. } => Some(CorrespondenceRule::MemoryAlloc),
        Operation::StaticData { .. } => Some(CorrespondenceRule::MemoryStaticData),
        Operation::Load { .. } => Some(CorrespondenceRule::MemoryLoad),
        Operation::Store { .. } => Some(CorrespondenceRule::MemoryStore),
        Operation::CopyBytes { .. } => Some(CorrespondenceRule::MemoryCopy),
        Operation::MoveRange { .. } => Some(CorrespondenceRule::MemoryOwnership),
        Operation::CloneRange { .. } => Some(CorrespondenceRule::MemoryCopy),
        Operation::UpdateCount { .. } => Some(CorrespondenceRule::MemoryUpdateCount),
        Operation::CloneRef { .. }
        | Operation::Release { .. }
        | Operation::DropRef { .. }
        | Operation::Reclaim { .. } => Some(CorrespondenceRule::MemoryOwnership),
        Operation::Call { .. } | Operation::VarCall { .. } => Some(CorrespondenceRule::ScalarCall),
        Operation::HostCall { .. } => Some(CorrespondenceRule::HostCall),
        Operation::Retain { .. } => None,
    }
}

fn matches_operation(operation: &Operation, target: &ScalarInstruction) -> bool {
    match (operation, target) {
        (Operation::ConstInt { destination, value }, ScalarInstruction::IntConst { destination: actual, value: actual_value }) => destination == actual && value == actual_value,
        (Operation::ConstFloat { destination, bits }, ScalarInstruction::FloatConst { destination: actual, bits: actual_bits }) => destination == actual && bits == actual_bits,
        (Operation::ConstBool { destination, value }, ScalarInstruction::BoolConst { destination: actual, value: actual_value }) => destination == actual && value == actual_value,
        (Operation::ConstSize { destination, value }, ScalarInstruction::SizeConst { destination: actual, value: actual_value }) => destination == actual && value == actual_value,
        (Operation::Arithmetic { destination, operator, left, right, overflow }, ScalarInstruction::Arithmetic { destination: actual_destination, operator: actual_operator, left: actual_left, right: actual_right, overflow: actual_overflow }) => destination == actual_destination && operator == actual_operator && left == actual_left && right == actual_right && overflow == actual_overflow,
        (Operation::Compare { destination, operator, left, right }, ScalarInstruction::Compare { destination: actual_destination, operator: actual_operator, left: actual_left, right: actual_right }) => destination == actual_destination && operator == actual_operator && left == actual_left && right == actual_right,
        (Operation::NarrowSize { destination, value }, ScalarInstruction::NarrowSize { destination: actual_destination, value: actual_value }) => destination == actual_destination && value == actual_value,
        (Operation::WidenSize { destination, value }, ScalarInstruction::WidenSize { destination: actual_destination, value: actual_value }) => destination == actual_destination && value == actual_value,
        (Operation::Guard { condition, kind }, ScalarInstruction::Guard { condition: actual_condition, kind: actual_kind }) => condition == actual_condition && kind == actual_kind,
        (Operation::MoveValue { destination, source }, ScalarInstruction::MoveValue { destination: actual_destination, source: actual_source }) => destination == actual_destination && source == actual_source,
        (Operation::MakeInlineAggregate { destination, layout, fields }, ScalarInstruction::MakeInlineAggregate { destination: actual_destination, layout: actual_layout, fields: actual_fields }) => destination == actual_destination && layout == actual_layout && fields == actual_fields,
        (Operation::ExtractInlineField { destination, aggregate, index }, ScalarInstruction::ExtractInlineField { destination: actual_destination, aggregate: actual_aggregate, index: actual_index, .. }) => destination == actual_destination && aggregate == actual_aggregate && index == actual_index,
        (Operation::FunctionValue { destination, function }, ScalarInstruction::FunctionValue { destination: actual_destination, function: actual_function, .. }) => destination == actual_destination && function == actual_function,
        (Operation::Alloc { destination, layout, extent, class }, ScalarInstruction::Alloc { destination: actual_destination, layout: actual_layout, extent: actual_extent, class: actual_class }) => destination == actual_destination && layout == actual_layout && extent == actual_extent && class == actual_class,
        (Operation::StaticData { destination, data, .. }, ScalarInstruction::StaticData { destination: actual_destination, data: actual_data }) => destination == actual_destination && data == actual_data,
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
#[derive(Clone, PartialEq, Eq)]
pub struct AdmittedCanonicalArtifact {
    bytes: Vec<u8>,
    checked_source: crate::canonical_safewir::CheckedSourceIdentity,
    final_module_digest: crate::canonical_safewir::ModuleDigest,
    authority_digest: crate::canonical_safewir::ModuleDigest,
    correspondence_digest: [u8; 32],
    wasm_digest: [u8; 32],
}

impl AdmittedCanonicalArtifact {
    /// Borrow the exact admitted Wasm bytes without transferring ownership.
    pub fn wasm(&self) -> &[u8] {
        &self.bytes
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
    Correspondence,
    SourceWitnessMismatch,
    WasmValidation,
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
        crate::canonical_safewir::ValueType::Bool => Ok(ValType::I32),
        crate::canonical_safewir::ValueType::Size => Ok(ValType::I32),
        crate::canonical_safewir::ValueType::Ref(_) => Ok(ValType::I32),
        crate::canonical_safewir::ValueType::Capability(_) => Ok(ValType::EXTERNREF),
        crate::canonical_safewir::ValueType::V128 => Ok(ValType::V128),
        // SafeWIR never exposes this representation. The callable is an
        // authenticated table-index value only after private WasmIR lowering.
        crate::canonical_safewir::ValueType::Callable(_) => Ok(ValType::I32),
        crate::canonical_safewir::ValueType::InlineAggregate(_) => {
            Err(EncodeError::UnsupportedValueType { function, value, value_type })
        }
        unsupported => Err(EncodeError::UnsupportedValueType { function, value, value_type: unsupported }),
    }
}

struct LocalLayout {
    indices: BTreeMap<ValueId, u32>,
    extra_types: Vec<ValType>,
    scratch: Vec<u32>,
}

fn drop_layout_depth(
    function: FunctionId,
    layouts: &LayoutInterner,
    layout: crate::layout::LayoutId,
    visiting: &mut BTreeSet<crate::layout::LayoutId>,
) -> Result<usize, EncodeError> {
    if !visiting.insert(layout) {
        return Err(EncodeError::UnsupportedLayout { function, operation: "recursive DropRef layout" });
    }
    let collection = collection_layout(function, layouts, layout);
    let depth = match collection {
        Ok(collection) => match collection.pointer_layout {
            Some(child) => 1 + drop_layout_depth(function, layouts, child, visiting)?,
            None => 1,
        },
        Err(_) => 1,
    };
    visiting.remove(&layout);
    Ok(depth)
}

fn local_layout(
    function: &Function,
    signature: &FunctionSignature,
    layouts: &LayoutInterner,
) -> Result<LocalLayout, EncodeError> {
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
    for (value, value_type) in &function.value_types {
        if indices.contains_key(value) {
            continue;
        }
        if matches!(value_type, ValueType::InlineAggregate(_)) {
            continue;
        }
        let index = entry.parameters.len() as u32 + extra_types.len() as u32;
        indices.insert(*value, index);
        extra_types.push(wasm_value_type(function.id, *value, *value_type)?);
    }
    let mut destructor_depth = 1;
    for block in &function.blocks {
        for operation in &block.operations {
            let object = match operation {
                Operation::DropRef { object } | Operation::Release { object } => Some(*object),
                _ => None,
            };
            let Some(object) = object else { continue };
            let Some(ValueType::Ref(object_layout)) = function.value_types.get(&object).copied() else {
                continue;
            };
            destructor_depth = destructor_depth.max(drop_layout_depth(
                function.id,
                layouts,
                object_layout,
                &mut BTreeSet::new(),
            )?);
        }
    }
    let mut scratch = vec![0; destructor_depth.saturating_mul(5).max(5)];
    for (slot, index) in scratch.iter_mut().enumerate() {
        *index = entry.parameters.len() as u32 + extra_types.len() as u32;
        extra_types.push(ValType::I32);
        if slot == 0 {
            // Keep the first scratch slot adjacent to the value locals. The
            // explicit indices below are still independent of value IDs.
        }
    }
    Ok(LocalLayout {
        indices,
        extra_types,
        scratch,
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
            Some(ValueType::Int),
            ArithmeticOperator::DivideSigned | ArithmeticOperator::RemainderSigned,
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
        ScalarKind::Bool | ScalarKind::Tag8 => body.instruction(&wasm_encoder::Instruction::I32Load8U(arg)),
        ScalarKind::Tag16 => body.instruction(&wasm_encoder::Instruction::I32Load16U(arg)),
        ScalarKind::U32 | ScalarKind::Tag32 => body.instruction(&wasm_encoder::Instruction::I32Load(arg)),
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
    emit_get(body, layout, function.id, value)?;
    let arg = memarg(access.width);
    match access.scalar {
        ScalarKind::Bool | ScalarKind::Tag8 => body.instruction(&wasm_encoder::Instruction::I32Store8(arg)),
        ScalarKind::Tag16 => body.instruction(&wasm_encoder::Instruction::I32Store16(arg)),
        ScalarKind::U32 | ScalarKind::Tag32 => body.instruction(&wasm_encoder::Instruction::I32Store(arg)),
        ScalarKind::Int | ScalarKind::Duration => body.instruction(&wasm_encoder::Instruction::I64Store(arg)),
        ScalarKind::Float => body.instruction(&wasm_encoder::Instruction::F64Store(arg)),
        _ => return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "scalar store" }),
    };
    Ok(())
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
    let collection = collection_layout(function.id, layouts, layout_id).map_err(EncodeError::Lowering)?;
    let Extent::Counted { capacity, count, header_bytes, stride_bytes } = extent else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Alloc extent" });
    };
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
    body.instruction(&wasm_encoder::Instruction::GlobalGet(0));
    emit_set_index(body, scratch_index(layout, 1));
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(RC_PREFIX_BYTES)));
    body.instruction(&wasm_encoder::Instruction::I64Add);
    emit_get_index(body, scratch_index(layout, 0));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(LINEAR_MEMORY_BYTES)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(RC_PREFIX_BYTES as i32));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_set_index(body, scratch_index(layout, 2));
    emit_get_index(body, scratch_index(layout, 2));
    emit_get_index(body, scratch_index(layout, 0));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::GlobalSet(0));
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
    Ok(())
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

fn emit_drop_ref_local(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    layouts: &LayoutInterner,
    object_layout: crate::layout::LayoutId,
    depth: usize,
) -> Result<(), EncodeError> {
    let base = depth * 5;
    if base + 4 >= layout.scratch.len() {
        return Err(EncodeError::UnsupportedLayout { function, operation: "DropRef scratch depth" });
    }
    emit_get_index(body, scratch_index(layout, base));
    body.instruction(&wasm_encoder::Instruction::GlobalGet(2));
    body.instruction(&wasm_encoder::Instruction::I32LtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Else);
    emit_get_index(body, scratch_index(layout, base));
    body.instruction(&wasm_encoder::Instruction::I32Const(8));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_set_index(body, scratch_index(layout, base + 1));
    emit_get_index(body, scratch_index(layout, base + 1));
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, base + 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Eq);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    if let Ok(collection) = collection_layout(function, layouts, object_layout) {
        if let Some(child_layout) = collection.pointer_layout {
            if let Some(capacity_offset) = collection.capacity_offset {
                emit_get_index(body, scratch_index(layout, base));
                body.instruction(&wasm_encoder::Instruction::I32Const(collection.length_offset as i32));
                body.instruction(&wasm_encoder::Instruction::I32Add);
                body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
                emit_get_index(body, scratch_index(layout, base));
                body.instruction(&wasm_encoder::Instruction::I32Const(capacity_offset as i32));
                body.instruction(&wasm_encoder::Instruction::I32Add);
                body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
                body.instruction(&wasm_encoder::Instruction::I32GtU);
                body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
                body.instruction(&wasm_encoder::Instruction::Unreachable);
                body.instruction(&wasm_encoder::Instruction::End);
            }
            body.instruction(&wasm_encoder::Instruction::I32Const(0));
            emit_set_index(body, scratch_index(layout, base + 2));
            body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
            body.instruction(&wasm_encoder::Instruction::Loop(BlockType::Empty));
            emit_get_index(body, scratch_index(layout, base + 2));
            emit_get_index(body, scratch_index(layout, base));
            body.instruction(&wasm_encoder::Instruction::I32Const(collection.length_offset as i32));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
            body.instruction(&wasm_encoder::Instruction::I32LtU);
            body.instruction(&wasm_encoder::Instruction::I32Eqz);
            body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
            body.instruction(&wasm_encoder::Instruction::Br(2));
            body.instruction(&wasm_encoder::Instruction::End);
            emit_get_index(body, scratch_index(layout, base));
            body.instruction(&wasm_encoder::Instruction::I32Const(collection.data_offset as i32));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_get_index(body, scratch_index(layout, base + 2));
            body.instruction(&wasm_encoder::Instruction::I32Const(collection.stride as i32));
            body.instruction(&wasm_encoder::Instruction::I32Mul);
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
            emit_set_index(body, scratch_index(layout, base + 4));
            emit_get_index(body, scratch_index(layout, base + 4));
            body.instruction(&wasm_encoder::Instruction::I32Eqz);
            body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
            body.instruction(&wasm_encoder::Instruction::Unreachable);
            body.instruction(&wasm_encoder::Instruction::End);
            emit_get_index(body, scratch_index(layout, base));
            body.instruction(&wasm_encoder::Instruction::I32Const(collection.data_offset as i32));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_get_index(body, scratch_index(layout, base + 2));
            body.instruction(&wasm_encoder::Instruction::I32Const(collection.stride as i32));
            body.instruction(&wasm_encoder::Instruction::I32Mul);
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Const(0));
            body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
            emit_drop_ref_local(body, layout, function, layouts, child_layout, depth + 1)?;
            emit_get_index(body, scratch_index(layout, base + 2));
            body.instruction(&wasm_encoder::Instruction::I32Const(1));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_set_index(body, scratch_index(layout, base + 2));
            body.instruction(&wasm_encoder::Instruction::Br(0));
            body.instruction(&wasm_encoder::Instruction::End);
            body.instruction(&wasm_encoder::Instruction::End);
        }
    }
    emit_reclaim_local(body, layout, base);
    body.instruction(&wasm_encoder::Instruction::Else);
    emit_get_index(body, scratch_index(layout, base));
    body.instruction(&wasm_encoder::Instruction::I32Const(8));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    emit_get_index(body, scratch_index(layout, base + 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(())
}

fn emit_release(body: &mut wasm_encoder::Function, layout: &LocalLayout, function: &Function, object: ValueId, layouts: &LayoutInterner) -> Result<(), EncodeError> {
    let Some(ValueType::Ref(object_layout)) = function.value_types.get(&object).copied() else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "DropRef object" });
    };
    emit_get(body, layout, function.id, object)?;
    emit_set_index(body, scratch_index(layout, 0));
    emit_drop_ref_local(body, layout, function.id, layouts, object_layout, 0)
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
    let HostCallArgument::Buffer { place, extent: HostBufferExtent::Slice } = argument else {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer" }));
    };
    let Projection::Slice { length, .. } = place.projection() else {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer projection" }));
    };
    emit_raw_byte_slice_address(body, layout, function, value_types, layouts, place, AccessMode::Read, *length)?;
    emit_get(body, layout, function, *length)
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
    if destination_layout.pointer_layout.is_none() || destination_layout != source_layout {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { function: function.id, operation: "MoveRange" }));
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
    if destination_layout.pointer_layout.is_some() || destination_layout != source_layout {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { function: function.id, operation: "CloneRange" }));
    }
    let (destination_start, source_start) = match (destination.projection(), source.projection()) {
        (Projection::Slice { start: destination_start, length: destination_length }, Projection::Slice { start: source_start, length: source_length })
            if *destination_length == count && *source_length == count => (*destination_start, *source_start),
        _ => return Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { function: function.id, operation: "CloneRange" })),
    };
    emit_move_range_base(body, function, layout, source.object(), source_start, source_layout, 1)?;
    emit_move_range_base(body, function, layout, destination.object(), destination_start, destination_layout, 2)?;
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, 2)));
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, 1)));
    emit_get(body, layout, function.id, count)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(destination_layout.stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
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
) -> Result<(), EncodeError> {
    match instruction {
        ScalarInstruction::IntConst { destination, value } => { body.instruction(&wasm_encoder::Instruction::I64Const(*value)); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::FloatConst { destination, bits } => { body.instruction(&wasm_encoder::Instruction::F64Const(Ieee64::new(*bits))); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::BoolConst { destination, value } => { body.instruction(&wasm_encoder::Instruction::I32Const(i32::from(*value))); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::SizeConst { destination, value } => {
            let value = u32::try_from(*value).map_err(|_| EncodeError::UnsupportedValueType { function: function.id, value: *destination, value_type: crate::canonical_safewir::ValueType::Size })?;
            body.instruction(&wasm_encoder::Instruction::I32Const(value as i32));
            emit_set(body, layout, function.id, *destination)?;
        }
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
        ScalarInstruction::Load { destination, place, transfer } => {
            let access = resolve_memory_place(function, layouts, place).map_err(EncodeError::Lowering)?;
            emit_typed_load(body, function, layout, access, *destination)?;
            if *transfer == LoadTransfer::CloneShared {
                emit_clone_ref(body, layout, *destination, *destination, function)?;
            }
        }
        ScalarInstruction::Store { place, value, transfer } => {
            if !matches!(*transfer, StoreTransfer::CopyScalar | StoreTransfer::MoveUnique) {
                return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Store transfer" });
            }
            let access = resolve_memory_place(function, layouts, place).map_err(EncodeError::Lowering)?;
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

struct StructuredEmitter<'a> {
    body: &'a mut WasmFunctionEncoder,
    source_function: &'a Function,
    target_function: &'a WasmFunction,
    layout: &'a LocalLayout,
    function_indices: &'a BTreeMap<FunctionId, u32>,
    signature_indices: &'a BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    import_indices: &'a BTreeMap<ImportId, u32>,
    imports: &'a BTreeMap<ImportId, ImportContract>,
    static_data: &'a BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    layouts: &'a LayoutInterner,
    emitted: BTreeSet<crate::canonical_safewir::BlockId>,
    labels: Vec<StructuredLabel>,
    loops: Vec<(crate::canonical_safewir::BlockId, usize)>,
}

impl StructuredEmitter<'_> {
    fn block(&self, id: crate::canonical_safewir::BlockId) -> Result<&WasmBlock, EncodeError> {
        self.target_function
            .blocks
            .iter()
            .find(|block| block.id == id)
            .ok_or(EncodeError::MissingBlock {
                function: self.source_function.id,
                block: id,
            })
    }

    fn successors(
        &self,
        id: crate::canonical_safewir::BlockId,
    ) -> Result<Vec<crate::canonical_safewir::BlockId>, EncodeError> {
        let block = self.block(id)?;
        Ok(match &block.terminator.operation {
            ScalarTerminator::Branch { target, .. } => vec![*target],
            ScalarTerminator::BranchIf {
                then_target,
                else_target,
                ..
            } => vec![*then_target, *else_target],
            ScalarTerminator::Return { .. }
            | ScalarTerminator::TailCall { .. }
            | ScalarTerminator::VarTailCall { .. }
            | ScalarTerminator::Trap
            | ScalarTerminator::Unreachable => Vec::new(),
        })
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
        Ok(self.distances_from(start)?.contains_key(&goal))
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

    /// Check finite paths to a proposed merge.  A terminal path is allowed to
    /// bypass a merge, but a cycle that can avoid it is not.  Recognized loop
    /// headers are treated as structured boundaries: their body must return to
    /// the header (or terminate), and their exit must still reach the merge.
    fn all_paths_reach_or_terminate(
        &self,
        start: crate::canonical_safewir::BlockId,
        merge: crate::canonical_safewir::BlockId,
        active: &mut BTreeSet<crate::canonical_safewir::BlockId>,
    ) -> Result<bool, EncodeError> {
        if start == merge || self.is_terminal(start)? {
            return Ok(true);
        }
        if !active.insert(start) {
            return Ok(false);
        }
        let result = if self.is_loop_header(start)? {
            let block = self.block(start)?;
            let ScalarTerminator::BranchIf {
                then_target,
                else_target,
                ..
            } = &block.terminator.operation
            else {
                unreachable!("is_loop_header checked the terminator");
            };
            self.all_paths_reach_or_terminate(
                *then_target,
                start,
                &mut BTreeSet::new(),
            )? && self.all_paths_reach_or_terminate(*else_target, merge, active)?
        } else {
            let mut result = true;
            for successor in self.successors(start)? {
                if !self.all_paths_reach_or_terminate(successor, merge, active)? {
                    result = false;
                    break;
                }
            }
            result
        };
        active.remove(&start);
        Ok(result)
    }

    fn is_loop_header(
        &self,
        id: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        let block = self.block(id)?;
        let ScalarTerminator::BranchIf {
            then_target,
            else_target,
            ..
        } = &block.terminator.operation
        else {
            return Ok(false);
        };
        if *then_target == id || *else_target == id || then_target == else_target {
            return Ok(false);
        }
        Ok(self.reaches(*then_target, id)? && !self.reaches(*else_target, id)?)
    }

    fn find_join(
        &self,
        current: crate::canonical_safewir::BlockId,
        then_target: crate::canonical_safewir::BlockId,
        else_target: crate::canonical_safewir::BlockId,
    ) -> Result<crate::canonical_safewir::BlockId, EncodeError> {
        if then_target == else_target {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "non-distinct structured branches",
            }));
        }
        let then_distances = self.distances_from(then_target)?;
        let else_distances = self.distances_from(else_target)?;
        let mut best = None;
        for candidate in then_distances.keys().copied() {
            if candidate == current || candidate == then_target || candidate == else_target {
                continue;
            }
            let Some(else_distance) = else_distances.get(&candidate) else {
                continue;
            };
            if !self.all_paths_reach_or_terminate(
                then_target,
                candidate,
                &mut BTreeSet::new(),
            )? || !self.all_paths_reach_or_terminate(
                else_target,
                candidate,
                &mut BTreeSet::new(),
            )? {
                continue;
            }
            let score = (then_distances[&candidate] + else_distance, candidate);
            if best.is_none_or(|previous: (usize, crate::canonical_safewir::BlockId)| score < previous) {
                best = Some(score);
            }
        }
        best.map(|(_, block)| block).ok_or(EncodeError::Lowering(
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
        if stop == Some(start) {
            return Ok(());
        }
        if self.is_loop_header(start)? {
            return self.emit_loop(start, stop);
        }
        if !self.emitted.insert(start) {
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
            )?;
        }
        match block.terminator.operation {
            ScalarTerminator::Branch { target, arguments } => {
                self.emit_branch_arguments(target, &arguments)?;
                if let Some((_, label_index)) = self
                    .loops
                    .iter()
                    .rev()
                    .find(|(header, _)| *header == target)
                {
                    self.body
                        .instruction(&wasm_encoder::Instruction::Br(self.label_depth(*label_index)));
                } else if stop == Some(target) {
                    return Ok(());
                } else {
                    self.emit_path(target, stop)?;
                }
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
            )?,
            terminator => emit_terminator(
                self.body,
                self.source_function,
                self.layout,
                &terminator,
                self.function_indices,
                self.signature_indices,
            )?,
        }
        Ok(())
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
        if self.is_loop_header(current)? {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "loop header outside structured loop",
            }));
        }
        let join = self.find_join(current, then_target, else_target)?;
        emit_get(self.body, self.layout, self.source_function.id, condition)?;
        self.body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        self.labels.push(StructuredLabel::If);
        self.emit_branch_arguments(then_target, then_arguments)?;
        self.emit_path(then_target, Some(join))?;
        self.body.instruction(&wasm_encoder::Instruction::Else);
        self.emit_branch_arguments(else_target, else_arguments)?;
        self.emit_path(else_target, Some(join))?;
        self.body.instruction(&wasm_encoder::Instruction::End);
        self.labels.pop();
        self.emit_path(join, stop)
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
        let block = self.block(header)?.clone();
        let ScalarTerminator::BranchIf {
            condition,
            then_target,
            then_arguments,
            else_target,
            else_arguments,
        } = block.terminator.operation
        else {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "non-conditional loop header",
            }));
        };
        if !self.is_loop_header(header)? {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "non-canonical loop shape",
            }));
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
            )?;
        }
        emit_get(self.body, self.layout, self.source_function.id, condition)?;
        self.body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        self.labels.push(StructuredLabel::If);
        self.loops.push((header, loop_label));
        self.emit_branch_arguments(then_target, &then_arguments)?;
        self.emit_path(then_target, Some(header))?;
        self.loops.pop();
        self.body.instruction(&wasm_encoder::Instruction::Else);
        self.emit_branch_arguments(else_target, &else_arguments)?;
        self.body
            .instruction(&wasm_encoder::Instruction::Br(self.label_depth(block_label)));
        self.body.instruction(&wasm_encoder::Instruction::End);
        self.labels.pop();
        self.body.instruction(&wasm_encoder::Instruction::End);
        self.labels.pop();
        self.body.instruction(&wasm_encoder::Instruction::End);
        self.labels.pop();
        self.emit_path(else_target, stop)
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
) -> Result<WasmFunctionEncoder, EncodeError> {
    let layout = local_layout(source_function, signature, layouts)?;
    let local_declarations: Vec<_> = layout.extra_types.iter().copied().map(|ty| (1, ty)).collect();
    let mut body = WasmFunctionEncoder::new(local_declarations);
    if target_function.entry != source_function.entry {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
            function: source_function.id,
            operation: "entry mismatch",
        }));
    }
    let entry = target_function.entry;
    let mut emitter = StructuredEmitter {
        body: &mut body,
        source_function,
        target_function,
        layout: &layout,
        function_indices,
        signature_indices,
        import_indices,
        imports,
        static_data,
        layouts,
        emitted: BTreeSet::new(),
        labels: Vec::new(),
        loops: Vec::new(),
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
    validated: ValidatedSafeWir,
    source_witness: &witchy_types::pipeline::CanonicalCheckedSource,
) -> Result<CheckedWasmIr, EncodeError> {
    authorize_wasm_ir_inner(validated, Some(source_witness))
}

#[cfg(test)]
fn authorize_wasm_ir_for_test(validated: ValidatedSafeWir) -> Result<CheckedWasmIr, EncodeError> {
    authorize_wasm_ir_inner(validated, None)
}

fn authorize_wasm_ir_inner(
    validated: ValidatedSafeWir,
    source_witness: Option<&witchy_types::pipeline::CanonicalCheckedSource>,
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
    let target = lower_validated(&validated).map_err(EncodeError::Lowering)?;
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
fn encode_checked(checked: &CheckedWasmIr) -> Result<Vec<u8>, EncodeError> {
    let source = &checked.source;
    let target = &checked.target;
    let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&source.layouts().canonical_bytes())
        .map_err(|_| EncodeError::Lowering(LoweringError::InvalidLayoutBundle))?;
    let import_count = u32::try_from(source.imports().len()).map_err(|_| EncodeError::Lowering(LoweringError::UnsupportedOperation { function: FunctionId::new(u32::MAX), operation: "import count" }))?;
    let function_indices: BTreeMap<_, _> = target.functions.iter().enumerate().map(|(index, function)| (function.id, import_count + index as u32)).collect();
    let import_indices: BTreeMap<_, _> = source.imports().iter().enumerate().map(|(index, import)| (import.id, index as u32)).collect();
    let import_contracts: BTreeMap<_, _> = source.imports().iter().map(|import| (import.id, import.clone())).collect();
    let signature_indices: BTreeMap<_, _> = source.signatures().iter().enumerate().map(|(index, signature)| (signature.id, index as u32)).collect();
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
        let (parameters, results) = host_import_wasm_signature(FunctionId::new(u32::MAX), import)
            .map_err(EncodeError::Lowering)?;
        let type_index = u32::try_from(source.signatures().len() + index)
            .map_err(|_| EncodeError::Lowering(LoweringError::UnsupportedOperation { function: FunctionId::new(u32::MAX), operation: "import type count" }))?;
        types.ty().function(parameters, results);
        import_type_indices.insert(import.id, type_index);
    }
    let mut imports = ImportSection::new();
    for import in source.imports() {
        let type_index = import_type_indices.get(&import.id).copied().ok_or(
            EncodeError::Lowering(LoweringError::UnsupportedOperation { function: FunctionId::new(u32::MAX), operation: "HostCall import" }),
        )?;
        imports.import(&import.module, &import.name, EntityType::Function(type_index));
    }
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
        minimum: 1,
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
    let mut code = CodeSection::new();
    let static_data_offsets: BTreeMap<_, _> = target.static_data.iter().map(|segment| (segment.id, segment.offset)).collect();
    let mut data = DataSection::new();
    for segment in &target.static_data {
        data.active(0, &ConstExpr::i32_const(segment.offset as i32), segment.bytes.clone());
    }
    for target_function in &target.functions {
        let source_function = source.functions().iter().find(|function| function.id == target_function.id).ok_or(EncodeError::MissingFunction(target_function.id))?;
        let signature = source.signatures().iter().find(|signature| signature.id == source_function.signature).ok_or(EncodeError::UnsupportedSignature { signature: source_function.signature })?;
        let type_index = signature_indices.get(&source_function.signature).copied().ok_or(EncodeError::UnsupportedSignature { signature: source_function.signature })?;
        functions.function(type_index);
        exports.export(&format!("f{}", target_function.id.get()), ExportKind::Func, function_indices[&target_function.id]);
        code.function(&encode_function(source_function, signature, target_function, &function_indices, &signature_indices, &import_indices, &import_contracts, &static_data_offsets, &layouts)?);
    }
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
    let bytes = module.finish();
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::default() | wasmparser::WasmFeatures::TAIL_CALL)
        .validate_all(&bytes)
        .map_err(|error| EncodeError::WasmValidation(error.to_string()))?;
    Ok(bytes)
}

#[cfg(test)]
fn encode_validated(validated: ValidatedSafeWir) -> Result<Vec<u8>, EncodeError> {
    let checked = authorize_wasm_ir_for_test(validated)?;
    encode_checked(&checked)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PlanToken {
    LocalGet(u32),
    LocalSet(u32),
    I32Const(i32),
    I64Const(i64),
    I64ExtendI32U,
    I64Mul,
    I64Add,
    I64Sub,
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
    Block(String),
    Loop(String),
    If(String),
    Else,
    Br(u32),
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
        wasmparser::Operator::LocalGet { local_index } => PlanToken::LocalGet(local_index),
        wasmparser::Operator::LocalSet { local_index } => PlanToken::LocalSet(local_index),
        wasmparser::Operator::I32Const { value } => PlanToken::I32Const(value),
        wasmparser::Operator::I64Const { value } => PlanToken::I64Const(value),
        wasmparser::Operator::I64ExtendI32U => PlanToken::I64ExtendI32U,
        wasmparser::Operator::I64Mul => PlanToken::I64Mul,
        wasmparser::Operator::I64Add => PlanToken::I64Add,
        wasmparser::Operator::I64Sub => PlanToken::I64Sub,
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
        wasmparser::Operator::Block { blockty } => PlanToken::Block(format!("{blockty:?}")),
        wasmparser::Operator::Loop { blockty } => PlanToken::Loop(format!("{blockty:?}")),
        wasmparser::Operator::If { blockty } => PlanToken::If(format!("{blockty:?}")),
        wasmparser::Operator::Else => PlanToken::Else,
        wasmparser::Operator::Br { relative_depth } => PlanToken::Br(relative_depth),
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
        ScalarKind::Bool | ScalarKind::Tag8 => PlanToken::I32Load8U { offset: 0, align: 0, memory: 0 },
        ScalarKind::Tag16 => PlanToken::I32Load16U { offset: 0, align: 1, memory: 0 },
        ScalarKind::U32 | ScalarKind::Tag32 => memarg(0, 2, 0),
        ScalarKind::Int | ScalarKind::Duration => PlanToken::I64Load { offset: 0, align: 3, memory: 0 },
        ScalarKind::Float => PlanToken::F64Load { offset: 0, align: 3, memory: 0 },
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    });
    push_set(plan, layout, destination)
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
    push_get(plan, layout, value)?;
    plan.push(match access.scalar {
        ScalarKind::Bool | ScalarKind::Tag8 => PlanToken::I32Store8 { offset: 0, align: 0, memory: 0 },
        ScalarKind::Tag16 => PlanToken::I32Store16 { offset: 0, align: 1, memory: 0 },
        ScalarKind::U32 | ScalarKind::Tag32 => PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
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
    let (object, start, length) = runtime_byte_slice(function.id, &function.value_types, layouts, argument)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    push_get(plan, layout, object)?;
    plan.extend([PlanToken::I64ExtendI32U, PlanToken::I64Const(4), PlanToken::I64Add]);
    push_get(plan, layout, start)?;
    plan.extend([PlanToken::I64ExtendI32U, PlanToken::I64Add]);
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
    push_get(plan, layout, length)
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
    if destination_layout.pointer_layout.is_none() || destination_layout != source_layout {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
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
    if destination_layout.pointer_layout.is_some() || destination_layout != source_layout {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    let (destination_start, source_start) = match (destination.projection(), source.projection()) {
        (Projection::Slice { start: destination_start, length: destination_length }, Projection::Slice { start: source_start, length: source_length })
            if *destination_length == count && *source_length == count => (*destination_start, *source_start),
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    };
    push_move_range_base_plan(plan, layout, source.object(), source_start, source_layout, 1)?;
    push_move_range_base_plan(plan, layout, destination.object(), destination_start, destination_layout, 2)?;
    plan.extend([
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::LocalGet(layout.scratch[1]),
    ]);
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

fn push_drop_ref_local_plan(
    plan: &mut Vec<PlanToken>,
    function: FunctionId,
    layouts: &LayoutInterner,
    layout: &LocalLayout,
    object_layout: crate::layout::LayoutId,
    depth: usize,
) -> Result<(), CanonicalAdmissionError> {
    let base = depth * 5;
    if base + 4 >= layout.scratch.len() {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    plan.extend([
        PlanToken::LocalGet(layout.scratch[base]),
        PlanToken::GlobalGet(2),
        PlanToken::I32LtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Else,
        PlanToken::LocalGet(layout.scratch[base]),
        PlanToken::I32Const(8),
        PlanToken::I32Sub,
        PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
        PlanToken::LocalSet(layout.scratch[base + 1]),
        PlanToken::LocalGet(layout.scratch[base + 1]),
        PlanToken::I32Eqz,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
        PlanToken::LocalGet(layout.scratch[base + 1]),
        PlanToken::I32Const(1),
        PlanToken::I32Eq,
        PlanToken::If("Empty".to_owned()),
    ]);
    if let Ok(collection) = collection_layout(function, layouts, object_layout) {
        if let Some(child_layout) = collection.pointer_layout {
            if let Some(capacity_offset) = collection.capacity_offset {
                plan.extend([
                    PlanToken::LocalGet(layout.scratch[base]),
                    PlanToken::I32Const(collection.length_offset as i32),
                    PlanToken::I32Add,
                    PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
                    PlanToken::LocalGet(layout.scratch[base]),
                    PlanToken::I32Const(capacity_offset as i32),
                    PlanToken::I32Add,
                    PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
                    PlanToken::I32GtU,
                    PlanToken::If("Empty".to_owned()),
                    PlanToken::Unreachable,
                    PlanToken::End,
                ]);
            }
            plan.extend([
                PlanToken::I32Const(0),
                PlanToken::LocalSet(layout.scratch[base + 2]),
                PlanToken::Block("Empty".to_owned()),
                PlanToken::Loop("Empty".to_owned()),
                PlanToken::LocalGet(layout.scratch[base + 2]),
                PlanToken::LocalGet(layout.scratch[base]),
                PlanToken::I32Const(collection.length_offset as i32),
                PlanToken::I32Add,
                PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
                PlanToken::I32LtU,
                PlanToken::I32Eqz,
                PlanToken::If("Empty".to_owned()),
                PlanToken::Br(2),
                PlanToken::End,
                PlanToken::LocalGet(layout.scratch[base]),
                PlanToken::I32Const(collection.data_offset as i32),
                PlanToken::I32Add,
                PlanToken::LocalGet(layout.scratch[base + 2]),
                PlanToken::I32Const(collection.stride as i32),
                PlanToken::I32Mul,
                PlanToken::I32Add,
                PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
                PlanToken::LocalSet(layout.scratch[base + 4]),
                PlanToken::LocalGet(layout.scratch[base + 4]),
                PlanToken::I32Eqz,
                PlanToken::If("Empty".to_owned()),
                PlanToken::Unreachable,
                PlanToken::End,
                PlanToken::LocalGet(layout.scratch[base]),
                PlanToken::I32Const(collection.data_offset as i32),
                PlanToken::I32Add,
                PlanToken::LocalGet(layout.scratch[base + 2]),
                PlanToken::I32Const(collection.stride as i32),
                PlanToken::I32Mul,
                PlanToken::I32Add,
                PlanToken::I32Const(0),
                PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
            ]);
            push_drop_ref_local_plan(plan, function, layouts, layout, child_layout, depth + 1)?;
            plan.extend([
                PlanToken::LocalGet(layout.scratch[base + 2]),
                PlanToken::I32Const(1),
                PlanToken::I32Add,
                PlanToken::LocalSet(layout.scratch[base + 2]),
                PlanToken::Br(0),
                PlanToken::End,
                PlanToken::End,
            ]);
        }
    }
    push_reclaim_local_plan(plan, layout, base);
    plan.extend([
        PlanToken::Else,
        PlanToken::LocalGet(layout.scratch[base]),
        PlanToken::I32Const(8),
        PlanToken::I32Sub,
        PlanToken::LocalGet(layout.scratch[base + 1]),
        PlanToken::I32Const(1),
        PlanToken::I32Sub,
        PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
        PlanToken::End,
        PlanToken::End,
    ]);
    Ok(())
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
    plan.push(PlanToken::LocalSet(layout.scratch[0]));
    push_drop_ref_local_plan(plan, function.id, layouts, layout, object_layout, 0)
}

fn expected_alloc_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    collection: CollectionLayout,
    destination: ValueId,
    extent: &Extent,
) -> Result<(), CanonicalAdmissionError> {
    let Extent::Counted { capacity, count, header_bytes, stride_bytes } = extent else {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    };
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
        PlanToken::GlobalGet(0),
        PlanToken::LocalSet(layout.scratch[1]),
    ]);
    plan.extend([
        PlanToken::LocalGet(layout.scratch[1]),
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(RC_PREFIX_BYTES)),
        PlanToken::I64Add,
        PlanToken::LocalGet(layout.scratch[0]),
        PlanToken::I64ExtendI32U,
        PlanToken::I64Add,
        PlanToken::I64Const(i64::from(LINEAR_MEMORY_BYTES)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
        PlanToken::LocalGet(layout.scratch[1]),
        PlanToken::I32Const(RC_PREFIX_BYTES as i32),
        PlanToken::I32Add,
        PlanToken::LocalSet(layout.scratch[2]),
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::LocalGet(layout.scratch[0]),
        PlanToken::I32Add,
        PlanToken::GlobalSet(0),
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
    let _ = function;
    Ok(())
}

fn expected_linear_function_plan(
    function: &Function,
    signature: &FunctionSignature,
    layouts: &LayoutInterner,
    function_indices: &BTreeMap<FunctionId, u32>,
    signature_indices: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    imports: &BTreeMap<ImportId, ImportContract>,
    import_indices: &BTreeMap<ImportId, u32>,
    static_data_offsets: &BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
) -> Result<Vec<PlanToken>, CanonicalAdmissionError> {
    if function.blocks.len() != 1 || function.blocks[0].id != function.entry {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    let layout = local_layout(function, signature, layouts).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let block = &function.blocks[0];
    let mut plan = Vec::new();
    let mut aggregates = BTreeMap::<ValueId, Vec<ValueId>>::new();
    for operation in &block.operations {
        match operation {
            Operation::ConstInt { destination, value } => {
                plan.push(PlanToken::I64Const(*value));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::Arithmetic { destination, operator, left, right, overflow } => {
                if matches!(
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
                } else if *overflow != crate::canonical_safewir::OverflowBehavior::Wrap {
                    return Err(CanonicalAdmissionError::UnsupportedPlan);
                }
                push_get(&mut plan, &layout, *left)?;
                push_get(&mut plan, &layout, *right)?;
                match (function.value_types.get(destination), operator) {
                    (Some(ValueType::Int), ArithmeticOperator::Add) => {
                        plan.push(PlanToken::I64Add)
                    }
                    (Some(ValueType::Int), ArithmeticOperator::Subtract) => {
                        plan.push(PlanToken::I64Sub)
                    }
                    (Some(ValueType::Int), ArithmeticOperator::Multiply) => {
                        plan.push(PlanToken::I64Mul)
                    }
                    (Some(ValueType::Size), ArithmeticOperator::Add) => {
                        plan.push(PlanToken::I32Add)
                    }
                    (Some(ValueType::Size), ArithmeticOperator::Subtract) => {
                        plan.push(PlanToken::I32Sub)
                    }
                    _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
                }
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::ConstBool { destination, value } => {
                plan.push(PlanToken::I32Const(i32::from(*value)));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::ConstSize { destination, value } => {
                let value = u32::try_from(*value)
                    .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
                plan.push(PlanToken::I32Const(value as i32));
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
            Operation::Compare {
                destination,
                operator: operator @ (ComparisonOperator::LessUnsigned | ComparisonOperator::LessEqualUnsigned),
                left,
                right,
            } => {
                push_get(&mut plan, &layout, *left)?;
                push_get(&mut plan, &layout, *right)?;
                plan.push(match operator {
                    ComparisonOperator::LessUnsigned => PlanToken::I32LtU,
                    ComparisonOperator::LessEqualUnsigned => PlanToken::I32LeU,
                    _ => unreachable!("pattern restricts unsigned comparisons"),
                });
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::Compare {
                destination,
                operator: operator @ (ComparisonOperator::LessSigned | ComparisonOperator::GreaterSigned),
                left,
                right,
            } if function.value_types.get(left) == Some(&ValueType::Int) => {
                push_get(&mut plan, &layout, *left)?;
                push_get(&mut plan, &layout, *right)?;
                plan.push(match operator {
                    ComparisonOperator::LessSigned => PlanToken::I64LtS,
                    ComparisonOperator::GreaterSigned => PlanToken::I64GtS,
                    _ => unreachable!("pattern restricts signed comparisons"),
                });
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::Compare {
                destination,
                operator: ComparisonOperator::Equal,
                left,
                right,
            } if function.value_types.get(left) == Some(&ValueType::Int)
                && function.value_types.get(right) == Some(&ValueType::Int) =>
            {
                push_get(&mut plan, &layout, *left)?;
                push_get(&mut plan, &layout, *right)?;
                plan.push(PlanToken::I64Eq);
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
                let collection = collection_layout(function.id, layouts, *layout_id)
                    .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
                expected_alloc_plan(&mut plan, function, &layout, collection, *destination, extent)?;
            }
            Operation::Load { destination, place, transfer }
                if matches!(
                    transfer,
                    LoadTransfer::CopyScalar
                        | LoadTransfer::CloneShared
                        | LoadTransfer::MoveUnique
                ) =>
            {
                push_memory_load_plan(&mut plan, function, layouts, &layout, place, *destination)?;
                if *transfer == LoadTransfer::CloneShared {
                    push_share_plan(&mut plan, &layout, *destination, *destination)?;
                }
            }
            Operation::Store { place, value, transfer }
                if matches!(transfer, StoreTransfer::CopyScalar | StoreTransfer::MoveUnique) =>
            {
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
    match &block.terminator {
        Terminator::Return { values } => {
            for value in values {
                push_get(&mut plan, &layout, *value)?;
            }
            plan.push(PlanToken::Return);
        }
        Terminator::TailCall { callee, arguments, .. } => {
            for argument in arguments {
                push_get(&mut plan, &layout, plan_call_argument(argument)?)?;
            }
            let index = function_indices
                .get(callee)
                .copied()
                .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            plan.push(PlanToken::ReturnCall(index));
        }
        Terminator::VarTailCall { callee, signature, arguments, .. } => {
            for argument in arguments {
                push_get(&mut plan, &layout, plan_call_argument(argument)?)?;
            }
            push_get(&mut plan, &layout, *callee)?;
            plan.push(PlanToken::ReturnCallIndirect {
                type_index: *signature_indices
                    .get(signature)
                    .ok_or(CanonicalAdmissionError::UnsupportedPlan)?,
                table_index: 0,
            });
        }
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    }
    plan.push(PlanToken::End);
    Ok(plan)
}

fn expected_block_operations_plan(
    function: &Function,
    signature: &FunctionSignature,
    block: &BasicBlock,
    layouts: &LayoutInterner,
    function_indices: &BTreeMap<FunctionId, u32>,
    signature_indices: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    imports: &BTreeMap<ImportId, ImportContract>,
    import_indices: &BTreeMap<ImportId, u32>,
    static_data_offsets: &BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
) -> Result<Vec<PlanToken>, CanonicalAdmissionError> {
    // Reuse only the source-operation rules above.  The synthetic terminator
    // is never emitted; it exists solely to satisfy the operation-level
    // planner's normal local-layout checks.  CFG structure is handled by the
    // independent PlanEmitter below.
    let entry = function
        .blocks
        .iter()
        .find(|candidate| candidate.id == function.entry)
        .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
    let synthetic = Function {
        id: function.id,
        signature: function.signature,
        entry: function.entry,
        value_types: function.value_types.clone(),
        blocks: vec![BasicBlock {
            id: function.entry,
            parameters: entry.parameters.clone(),
            operations: block.operations.clone(),
            terminator: Terminator::Return { values: Vec::new() },
        }],
    };
    let mut plan = expected_linear_function_plan(
        &synthetic,
        signature,
        layouts,
        function_indices,
        signature_indices,
        imports,
        import_indices,
        static_data_offsets,
    )?;
    if plan.pop() != Some(PlanToken::End) || plan.pop() != Some(PlanToken::Return) {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    Ok(plan)
}

struct PlanEmitter<'a> {
    source_function: &'a Function,
    signature: &'a FunctionSignature,
    layout: &'a LocalLayout,
    layouts: &'a LayoutInterner,
    function_indices: &'a BTreeMap<FunctionId, u32>,
    signature_indices: &'a BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    imports: &'a BTreeMap<ImportId, ImportContract>,
    import_indices: &'a BTreeMap<ImportId, u32>,
    static_data_offsets: &'a BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    plan: Vec<PlanToken>,
    emitted: BTreeSet<crate::canonical_safewir::BlockId>,
    labels: Vec<StructuredLabel>,
    loops: Vec<(crate::canonical_safewir::BlockId, usize)>,
}

impl PlanEmitter<'_> {
    fn block(&self, id: crate::canonical_safewir::BlockId) -> Result<&BasicBlock, CanonicalAdmissionError> {
        self.source_function
            .blocks
            .iter()
            .find(|block| block.id == id)
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)
    }

    fn successors(&self, id: crate::canonical_safewir::BlockId) -> Result<Vec<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
        Ok(match &self.block(id)?.terminator {
            Terminator::Branch { target, .. } => vec![*target],
            Terminator::BranchIf { then_target, else_target, .. } => vec![*then_target, *else_target],
            Terminator::Return { .. }
            | Terminator::TailCall { .. }
            | Terminator::VarTailCall { .. }
            | Terminator::Trap
            | Terminator::Unreachable => Vec::new(),
        })
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
        Ok(self.distances_from(start)?.contains_key(&goal))
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

    fn all_paths_reach_or_terminate(
        &self,
        start: crate::canonical_safewir::BlockId,
        merge: crate::canonical_safewir::BlockId,
        active: &mut BTreeSet<crate::canonical_safewir::BlockId>,
    ) -> Result<bool, CanonicalAdmissionError> {
        if start == merge || self.is_terminal(start)? {
            return Ok(true);
        }
        if !active.insert(start) {
            return Ok(false);
        }
        let result = if self.is_loop_header(start)? {
            let Terminator::BranchIf { then_target, else_target, .. } = &self.block(start)?.terminator else {
                unreachable!("is_loop_header checked the terminator");
            };
            self.all_paths_reach_or_terminate(*then_target, start, &mut BTreeSet::new())?
                && self.all_paths_reach_or_terminate(*else_target, merge, active)?
        } else {
            let mut result = true;
            for successor in self.successors(start)? {
                if !self.all_paths_reach_or_terminate(successor, merge, active)? {
                    result = false;
                    break;
                }
            }
            result
        };
        active.remove(&start);
        Ok(result)
    }

    fn is_loop_header(&self, id: crate::canonical_safewir::BlockId) -> Result<bool, CanonicalAdmissionError> {
        let Terminator::BranchIf { then_target, else_target, .. } = &self.block(id)?.terminator else {
            return Ok(false);
        };
        if *then_target == id || *else_target == id || then_target == else_target {
            return Ok(false);
        }
        Ok(self.reaches(*then_target, id)? && !self.reaches(*else_target, id)?)
    }

    fn find_join(
        &self,
        current: crate::canonical_safewir::BlockId,
        then_target: crate::canonical_safewir::BlockId,
        else_target: crate::canonical_safewir::BlockId,
    ) -> Result<crate::canonical_safewir::BlockId, CanonicalAdmissionError> {
        if then_target == else_target {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        let then_distances = self.distances_from(then_target)?;
        let else_distances = self.distances_from(else_target)?;
        let mut best = None;
        for candidate in then_distances.keys().copied() {
            if candidate == current || candidate == then_target || candidate == else_target {
                continue;
            }
            let Some(else_distance) = else_distances.get(&candidate) else {
                continue;
            };
            if !self.all_paths_reach_or_terminate(then_target, candidate, &mut BTreeSet::new())?
                || !self.all_paths_reach_or_terminate(else_target, candidate, &mut BTreeSet::new())?
            {
                continue;
            }
            let score = (then_distances[&candidate] + else_distance, candidate);
            if best.is_none_or(|previous: (usize, crate::canonical_safewir::BlockId)| score < previous) {
                best = Some(score);
            }
        }
        best.map(|(_, block)| block).ok_or(CanonicalAdmissionError::UnsupportedPlan)
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
        if stop == Some(start) {
            return Ok(());
        }
        if self.is_loop_header(start)? {
            return self.emit_loop(start, stop);
        }
        if !self.emitted.insert(start) {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        let block = self.block(start)?.clone();
        self.plan.extend(expected_block_operations_plan(
            self.source_function,
            self.signature,
            &block,
            self.layouts,
        self.function_indices,
        self.signature_indices,
            self.imports,
            self.import_indices,
            self.static_data_offsets,
        )?);
        match block.terminator {
            Terminator::Branch { target, arguments } => {
                self.push_branch_arguments(target, &arguments)?;
                if let Some((_, label_index)) = self.loops.iter().rev().find(|(header, _)| *header == target) {
                    self.plan.push(PlanToken::Br(self.label_depth(*label_index)));
                    return Ok(());
                }
                if stop == Some(target) {
                    return Ok(());
                }
                self.emit_path(target, stop)?;
            }
            Terminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments } => {
                self.emit_if(start, condition, then_target, &then_arguments, else_target, &else_arguments, stop)?;
            }
            terminator => self.push_terminator(&terminator)?,
        }
        Ok(())
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
        if self.is_loop_header(current)? {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        let join = self.find_join(current, then_target, else_target)?;
        push_get(&mut self.plan, self.layout, condition)?;
        self.plan.push(PlanToken::If("Empty".to_owned()));
        self.labels.push(StructuredLabel::If);
        self.push_branch_arguments(then_target, then_arguments)?;
        self.emit_path(then_target, Some(join))?;
        self.plan.push(PlanToken::Else);
        self.push_branch_arguments(else_target, else_arguments)?;
        self.emit_path(else_target, Some(join))?;
        self.plan.push(PlanToken::End);
        self.labels.pop();
        self.emit_path(join, stop)
    }

    fn emit_loop(&mut self, header: crate::canonical_safewir::BlockId, stop: Option<crate::canonical_safewir::BlockId>) -> Result<(), CanonicalAdmissionError> {
        if !self.emitted.insert(header) {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        let block = self.block(header)?.clone();
        let Terminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments } = &block.terminator else {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        };
        if !self.is_loop_header(header)? {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        self.plan.push(PlanToken::Block("Empty".to_owned()));
        let block_label = self.labels.len();
        self.labels.push(StructuredLabel::Block);
        self.plan.push(PlanToken::Loop("Empty".to_owned()));
        let loop_label = self.labels.len();
        self.labels.push(StructuredLabel::Loop);
        self.plan.extend(expected_block_operations_plan(self.source_function, self.signature, &block, self.layouts, self.function_indices, self.signature_indices, self.imports, self.import_indices, self.static_data_offsets)?);
        push_get(&mut self.plan, self.layout, *condition)?;
        self.plan.push(PlanToken::If("Empty".to_owned()));
        self.labels.push(StructuredLabel::If);
        self.loops.push((header, loop_label));
        self.push_branch_arguments(*then_target, then_arguments)?;
        self.emit_path(*then_target, Some(header))?;
        self.loops.pop();
        self.plan.push(PlanToken::Else);
        self.push_branch_arguments(*else_target, else_arguments)?;
        self.plan.push(PlanToken::Br(self.label_depth(block_label)));
        self.plan.push(PlanToken::End);
        self.labels.pop();
        self.plan.push(PlanToken::End);
        self.labels.pop();
        self.plan.push(PlanToken::End);
        self.labels.pop();
        self.emit_path(*else_target, stop)
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
) -> Result<Vec<PlanToken>, CanonicalAdmissionError> {
    let layout = local_layout(function, signature, layouts).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let mut emitter = PlanEmitter {
        source_function: function,
        signature,
        layout: &layout,
        layouts,
        function_indices,
        signature_indices,
        imports,
        import_indices,
        static_data_offsets,
        plan: Vec::new(),
        emitted: BTreeSet::new(),
        labels: Vec::new(),
        loops: Vec::new(),
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
    let mut import_type_indices = BTreeMap::new();
    for (index, import) in target.imports.iter().enumerate() {
        let contract = source.imports().iter().find(|candidate| candidate.id == import.id)
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        let (parameters, results) = host_import_wasm_signature(FunctionId::new(u32::MAX), contract)
            .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
        let type_index = u32::try_from(source.signatures().len() + index)
            .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
        types.ty().function(parameters, results);
        import_type_indices.insert(import.id, type_index);
    }
    let mut imports = ImportSection::new();
    for import in &target.imports {
        let type_index = import_type_indices.get(&import.id).copied()
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        let contract = source.imports().iter().find(|candidate| candidate.id == import.id)
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        imports.import(&contract.module, &contract.name, EntityType::Function(type_index));
    }
    let mut functions = FunctionSection::new();
    let mut tables = TableSection::new();
    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    let import_count = u32::try_from(target.imports.len()).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    for (index, function) in target.functions.iter().enumerate() {
        let type_index = signature_indices.get(&function.signature).copied()
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        functions.function(type_index);
        exports.export(&format!("f{}", function.id.get()), ExportKind::Func, import_count + index as u32);
    }
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
    memory.memory(MemoryType { minimum: 1, maximum: None, memory64: false, shared: false, page_size_log2: None });
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
    .map_err(|_| CanonicalAdmissionError::WasmValidation)?;
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
    let import_count = u32::try_from(target.imports.len()).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let function_indices: BTreeMap<_, _> = target.functions.iter().enumerate()
        .map(|(index, function)| (function.id, import_count + index as u32))
        .collect();
    let signature_indices: BTreeMap<_, _> = source.signatures().iter().enumerate()
        .map(|(index, signature)| (signature.id, index as u32))
        .collect();
    let import_indices: BTreeMap<_, _> = target.imports.iter().enumerate()
        .map(|(index, import)| (import.id, index as u32))
        .collect();
    let static_data_offsets: BTreeMap<_, _> = target.static_data.iter()
        .map(|segment| (segment.id, segment.offset))
        .collect();
    let expected = source.functions().iter()
        .map(|function| {
            let signature = source.signatures().iter()
                .find(|signature| signature.id == function.signature)
                .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            let local = local_layout(function, signature, &layouts).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            let locals = local.extra_types.iter().map(|ty| (1, format!("{ty:?}"))).collect();
            let imports = source.imports().iter().map(|import| (import.id, import.clone())).collect::<BTreeMap<_, _>>();
            Ok((locals, expected_function_plan(function, signature, &layouts, &function_indices, &signature_indices, &imports, &import_indices, &static_data_offsets)?))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut actual = Vec::new();
    let mut code_count = None;
    let mut section_order = Vec::new();
    let mut code_started = false;
    let mut actual_data = Vec::new();
    let mut actual_table = None;
    let mut actual_elements = None;
    let mut actual_launch_contract = None;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        match payload.map_err(|_| CanonicalAdmissionError::WasmValidation)? {
            wasmparser::Payload::Version { .. } => {}
            wasmparser::Payload::TypeSection(_) => section_order.push(0),
            wasmparser::Payload::ImportSection(_) => section_order.push(1),
            wasmparser::Payload::FunctionSection(_) => section_order.push(2),
            wasmparser::Payload::TableSection(reader) => {
                if actual_table.is_some() {
                    return Err(CanonicalAdmissionError::PlanMismatch("duplicate table section"));
                }
                let tables = reader.into_iter().collect::<Result<Vec<_>, _>>().map_err(|_| CanonicalAdmissionError::WasmValidation)?;
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
                let elements = reader.into_iter().collect::<Result<Vec<_>, _>>().map_err(|_| CanonicalAdmissionError::WasmValidation)?;
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
                if !matches!(offset.read().map_err(|_| CanonicalAdmissionError::WasmValidation)?, wasmparser::Operator::I32Const { value: 0 })
                    || !matches!(offset.read().map_err(|_| CanonicalAdmissionError::WasmValidation)?, wasmparser::Operator::End)
                {
                    return Err(CanonicalAdmissionError::PlanMismatch("element offset"));
                }
                let wasmparser::ElementItems::Functions(reader) = &element.items else {
                    return Err(CanonicalAdmissionError::PlanMismatch("element expressions"));
                };
                actual_elements = Some(reader.clone().into_iter().collect::<Result<Vec<_>, _>>().map_err(|_| CanonicalAdmissionError::WasmValidation)?);
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
                let mut locals_reader = body.get_locals_reader().map_err(|_| CanonicalAdmissionError::WasmValidation)?;
                let mut locals = Vec::new();
                for _ in 0..locals_reader.get_count() {
                    let (count, ty) = locals_reader.read().map_err(|_| CanonicalAdmissionError::WasmValidation)?;
                    locals.push((count, format!("{ty:?}")));
                }
                let mut operators = body.get_operators_reader().map_err(|_| CanonicalAdmissionError::WasmValidation)?;
                let mut plan = Vec::new();
                while !operators.eof() {
                    plan.push(decode_plan_token(operators.read().map_err(|_| CanonicalAdmissionError::WasmValidation)?)?);
                }
                actual.push((locals, plan));
            }
            wasmparser::Payload::DataSection(reader) => {
                section_order.push(9);
                for data in reader {
                    let data = data.map_err(|_| CanonicalAdmissionError::WasmValidation)?;
                    let wasmparser::DataKind::Active { memory_index, offset_expr } = data.kind else {
                        return Err(CanonicalAdmissionError::PlanMismatch("passive data segment"));
                    };
                    if memory_index != 0 {
                        return Err(CanonicalAdmissionError::PlanMismatch("nonzero data memory"));
                    }
                    let mut operators = offset_expr.get_operators_reader();
                    let wasmparser::Operator::I32Const { value } = operators
                        .read()
                        .map_err(|_| CanonicalAdmissionError::WasmValidation)?
                    else {
                        return Err(CanonicalAdmissionError::PlanMismatch("dynamic data offset"));
                    };
                    let offset = u32::try_from(value)
                        .map_err(|_| CanonicalAdmissionError::PlanMismatch("negative data offset"))?;
                    actual_data.push((offset, data.data.to_vec()));
                }
            }
            wasmparser::Payload::CustomSection(section) => {
                if section.name() != "witchy.launch"
                    || actual_launch_contract.replace(section.data().to_vec()).is_some()
                {
                    return Err(CanonicalAdmissionError::PlanMismatch("launch contract section"));
                }
                section_order.push(10);
            }
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
        return Err(CanonicalAdmissionError::PlanMismatch("function body"));
    }
    if actual_data != expected_data {
        return Err(CanonicalAdmissionError::PlanMismatch("data contents"));
    }
    if actual_launch_contract.as_deref() != Some(target.launch_contract_payload.as_slice()) {
        return Err(CanonicalAdmissionError::PlanMismatch("launch contract payload"));
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
) -> Result<AdmittedCanonicalArtifact, CanonicalAdmissionError> {
    if !target_witness.matches_bytes(&bytes) {
        return Err(CanonicalAdmissionError::PlanMismatch("target witness"));
    }
    let wasm_digest = digest_bytes(b"witchy/canonical-wasmir/wasm/v1", &[&bytes]);
    Ok(AdmittedCanonicalArtifact {
        bytes,
        checked_source,
        final_module_digest,
        authority_digest,
        correspondence_digest: correspondence_digest(&receipt),
        wasm_digest,
    })
}

#[cfg(not(test))]
pub fn admit_canonical(
    validated: ValidatedSafeWir,
    source: &witchy_types::pipeline::CanonicalCheckedSource,
) -> Result<AdmittedCanonicalArtifact, CanonicalAdmissionError> {
    let checked = authorize_wasm_ir(validated, source).map_err(|error| match error {
        EncodeError::Lowering(_) | EncodeError::UnsupportedLayout { .. } => {
            CanonicalAdmissionError::Lowering(format!("{error:?}"))
        }
        EncodeError::Correspondence(_) => CanonicalAdmissionError::Correspondence,
        EncodeError::SourceWitness => CanonicalAdmissionError::SourceWitnessMismatch,
        EncodeError::WasmValidation(_) => CanonicalAdmissionError::WasmValidation,
        _ => CanonicalAdmissionError::UnsupportedPlan,
    })?;
    let bytes = encode_checked(&checked)
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
    admit_parts(source, target, receipt, bytes, checked_source, final_module_digest, authority_digest, target_witness)
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
        EncodeError::WasmValidation(_) => CanonicalAdmissionError::WasmValidation,
        _ => CanonicalAdmissionError::UnsupportedPlan,
    })?;
    let bytes = encode_checked(&checked).map_err(|error| match error {
        EncodeError::WasmValidation(_) => CanonicalAdmissionError::WasmValidation,
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
    admit_parts(source, target, receipt, bytes, checked_source, final_module_digest, authority_digest, target_witness)
}
#[cfg(test)]
mod tests {
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
            Err(CanonicalAdmissionError::PlanMismatch(_) | CanonicalAdmissionError::WasmValidation)
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
                function: FunctionId::new(22),
            }],
        }))
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
                | Err(CanonicalAdmissionError::WasmValidation)
        ));
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
            proper_tail_sites: if tail { vec![crate::canonical_safewir::ProperTailSite { id: tail_site, function: FunctionId::new(1) }] } else { Vec::new() },
        });
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
                    value_types: [(count, ValueType::Size), (capacity, ValueType::Size), (object, ValueType::Ref(layout)), (clone, ValueType::Ref(layout))].into_iter().collect(),
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
                | Err(CanonicalAdmissionError::WasmValidation)
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
                    Operation::ConstSize { destination: index, value: 1 },
                    Operation::Compare { destination: condition, operator: ComparisonOperator::LessUnsigned, left: index, right: count },
                    Operation::Guard { condition, kind: GuardKind::Bounds },
                    Operation::Compare { destination: capacity_condition, operator: ComparisonOperator::LessUnsigned, left: index, right: capacity },
                    Operation::Guard { condition: capacity_condition, kind: GuardKind::Bounds },
                    Operation::Alloc { destination: object, layout: list, extent: Extent::Counted { header_bytes: 8, count, capacity, stride_bytes: 4 }, class: crate::canonical_safewir::AllocationClass::ReferenceCounted },
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
                    Operation::UpdateCount { object, count: new_count },
                    Operation::Load { destination: capacity_loaded, place: Place::projected(object, Projection::Header(HeaderField::Capacity), ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                    Operation::ConstSize { destination: second_index, value: 1 },
                    Operation::Compare { destination: second_condition, operator: ComparisonOperator::LessUnsigned, left: second_index, right: new_count },
                    Operation::Guard { condition: second_condition, kind: GuardKind::Bounds },
                    Operation::Compare { destination: second_capacity_condition, operator: ComparisonOperator::LessUnsigned, left: second_index, right: capacity },
                    Operation::Guard { condition: second_capacity_condition, kind: GuardKind::Bounds },
                    Operation::ConstSize { destination: second_value, value: 42 },
                    Operation::Store { place: Place::projected(object, Projection::Element { index: second_index }, ValueType::Size, AccessMode::Write), value: second_value, transfer: StoreTransfer::CopyScalar },
                    Operation::Load { destination: length_loaded, place: Place::projected(object, Projection::Header(HeaderField::Length), ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                    Operation::Load { destination: element_loaded, place: Place::projected(object, Projection::Element { index: second_index }, ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                    Operation::Reclaim { object },
                ],
                vec![element_loaded],
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
        if !oob {
            for value in [capacity_loaded, new_count, second_index, value, second_value, length_loaded] {
                value_types.insert(value, ValueType::Size);
            }
            value_types.insert(second_condition, ValueType::Bool);
            value_types.insert(second_capacity_condition, ValueType::Bool);
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
        let count = ValueId::new(1);
        let capacity = ValueId::new(2);
        let object = ValueId::new(3);
        finalize(CanonicalModule::from_lowering(LoweredModuleParts {
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
                    operations: vec![
                        Operation::ConstSize { destination: count, value: 0 },
                        Operation::ConstSize { destination: capacity, value: capacity_value },
                        Operation::Alloc { destination: object, layout, extent: Extent::Counted { header_bytes, count, capacity, stride_bytes }, class },
                    ],
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            }],
            proper_tail_sites: Vec::new(),
        }))
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

    fn checked_scalar_clone_range(range_count: u64, source_start: u64, destination_start: u64) -> ValidatedSafeWir {
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
                        Operation::CloneRange {
                            destination: destination_place,
                            source: source_place,
                            count: range_count_value,
                        },
                        Operation::Load {
                            destination: loaded,
                            place: Place::projected(destination, Projection::Element { index }, ValueType::Int, AccessMode::Read),
                            transfer: LoadTransfer::CopyScalar,
                        },
                        Operation::Reclaim { object: source },
                        Operation::Reclaim { object: destination },
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
                        Operation::Load { destination: loaded, place: Place::projected(shared, Projection::Header(HeaderField::Length), ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                        Operation::DropRef { object: shared },
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
        let layout = LocalLayout {
            indices: [(object, 0), (destination, 1)].into_iter().collect(),
            extra_types: vec![ValType::I32; 5],
            scratch: vec![2, 3, 4, 5],
        };
        let mut boundary = WasmFunctionEncoder::new(vec![(5, ValType::I32)]);
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
        let signature = scalar_signature_with(FunctionSignatureId::new(36), Vec::new(), Vec::new());
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
                    ],
                    terminator: Terminator::Return { values: Vec::new() },
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
        let run = instance.get_typed_func::<(), ()>(&mut store, "f36").unwrap();
        run.call(&mut store, ()).unwrap();
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        let rc = u32::from_le_bytes(memory.data(&store)[16..20].try_into().unwrap());
        assert_eq!(rc, 2);
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
            Err(CanonicalAdmissionError::WasmValidation | CanonicalAdmissionError::PlanMismatch(_))
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
        let wasm = encode_validated(source).expect("packed scalar collection encodes");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance.get_typed_func::<(), i32>(&mut store, "f30").unwrap();
        assert_eq!(run.call(&mut store, ()).unwrap(), 42);
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
    fn shared_destination_preserves_root_for_use() {
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
    fn allocation_refuses_to_return_storage_past_fixed_linear_memory() {
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
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance.get_typed_func::<(), ()>(&mut store, "f32").unwrap();
        assert!(run.call(&mut store, ()).is_err());
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
    fn unsupported_allocator_class_is_refused_by_canonical_backend() {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::U32).unwrap();
        let layout = interner.intern_packed_list(element, RcHeader::Required).unwrap();
        let bundle = LayoutBundle::from_interner(&interner, [layout]).unwrap();
        assert!(matches!(
            encode_validated(checked_alloc_only(layout, bundle, crate::canonical_safewir::AllocationClass::Bump, 8, 4, 1)),
            Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { operation: "Alloc", .. }))
        ));
    }

    #[test]
    fn pointer_bearing_catalog_layout_is_refused_by_canonical_backend() {
        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::U32).unwrap();
        let layout = interner.intern_runtime_pointer_buffer(element, PointerSlot::Owned).unwrap();
        let bundle = LayoutBundle::from_interner(&interner, [layout]).unwrap();
        assert!(matches!(
            encode_validated(checked_alloc_only(layout, bundle, crate::canonical_safewir::AllocationClass::ReferenceCounted, 4, 8, 1)),
            Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { operation: "Alloc", .. }))
        ));
    }

    #[cfg(feature = "native")]
    fn run_scalar_clone_range(range_count: u64, source_start: u64, destination_start: u64) -> Result<i64, wasmtime::Error> {
        let artifact = admit_canonical(checked_scalar_clone_range(range_count, source_start, destination_start))
            .expect("scalar CloneRange fixture admits before execution");
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
    #[test]
    fn scalar_clone_range_admits_emits_and_matches_exact_correspondence() {
        let validated = checked_scalar_clone_range(1, 0, 0);
        let target = lower_validated(&validated).expect("scalar CloneRange lowers");
        assert!(target.functions.iter().flat_map(|function| function.blocks.iter()).flat_map(|block| block.instructions.iter()).any(
            |instruction| matches!(instruction.operation, ScalarInstruction::CloneRange { .. })
        ));
        let receipt = check_correspondence(validated.module(), &target).expect("CloneRange correspondence is exact");
        assert!(receipt.mappings.iter().any(|mapping| mapping.rule == CorrespondenceRule::MemoryCopy));

        let artifact = admit_canonical(validated).expect("scalar CloneRange admits with its exact plan");
        wasmparser::validate(artifact.wasm()).expect("admitted scalar CloneRange is valid Wasm");
        assert!(artifact.wasm().windows(2).any(|window| window == [0xfc, 0x0a]));
        assert_eq!(run_scalar_clone_range(1, 0, 0).unwrap(), 42);
    }

    #[test]
    fn pointer_bearing_clone_range_is_refused_by_canonical_backend() {
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
