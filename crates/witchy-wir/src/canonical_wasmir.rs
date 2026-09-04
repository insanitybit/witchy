//! Private mechanical WasmIR bring-up for the admitted scalar SafeWIR subset.
//!
//! This module is deliberately not public.  The current canonical checker
//! mints `InitialValid`, but there is no final SafeWIR/authority typestate yet,
//! so the lowering entry is test-only.  When that authority exists, the entry
//! point takes final `ValidatedSafeWir` authority, and no weaker state is
//! accepted.
//!
//! The target is an IR, not wasm bytes.  It has no raw constructors visible to
//! callers and no encoder.  Every scalar source item emits exactly one target
//! item, which makes the receipt matcher independently checkable without
//! replaying the lowering function.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use wasm_encoder::{
    BlockType, CodeSection, ExportKind, ExportSection, Function as WasmFunctionEncoder,
    FunctionSection, Ieee64, Module as WasmModule, TypeSection, ValType,
};

use crate::canonical_safewir::{
    ArithmeticOperator, BasicBlock, CallArgument, CanonicalModule, ComparisonOperator,
    Function, FunctionId, ModuleStage, Operation, OptimizedValid, Terminator, ValidatedSafeWir,
    ValueId,
};

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

fn lower_operation(
    function: FunctionId,
    block: crate::canonical_safewir::BlockId,
    index: usize,
    operation: &Operation,
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
        Operation::MoveValue { destination, source } => ScalarInstruction::MoveValue { destination: *destination, source: *source },
        Operation::Call { destinations, callee, arguments } => ScalarInstruction::Call {
            destinations: destinations.clone(),
            callee: *callee,
            arguments: scalar_arguments(function, arguments)?,
        },
        Operation::VarCall { .. } => {
            return Err(LoweringError::UnsupportedOperation { function, operation: "VarCall" });
        }
        Operation::Alloc { .. }
        | Operation::StaticData { .. }
        | Operation::Load { .. }
        | Operation::Store { .. }
        | Operation::CopyBytes { .. }
        | Operation::MoveRange { .. }
        | Operation::CloneRange { .. }
        | Operation::UpdateCount { .. }
        | Operation::Share { .. }
        | Operation::Retain { .. }
        | Operation::Release { .. }
        | Operation::Reclaim { .. }
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

fn lower_block(function: &Function, block: &BasicBlock) -> Result<WasmBlock, LoweringError> {
    let instructions = block
        .operations
        .iter()
        .enumerate()
        .map(|(index, operation)| lower_operation(function.id, block.id, index, operation))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WasmBlock {
        id: block.id,
        instructions,
        terminator: lower_terminator(function.id, block.id, &block.terminator)?,
    })
}

fn lower_function(function: &Function) -> Result<WasmFunction, LoweringError> {
    Ok(WasmFunction {
        id: function.id,
        signature: function.signature,
        entry: function.entry,
        blocks: function
            .blocks
            .iter()
            .map(|block| lower_block(function, block))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn lower_validated(module: &ValidatedSafeWir) -> Result<WasmIr, LoweringError> {
    let module = module.module();
    Ok(WasmIr {
        revision: module.revision(),
        checked_source: module.checked_source(),
        functions: module
            .functions()
            .iter()
            .map(lower_function)
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
        Operation::Call { .. } | Operation::VarCall { .. } => Some(CorrespondenceRule::ScalarCall),
        Operation::Alloc { .. }
        | Operation::StaticData { .. }
        | Operation::Load { .. }
        | Operation::Store { .. }
        | Operation::CopyBytes { .. }
        | Operation::MoveRange { .. }
        | Operation::CloneRange { .. }
        | Operation::UpdateCount { .. }
        | Operation::Share { .. }
        | Operation::Retain { .. }
        | Operation::Release { .. }
        | Operation::Reclaim { .. }
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
    WasmValidation(String),
}

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
        crate::canonical_safewir::ValueType::V128 => Ok(ValType::V128),
        unsupported => Err(EncodeError::UnsupportedValueType { function, value, value_type: unsupported }),
    }
}

struct LocalLayout {
    indices: BTreeMap<ValueId, u32>,
    extra_types: Vec<ValType>,
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
    Ok(LocalLayout {
        indices,
        extra_types,
    })
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
) -> Result<WasmFunctionEncoder, EncodeError> {
    if target_function.blocks.len() != 1 {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
            function: source_function.id,
            operation: "multi-block function",
        }));
    }
    let layout = local_layout(source_function)?;
    let local_declarations: Vec<_> = layout.extra_types.iter().copied().map(|ty| (1, ty)).collect();
    let mut body = WasmFunctionEncoder::new(local_declarations);
    let target_block = &target_function.blocks[0];
    if matches!(target_block.terminator.operation, ScalarTerminator::Branch { .. } | ScalarTerminator::BranchIf { .. }) {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
            function: source_function.id,
            operation: "branch terminator",
        }));
    }
    for instruction in &target_block.instructions { emit_instruction(&mut body, source_function, &layout, &instruction.operation, function_indices)?; }
    emit_terminator(&mut body, source_function, &layout, &target_block.terminator.operation, function_indices)?;
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(body)
}

/// Encode only final canonical SafeWIR authority. The correspondence receipt
/// is independently minted immediately before bytes exist; no candidate or
/// private WasmIR value can enter this function.
fn encode_validated(validated: ValidatedSafeWir) -> Result<Vec<u8>, EncodeError> {
    let target = lower_validated(&validated).map_err(EncodeError::Lowering)?;
    let source = validated.module();
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
    let mut exports = ExportSection::new();
    let mut code = CodeSection::new();
    for target_function in &target.functions {
        let source_function = source.functions().iter().find(|function| function.id == target_function.id).ok_or(EncodeError::MissingFunction(target_function.id))?;
        let type_index = signature_indices.get(&source_function.signature).copied().ok_or(EncodeError::UnsupportedSignature { signature: source_function.signature })?;
        functions.function(type_index);
        exports.export(&format!("f{}", target_function.id.get()), ExportKind::Func, function_indices[&target_function.id]);
        code.function(&encode_function(source_function, target_function, &function_indices)?);
    }
    let mut module = WasmModule::new();
    module.section(&types).section(&functions).section(&exports).section(&code);
    let bytes = module.finish();
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::default() | wasmparser::WasmFeatures::TAIL_CALL)
        .validate_all(&bytes)
        .map_err(|error| EncodeError::WasmValidation(error.to_string()))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_safewir::{
        CallableContract, CheckedSourceIdentity, FunctionSignature, FunctionSignatureId,
        LoweredModuleParts, Revision, ResultContract, check_initial_with_witness,
        optimize_unreachable_blocks, validate_final,
    };
    use crate::layout::{LayoutBundle, LayoutInterner};

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
        let checked = check_initial_with_witness(candidate).expect("scalar fixture is initially valid");
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
        FunctionSignature {
            id,
            parameters: Vec::new(),
            results: vec![crate::canonical_safewir::ValueType::Int],
            contract: CallableContract {
                parameters: Vec::new(),
                results: vec![ResultContract::Value(crate::canonical_safewir::ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
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
            proper_tail_sites: if tail { vec![crate::canonical_safewir::ProperTailSite { id: tail_site, function: FunctionId::new(1) }] } else { Vec::new() },
        });
        let checked = check_initial_with_witness(candidate).expect("direct scalar call is initially valid");
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
        let checked = check_initial_with_witness(candidate).unwrap();
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
        let checked = check_initial_with_witness(candidate).unwrap();
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
        let checked = check_initial_with_witness(candidate).unwrap();
        let initial_snapshot = checked.module().clone();
        let (initial, witness) = checked.into_parts();
        let optimized = optimize_unreachable_blocks(initial).unwrap();
        validate_final(&initial_snapshot, witness, optimized).unwrap()
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
