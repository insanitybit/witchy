use super::{Body, I64, InstBuilder, Result, TRAP, Value};
use crate::compiler::logical_body::{Expr, Float, Signed, Ty, Value as Logical};
use cranelift_codegen::ir::{
    condcodes::{FloatCC, IntCC},
    immediates::Ieee64,
    types::F64,
};

impl Body<'_, '_> {
    pub(super) fn narrow(&mut self, value: Value, ty: Ty) -> Value {
        if let Ty::Unsigned(scalar) = ty
            && scalar.narrow_index().is_some()
        {
            let past = self.fb.ins().icmp_imm_s(
                IntCC::UnsignedGreaterThan,
                value,
                scalar.layout().maximum() as i64,
            );
            self.fb.ins().trapnz(past, TRAP);
        }
        value
    }

    pub(super) fn signed(&mut self, op: Signed, operands: &[Expr]) -> Result<Value> {
        let a = self.one(&operands[0])?;
        if matches!(op, Signed::FromBits | Signed::Bits) {
            return Ok(a);
        }
        let b = self.one(&operands[1])?;
        let comparison = match op {
            Signed::Add | Signed::Sub | Signed::Mul => {
                let (value, overflow) = match op {
                    Signed::Add => self.fb.ins().sadd_overflow(a, b),
                    Signed::Sub => self.fb.ins().ssub_overflow(a, b),
                    _ => self.fb.ins().smul_overflow(a, b),
                };
                self.fb.ins().trapnz(overflow, TRAP);
                return Ok(value);
            }
            Signed::Div => return Ok(self.fb.ins().sdiv(a, b)),
            Signed::Rem => return Ok(self.fb.ins().srem(a, b)),
            Signed::Eq => IntCC::Equal,
            Signed::Ne => IntCC::NotEqual,
            Signed::Lt => IntCC::SignedLessThan,
            Signed::Le => IntCC::SignedLessThanOrEqual,
            Signed::Gt => IntCC::SignedGreaterThan,
            Signed::Ge => IntCC::SignedGreaterThanOrEqual,
            Signed::FromBits | Signed::Bits => unreachable!(),
        };
        let value = self.fb.ins().icmp(comparison, a, b);
        Ok(self.fb.ins().uextend(I64, value))
    }

    pub(super) fn float(&mut self, op: Float, operands: &[Expr]) -> Result<Value> {
        if matches!(op, Float::FromBits)
            && let Logical::U64(bits) = operands[0].kind
        {
            return Ok(self.fb.ins().f64const(Ieee64::with_bits(bits)));
        }
        let a = self.one(&operands[0])?;
        Ok(match op {
            Float::FromBits => self.bitcast_to(a, F64),
            Float::Bits => {
                let nan = self.fb.ins().fcmp(FloatCC::Unordered, a, a);
                let canonical = self.constant(0x7ff8_0000_0000_0000);
                let bits = self.bitcast_to(a, I64);
                self.fb.ins().select(nan, canonical, bits)
            }
            Float::FromU64 => self.fb.ins().fcvt_from_uint(F64, a),
            Float::FromI64 => self.fb.ins().fcvt_from_sint(F64, a),
            Float::ToI64 => self.fb.ins().fcvt_to_sint(I64, a),
            Float::Neg => self.fb.ins().fneg(a),
            Float::Sqrt => self.fb.ins().sqrt(a),
            Float::Abs => self.fb.ins().fabs(a),
            Float::Floor => self.fb.ins().floor(a),
            _ => {
                let b = self.one(&operands[1])?;
                let comparison = match op {
                    Float::Add => return Ok(self.fb.ins().fadd(a, b)),
                    Float::Sub => return Ok(self.fb.ins().fsub(a, b)),
                    Float::Mul => return Ok(self.fb.ins().fmul(a, b)),
                    Float::Div => return Ok(self.fb.ins().fdiv(a, b)),
                    Float::Eq => FloatCC::Equal,
                    Float::Ne => FloatCC::NotEqual,
                    Float::Lt => FloatCC::LessThan,
                    Float::Le => FloatCC::LessThanOrEqual,
                    Float::Gt => FloatCC::GreaterThan,
                    Float::Ge => FloatCC::GreaterThanOrEqual,
                    _ => unreachable!("unary float operation handled above"),
                };
                let value = self.fb.ins().fcmp(comparison, a, b);
                self.fb.ins().uextend(I64, value)
            }
        })
    }
}
