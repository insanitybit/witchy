//! Executable reference semantics for validated canonical SafeWIR.
//!
//! This interpreter intentionally operates on semantic operations rather than
//! decoding or sharing logic with the Wasm backend. Unsupported semantics fail
//! closed so differential execution cannot accidentally bless backend behavior
//! that the oracle does not model.

use std::collections::BTreeMap;
use std::fmt;

use crate::canonical_safewir::{
    AllocationClass, ArithmeticOperator, BasicBlock, BlockId, CallArgument, CanonicalModule,
    ComparisonOperator, Extent, Function, FunctionId, HeaderField, LoadTransfer, Operation,
    OptimizedValid, OverflowBehavior, Place, Projection, StoreTransfer, Terminator,
    ValidatedSafeWir, ValueId,
};
use crate::layout::LayoutId;

const SIZE_MAX: u64 = u32::MAX as u64;

/// A value observable at a SafeWIR function boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpreterValue {
    Int(i64),
    FloatBits(u64),
    Bool(bool),
    Size(u64),
    Unit,
    V128([u8; 16]),
    ObjectRef(u64),
    Function(FunctionId),
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
    UnsupportedOperation(&'static str),
}

impl fmt::Display for InterpretError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for InterpretError {}

/// Bounds execution so malformed or accidentally nonterminating fixtures do
/// not hang a differential test.
#[derive(Debug, Clone, Copy)]
pub struct InterpreterConfig {
    pub max_steps: u64,
}

impl Default for InterpreterConfig {
    fn default() -> Self {
        Self { max_steps: 1_000_000 }
    }
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
    Engine::new(module.module(), config).execute(function, arguments)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CellKey {
    Field(u32),
    Element(u64),
}

#[derive(Debug)]
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
    class: AllocationClass,
    extent: RuntimeExtent,
    references: u64,
    immutable: bool,
    cells: BTreeMap<CellKey, InterpreterValue>,
}

struct Engine<'a> {
    module: &'a CanonicalModule<OptimizedValid>,
    config: InterpreterConfig,
    steps: u64,
    next_object: u64,
    objects: BTreeMap<u64, Object>,
}

enum FunctionExit {
    Return(Vec<InterpreterValue>),
    TailCall(FunctionId, Vec<InterpreterValue>),
}

impl<'a> Engine<'a> {
    fn new(module: &'a CanonicalModule<OptimizedValid>, config: InterpreterConfig) -> Self {
        Self {
            module,
            config,
            steps: 0,
            next_object: 0,
            objects: BTreeMap::new(),
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
        let function = self.function(function_id)?.clone();
        let entry = self.block(&function, function.entry)?;
        let mut values = BTreeMap::new();
        self.bind_parameters(entry, arguments, &mut values)?;
        let mut block_id = function.entry;

        loop {
            let block = self.block(&function, block_id)?.clone();
            for operation in &block.operations {
                self.bump_step()?;
                self.execute_operation(operation, &mut values)?;
            }

            self.bump_step()?;
            match &block.terminator {
                Terminator::Branch { target, arguments } => {
                    let next = self.read_values(arguments, &values)?;
                    self.bind_parameters(self.block(&function, *target)?, next, &mut values)?;
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
                    self.bind_parameters(self.block(&function, target)?, next, &mut values)?;
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
            Operation::StaticData { .. } => {
                return Err(InterpretError::UnsupportedOperation("StaticData"));
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
            Operation::CopyBytes { .. } => {
                return Err(InterpretError::UnsupportedOperation("CopyBytes"));
            }
            Operation::MoveRange { .. } => {
                return Err(InterpretError::UnsupportedOperation("MoveRange"));
            }
            Operation::CloneRange { .. } => {
                return Err(InterpretError::UnsupportedOperation("CloneRange"));
            }
            Operation::UpdateCount { object, count } => {
                let object = self.object_value(*object, values)?;
                let count = self.size_value(*count, values)?;
                self.update_count(object, count)?;
            }
            Operation::MoveValue { destination, source } => {
                let value = values
                    .remove(source)
                    .ok_or(InterpretError::MissingValue(*source))?;
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
            Operation::HostCall { .. } => {
                return Err(InterpretError::UnsupportedOperation("HostCall"));
            }
        }
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
            ArithmeticOperator::RemainderSigned => checked(left.checked_rem(right)),
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
                signed_checked((left as i32).checked_rem(right as i32))
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
                class,
                extent,
                references: 1,
                immutable,
                cells: BTreeMap::new(),
            },
        );
        Ok(object)
    }

    fn load(
        &mut self,
        place: &Place,
        transfer: LoadTransfer,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<InterpreterValue, InterpretError> {
        if transfer != LoadTransfer::CopyScalar {
            return Err(InterpretError::UnsupportedOperation(
                "reference-bearing Load transfer",
            ));
        }
        let object = self.object_value(place.object(), values)?;
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
        if transfer != StoreTransfer::CopyScalar {
            return Err(InterpretError::UnsupportedOperation(
                "reference-bearing Store transfer",
            ));
        }
        let object = self.object_value(place.object(), values)?;
        let value = self.value(source, values)?.clone();
        if self.object(object)?.immutable {
            return Err(InterpretError::ImmutableObject(object));
        }
        let key = match place.projection() {
            Projection::Header(_) => {
                return Err(InterpretError::UnsupportedOperation("direct header Store"));
            }
            Projection::Field { index } => CellKey::Field(*index),
            Projection::Element { index } => {
                let index = self.size_value(*index, values)?;
                self.check_element(object, index)?;
                CellKey::Element(index)
            }
            Projection::Slice { .. } => {
                return Err(InterpretError::UnsupportedOperation("Slice store"));
            }
        };
        self.object_mut(object)?.cells.insert(key, value);
        Ok(())
    }

    fn update_count(&mut self, object: u64, count: u64) -> Result<(), InterpretError> {
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
        if object_state.class != AllocationClass::ReferenceCounted {
            return Err(InterpretError::ObjectNotReferenceCounted(object));
        }
        object_state.references = u32::try_from(object_state.references)
            .ok()
            .and_then(|references| references.checked_add(1))
            .map(u64::from)
            .ok_or(InterpretError::ReferenceCountOverflow(object))?;
        Ok(())
    }

    fn release(&mut self, object: u64) -> Result<(), InterpretError> {
        let remaining = {
            let object_state = self.object_mut(object)?;
            if object_state.class != AllocationClass::ReferenceCounted {
                return Err(InterpretError::ObjectNotReferenceCounted(object));
            }
            object_state.references = u32::try_from(object_state.references)
                .ok()
                .and_then(|references| references.checked_sub(1))
                .map(u64::from)
                .ok_or(InterpretError::ReferenceCountUnderflow(object))?;
            object_state.references
        };
        if remaining == 0 {
            self.objects.remove(&object);
        }
        Ok(())
    }

    fn reclaim(&mut self, object: u64) -> Result<(), InterpretError> {
        let object_state = self.object(object)?;
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
                CallArgument::Share(value) => {
                    let object = self.object_value(*value, values)?;
                    self.retain(object)?;
                    Ok(InterpreterValue::ObjectRef(object))
                }
                CallArgument::Borrow(borrow) => Ok(self.value(borrow.source(), values)?.clone()),
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
        function
            .blocks
            .iter()
            .find(|block| block.id == id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_safewir::{
        check_initial_with_test_witness, optimize_unreachable_blocks, validate_final, BasicBlock,
        AccessMode, BlockParameter, CallableContract, CheckedSourceIdentity, FunctionSignature,
        FunctionSignatureId, LoweredModuleParts, Operation, ParameterContract, ParameterOwnership,
        ProperTailSite, ResultContract, Revision, TailSiteId, ValueType,
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
                non_escaping: true,
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
        let initial_snapshot = checked.module().clone();
        let (initial, witness) = checked.into_parts();
        let optimized = optimize_unreachable_blocks(initial).expect("zero-risk optimization");
        validate_final(&initial_snapshot, witness, optimized).expect("final validation")
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

    #[test]
    fn validated_size_arithmetic_uses_i32_width_and_signed_bits() {
        let cases = [
            (
                ArithmeticOperator::Add,
                u64::from(u32::MAX),
                1,
                OverflowBehavior::Wrap,
                Ok(vec![InterpreterValue::Size(0)]),
            ),
            (
                ArithmeticOperator::ShiftLeft,
                1,
                32,
                OverflowBehavior::Wrap,
                Ok(vec![InterpreterValue::Size(1)]),
            ),
            (
                ArithmeticOperator::ShiftRightSigned,
                u64::from(u32::MAX),
                1,
                OverflowBehavior::Wrap,
                Ok(vec![InterpreterValue::Size(u64::from(u32::MAX))]),
            ),
            (
                ArithmeticOperator::DivideSigned,
                u64::from(u32::MAX - 1),
                2,
                OverflowBehavior::Wrap,
                Ok(vec![InterpreterValue::Size(u64::from(u32::MAX))]),
            ),
        ];

        for (operator, left, right, overflow, expected) in cases {
            let module = size_arithmetic_module(operator, left, right, overflow);
            assert_eq!(interpret(&module, FUNCTION, Vec::new()), expected);
        }
    }

    #[test]
    fn validated_size_arithmetic_traps_at_i32_boundary() {
        let wrap_trap = size_arithmetic_module(
            ArithmeticOperator::Add,
            u64::from(u32::MAX),
            1,
            OverflowBehavior::Trap,
        );
        assert_eq!(
            interpret(&wrap_trap, FUNCTION, Vec::new()),
            Err(InterpretError::ArithmeticTrap)
        );

        let signed_division_trap = size_arithmetic_module(
            ArithmeticOperator::DivideSigned,
            u64::from(i32::MIN as u32),
            u64::from(u32::MAX),
            OverflowBehavior::Wrap,
        );
        assert_eq!(
            interpret(&signed_division_trap, FUNCTION, Vec::new()),
            Err(InterpretError::ArithmeticTrap)
        );
    }

    #[test]
    fn validated_size_values_reject_values_wider_than_i32() {
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
                    overflow: OverflowBehavior::Trap,
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

    #[test]
    fn reference_counts_trap_at_i32_maximum() {
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

    #[test]
    fn validated_scalar_arithmetic_executes() {
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
                    overflow: OverflowBehavior::Trap,
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

    #[test]
    fn validated_false_guard_traps() {
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

    #[test]
    fn validated_counted_element_store_and_load_execute() {
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

    #[test]
    fn validated_clone_and_two_drops_execute() {
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

    #[test]
    fn validated_direct_call_executes_callee() {
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
        let initial_snapshot = checked.module().clone();
        let (initial, witness) = checked.into_parts();
        let optimized = optimize_unreachable_blocks(initial).expect("zero-risk optimization");
        let module =
            validate_final(&initial_snapshot, witness, optimized).expect("final validation");

        assert_eq!(
            interpret(&module, FUNCTION, Vec::new()),
            Ok(vec![InterpreterValue::Int(42)])
        );
    }

    #[test]
    fn validated_tail_call_trampolines_three_hundred_thousand_times() {
        let base = BlockId::new(1);
        let recurse = BlockId::new(2);
        let n = ValueId::new(0);
        let zero = ValueId::new(1);
        let one = ValueId::new(2);
        let done = ValueId::new(3);
        let next = ValueId::new(4);
        let site = TailSiteId::from_digest([33; 32]);
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([34; 32]),
            layouts: empty_layouts(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: vec![ValueType::Int],
                results: vec![ValueType::Int],
                contract: CallableContract {
                    parameters: vec![ParameterContract {
                        value_type: ValueType::Int,
                        ownership: ParameterOwnership::Value,
                        non_escaping: false,
                    }],
                    results: vec![ResultContract::Value(ValueType::Int)],
                    parameter_effects: BTreeMap::new(),
                },
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [
                    (n, ValueType::Int),
                    (zero, ValueType::Int),
                    (one, ValueType::Int),
                    (done, ValueType::Bool),
                    (next, ValueType::Int),
                ]
                .into_iter()
                .collect(),
                blocks: vec![
                    BasicBlock {
                        id: ENTRY,
                        parameters: vec![BlockParameter {
                            value: n,
                            value_type: ValueType::Int,
                            ownership: None,
                        }],
                        operations: vec![
                            Operation::ConstInt {
                                destination: zero,
                                value: 0,
                            },
                            Operation::ConstInt {
                                destination: one,
                                value: 1,
                            },
                            Operation::Compare {
                                destination: done,
                                operator: ComparisonOperator::LessEqualSigned,
                                left: n,
                                right: zero,
                            },
                        ],
                        terminator: Terminator::BranchIf {
                            condition: done,
                            then_target: base,
                            then_arguments: Vec::new(),
                            else_target: recurse,
                            else_arguments: Vec::new(),
                        },
                    },
                    BasicBlock {
                        id: base,
                        parameters: Vec::new(),
                        operations: Vec::new(),
                        terminator: Terminator::Return { values: vec![zero] },
                    },
                    BasicBlock {
                        id: recurse,
                        parameters: Vec::new(),
                        operations: vec![Operation::Arithmetic {
                            destination: next,
                            operator: ArithmeticOperator::Subtract,
                            left: n,
                            right: one,
                            overflow: OverflowBehavior::Trap,
                        }],
                        terminator: Terminator::TailCall {
                            site,
                            callee: FUNCTION,
                            arguments: vec![CallArgument::Value(next)],
                        },
                    },
                ],
            }],
            proper_tail_sites: vec![ProperTailSite {
                id: site,
                function: FUNCTION,
            }],
        });
        let checked = check_initial_with_test_witness(candidate).expect("initial validation");
        let initial_snapshot = checked.module().clone();
        let (initial, witness) = checked.into_parts();
        let optimized = optimize_unreachable_blocks(initial).expect("zero-risk optimization");
        let module =
            validate_final(&initial_snapshot, witness, optimized).expect("final validation");

        assert_eq!(
            interpret_with_config(
                &module,
                FUNCTION,
                vec![InterpreterValue::Int(300_000)],
                InterpreterConfig {
                    max_steps: 2_000_010,
                },
            ),
            Ok(vec![InterpreterValue::Int(0)])
        );
    }
}
