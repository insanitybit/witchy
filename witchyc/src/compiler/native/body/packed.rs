use super::{Body, Local, I64, InstBuilder, Result, Value};
use crate::compiler::{
    logical_body::{Bits, Expr, Value as Logical},
    logical_place::Projection,
    operations::{LaneOp, Packed},
};
use cranelift_codegen::ir::{Endianness, MemFlagsData, Type, condcodes::IntCC, types::*};

impl Body<'_, '_> {
    fn packed_cast(&mut self, value: Value, ty: Type) -> Value {
        let flags = MemFlagsData::new().with_endianness(Endianness::Little);
        self.fb.ins().bitcast(ty, flags, value)
    }

    pub(super) fn float_pair_expr(&mut self, expr: &Expr) -> Result<Value> {
        match &expr.kind {
            Logical::ReadPlace(place) | Logical::Move { place, .. } | Logical::Extract { place, .. }
                if !place.pointer && place.projections.is_empty() =>
            {
                if let Local::FloatPair(var) = self.locals[place.root as usize] {
                    return Ok(self.fb.use_var(var));
                }
            }
            Logical::Aggregate(fields) => return self.packed_pair(fields, F64X2),
            Logical::Call(call) => return Ok(self.emit_call_carriers(call, None)?[0]),
            Logical::Bits(Bits::Packed(op), args) if op.both => {
                return self.packed_float(*op, args);
            }
            _ => {}
        }
        let values = self.expr(expr)?;
        Ok(self.pack_float_values(&values))
    }

    fn packed_float(&mut self, op: Packed, args: &[Expr]) -> Result<Value> {
        let a = self.packed_pair(args, F64X2)?;
        let b = self.packed_pair(&args[2..], F64X2)?;
        Ok(match op.op {
            LaneOp::FloatAdd => self.fb.ins().fadd(a, b),
            LaneOp::FloatMul => self.fb.ins().fmul(a, b),
            LaneOp::FloatDiv => self.fb.ins().fdiv(a, b),
            _ => unreachable!(),
        })
    }

    pub(super) fn pack_float_values(&mut self, values: &[Value]) -> Value {
        let low = self.bitcast_to(values[0], F64);
        let high = self.bitcast_to(values[1], F64);
        let vector = self.fb.ins().splat(F64X2, low);
        self.fb.ins().insertlane(vector, high, 1)
    }

    pub(super) fn float_pair_words(&mut self, vector: Value) -> Vec<Value> {
        let words = self.packed_cast(vector, I64X2);
        vec![self.fb.ins().extractlane(words, 0), self.fb.ins().extractlane(words, 1)]
    }

    fn packed_pair(&mut self, operands: &[Expr], ty: Type) -> Result<Value> {
        if ty == F64X2
            && let Some(low) = self.float_pair_lane(&operands[0], 0)
            && self.float_pair_lane(&operands[1], 1) == Some(low)
        {
            return Ok(self.fb.use_var(low));
        }
        let low = self.one(&operands[0])?;
        let high = self.one(&operands[1])?;
        let pair = self.fb.ins().splat(ty, low);
        Ok(if low == high { pair } else { self.fb.ins().insertlane(pair, high, 1) })
    }

    fn float_pair_lane(&self, expr: &Expr, lane: u32) -> Option<cranelift_frontend::Variable> {
        let place = match &expr.kind {
            Logical::ReadPlace(place) if matches!(place.projections.as_slice(),
                [Projection::Field { ordinal }] if *ordinal == lane) => place,
            Logical::Project(inner, ordinal) if *ordinal == lane => match &inner.kind {
                Logical::ReadPlace(place) if place.projections.is_empty() => place,
                _ => return None,
            },
            _ => return None,
        };
        match self.locals[place.root as usize] {
            Local::FloatPair(var) if !place.pointer => Some(var),
            _ => None,
        }
    }

    pub(super) fn packed(&mut self, op: Packed, args: &[Expr]) -> Result<Vec<Value>> {
        if op.both {
            let value = self.packed_float(op, args)?;
            return Ok(self.float_pair_words(value));
        }
        let ty = match op.bits {
            8 => I8X16,
            16 => I16X8,
            32 => I32X4,
            64 => I64X2,
            _ => unreachable!(),
        };
        let pair = self.packed_pair(args, I64X2)?;
        let a = if ty == I64X2 { pair } else { self.packed_cast(pair, ty) };
        if op.op == LaneOp::Mask {
            let mask = self.fb.ins().vhigh_bits(I16, a);
            return Ok(vec![self.fb.ins().uextend(I64, mask)]);
        }
        let pair = self.packed_pair(&args[2..], I64X2)?;
        let b = if ty == I64X2 { pair } else { self.packed_cast(pair, ty) };
        let result = match op.op {
            LaneOp::Add => self.fb.ins().iadd(a, b),
            LaneOp::Sub => self.fb.ins().isub(a, b),
            LaneOp::Mul => self.fb.ins().imul(a, b),
            LaneOp::Eq | LaneOp::EqMask => self.fb.ins().icmp(IntCC::Equal, a, b),
            LaneOp::Gt => self.fb.ins().icmp(IntCC::UnsignedGreaterThan, a, b),
            LaneOp::And => self.fb.ins().band(a, b),
            LaneOp::Or => self.fb.ins().bor(a, b),
            LaneOp::Xor => self.fb.ins().bxor(a, b),
            LaneOp::FloatAdd | LaneOp::FloatMul | LaneOp::FloatDiv | LaneOp::Mask => unreachable!(),
        };
        if op.op == LaneOp::EqMask {
            let mask = self.fb.ins().vhigh_bits(I16, result);
            return Ok(vec![self.fb.ins().uextend(I64, mask)]);
        }
        let pair = if ty == I64X2 { result } else { self.packed_cast(result, I64X2) };
        Ok(vec![self.fb.ins().extractlane(pair, u8::from(op.high))])
    }
}
