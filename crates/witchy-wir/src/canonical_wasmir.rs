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

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, DataSection, ExportKind, ExportSection,
    Function as WasmFunctionEncoder, FunctionSection, GlobalSection, GlobalType, Ieee64,
    MemArg, MemorySection, MemoryType, Module as WasmModule, TypeSection, ValType,
};

use crate::canonical_safewir::{
    AccessMode, ArithmeticOperator, BasicBlock, CallArgument, CanonicalModule,
    ComparisonOperator, Extent, Function, FunctionId, HeaderField, LoadTransfer,
    ModuleStage, Operation, OptimizedValid, Place, Projection, StoreTransfer, Terminator,
    ValidatedSafeWir, ValueId, ValueType,
};
use crate::layout::{HeaderLayout, LayoutInterner, LayoutKind, LayoutSize, RcHeader, ScalarKind};

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
    Guard { condition: ValueId, kind: crate::canonical_safewir::GuardKind },
    MoveValue { destination: ValueId, source: ValueId },
    Alloc {
        destination: ValueId,
        layout: crate::layout::LayoutId,
        extent: Extent,
        class: crate::canonical_safewir::AllocationClass,
    },
    Load { destination: ValueId, place: Place, transfer: LoadTransfer },
    Store { place: Place, value: ValueId, transfer: StoreTransfer },
    UpdateCount { object: ValueId, count: ValueId },
    Share { destination: ValueId, source: ValueId },
    Release { object: ValueId },
    Reclaim { object: ValueId },
    Call { destinations: Vec<ValueId>, callee: FunctionId, arguments: Vec<ValueId> },
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemoryAccess {
    object: ValueId,
    collection: CollectionLayout,
    offset: u32,
    width: u32,
    scalar: ScalarKind,
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
    functions: Vec<WasmFunction>,
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
    MemoryLoad,
    MemoryStore,
    MemoryUpdateCount,
    MemoryOwnership,
    ScalarCall,
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum LoweringError {
    UnsupportedOperation { function: FunctionId, operation: &'static str },
    UnsupportedCallArgument { function: FunctionId },
    UnsupportedOverflow { function: FunctionId },
    UnsupportedControlFlow { function: FunctionId, operation: &'static str },
    UnsupportedLayout { function: FunctionId, operation: &'static str },
    InvalidLayoutBundle,
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
    function: FunctionId,
    arguments: &[CallArgument],
) -> Result<Vec<ValueId>, LoweringError> {
    arguments
        .iter()
        .map(|argument| match argument {
            CallArgument::Value(value) => Ok(*value),
            CallArgument::Move(_) | CallArgument::Share(_) | CallArgument::Borrow(_) => {
                Err(LoweringError::UnsupportedCallArgument { function })
            }
        })
        .collect()
}

fn collection_layout(
    function: FunctionId,
    layouts: &LayoutInterner,
    layout: crate::layout::LayoutId,
) -> Result<CollectionLayout, LoweringError> {
    let descriptor = layouts
        .get(layout)
        .ok_or(LoweringError::UnsupportedLayout { function, operation: "layout" })?;
    let element = match descriptor.kind() {
        LayoutKind::PackedList { element, .. } => *element,
        LayoutKind::RuntimeBuffer { .. } => {
            return Err(LoweringError::UnsupportedLayout { function, operation: "RuntimeBuffer" });
        }
        _ => return Err(LoweringError::UnsupportedLayout { function, operation: "layout" }),
    };
    let element_descriptor = layouts
        .get(element)
        .ok_or(LoweringError::UnsupportedLayout { function, operation: "layout" })?;
    let scalar = match element_descriptor.kind() {
        LayoutKind::Scalar(scalar) if scalar.is_ordinary_lowering_target() => *scalar,
        _ => return Err(LoweringError::UnsupportedLayout { function, operation: "pointer-bearing layout" }),
    };
    let LayoutSize::Dynamic { base, stride } = descriptor.size() else {
        return Err(LoweringError::UnsupportedLayout { function, operation: "layout" });
    };
    let (length_offset, capacity_offset, data_offset) = match descriptor.header() {
        HeaderLayout::PackedList { length_offset, capacity_offset, data_offset, rc: RcHeader::Required } =>
            (length_offset, Some(capacity_offset), data_offset),
        _ => return Err(LoweringError::UnsupportedLayout { function, operation: "layout" }),
    };
    let LayoutSize::Fixed(element_size) = element_descriptor.size() else {
        return Err(LoweringError::UnsupportedLayout { function, operation: "layout" });
    };
    if data_offset != base || stride != element_size {
        return Err(LoweringError::UnsupportedLayout { function, operation: "layout" });
    }
    Ok(CollectionLayout { layout, length_offset, capacity_offset, data_offset, stride, scalar })
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
            Ok(MemoryAccess { object: place.object(), collection, offset, width: 4, scalar: ScalarKind::U32, index: None })
        }
        Projection::Element { index } => Ok(MemoryAccess {
            object: place.object(),
            collection,
            offset: collection.data_offset,
            width: collection.scalar.size(),
            scalar: collection.scalar,
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
            Ok(MemoryAccess { object: place.object(), collection, offset, width: 4, scalar: ScalarKind::U32, index: None })
        }
        Projection::Element { index } => Ok(MemoryAccess {
            object: place.object(),
            collection,
            offset: collection.data_offset,
            width: collection.scalar.size(),
            scalar: collection.scalar,
            index: Some(*index),
        }),
        Projection::Field { .. } | Projection::Slice { .. } => Err(LoweringError::UnsupportedLayout {
            function,
            operation: "projection",
        }),
    }
}

fn lower_operation(
    function: FunctionId,
    block: crate::canonical_safewir::BlockId,
    index: usize,
    operation: &Operation,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    allocation_classes: &mut BTreeMap<ValueId, crate::canonical_safewir::AllocationClass>,
) -> Result<WasmInstruction, LoweringError> {
    let source = SourceItemId::Operation { function, block, index };
    let operation = match operation {
        Operation::ConstInt { destination, value } => ScalarInstruction::IntConst { destination: *destination, value: *value },
        Operation::ConstFloat { destination, bits } => ScalarInstruction::FloatConst { destination: *destination, bits: *bits },
        Operation::ConstBool { destination, value } => ScalarInstruction::BoolConst { destination: *destination, value: *value },
        Operation::ConstSize { destination, value } => ScalarInstruction::SizeConst { destination: *destination, value: *value },
        Operation::Arithmetic { destination, operator, left, right, overflow } => {
            if *overflow == crate::canonical_safewir::OverflowBehavior::Trap {
                return Err(LoweringError::UnsupportedOverflow { function });
            }
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
        Operation::NarrowSize { .. } => {
            return Err(LoweringError::UnsupportedOperation { function, operation: "NarrowSize" });
        }
        Operation::Guard { condition, kind } => ScalarInstruction::Guard { condition: *condition, kind: *kind },
        Operation::MoveValue { destination, source } => {
            if matches!(value_types.get(source), Some(ValueType::Ref(_))) {
                if let Some(class) = allocation_classes.remove(source) {
                    allocation_classes.insert(*destination, class);
                }
            }
            ScalarInstruction::MoveValue { destination: *destination, source: *source }
        }
        Operation::Alloc { destination, layout, extent, class } => {
            if *class != crate::canonical_safewir::AllocationClass::ReferenceCounted
                || collection_layout(function, layouts, *layout).is_err()
                || !matches!(extent, Extent::Counted { .. })
            {
                return Err(LoweringError::UnsupportedLayout { function, operation: "Alloc" });
            }
            allocation_classes.insert(*destination, *class);
            ScalarInstruction::Alloc { destination: *destination, layout: *layout, extent: extent.clone(), class: *class }
        }
        Operation::Load { destination, place, transfer } => {
            if *transfer != LoadTransfer::CopyScalar || place.access() != AccessMode::Read
                || resolve_memory_place_for_values(function, value_types, layouts, place).is_err()
            {
                return Err(LoweringError::UnsupportedLayout { function, operation: "Load" });
            }
            ScalarInstruction::Load { destination: *destination, place: place.clone(), transfer: *transfer }
        }
        Operation::Store { place, value, transfer } => {
            if *transfer != StoreTransfer::CopyScalar || place.access() != AccessMode::Write
                || resolve_memory_place_for_values(function, value_types, layouts, place).is_err()
            {
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
        Operation::Share { destination, source } => {
            if allocation_classes.get(source) != Some(&crate::canonical_safewir::AllocationClass::ReferenceCounted) {
                return Err(LoweringError::UnsupportedLayout { function, operation: "Share" });
            }
            allocation_classes.insert(*destination, crate::canonical_safewir::AllocationClass::ReferenceCounted);
            ScalarInstruction::Share { destination: *destination, source: *source }
        }
        Operation::Release { object } | Operation::Reclaim { object } => {
            if allocation_classes.get(object) != Some(&crate::canonical_safewir::AllocationClass::ReferenceCounted) {
                return Err(LoweringError::UnsupportedLayout { function, operation: operation_name(operation) });
            }
            allocation_classes.remove(object);
            match operation {
                Operation::Release { object } => ScalarInstruction::Release { object: *object },
                Operation::Reclaim { object } => ScalarInstruction::Reclaim { object: *object },
                _ => unreachable!(),
            }
        }
        Operation::Call { destinations, callee, arguments } => ScalarInstruction::Call {
            destinations: destinations.clone(),
            callee: *callee,
            arguments: scalar_arguments(function, arguments)?,
        },
        Operation::VarCall { .. } => {
            return Err(LoweringError::UnsupportedOperation { function, operation: "VarCall" });
        }
        Operation::StaticData { .. }
        | Operation::CopyBytes { .. }
        | Operation::MoveRange { .. }
        | Operation::CloneRange { .. }
        | Operation::Retain { .. }
        | Operation::HostCall { .. } => {
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
        Operation::Share { .. } => "Share",
        Operation::Retain { .. } => "Retain",
        Operation::Release { .. } => "Release",
        Operation::Reclaim { .. } => "Reclaim",
        Operation::Call { .. } => "Call",
        Operation::VarCall { .. } => "VarCall",
        Operation::HostCall { .. } => "HostCall",
    }
}

fn lower_terminator(
    function: FunctionId,
    block: crate::canonical_safewir::BlockId,
    terminator: &Terminator,
) -> Result<WasmTerminator, LoweringError> {
    let source = SourceItemId::Terminator { function, block };
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
            arguments: scalar_arguments(function, arguments)?,
        },
        Terminator::VarTailCall { .. } => {
            return Err(LoweringError::UnsupportedOperation { function, operation: "VarTailCall" });
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
    allocation_classes: &mut BTreeMap<ValueId, crate::canonical_safewir::AllocationClass>,
) -> Result<WasmBlock, LoweringError> {
    let instructions = block
        .operations
        .iter()
        .enumerate()
        .map(|(index, operation)| lower_operation(function.id, block.id, index, operation, &function.value_types, layouts, allocation_classes))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WasmBlock {
        id: block.id,
        instructions,
        terminator: lower_terminator(function.id, block.id, &block.terminator)?,
    })
}

fn lower_function(function: &Function, layouts: &LayoutInterner) -> Result<WasmFunction, LoweringError> {
    let mut allocation_classes = BTreeMap::new();
    Ok(WasmFunction {
        id: function.id,
        signature: function.signature,
        entry: function.entry,
        blocks: function
            .blocks
            .iter()
            .map(|block| lower_block(function, block, layouts, &mut allocation_classes))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn lower_validated(module: &ValidatedSafeWir) -> Result<WasmIr, LoweringError> {
    let module = module.module();
    let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&module.layouts().canonical_bytes())
        .map_err(|_| LoweringError::InvalidLayoutBundle)?;
    Ok(WasmIr {
        revision: module.revision(),
        checked_source: module.checked_source(),
        functions: module
            .functions()
            .iter()
            .map(|function| lower_function(function, &layouts))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn operation_rule(operation: &Operation) -> Option<CorrespondenceRule> {
    match operation {
        Operation::ConstInt { .. }
        | Operation::ConstFloat { .. }
        | Operation::ConstBool { .. }
        | Operation::ConstSize { .. } => Some(CorrespondenceRule::ScalarConstant),
        Operation::Arithmetic { .. } => Some(CorrespondenceRule::ScalarArithmetic),
        Operation::Compare { .. } => Some(CorrespondenceRule::ScalarComparison),
        Operation::NarrowSize { .. } => None,
        Operation::Guard { .. } => Some(CorrespondenceRule::ScalarGuard),
        Operation::MoveValue { .. } => Some(CorrespondenceRule::ScalarMove),
        Operation::Alloc { .. } => Some(CorrespondenceRule::MemoryAlloc),
        Operation::Load { .. } => Some(CorrespondenceRule::MemoryLoad),
        Operation::Store { .. } => Some(CorrespondenceRule::MemoryStore),
        Operation::UpdateCount { .. } => Some(CorrespondenceRule::MemoryUpdateCount),
        Operation::Share { .. }
        | Operation::Release { .. }
        | Operation::Reclaim { .. } => Some(CorrespondenceRule::MemoryOwnership),
        Operation::Call { .. } | Operation::VarCall { .. } => Some(CorrespondenceRule::ScalarCall),
        Operation::StaticData { .. }
        | Operation::CopyBytes { .. }
        | Operation::MoveRange { .. }
        | Operation::CloneRange { .. }
        | Operation::Retain { .. }
        | Operation::HostCall { .. } => None,
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
        (Operation::Guard { condition, kind }, ScalarInstruction::Guard { condition: actual_condition, kind: actual_kind }) => condition == actual_condition && kind == actual_kind,
        (Operation::MoveValue { destination, source }, ScalarInstruction::MoveValue { destination: actual_destination, source: actual_source }) => destination == actual_destination && source == actual_source,
        (Operation::Alloc { destination, layout, extent, class }, ScalarInstruction::Alloc { destination: actual_destination, layout: actual_layout, extent: actual_extent, class: actual_class }) => destination == actual_destination && layout == actual_layout && extent == actual_extent && class == actual_class,
        (Operation::Load { destination, place, transfer }, ScalarInstruction::Load { destination: actual_destination, place: actual_place, transfer: actual_transfer }) => destination == actual_destination && place == actual_place && transfer == actual_transfer,
        (Operation::Store { place, value, transfer }, ScalarInstruction::Store { place: actual_place, value: actual_value, transfer: actual_transfer }) => place == actual_place && value == actual_value && transfer == actual_transfer,
        (Operation::UpdateCount { object, count }, ScalarInstruction::UpdateCount { object: actual_object, count: actual_count }) => object == actual_object && count == actual_count,
        (Operation::Share { destination, source }, ScalarInstruction::Share { destination: actual_destination, source: actual_source }) => destination == actual_destination && source == actual_source,
        (Operation::Release { object }, ScalarInstruction::Release { object: actual_object }) | (Operation::Reclaim { object }, ScalarInstruction::Reclaim { object: actual_object }) => object == actual_object,
        (Operation::Call { destinations, callee, arguments }, ScalarInstruction::Call { destinations: actual_destinations, callee: actual_callee, arguments: actual_arguments }) => *callee == *actual_callee && destinations == actual_destinations && scalar_argument_values(arguments) == Some(actual_arguments.clone()),
        (Operation::VarCall { destinations, callee, signature, arguments }, ScalarInstruction::VarCall { destinations: actual_destinations, callee: actual_callee, signature: actual_signature, arguments: actual_arguments }) => *callee == *actual_callee && *signature == *actual_signature && destinations == actual_destinations && scalar_argument_values(arguments) == Some(actual_arguments.clone()),
        _ => false,
    }
}

fn scalar_argument_values(arguments: &[CallArgument]) -> Option<Vec<ValueId>> {
    arguments.iter().map(|argument| match argument {
        CallArgument::Value(value) => Some(*value),
        CallArgument::Move(_) | CallArgument::Share(_) | CallArgument::Borrow(_) => None,
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
    source: &CanonicalModule<OptimizedValid>,
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
    UnsupportedValueType { function: FunctionId, value: ValueId, value_type: crate::canonical_safewir::ValueType },
    UnsupportedSignature { signature: crate::canonical_safewir::FunctionSignatureId },
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
    history_digest: crate::canonical_safewir::ModuleDigest,
    correspondence_digest: [u8; 32],
    wasm_digest: [u8; 32],
}

impl AdmittedCanonicalArtifact {
    /// Borrow the exact admitted Wasm bytes without transferring ownership.
    pub fn wasm(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalAdmissionError {
    Lowering,
    Correspondence,
    WasmValidation,
    UnsupportedPlan,
    PlanMismatch,
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
        crate::canonical_safewir::ValueType::V128 => Ok(ValType::V128),
        unsupported => Err(EncodeError::UnsupportedValueType { function, value, value_type: unsupported }),
    }
}

struct LocalLayout {
    indices: BTreeMap<ValueId, u32>,
    extra_types: Vec<ValType>,
    scratch: [u32; 4],
}

fn local_layout(function: &Function) -> Result<LocalLayout, EncodeError> {
    let entry = function
        .blocks
        .iter()
        .find(|block| block.id == function.entry)
        .ok_or(EncodeError::MissingBlock { function: function.id, block: function.entry })?;
    let mut indices = BTreeMap::new();
    for (index, parameter) in entry.parameters.iter().enumerate() {
        indices.insert(parameter.value, index as u32);
    }
    let mut extra_types = Vec::new();
    for (value, value_type) in &function.value_types {
        if indices.contains_key(value) {
            continue;
        }
        let index = entry.parameters.len() as u32 + extra_types.len() as u32;
        indices.insert(*value, index);
        extra_types.push(wasm_value_type(function.id, *value, *value_type)?);
    }
    let mut scratch = [0; 4];
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

fn emit_share(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    destination: ValueId,
    object: ValueId,
    function: &Function,
) -> Result<(), EncodeError> {
    emit_get(body, layout, function.id, object)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(8));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_set_index(body, scratch_index(layout, 2));
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Const(-1));
    body.instruction(&wasm_encoder::Instruction::I32Eq);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_set_index(body, scratch_index(layout, 2));
    emit_get(body, layout, function.id, object)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(8));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    emit_get(body, layout, function.id, object)?;
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

fn emit_release(body: &mut wasm_encoder::Function, layout: &LocalLayout, function: &Function, object: ValueId) -> Result<(), EncodeError> {
    emit_get(body, layout, function.id, object)?;
    emit_set_index(body, scratch_index(layout, 1));
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
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Eq);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    emit_reclaim(body, layout, function, object)?;
    body.instruction(&wasm_encoder::Instruction::Else);
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(8));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(())
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
            if *overflow != crate::canonical_safewir::OverflowBehavior::Wrap { return Err(EncodeError::Lowering(LoweringError::UnsupportedOverflow { function: function.id })); }
            emit_arithmetic(body, function, layout, *destination, *operator, *left, *right)?;
        }
        ScalarInstruction::Compare { destination, operator, left, right } => { emit_compare(body, function, layout, *destination, *operator, *left, *right)?; }
        ScalarInstruction::Guard { condition, .. } => { emit_get(body, layout, function.id, *condition)?; body.instruction(&wasm_encoder::Instruction::I32Eqz); body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty)); body.instruction(&wasm_encoder::Instruction::Unreachable); body.instruction(&wasm_encoder::Instruction::End); }
        ScalarInstruction::MoveValue { destination, source } => { emit_get(body, layout, function.id, *source)?; emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::Alloc { destination, layout: layout_id, extent, class } => {
            if *class != crate::canonical_safewir::AllocationClass::ReferenceCounted {
                return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Alloc class" });
            }
            emit_alloc(body, function, layout, layouts, *destination, *layout_id, extent)?;
        }
        ScalarInstruction::Load { destination, place, transfer } => {
            if *transfer != LoadTransfer::CopyScalar {
                return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Load transfer" });
            }
            let access = resolve_memory_place(function, layouts, place).map_err(EncodeError::Lowering)?;
            emit_typed_load(body, function, layout, access, *destination)?;
        }
        ScalarInstruction::Store { place, value, transfer } => {
            if *transfer != StoreTransfer::CopyScalar {
                return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Store transfer" });
            }
            let access = resolve_memory_place(function, layouts, place).map_err(EncodeError::Lowering)?;
            emit_typed_store(body, function, layout, access, *value)?;
        }
        ScalarInstruction::UpdateCount { object, count } => emit_update_count(body, function, layout, layouts, *object, *count)?,
        ScalarInstruction::Share { destination, source } => emit_share(body, layout, *destination, *source, function)?,
        ScalarInstruction::Release { object } => emit_release(body, layout, function, *object)?,
        ScalarInstruction::Reclaim { object } => emit_reclaim(body, layout, function, *object)?,
        ScalarInstruction::Call { destinations, callee, arguments } => {
            emit_call_arguments(body, layout, function.id, arguments)?;
            let index = function_indices.get(callee).copied().ok_or(EncodeError::MissingFunction(*callee))?;
            body.instruction(&wasm_encoder::Instruction::Call(index));
            emit_results(body, layout, function.id, destinations)?;
        }
        ScalarInstruction::VarCall { .. } => return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation { function: function.id, operation: "VarCall" })),
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
        ScalarTerminator::VarTailCall { .. } => return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation { function: source_function.id, operation: "VarTailCall" })),
        ScalarTerminator::Trap | ScalarTerminator::Unreachable => { body.instruction(&wasm_encoder::Instruction::Unreachable); }
    }
    Ok(())
}

fn encode_function(
    source_function: &Function,
    target_function: &WasmFunction,
    function_indices: &BTreeMap<FunctionId, u32>,
    layouts: &LayoutInterner,
) -> Result<WasmFunctionEncoder, EncodeError> {
    let layout = local_layout(source_function)?;
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

/// Encode only final canonical SafeWIR authority. The correspondence receipt
/// is independently minted immediately before bytes exist; no candidate or
/// private WasmIR value can enter this function.
fn encode_validated(validated: ValidatedSafeWir) -> Result<Vec<u8>, EncodeError> {
    let target = lower_validated(&validated).map_err(EncodeError::Lowering)?;
    let source = validated.module();
    let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&source.layouts().canonical_bytes())
        .map_err(|_| EncodeError::Lowering(LoweringError::InvalidLayoutBundle))?;
    let receipt = check_correspondence(source, &target).map_err(EncodeError::Correspondence)?;
    if receipt.mappings.len() != expected_source_items(source).len() {
        return Err(EncodeError::Correspondence(CorrespondenceError::MissingSourceItem(
            expected_source_items(source).into_iter().next().expect("nonempty canonical module"),
        )));
    }
    let function_indices: BTreeMap<_, _> = target.functions.iter().enumerate().map(|(index, function)| (function.id, index as u32)).collect();
    let signature_indices: BTreeMap<_, _> = source.signatures().iter().enumerate().map(|(index, signature)| (signature.id, index as u32)).collect();
    let mut types = TypeSection::new();
    for signature in source.signatures() {
        let parameters = signature.parameters.iter().enumerate().map(|(index, value_type)| wasm_value_type(FunctionId::new(u32::MAX), ValueId::new(index as u32), *value_type));
        let results = signature.results.iter().enumerate().map(|(index, value_type)| wasm_value_type(FunctionId::new(u32::MAX), ValueId::new(index as u32), *value_type));
        let parameters = parameters.collect::<Result<Vec<_>, _>>()?;
        let results = results.collect::<Result<Vec<_>, _>>()?;
        types.ty().function(parameters, results);
    }
    let mut functions = FunctionSection::new();
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
    let mut exports = ExportSection::new();
    let mut code = CodeSection::new();
    for target_function in &target.functions {
        let source_function = source.functions().iter().find(|function| function.id == target_function.id).ok_or(EncodeError::MissingFunction(target_function.id))?;
        let type_index = signature_indices.get(&source_function.signature).copied().ok_or(EncodeError::UnsupportedSignature { signature: source_function.signature })?;
        functions.function(type_index);
        exports.export(&format!("f{}", target_function.id.get()), ExportKind::Func, function_indices[&target_function.id]);
        code.function(&encode_function(source_function, target_function, &function_indices, &layouts)?);
    }
    let mut module = WasmModule::new();
    module
        .section(&types)
        .section(&functions)
        .section(&memory)
        .section(&globals)
        .section(&exports)
        .section(&code);
    let bytes = module.finish();
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::default() | wasmparser::WasmFeatures::TAIL_CALL)
        .validate_all(&bytes)
        .map_err(|error| EncodeError::WasmValidation(error.to_string()))?;
    Ok(bytes)
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
    I32WrapI64,
    GlobalGet(u32),
    GlobalSet(u32),
    I32Add,
    I32LtU,
    I32Eqz,
    I32Store { offset: u64, align: u8, memory: u32 },
    I64GtU,
    If(String),
    Unreachable,
    Return,
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
        wasmparser::Operator::I32WrapI64 => PlanToken::I32WrapI64,
        wasmparser::Operator::GlobalGet { global_index } => PlanToken::GlobalGet(global_index),
        wasmparser::Operator::GlobalSet { global_index } => PlanToken::GlobalSet(global_index),
        wasmparser::Operator::I32Add => PlanToken::I32Add,
        wasmparser::Operator::I32LtU => PlanToken::I32LtU,
        wasmparser::Operator::I32Eqz => PlanToken::I32Eqz,
        wasmparser::Operator::I32Store { memarg } => PlanToken::I32Store {
            offset: memarg.offset,
            align: memarg.align,
            memory: memarg.memory,
        },
        wasmparser::Operator::I64GtU => PlanToken::I64GtU,
        wasmparser::Operator::If { blockty } => PlanToken::If(format!("{blockty:?}")),
        wasmparser::Operator::Unreachable => PlanToken::Unreachable,
        wasmparser::Operator::Return => PlanToken::Return,
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

fn push_store(plan: &mut Vec<PlanToken>) {
    plan.push(PlanToken::I32Store { offset: 0, align: 2, memory: 0 });
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

fn expected_function_plan(
    function: &Function,
    layouts: &LayoutInterner,
) -> Result<Vec<PlanToken>, CanonicalAdmissionError> {
    if function.blocks.len() != 1 || function.blocks[0].id != function.entry || !function.blocks[0].parameters.is_empty() {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    let layout = local_layout(function).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let block = &function.blocks[0];
    let mut plan = Vec::new();
    for operation in &block.operations {
        match operation {
            Operation::ConstInt { destination, value } => {
                plan.push(PlanToken::I64Const(*value));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::ConstBool { destination, value } => {
                plan.push(PlanToken::I32Const(i32::from(*value)));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::ConstSize { destination, value } => {
                let value = i32::try_from(*value).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
                plan.push(PlanToken::I32Const(value));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::MoveValue { destination, source } => {
                push_get(&mut plan, &layout, *source)?;
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::Compare { destination, operator: ComparisonOperator::LessUnsigned, left, right } => {
                push_get(&mut plan, &layout, *left)?;
                push_get(&mut plan, &layout, *right)?;
                plan.push(PlanToken::I32LtU);
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::Guard { condition, kind: crate::canonical_safewir::GuardKind::Bounds } => {
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
            _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
        }
    }
    let Terminator::Return { values } = &block.terminator else {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    };
    for value in values {
        push_get(&mut plan, &layout, *value)?;
    }
    plan.push(PlanToken::Return);
    plan.push(PlanToken::End);
    Ok(plan)
}

fn expected_metadata_prefix(
    source: &CanonicalModule<OptimizedValid>,
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
    let mut functions = FunctionSection::new();
    let mut exports = ExportSection::new();
    for (index, function) in target.functions.iter().enumerate() {
        let type_index = signature_indices.get(&function.signature).copied()
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        functions.function(type_index);
        exports.export(&format!("f{}", function.id.get()), ExportKind::Func, index as u32);
    }
    let mut memory = MemorySection::new();
    memory.memory(MemoryType { minimum: 1, maximum: None, memory64: false, shared: false, page_size_log2: None });
    let mut globals = GlobalSection::new();
    globals.global(GlobalType { val_type: ValType::I32, mutable: true, shared: false }, &ConstExpr::i32_const(16));
    globals.global(GlobalType { val_type: ValType::I32, mutable: true, shared: false }, &ConstExpr::i32_const(0));
    let mut module = WasmModule::new();
    module.section(&types).section(&functions).section(&memory).section(&globals).section(&exports);
    Ok(module.finish())
}

fn decode_and_match_plan(
    source: &CanonicalModule<OptimizedValid>,
    target: &WasmIr,
    bytes: &[u8],
) -> Result<(), CanonicalAdmissionError> {
    wasmparser::Validator::new().validate_all(bytes).map_err(|_| CanonicalAdmissionError::WasmValidation)?;
    let metadata = expected_metadata_prefix(source, target)?;
    if bytes.len() < metadata.len() || bytes[..metadata.len()] != metadata[..] {
        return Err(CanonicalAdmissionError::PlanMismatch);
    }
    let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&source.layouts().canonical_bytes())
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let expected = source.functions().iter()
        .map(|function| {
            let local = local_layout(function).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            let locals = local.extra_types.iter().map(|ty| (1, format!("{ty:?}"))).collect();
            Ok((locals, expected_function_plan(function, &layouts)?))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut actual = Vec::new();
    let mut code_count = None;
    let mut section_order = Vec::new();
    let mut code_started = false;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        match payload.map_err(|_| CanonicalAdmissionError::WasmValidation)? {
            wasmparser::Payload::Version { .. } => {}
            wasmparser::Payload::TypeSection(_) => section_order.push(0),
            wasmparser::Payload::FunctionSection(_) => section_order.push(1),
            wasmparser::Payload::MemorySection(_) => section_order.push(2),
            wasmparser::Payload::GlobalSection(_) => section_order.push(3),
            wasmparser::Payload::ExportSection(_) => section_order.push(4),
            wasmparser::Payload::CodeSectionStart { count, .. } => {
                if code_count.replace(count).is_some() {
                    return Err(CanonicalAdmissionError::PlanMismatch);
                }
                section_order.push(5);
                code_started = true;
            }
            wasmparser::Payload::CodeSectionEntry(body) => {
                if !code_started {
                    return Err(CanonicalAdmissionError::PlanMismatch);
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
            wasmparser::Payload::End(_) => {}
            _ => return Err(CanonicalAdmissionError::PlanMismatch),
        }
    }
    let expected_count = u32::try_from(expected.len()).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    if section_order != [0, 1, 2, 3, 4, 5]
        || code_count != Some(expected_count)
        || actual != expected
    {
        return Err(CanonicalAdmissionError::PlanMismatch);
    }
    Ok(())
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
    source: CanonicalModule<OptimizedValid>,
    target: WasmIr,
    receipt: CorrespondenceReceipt,
    bytes: Vec<u8>,
    checked_source: crate::canonical_safewir::CheckedSourceIdentity,
    final_module_digest: crate::canonical_safewir::ModuleDigest,
    history_digest: crate::canonical_safewir::ModuleDigest,
) -> Result<AdmittedCanonicalArtifact, CanonicalAdmissionError> {
    decode_and_match_plan(&source, &target, &bytes)?;
    let wasm_digest = digest_bytes(b"witchy/canonical-wasmir/wasm/v1", &[&bytes]);
    Ok(AdmittedCanonicalArtifact {
        bytes,
        checked_source,
        final_module_digest,
        history_digest,
        correspondence_digest: correspondence_digest(&receipt),
        wasm_digest,
    })
}

pub fn admit_canonical(validated: ValidatedSafeWir) -> Result<AdmittedCanonicalArtifact, CanonicalAdmissionError> {
    let source = validated.module().clone();
    let checked_source = validated.checked_source();
    let final_module_digest = validated.final_module_digest();
    let history_digest = validated.history_digest();
    let target = lower_validated(&validated).map_err(|_| CanonicalAdmissionError::Lowering)?;
    let receipt = check_correspondence(&source, &target).map_err(|_| CanonicalAdmissionError::Correspondence)?;
    let bytes = encode_validated(validated).map_err(|_| CanonicalAdmissionError::Lowering)?;
    admit_parts(source, target, receipt, bytes, checked_source, final_module_digest, history_digest)
}

#[cfg(test)]
fn admit_canonical_with_bytes_for_test(
    validated: ValidatedSafeWir,
    bytes: Vec<u8>,
) -> Result<AdmittedCanonicalArtifact, CanonicalAdmissionError> {
    let source = validated.module().clone();
    let checked_source = validated.checked_source();
    let final_module_digest = validated.final_module_digest();
    let history_digest = validated.history_digest();
    let target = lower_validated(&validated).map_err(|_| CanonicalAdmissionError::Lowering)?;
    let receipt = check_correspondence(&source, &target).map_err(|_| CanonicalAdmissionError::Correspondence)?;
    admit_parts(source, target, receipt, bytes, checked_source, final_module_digest, history_digest)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_safewir::{
        BasicBlock, BlockParameter, CallableContract, CheckedSourceIdentity, FunctionSignature,
        FunctionSignatureId,
        GuardKind,
        LoweredCandidate, LoweredModuleParts, ParameterContract, ParameterOwnership, Revision,
        ResultContract, check_initial_with_test_witness, optimize_unreachable_blocks, validate_final,
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
        let initial_snapshot = checked.module().clone();
        let (initial, witness) = checked.into_parts();
        let optimized = optimize_unreachable_blocks(initial).expect("scalar fixture optimizes");
        validate_final(&initial_snapshot, witness, optimized).expect("scalar fixture is final-valid")
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
                        non_escaping: false,
                    })
                    .collect(),
                results: results.into_iter().map(ResultContract::Value).collect(),
                parameter_effects: BTreeMap::new(),
            },
        }
    }

    fn finalize(candidate: CanonicalModule<LoweredCandidate>) -> ValidatedSafeWir {
        let checked = check_initial_with_test_witness(candidate).expect("fixture is initially valid");
        let initial_snapshot = checked.module().clone();
        let (initial, witness) = checked.into_parts();
        let optimized = optimize_unreachable_blocks(initial).expect("fixture optimizes");
        validate_final(&initial_snapshot, witness, optimized).expect("fixture is final-valid")
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
        let wasm = encode_validated(checked_loop()).expect("structured loop encodes");
        wasmparser::validate(&wasm).expect("structured loop is valid Wasm");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let function = instance.get_typed_func::<i64, i64>(&mut store, "f7").unwrap();
        assert_eq!(function.call(&mut store, 0).unwrap(), 0);
        assert_eq!(function.call(&mut store, 5).unwrap(), 5);
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
                &LayoutInterner::new(),
                &mut BTreeMap::new(),
            ),
            Err(LoweringError::UnsupportedOperation { operation: "VarCall", .. })
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
        let initial_snapshot = checked.module().clone();
        let (initial, witness) = checked.into_parts();
        let optimized = optimize_unreachable_blocks(initial).expect("direct scalar call optimizes");
        validate_final(&initial_snapshot, witness, optimized).expect("direct scalar call is final-valid")
    }

    #[test]
    fn direct_call_encodes_as_wasm_call() {
        let wasm = encode_validated(checked_direct_call(false)).expect("validated direct call encodes");
        wasmparser::validate(&wasm).expect("direct call module is structurally valid");
        assert!(wasm.windows(2).any(|window| window == [0x10, 0x01]));
    }

    #[test]
    fn proper_tail_call_encodes_as_wasm_return_call() {
        let wasm = encode_validated(checked_direct_call(true)).expect("validated proper tail call encodes");
        wasmparser::validate(&wasm).expect("tail-call module is structurally valid");
        assert!(wasm.windows(2).any(|window| window == [0x12, 0x01]));
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
        let initial_snapshot = checked.module().clone();
        let (initial, witness) = checked.into_parts();
        let optimized = optimize_unreachable_blocks(initial).unwrap();
        validate_final(&initial_snapshot, witness, optimized).unwrap()
    }

    #[test]
    fn comparison_encodes_and_validates() {
        let wasm = encode_validated(checked_comparison()).expect("validated comparison encodes");
        wasmparser::validate(&wasm).expect("comparison module is structurally valid");
    }

    fn checked_size_operation(operation: Operation, input: ValueId, result: ValueId) -> ValidatedSafeWir {
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
                    operations: vec![Operation::ConstInt { destination: input, value: -1 }, operation],
                    terminator: Terminator::Return { values: vec![result] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        let checked = check_initial_with_test_witness(candidate).unwrap();
        let initial_snapshot = checked.module().clone();
        let (initial, witness) = checked.into_parts();
        let optimized = optimize_unreachable_blocks(initial).unwrap();
        validate_final(&initial_snapshot, witness, optimized).unwrap()
    }

    #[test]
    fn narrow_size_fails_closed_in_the_scalar_backend() {
        let input = ValueId::new(1);
        let result = ValueId::new(2);
        let validated = checked_size_operation(Operation::NarrowSize { destination: result, value: input }, input, result);
        assert!(matches!(
            encode_validated(validated),
            Err(EncodeError::Lowering(LoweringError::UnsupportedOperation { operation: "NarrowSize", .. }))
        ));
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
        let initial_snapshot = checked.module().clone();
        let (initial, witness) = checked.into_parts();
        let optimized = optimize_unreachable_blocks(initial).unwrap();
        validate_final(&initial_snapshot, witness, optimized).unwrap()
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
        let (operations, results) = if oob {
            (
                vec![
                    Operation::ConstSize { destination: count, value: 1 },
                    Operation::ConstSize { destination: capacity, value: 1 },
                    Operation::ConstSize { destination: index, value: 1 },
                    Operation::Compare { destination: condition, operator: ComparisonOperator::LessUnsigned, left: index, right: count },
                    Operation::Guard { condition, kind: GuardKind::Bounds },
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
                    Operation::Alloc { destination: object, layout: list, extent: Extent::Counted { header_bytes: 8, count, capacity, stride_bytes: 4 }, class: crate::canonical_safewir::AllocationClass::ReferenceCounted },
                    Operation::ConstSize { destination: value, value: 7 },
                    Operation::Store { place: Place::projected(object, Projection::Element { index }, ValueType::Size, AccessMode::Write), value, transfer: StoreTransfer::CopyScalar },
                    Operation::ConstSize { destination: new_count, value: 2 },
                    Operation::UpdateCount { object, count: new_count },
                    Operation::Load { destination: capacity_loaded, place: Place::projected(object, Projection::Header(HeaderField::Capacity), ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                    Operation::ConstSize { destination: second_index, value: 1 },
                    Operation::Compare { destination: second_condition, operator: ComparisonOperator::LessUnsigned, left: second_index, right: new_count },
                    Operation::Guard { condition: second_condition, kind: GuardKind::Bounds },
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
        value_types.insert(object, ValueType::Ref(list));
        value_types.insert(element_loaded, ValueType::Size);
        if !oob {
            for value in [capacity_loaded, new_count, second_index, value, second_value, length_loaded] {
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
                        Operation::Share { destination: shared, source: object },
                        Operation::Load { destination: loaded, place: Place::projected(shared, Projection::Header(HeaderField::Length), ValueType::Size, AccessMode::Read), transfer: LoadTransfer::CopyScalar },
                        Operation::Release { object: shared },
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
        let function = Function {
            id: FunctionId::new(90),
            signature: crate::canonical_safewir::FunctionSignatureId::new(90),
            entry: crate::canonical_safewir::BlockId::new(90),
            value_types: BTreeMap::new(),
            blocks: Vec::new(),
        };
        let layout = LocalLayout {
            indices: [(object, 0), (destination, 1)].into_iter().collect(),
            extra_types: vec![ValType::I32; 5],
            scratch: [2, 3, 4, 5],
        };
        let mut boundary = WasmFunctionEncoder::new(vec![(5, ValType::I32)]);
        if share {
            emit_share(&mut boundary, &layout, destination, object, &function).unwrap();
            boundary.instruction(&wasm_encoder::Instruction::I32Const(0));
            boundary.instruction(&wasm_encoder::Instruction::LocalGet(1));
            boundary.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
        } else {
            emit_release(&mut boundary, &layout, &function, object).unwrap();
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
            Err(CanonicalAdmissionError::PlanMismatch)
        ));
    }

    #[test]
    fn mismatched_source_plan_fails_admission() {
        let bytes = encode_validated(checked_scalar()).unwrap();
        assert!(matches!(
            admit_canonical_with_bytes_for_test(checked_packed_list_allocation_artifact(), bytes),
            Err(CanonicalAdmissionError::PlanMismatch)
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
            Err(CanonicalAdmissionError::WasmValidation | CanonicalAdmissionError::PlanMismatch)
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
            Err(CanonicalAdmissionError::PlanMismatch)
        ));
    }
    #[test]
    fn packed_scalar_collection_access_mutates_and_preserves_headers() {
        let source = checked_packed_list_fixture(false);
        let target = lower_validated(&source).unwrap();
        let receipt = check_correspondence(source.module(), &target).unwrap();
        assert_eq!(receipt.mappings.len(), 20);
        let wasm = encode_validated(source).expect("packed scalar collection encodes");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance.get_typed_func::<(), i32>(&mut store, "f30").unwrap();
        assert_eq!(run.call(&mut store, ()).unwrap(), 42);
    }

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

    #[test]
    fn shared_destination_preserves_root_for_use() {
        let source = checked_share_fixture();
        let target = lower_validated(&source).unwrap();
        let receipt = check_correspondence(source.module(), &target).unwrap();
        assert_eq!(receipt.mappings.len(), 7);
        let wasm = encode_validated(source).expect("shared scalar collection encodes");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance.get_typed_func::<(), i32>(&mut store, "f33").unwrap();
        assert_eq!(run.call(&mut store, ()).unwrap(), 1);
    }

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
