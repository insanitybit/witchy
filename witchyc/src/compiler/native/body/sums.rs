use super::{Block, Body, I64, InstBuilder, LANE_BYTES, Result, TRAP, Value};
use crate::compiler::logical_body::{Arm, Expr, Pointee, Ty};
use cranelift_codegen::ir::{BlockArg, condcodes::IntCC};
use cranelift_frontend::Switch;

impl Body<'_, '_> {
    pub(super) fn sum_value(&mut self, ty: Ty, arm: u8, payload: Vec<Value>) -> Result<Vec<Value>> {
        let size = self.layout.lanes(ty)? as usize;
        let mut lanes = Vec::with_capacity(size);
        lanes.push(self.fb.ins().iconst(I64, i64::from(arm)));
        let payload_ty = self.layout.sum(ty)?.arms[arm as usize];
        lanes.extend(self.pack_arm(payload_ty, payload)?);
        if lanes.len() < size {
            let zero = self.fb.ins().iconst(I64, 0);
            lanes.resize(size, zero);
        }
        Ok(lanes)
    }

    pub(super) fn retag(&mut self, ty: Ty, inner: &Expr, map: &[u8]) -> Result<Vec<Value>> {
        let mut lanes = self.expr(inner)?;
        let original = lanes[0];
        let invalid = self.fb.ins().icmp_imm_s(
            IntCC::UnsignedGreaterThanOrEqual,
            original,
            map.len() as i64,
        );
        self.fb.ins().trapnz(invalid, TRAP);
        let mut tag = self.fb.ins().iconst(I64, i64::from(map[0]));
        for (i, new) in map.iter().enumerate().skip(1) {
            let matches = self.fb.ins().icmp_imm_s(IntCC::Equal, original, i as i64);
            let new = self.fb.ins().iconst(I64, i64::from(*new));
            tag = self.fb.ins().select(matches, new, tag);
        }
        lanes[0] = tag;
        let size = self.layout.lanes(ty)? as usize;
        if lanes.len() < size {
            let zero = self.fb.ins().iconst(I64, 0);
            lanes.resize(size, zero);
        }
        Ok(lanes)
    }

    pub(super) fn result(
        &mut self,
        ty: Ty,
        error: Value,
        success: impl FnOnce(&mut Self) -> Result<Vec<Value>>,
        failure: impl FnOnce(&mut Self) -> Result<Vec<Value>>,
    ) -> Result<Vec<Value>> {
        let failed = self.fb.create_block();
        let succeeded = self.fb.create_block();
        let join = self.fb.create_block();
        for _ in 0..self.layout.lanes(ty)? {
            self.fb.append_block_param(join, I64);
        }
        self.fb.ins().brif(error, failed, &[], succeeded, &[]);
        self.switch_to(succeeded);
        let values = success(self)?;
        let ok = self.sum_value(ty, 0, values)?;
        let ok = ok.into_iter().map(BlockArg::from).collect::<Vec<_>>();
        self.fb.ins().jump(join, &ok);
        self.switch_to(failed);
        let values = failure(self)?;
        let err = self.sum_value(ty, 1, values)?;
        let err = err.into_iter().map(BlockArg::from).collect::<Vec<_>>();
        self.fb.ins().jump(join, &err);
        self.switch_to(join);
        Ok(self.fb.block_params(join).to_vec())
    }

    pub(super) fn match_sum(
        &mut self,
        value: &Expr,
        arms: &[Arm],
        loop_blocks: Option<(Block, Block)>,
    ) -> Result<bool> {
        let lanes = self.expr(value)?;
        let reference = if matches!(
            value.ty,
            Ty::Borrow {
                pointee: Pointee::Sum(_),
                ..
            }
        ) {
            Some(lanes[0])
        } else {
            None
        };
        let tag = match reference {
            Some(addr) => self.load_word(addr),
            None => lanes[0],
        };
        let blocks = arms
            .iter()
            .map(|_| self.fb.create_block())
            .collect::<Vec<_>>();
        let invalid = self.fb.create_block();
        let join = self.fb.create_block();
        if blocks.len() <= 4 {
            // A few arms: compares in a row. A jump table costs an indirect branch, which
            // a two-armed `Result` taken apart on every call cannot afford.
            for (value, block) in blocks.iter().enumerate() {
                let next = self.fb.create_block();
                let is = self.fb.ins().icmp_imm_u(super::IntCC::Equal, tag, value as i64);
                self.fb.ins().brif(is, *block, &[], next, &[]);
                self.switch_to(next);
            }
            self.fb.ins().jump(invalid, &[]);
        } else {
            let mut select = Switch::new();
            for (tag, block) in blocks.iter().enumerate() {
                select.set_entry(tag as u128, *block);
            }
            select.emit(&mut self.fb, tag, invalid);
        }
        self.switch_to(invalid);
        self.fb.ins().trap(TRAP);

        let mut falls_through = false;
        for (arm, block) in arms.iter().zip(blocks) {
            self.switch_to(block);
            if let Some(local) = arm.binding {
                if let Some(addr) = reference {
                    let payload = self.fb.ins().iadd_imm_s(addr, i64::from(LANE_BYTES));
                    self.set(local, &[payload])?;
                } else {
                    let payload = self.unpack_arm(arm.payload, &lanes[1..])?;
                    self.set(local, &payload)?;
                }
            }
            if self.block(&arm.body, loop_blocks)? {
                self.fb.ins().jump(join, &[]);
                falls_through = true;
            }
        }
        if falls_through {
            self.switch_to(join);
        }
        Ok(falls_through)
    }
}
