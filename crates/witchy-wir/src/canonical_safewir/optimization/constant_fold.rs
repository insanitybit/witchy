use std::collections::BTreeMap;

use super::super::{
    ArithmeticOperator, BlockId, ComparisonOperator, Function, FunctionId, Operation, OverflowBehavior,
    ValueId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarConstant {
    Int(i64),
    Size(u32),
    Bool(bool),
    Float(u64),
}

pub(crate) struct FoldedOperation {
    pub(crate) function: FunctionId,
    pub(crate) block: BlockId,
    pub(crate) operation_index: usize,
}

/// Return the first operation whose exact scalar result can be replaced by a
/// constant. The map is derived only from canonical operations. In
/// particular, it never consumes an analysis fact as a proof of constancy.
pub(crate) fn first_fold(functions: &[Function]) -> Option<(Vec<Function>, FoldedOperation)> {
    for (function_index, function) in functions.iter().enumerate() {
        let constants = constant_values(function);
        for (block_index, block) in function.blocks.iter().enumerate() {
            for (operation_index, operation) in block.operations.iter().enumerate() {
                let Some(replacement) = reference_fold(operation, &constants) else {
                    continue;
                };
                let mut transformed = functions.to_vec();
                transformed[function_index].blocks[block_index].operations[operation_index] =
                    replacement;
                return Some((
                    transformed,
                    FoldedOperation {
                        function: function.id,
                        block: block.id,
                        operation_index,
                    },
                ));
            }
        }
    }
    None
}

/// Independently reconstruct the only constant replacement permitted at one
/// receipt location. The assurance boundary calls this from the immutable
/// pre-transform function rather than trusting the optimizer's candidate.
pub(crate) fn expected_fold(
    function: &Function,
    block: BlockId,
    operation_index: usize,
) -> Option<Operation> {
    let operation = function
        .blocks
        .iter()
        .find(|candidate| candidate.id == block)?
        .operations
        .get(operation_index)?;
    reference_fold(operation, &constant_values(function))
}

fn constant_values(function: &Function) -> BTreeMap<ValueId, ScalarConstant> {
    let mut constants = BTreeMap::new();
    for block in &function.blocks {
        for operation in &block.operations {
            if let Some((destination, value)) = evaluated_value(operation, &constants) {
                constants.insert(destination, value);
            }
        }
    }

    loop {
        let mut changed = false;
        for block in &function.blocks {
            for operation in &block.operations {
                let Some((destination, value)) = evaluated_value(operation, &constants) else {
                    continue;
                };
                if constants.insert(destination, value) != Some(value) {
                    changed = true;
                }
            }
        }
        if !changed {
            return constants;
        }
    }
}

/// This is the executable reference semantics for one candidate rewrite. It
/// computes the original operation once and constructs the matching constant;
/// the common checker validates the resulting candidate independently.
fn reference_fold(
    operation: &Operation,
    constants: &BTreeMap<ValueId, ScalarConstant>,
) -> Option<Operation> {
    if !matches!(operation, Operation::Arithmetic { .. } | Operation::Compare { .. }) {
        return None;
    }
    let (destination, value) = evaluated_value(operation, constants)?;
    Some(constant_operation(destination, value))
}

fn evaluated_value(
    operation: &Operation,
    constants: &BTreeMap<ValueId, ScalarConstant>,
) -> Option<(ValueId, ScalarConstant)> {
    match operation {
        Operation::ConstInt { destination, value } => {
            Some((*destination, ScalarConstant::Int(*value)))
        }
        Operation::ConstFloat { destination, bits } => {
            Some((*destination, ScalarConstant::Float(*bits)))
        }
        Operation::ConstBool { destination, value } => {
            Some((*destination, ScalarConstant::Bool(*value)))
        }
        Operation::ConstSize { destination, value } => {
            Some((*destination, ScalarConstant::Size(u32::try_from(*value).ok()?)))
        }
        Operation::Arithmetic {
            destination,
            operator,
            left,
            right,
            overflow,
        } => {
            let left = constants.get(left).copied()?;
            let right = constants.get(right).copied()?;
            let value = match (left, right) {
                (ScalarConstant::Int(left), ScalarConstant::Int(right)) => {
                    ScalarConstant::Int(arithmetic_int(*operator, left, right, *overflow)?)
                }
                (ScalarConstant::Size(left), ScalarConstant::Size(right)) => {
                    ScalarConstant::Size(arithmetic_size(*operator, left, right, *overflow)?)
                }
                _ => return None,
            };
            Some((*destination, value))
        }
        Operation::Compare { destination, operator, left, right } => {
            let left = constants.get(left).copied()?;
            let right = constants.get(right).copied()?;
            Some((*destination, ScalarConstant::Bool(compare(*operator, left, right)?)))
        }
        _ => None,
    }
}

fn constant_operation(destination: ValueId, value: ScalarConstant) -> Operation {
    match value {
        ScalarConstant::Int(value) => Operation::ConstInt { destination, value },
        ScalarConstant::Size(value) => Operation::ConstSize {
            destination,
            value: u64::from(value),
        },
        ScalarConstant::Bool(value) => Operation::ConstBool { destination, value },
        ScalarConstant::Float(bits) => Operation::ConstFloat { destination, bits },
    }
}

fn arithmetic_int(
    operator: ArithmeticOperator,
    left: i64,
    right: i64,
    overflow: OverflowBehavior,
) -> Option<i64> {
    if overflow != OverflowBehavior::Wrap {
        return None;
    }
    Some(match operator {
        ArithmeticOperator::Add => left.wrapping_add(right),
        ArithmeticOperator::Subtract => left.wrapping_sub(right),
        ArithmeticOperator::Multiply => left.wrapping_mul(right),
        ArithmeticOperator::DivideSigned => {
            if right == 0 || (left == i64::MIN && right == -1) {
                return None;
            }
            left / right
        }
        ArithmeticOperator::RemainderSigned => {
            if right == 0 || (left == i64::MIN && right == -1) {
                return None;
            }
            left % right
        }
        ArithmeticOperator::DivideUnsigned => {
            if right == 0 {
                return None;
            }
            (left as u64 / right as u64) as i64
        }
        ArithmeticOperator::RemainderUnsigned => {
            if right == 0 {
                return None;
            }
            (left as u64 % right as u64) as i64
        }
        ArithmeticOperator::BitAnd => left & right,
        ArithmeticOperator::BitOr => left | right,
        ArithmeticOperator::BitXor => left ^ right,
        ArithmeticOperator::ShiftLeft => left.wrapping_shl(right as u32),
        ArithmeticOperator::ShiftRightSigned => left.wrapping_shr(right as u32),
        ArithmeticOperator::ShiftRightUnsigned => (left as u64).wrapping_shr(right as u32) as i64,
    })
}

fn arithmetic_size(
    operator: ArithmeticOperator,
    left: u32,
    right: u32,
    overflow: OverflowBehavior,
) -> Option<u32> {
    if overflow != OverflowBehavior::Wrap {
        return None;
    }
    Some(match operator {
        ArithmeticOperator::Add => left.wrapping_add(right),
        ArithmeticOperator::Subtract => left.wrapping_sub(right),
        ArithmeticOperator::Multiply => left.wrapping_mul(right),
        ArithmeticOperator::DivideUnsigned => {
            if right == 0 {
                return None;
            }
            left / right
        }
        ArithmeticOperator::RemainderUnsigned => {
            if right == 0 {
                return None;
            }
            left % right
        }
        ArithmeticOperator::BitAnd => left & right,
        ArithmeticOperator::BitOr => left | right,
        ArithmeticOperator::BitXor => left ^ right,
        ArithmeticOperator::ShiftLeft => left.wrapping_shl(right),
        ArithmeticOperator::ShiftRightUnsigned => left.wrapping_shr(right),
        ArithmeticOperator::DivideSigned
        | ArithmeticOperator::RemainderSigned
        | ArithmeticOperator::ShiftRightSigned => return None,
    })
}

fn compare(
    operator: ComparisonOperator,
    left: ScalarConstant,
    right: ScalarConstant,
) -> Option<bool> {
    match (left, right) {
        (ScalarConstant::Int(left), ScalarConstant::Int(right)) => Some(match operator {
            ComparisonOperator::Equal => left == right,
            ComparisonOperator::NotEqual => left != right,
            ComparisonOperator::LessSigned => left < right,
            ComparisonOperator::LessUnsigned => (left as u64) < (right as u64),
            ComparisonOperator::LessEqualSigned => left <= right,
            ComparisonOperator::LessEqualUnsigned => (left as u64) <= (right as u64),
            ComparisonOperator::GreaterSigned => left > right,
            ComparisonOperator::GreaterUnsigned => (left as u64) > (right as u64),
            ComparisonOperator::GreaterEqualSigned => left >= right,
            ComparisonOperator::GreaterEqualUnsigned => (left as u64) >= (right as u64),
        }),
        (ScalarConstant::Size(left), ScalarConstant::Size(right)) => Some(match operator {
            ComparisonOperator::Equal => left == right,
            ComparisonOperator::NotEqual => left != right,
            ComparisonOperator::LessUnsigned => left < right,
            ComparisonOperator::LessEqualUnsigned => left <= right,
            ComparisonOperator::GreaterUnsigned => left > right,
            ComparisonOperator::GreaterEqualUnsigned => left >= right,
            ComparisonOperator::LessSigned
            | ComparisonOperator::LessEqualSigned
            | ComparisonOperator::GreaterSigned
            | ComparisonOperator::GreaterEqualSigned => return None,
        }),
        (ScalarConstant::Bool(left), ScalarConstant::Bool(right)) => match operator {
            ComparisonOperator::Equal => Some(left == right),
            ComparisonOperator::NotEqual => Some(left != right),
            _ => None,
        },
        (ScalarConstant::Float(left), ScalarConstant::Float(right)) => {
            let left = f64::from_bits(left);
            let right = f64::from_bits(right);
            match operator {
                ComparisonOperator::Equal => Some(left == right),
                ComparisonOperator::NotEqual => Some(left != right),
                _ => None,
            }
        }
        _ => None,
    }
}
