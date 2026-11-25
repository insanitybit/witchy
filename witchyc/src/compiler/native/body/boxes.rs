use super::{Body, I64, InstBuilder, Result, TRAP, Value};
use super::storage::{CellMode, Runtime};
use crate::compiler::{allocator::BOX_TYPE, logical_body::{Expr, Ty}, operations::BoxOp};

impl Body<'_, '_> {
    pub(super) fn box_operation(
        &mut self, result: Ty, op: BoxOp, id: u32, args: &[Expr],
    ) -> Result<Vec<Value>> {
        use BoxOp::*;
        let element = self.buffer_element(id)?;
        let element_type = self.constant(u64::from(id) + 1);
        let box_type = self.constant(BOX_TYPE | (u64::from(id) + 1));
        match op {
            New => {
                let grant = self.handle(&args[0])?;
                let payload = self.expr(&args[1])?;
                let stride = self.buffer_stride(element)?;
                let stride = self.constant(stride);
                let one = self.constant(1);
                let handle = self.runtime_value(Runtime::Allocate, &[grant, box_type, stride, one])?;
                let failed = self.allocation_failed(handle);
                self.result(result, failed, |b| {
                    let zero = b.constant(0);
                    let (status, addr) = b.cell(handle, box_type, zero, CellMode::Initialize)?;
                    b.fb.ins().trapnz(status, TRAP);
                    b.store_typed(addr, element, &payload)?;
                    Ok(vec![handle])
                }, |_| Ok(payload.clone()))
            }
            Address | Weaken | FromBuffer | WeakBits => {
                // Each of these reads its operand as a slot and generation.
                self.not_dense(args[0].ty)?;
                let handle = if op == WeakBits { self.one(&args[0])? } else { self.handle(&args[0])? };
                // The low word is the logical slot address. The high word is
                // the incarnation and must survive in the actual weak handle.
                Ok(vec![if matches!(op, Address | WeakBits) {
                    self.fb.ins().band_imm_s(handle, u32::MAX as i64)
                } else { handle }])
            }
            EndNull => {
                let handle = self.one(&args[0])?;
                self.fb.ins().trapnz(handle, TRAP);
                Ok(vec![])
            }
            Follow | FollowMut => {
                let owner = self.handle(&args[0])?;
                let weak = self.one(&args[1])?;
                let different = self.fb.ins().bxor(owner, weak);
                self.fb.ins().trapnz(different, TRAP);
                Ok(vec![self.runtime_value(Runtime::BoxLoan, &[weak, element_type])?])
            }
            FollowLoan | FollowLoanMut | Enclosing => {
                let index = if op == Enclosing { 2 } else { 1 };
                let weak = self.one(&args[index])?;
                Ok(vec![self.runtime_value(Runtime::BoxLoan, &[weak, element_type])?])
            }
            Lend | LendMut => {
                let handle = self.handle(&args[0])?;
                Ok(vec![self.runtime_value(Runtime::BoxLoan, &[handle, element_type])?])
            }
            Ref | Mut | Take => {
                let handle = if op == Take { self.one(&args[0])? } else { self.handle(&args[0])? };
                let is_null = self.fb.ins().icmp_imm_s(super::IntCC::Equal, handle, 0);
                let failed = self.fb.ins().uextend(I64, is_null);
                self.result(result, failed, |b| {
                    if op != Take {
                        return Ok(vec![b.runtime_value(Runtime::BoxLoan, &[handle, element_type])?]);
                    }
                    let zero = b.constant(0);
                    let (status, addr) = b.cell(handle, box_type, zero, CellMode::Take)?;
                    b.fb.ins().trapnz(status, TRAP);
                    let payload = b.load_typed(addr, element)?;
                    let status = b.runtime_value(Runtime::Release, &[handle, box_type])?;
                    b.fb.ins().trapnz(status, TRAP);
                    Ok(payload)
                }, |_| Ok(vec![]))
            }
        }
    }
}
