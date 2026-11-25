use super::{Body, Location, InstBuilder, Result, TRAP, Value, error, storage::Runtime};
use crate::compiler::logical_body::{Expr, Pointee, Reach, Ty, Value as Logical};

impl Body<'_, '_> {
    pub(super) fn element_stride(&self, ty: Ty) -> Result<u32> {
        let Ty::Borrow {
            pointee: Pointee::Slice(slice),
            ..
        } = ty
        else {
            return Err(error("expected native slice type"));
        };
        let element = self.layout.types.slice_element(slice);
        Ok(self.layout.storage(element)?.size)
    }

    pub(super) fn slice_at(
        &mut self,
        result: Ty,
        slice: &Expr,
        index: &Expr,
        reach: Reach,
    ) -> Result<Vec<Value>> {
        if reach == Reach::Walk {
            // The loop test establishes bounds for this access.
            let address = self.slice_word(slice, 0)?;
            let index = self.one(index)?;
            let stride = self.element_stride(slice.ty)?;
            return Ok(vec![self.offset(address, index, stride)]);
        }
        let (address, length) = self.slice(slice)?;
        let index = self.one(index)?;
        let stride = self.element_stride(slice.ty)?;
        let past = self
            .fb
            .ins()
            .icmp(super::IntCC::UnsignedGreaterThanOrEqual, index, length);
        if reach == Reach::Result {
            self.result(
                result,
                past,
                |b| Ok(vec![b.offset(address, index, stride)]),
                |_| Ok(vec![index]),
            )
        } else {
            self.fb.ins().trapnz(past, TRAP);
            Ok(vec![self.offset(address, index, stride)])
        }
    }

    pub(super) fn offset(&mut self, address: Value, index: Value, stride: u32) -> Value {
        // SSA operands already account for rebinding and effects in the index.
        let block = self.fb.current_block().expect("offset in a native block");
        let key = (block, address, index, stride);
        if let Some(&computed) = self.offsets.get(&key) {
            return computed;
        }
        let delta = if stride.is_power_of_two() {
            self.fb.ins().ishl_imm_s(index, i64::from(stride.trailing_zeros()))
        } else {
            self.fb.ins().imul_imm_s(index, i64::from(stride))
        };
        let computed = self.fb.ins().iadd(address, delta);
        self.offsets.insert(key, computed);
        computed
    }

    /// `s.part(lo, hi)`, or with `within` `s.part_proved(lo, hi)`: no range test where the
    /// function's proof showed the range, and on text the empty text, not a trap, where an end
    /// is inside a scalar.
    pub(super) fn slice_part(&mut self, slice: &Expr, lo: &Expr, hi: &Expr, within: bool) -> Result<Vec<Value>> {
        use cranelift_codegen::ir::{BlockArg, types::I64};
        let (address, length) = self.slice(slice)?;
        let lo = self.one(lo)?;
        let hi = self.one(hi)?;
        if !(within && self.abi.proved_slice_bounds) {
            let reversed = self
                .fb
                .ins()
                .icmp(super::IntCC::UnsignedGreaterThan, lo, hi);
            let past = self
                .fb
                .ins()
                .icmp(super::IntCC::UnsignedGreaterThan, hi, length);
            let invalid = self.fb.ins().bor(reversed, past);
            self.fb.ins().trapnz(invalid, TRAP);
        }
        let mut count = self.fb.ins().isub(hi, lo);
        if matches!(slice.ty, Ty::Borrow { pointee: Pointee::Slice(id), .. } if self.layout.types.is_str(id)) {
            for endpoint in [lo, hi] {
                let inspect = self.fb.create_block();
                let done = self.fb.create_block();
                // What is left of the count: all of it at the end of the text or where a scalar
                // starts, none of it inside one.
                let kept = self.fb.append_block_param(done, I64);
                let at_end = self.fb.ins().icmp(super::IntCC::Equal, endpoint, length);
                self.fb.ins().brif(at_end, done, &[BlockArg::from(count)], inspect, &[]);
                self.fb.seal_block(inspect);
                self.fb.switch_to_block(inspect);
                let pointer = self.fb.ins().iadd(address, endpoint);
                let byte = self.fb.ins().load(cranelift_codegen::ir::types::I8, cranelift_codegen::ir::MemFlagsData::new(), pointer, 0);
                let high = self.fb.ins().band_imm_u(byte, 192);
                let continuation = self.fb.ins().icmp_imm_u(super::IntCC::Equal, high, 128);
                if within {
                    let zero = self.fb.ins().iconst(I64, 0);
                    let rest = self.fb.ins().select(continuation, zero, count);
                    self.fb.ins().jump(done, &[BlockArg::from(rest)]);
                } else {
                    self.fb.ins().trapnz(continuation, TRAP);
                    self.fb.ins().jump(done, &[BlockArg::from(count)]);
                }
                self.fb.seal_block(done);
                self.fb.switch_to_block(done);
                count = kept;
            }
        }
        let stride = self.element_stride(slice.ty)?;
        let address = self.offset(address, lo, stride);
        Ok(vec![address, count])
    }

    pub(super) fn slice_read_le64(&mut self, slice: &Expr, index: &Expr) -> Result<Value> {
        use cranelift_codegen::ir::{MemFlagsData, types::I64};

        let (base, len) = self.slice(slice)?;
        let index = self.one(index)?;
        // The function's proof showed the eight bytes inside the slice: no test.
        if !self.abi.proved_slice_bounds {
            let past = self.fb.ins().icmp(super::IntCC::UnsignedGreaterThan, index, len);
            self.fb.ins().trapnz(past, TRAP);
            let remaining = self.fb.ins().isub(len, index);
            let short = self.fb.ins().icmp_imm_u(super::IntCC::UnsignedLessThan, remaining, 8);
            self.fb.ins().trapnz(short, TRAP);
        }
        let address = self.fb.ins().iadd(base, index);
        Ok(self.fb.ins().load(I64, MemFlagsData::new(), address, 0))
    }

    pub(super) fn slice_copy(&mut self, target: &Expr, source: &Expr) -> Result<()> {
        let (to, target_length) = self.slice(target)?;
        let (from, length) = self.slice(source)?;
        let mismatch = self
            .fb
            .ins()
            .icmp(super::IntCC::NotEqual, target_length, length);
        self.fb.ins().trapnz(mismatch, TRAP);
        let stride = self.element_stride(source.ty)?;
        let bytes = self.fb.ins().imul_imm_s(length, i64::from(stride));
        self.runtime(Runtime::CopySlice, &[to, from, bytes])?;
        Ok(())
    }

    pub(super) fn slice_word(&mut self, expr: &Expr, word: usize) -> Result<Value> {
        if let Logical::ReadPlace(place) = &expr.kind {
            match self.locate(place)? {
                Location::Words(words, None) => return Ok(self.fb.use_var(words[word])),
                Location::Address(address, _) => {
                    return Ok(self.fb.ins().load(super::I64, super::MemFlagsData::new(), address, (word * 8) as i32));
                }
                _ => {}
            }
        }
        let (address, length) = self.slice(expr)?;
        Ok([address, length][word])
    }

    pub(super) fn slice(&mut self, expr: &Expr) -> Result<(Value, Value)> {
        match self.expr(expr)?.as_slice() {
            [address, length] => Ok((*address, *length)),
            _ => Err(error("expected a native slice")),
        }
    }
}
