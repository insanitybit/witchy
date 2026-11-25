use super::*;

impl Checker<'_> {
    /// An I64 operation. The memory and record models follow only what they were written for,
    /// so a function they prove does not use I64; a scalar proof treats the result as unknown.
    pub(super) fn signed(&self, op: Signed, operands: Vec<Expr>) -> Result<(Ty, Value)> {
        if self.modelled() { return Err(error("a function proved by the memory or record model cannot use I64 yet")); }
        let ty = match op { Signed::Eq | Signed::Ne | Signed::Lt | Signed::Le | Signed::Gt | Signed::Ge => Ty::Bool, Signed::Bits => Ty::U64, _ => Ty::I64 };
        Ok((ty, Value::Signed(op, operands)))
    }
    /// An F64 operation. No proof model reads floating point yet, so a function with a contract
    /// does not use F64 (docs/wasm-f64.md).
    /// A bit operation on an unsigned type (`& | ^ ~ << >>`). A scalar proof reads its result
    /// as an unknown value of the type, as for F64; the memory and record models refuse it.
    pub(super) fn bits(&self, op: crate::compiler::logical_body::Bits, ty: Ty, operands: Vec<Expr>) -> Result<(Ty, Value)> {
        if !matches!(ty, Ty::Unsigned(_)) { return Err(error("bit operations `& | ^ ~ << >>` are on unsigned integers (U8 to U64)")); }
        if self.modelled() { return Err(error("a function proved by the memory or record model cannot use bit operations yet")); }
        Ok((ty, Value::Bits(op, operands)))
    }
    pub(super) fn float(&self, op: crate::compiler::logical_body::Float, operands: Vec<Expr>) -> Result<(Ty, Value)> {
        use crate::compiler::logical_body::Float as F;
        // A scalar proof reads an F64 result as an unknown value, as it does what it does not
        // follow; the memory and record models follow only what they were written for.
        if self.modelled() { return Err(error("a function proved by the memory or record model cannot use F64 yet")); }
        // `to_i64` traps on NaN and outside I64, and a proof reads F64 values as unknowns, so it
        // could never show the conversion does not trap; a proved function must return (red-team
        // review: one with a contract trapped here).
        if self.contracted && matches!(op, F::ToI64) { return Err(error("`F64.to_i64` may trap, which a function with a contract does not: convert in a function without one")); }
        let ty = match op { F::Eq | F::Ne | F::Lt | F::Le | F::Gt | F::Ge => Ty::Bool, F::Bits => Ty::U64, F::ToI64 => Ty::I64, _ => Ty::F64 };
        Ok((ty, Value::Float(op, operands)))
    }
    /// Whether `e` is an F64, as far as its form says before it is checked.
    pub(super) fn floating(&mut self, e: &a::Expr) -> bool {
        matches!(e.kind, a::Expression::Float(_)) || matches!(self.infer_hint(e, None, 0, &mut 0), Ok(Some(Ty::F64)))
    }
    /// A proof by a memory or record model, which follow only some shapes of body.
    pub(super) fn modelled(&self) -> bool { self.contracted && !self.scalar_proof }
    /// `value.method(args)` and `Type.function(args)` as the call of the function they name.
    /// The receiver is lent, moved or copied as the method's first parameter asks.
    /// Whether `value[at]` is a fixed array's own indexing: the value is an array, a reference to
    /// one, or of a type not known yet (which the array rule then reports).
    /// The type a read gives: the hint, or for an index of a fixed array or slice its element's.
    fn read_type(&self, e: &a::Expr) -> Option<Ty> {
        if let Some(ty) = self.hint(e) { return Some(ty); }
        let a::Expression::Index(v, _) = &e.kind else { return None };
        match self.hint(v) {
            Some(Ty::Borrow { pointee: Pointee::Slice(id), .. }) => Some(self.layouts.slice_element(id)),
            Some(ty) => {
                let ty = match ty { Ty::Borrow { pointee, .. } => pointee.ty(), other => other };
                match self.layouts.get(ty).map(|layout| layout.kind.clone()) { Some(Kind::Array(element, _)) => Some(element), _ => None }
            }
            None => None,
        }
    }
    /// Whether evaluating `e` reads and nothing else: names, literals, fields, `*` and `!`, indexes
    /// of fixed arrays and slices, and operators whose operands are numbers or Bools. An index or
    /// operator on any other type becomes a method call, which may change anything.
    pub(super) fn only_reads(&self, e: &a::Expr) -> bool {
        let scalar = |ty: Option<Ty>| matches!(ty, Some(Ty::Unsigned(_) | Ty::Bool | Ty::I64));
        match &e.kind {
            a::Expression::Name(_) | a::Expression::Integer(_) | a::Expression::Bool(_) => true,
            a::Expression::Field(v, _) | a::Expression::Unary("*" | "!", v) => self.only_reads(v),
            a::Expression::Index(v, at) => {
                let indexed = match self.hint(v) { Some(Ty::Borrow { pointee, .. }) => Some(pointee.ty()), other => other };
                let plain = matches!(self.hint(v), Some(Ty::Borrow { pointee: Pointee::Slice(_), .. }))
                    || indexed.is_some_and(|ty| self.layouts.get(ty).is_some_and(|layout| matches!(layout.kind, Kind::Array(..))));
                plain && self.only_reads(v) && self.only_reads(at)
            }
            // An integer literal takes the type of the number beside it (`values[i] * 2`).
            a::Expression::Binary(_, left, right) => {
                let literal = |e: &a::Expr| matches!(e.kind, a::Expression::Integer(_));
                let number = |e: &a::Expr, other: &a::Expr| scalar(self.read_type(e)) || (literal(e) && scalar(self.read_type(other)));
                number(left, right) && number(right, left) && self.only_reads(left) && self.only_reads(right)
            }
            _ => false,
        }
    }
}
