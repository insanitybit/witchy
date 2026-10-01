//! Content hashes for canonical dictionary keys.
//!
//! Equality remains the authority on a probe hit. A key with only an `Eq`
//! witness uses hash zero, so an arbitrary user-defined equivalence relation
//! cannot make equal keys unreachable by assigning them different buckets.

use super::{
    ArithmeticOperator, CanonicalLoweringError, ComparisonOperator, FunctionLowerer, Operation,
    OverflowBehavior, Terminator, Type, ValueId, ValueType,
};

impl FunctionLowerer<'_> {
    pub(super) fn lower_dict_key_hash(
        &mut self,
        key: ValueId,
        key_type: &Type,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let Type::Named(name, arguments) = key_type.unqualified() else {
            return self.push_const_int(0);
        };
        if !arguments.is_empty() {
            return self.push_const_int(0);
        }
        match name.as_str() {
            "Int" | "Duration" => self.hash_int(key),
            "String" => self.hash_string(key),
            _ => self.push_const_int(0),
        }
    }

    fn hash_int(&mut self, key: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        // Multiplicative high-bit mixing distributes adjacent and strided
        // integers across power-of-two bucket counts with one multiply.
        let factor = self.push_const_int(-7046029254386353131)?;
        let mixed = self.push_arithmetic(
            ValueType::Int,
            ArithmeticOperator::Multiply,
            key,
            factor,
            OverflowBehavior::Wrap,
        )?;
        let shift = self.push_const_int(32)?;
        self.push_arithmetic(
            ValueType::Int,
            ArithmeticOperator::ShiftRightUnsigned,
            mixed,
            shift,
            OverflowBehavior::Wrap,
        )
    }

    fn hash_string(&mut self, key: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        self.ensure_string_extent(key)?;
        let ValueType::Ref(layout) = self.values[&key] else {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "dict string key hash",
            ));
        };
        let length =
            *self
                .string_lengths
                .get(&key)
                .ok_or(CanonicalLoweringError::UnsupportedExpression(
                    "dict string key extent",
                ))?;
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let seed = self.push_const_int(-3750763034362895579)?;
        let prime = self.push_const_int(1099511628211)?;
        let header = self.new_block(&[ValueType::Size, ValueType::Int])?;
        let body = self.new_block(&[ValueType::Size, ValueType::Int])?;
        let exit = self.new_block(&[ValueType::Int])?;
        self.finish_current(Terminator::Branch {
            target: header,
            arguments: vec![zero, seed],
        })?;
        self.current = header;
        let state = self.block_parameter_values(header);
        let more = self.push_compare(ComparisonOperator::LessUnsigned, state[0], length)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: state.clone(),
            else_target: exit,
            else_arguments: vec![state[1]],
        })?;
        self.current = body;
        let state = self.block_parameter_values(body);
        let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, state[0], length)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let byte = self.push_string_byte_load(key, state[0], layout)?;
        let widened = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize {
            destination: widened,
            value: byte,
        });
        let mixed = self.push_arithmetic(
            ValueType::Int,
            ArithmeticOperator::BitXor,
            state[1],
            widened,
            OverflowBehavior::Wrap,
        )?;
        let mixed = self.push_arithmetic(
            ValueType::Int,
            ArithmeticOperator::Multiply,
            mixed,
            prime,
            OverflowBehavior::Wrap,
        )?;
        let next = self.push_arithmetic(
            ValueType::Size,
            ArithmeticOperator::Add,
            state[0],
            one,
            OverflowBehavior::Wrap,
        )?;
        self.finish_current(Terminator::Branch {
            target: header,
            arguments: vec![next, mixed],
        })?;
        self.current = exit;
        Ok(self.block_parameter_values(exit)[0])
    }
}
