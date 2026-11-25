//! Only internal, shared parameter loans can supply a reusable read. The
//! caller owns their backing storage throughout the call. Local loans (even
//! immutable ones) can designate different storage on each loop iteration.
use super::{Binary, Body, Expr, InstBuilder, Place, Projection, Result, Stmt, Ty, Value};
use crate::compiler::logical_body::Value as Logical;
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Field {
    root: u32,
    offset: u32,
}

impl Body<'_, '_> {
    fn field(&self, p: &Place) -> Option<Field> {
        if !p.pointer
            || !self.shared_params.contains(&p.root)
            || !matches!(p.ty, Ty::U64 | Ty::Bool)
        {
            return None;
        }
        let mut ty = p.root_ty;
        let mut offset = 0;
        for projection in &p.projections {
            let Projection::Field { ordinal } = projection else {
                return None;
            };
            let field = self.layout.field(ty, *ordinal).ok()?;
            offset += field.byte_offset;
            ty = field.ty;
        }
        Some(Field {
            root: p.root,
            offset,
        })
    }

    pub(super) fn read_shared(&mut self, p: &Place) -> Result<Option<Value>> {
        let Some(key) = self.field(p) else {
            return Ok(None);
        };
        if let Some(value) = self.reads.get(&key) {
            return Ok(Some(*value));
        }
        let addr = self.place(p)?;
        let value = self.load_typed(addr, p.ty)?[0];
        self.reads.insert(key, value);
        Ok(Some(value))
    }

    pub(super) fn forget_local(&mut self, root: u32) {
        if self.shared_params.contains(&root) {
            self.reads.retain(|key, _| key.root != root);
        }
    }

    // A straight-line loop with only private writes, fixed parameter loans
    // and eager operands. Collect only reads on the taken path. The indexed
    // element addresses themselves stay in the body, including their traps.
    fn loop_reads<'p>(&self, stmts: &'p [Stmt]) -> Option<BTreeMap<Field, &'p Place>> {
        let mut reads = BTreeMap::new();
        for stmt in stmts {
            let value = match stmt {
                Stmt::Set(id, e) if !self.shared_params.contains(id) => e,
                Stmt::StorePlace(p, _, e)
                    if !p.pointer
                        && p.projections.is_empty()
                        && !self.shared_params.contains(&p.root) =>
                {
                    e
                }
                Stmt::Expire(ids) if ids.iter().all(|id| !self.shared_params.contains(id)) => {
                    continue;
                }
                _ => return None,
            };
            if !self.loop_operand(value, &mut reads) {
                return None;
            }
        }
        Some(reads)
    }

    fn loop_operand<'p>(&self, e: &'p Expr, reads: &mut BTreeMap<Field, &'p Place>) -> bool {
        match &e.kind {
            Logical::Bool(_) | Logical::U64(_) => true,
            Logical::Binary(Binary::And | Binary::Or, ..) => false,
            Logical::Binary(_, a, b) => self.loop_operand(a, reads) && self.loop_operand(b, reads),
            Logical::ReadPlace(p) | Logical::Address(p) => {
                if matches!(e.kind, Logical::ReadPlace(_))
                    && let Some(key) = self.field(p)
                {
                    reads.insert(key, p);
                }
                p.projections.iter().all(|projection| match projection {
                    Projection::Field { .. } => true,
                    Projection::Index { value, .. } => self.loop_operand(value, reads),
                })
            }
            _ => false,
        }
    }

    pub(super) fn shared_loop(&mut self, condition: &Expr, stmts: &[Stmt]) -> Result<bool> {
        // This test has no calls, temporaries or short circuit edges. Emit
        // it once at entry and once at the latch; exactly one executes per
        // original test, with the original logical frame charge (zero).
        let simple = |e: &Expr| {
            matches!(&e.kind, Logical::U64(_) | Logical::Bool(_))
                || matches!(&e.kind, Logical::ReadPlace(p) if !p.pointer && p.projections.is_empty())
        };
        if !matches!(&condition.kind, Logical::Binary(Binary::Lt | Binary::Le | Binary::Gt | Binary::Ge | Binary::Eq | Binary::Ne, a, b) if simple(a) && simple(b))
        {
            return Ok(false);
        }
        let Some(reads) = self.loop_reads(stmts) else {
            return Ok(false);
        };
        if reads.is_empty() {
            return Ok(false);
        }
        let prepare = self.fb.create_block();
        let run = self.fb.create_block();
        let exit = self.fb.create_block();
        let first = self.one(condition)?;
        self.fb.ins().brif(first, prepare, &[], exit, &[]);
        self.switch_to(prepare);
        // These fixed, in-bounds scalar fields of live internal arguments
        // cannot fault. No dynamic projection or user check moves here.
        for p in reads.values() {
            self.read_shared(p)?;
        }
        self.fb.ins().jump(run, &[]);
        // The preheader dominates every iteration and the scan above
        // established that no iteration can change these locations.
        self.fb.switch_to_block(run);
        self.block(stmts, None)?;
        let again = self.one(condition)?;
        self.fb.ins().brif(again, run, &[], exit, &[]);
        self.switch_to(exit);
        Ok(true)
    }
}
