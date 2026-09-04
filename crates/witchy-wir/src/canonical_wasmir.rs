//! Private mechanical WasmIR bring-up for the admitted scalar SafeWIR subset.
//!
//! This module is deliberately not public.  The current canonical checker
//! mints `InitialValid`, but there is no final SafeWIR/authority typestate yet,
//! so the lowering entry is test-only.  When that authority exists, the entry
//! point must take `CanonicalModule<ValidatedSafeWir>` by reference (or an
//! equivalent final validated typestate), and no weaker state may be accepted.
//!
//! The target is an IR, not wasm bytes.  It has no raw constructors visible to
//! callers and no encoder.  Every scalar source item emits exactly one target
//! item, which makes the receipt matcher independently checkable without
//! replaying the lowering function.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use crate::canonical_safewir::{
    ArithmeticOperator, BasicBlock, CallArgument, CanonicalModule, ComparisonOperator,
    Function, FunctionId, InitialValid, Operation, Terminator, ValueId,
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
    NarrowSize { destination: ValueId, value: ValueId },
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
        Operation::Arithmetic { destination, operator, left, right, overflow } => ScalarInstruction::Arithmetic {
            destination: *destination,
            operator: *operator,
            left: *left,
            right: *right,
            overflow: *overflow,
        },
        Operation::Compare { destination, operator, left, right } => ScalarInstruction::Compare {
            destination: *destination,
            operator: *operator,
            left: *left,
            right: *right,
        },
        Operation::NarrowSize { destination, value } => ScalarInstruction::NarrowSize { destination: *destination, value: *value },
        Operation::Guard { condition, kind } => ScalarInstruction::Guard { condition: *condition, kind: *kind },
        Operation::MoveValue { destination, source } => ScalarInstruction::MoveValue { destination: *destination, source: *source },
        Operation::Call { destinations, callee, arguments } => ScalarInstruction::Call {
            destinations: destinations.clone(),
            callee: *callee,
            arguments: scalar_arguments(function, arguments)?,
        },
        Operation::VarCall { destinations, callee, signature, arguments } => ScalarInstruction::VarCall {
            destinations: destinations.clone(),
            callee: *callee,
            signature: *signature,
            arguments: scalar_arguments(function, arguments)?,
        },
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
        Terminator::VarTailCall { site, callee, signature, arguments } => ScalarTerminator::VarTailCall {
            site: *site,
            callee: *callee,
            signature: *signature,
            arguments: scalar_arguments(function, arguments)?,
        },
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

#[cfg(test)]
fn lower_initial_for_test(module: &CanonicalModule<InitialValid>) -> Result<WasmIr, LoweringError> {
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
        Operation::NarrowSize { .. } => Some(CorrespondenceRule::ScalarConversion),
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
        (Operation::NarrowSize { destination, value }, ScalarInstruction::NarrowSize { destination: actual_destination, value: actual_value }) => destination == actual_destination && value == actual_value,
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

fn expected_source_items(module: &CanonicalModule<InitialValid>) -> BTreeSet<SourceItemId> {
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
                mappings.push(CorrespondenceMapping { source: expected_source, target: TargetItemId::Instruction { function: function.id, block: block.id, index }, rule: operation_rule(&source_block.operations[index]).expect("validated scalar operation") });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_safewir::{
        check_initial, CallableContract, CheckedSourceIdentity, FunctionSignature,
        FunctionSignatureId, LoweredModuleParts, Revision, ResultContract,
    };
    use crate::layout::{LayoutBundle, LayoutInterner};

    fn checked_scalar() -> CanonicalModule<InitialValid> {
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
        check_initial(candidate).expect("scalar fixture is initially valid")
    }

    #[test]
    fn scalar_lowering_has_one_mapping_per_operation_and_terminator() {
        let source = checked_scalar();
        let target = lower_initial_for_test(&source).unwrap();
        let receipt = check_correspondence(&source, &target).unwrap();
        assert_eq!(receipt.mappings.len(), 2);
    }

    #[test]
    fn correspondence_rejects_semantic_mismatch() {
        let source = checked_scalar();
        let mut target = lower_initial_for_test(&source).unwrap();
        target.functions[0].blocks[0].instructions[0].operation = ScalarInstruction::IntConst { destination: ValueId::new(1), value: 10 };
        assert!(matches!(check_correspondence(&source, &target), Err(CorrespondenceError::InstructionMismatch(_))));
    }

    #[test]
    fn correspondence_rejects_omission() {
        let source = checked_scalar();
        let mut target = lower_initial_for_test(&source).unwrap();
        target.functions[0].blocks[0].instructions.clear();
        assert!(matches!(check_correspondence(&source, &target), Err(CorrespondenceError::MissingSourceItem(_))));
    }

    #[test]
    fn correspondence_rejects_duplication() {
        let source = checked_scalar();
        let mut target = lower_initial_for_test(&source).unwrap();
        let duplicate = target.functions[0].blocks[0].instructions[0].clone();
        target.functions[0].blocks[0].instructions.push(duplicate);
        assert!(matches!(check_correspondence(&source, &target), Err(CorrespondenceError::DuplicateSourceItem(_))));
    }
}
