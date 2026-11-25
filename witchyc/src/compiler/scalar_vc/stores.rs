use super::*;

impl Context<'_> {
    /// A record written to a place: what is known of its scalar parts goes with it. Every part is
    /// evaluated before anything is written, as the emitted code builds the whole value and then
    /// stores it: a part whose call changes the destination does not change what the others wrote.
    pub(super) fn store(&mut self, root: u32, path: Vec<u32>, e: &Expr) -> Result<()> {
        let (mut writes, mut pending) = (Vec::new(), None);
        self.gather(root, &path, e, &mut writes, &mut pending)?;
        self.forget(root, &path);
        for (key, value) in writes { self.remember(key, value); }
        if let Some(outcome) = pending { self.pending.insert(root, outcome); }
        Ok(())
    }
    pub(super) fn gather(&mut self, root: u32, path: &[u32], e: &Expr, writes: &mut Vec<(Key, Math)>, pending: &mut Option<Outcome>) -> Result<()> {
        match &e.kind {
            Value::Sum { arm, payload } => {
                writes.push(((root, join(path, &[SUM_TAG])), Math::Literal(u64::from(*arm))));
                let inner = join(path, &[ARM + u32::from(*arm)]);
                if tracked(payload.ty) {
                    let number = self.number(payload)?;
                    writes.push(((root, inner), self.bind(number)));
                } else {
                    self.gather(root, &inner, payload, writes, pending)?;
                }
            }
            Value::U64(0) if matches!(e.ty, Ty::Boxed(_) | Ty::Weak(_)) => {
                writes.push(((root, join(path, &[BOX_ID])), Math::Literal(0)));
                writes.push(((root, join(path, &[BOX_ADDRESS])), Math::Literal(0)));
            }
            Value::BoxOp(crate::compiler::operations::BoxOp::Enclosing, _, args) => {
                self.walk(e)?;
                let (owner, walk) = self.shared_path(&args[0], 0).ok_or_else(|| error("missing checked structural witness"))?;
                let (key, tag) = member::stamps(&walk)?;
                let stamp = self.raw_leaf((owner, key), Ty::U64);
                writes.push(((root, join(path, &tag)), stamp));
            }
            Value::BoxOp(crate::compiler::operations::BoxOp::FollowLoan | crate::compiler::operations::BoxOp::FollowLoanMut, _, args) => {
                // The proof identifies precisely this initialized element. Copy its
                // known fields before taking a unique loan invalidates the owner.
                if let Some((owner, mut cell)) = self.loan_base(&args[0], 0) && !self.lent.contains(&owner) {
                    cell.push(BOX_VALUE);
                    if matches!(e.ty, Ty::Borrow { mutable: false, .. })
                        && let (Some(resource), Some(carried)) = (member_path(&cell, true), member_path(&cell, false)) {
                        let stamp = self.raw_leaf((owner, resource), Ty::U64);
                        writes.push(((root, join(path, &carried)), stamp));
                    }
                    for ((local, fields), value) in &self.state {
                        if *local == owner && let Some(rest) = fields.strip_prefix(cell.as_slice()) {
                            writes.push(((root, join(path, rest)), value.clone()));
                        }
                    }
                }
                self.walk(e)?;
            }
            Value::BoxOp(crate::compiler::operations::BoxOp::Lend | crate::compiler::operations::BoxOp::LendMut
                | crate::compiler::operations::BoxOp::Follow | crate::compiler::operations::BoxOp::FollowMut, _, args) => {
                let snapshot = self.box_contents(&args[0]);
                self.walk(e)?;
                for (field, value) in snapshot { writes.push(((root, join(path, &field)), value)); }
            }
            Value::BoxOp(crate::compiler::operations::BoxOp::FromBuffer, _, args) => {
                let (word, incarnation) = self.pointer_parts(&args[0])?;
                // Allocation bases lie in the nonzero program heap; a projected
                // pointer carries the source allocation incarnation even at extent zero.
                self.hypotheses.push(Prop::Compare(">", word.clone(), Math::Literal(0)));
                writes.push(((root, join(path, &[BOX_ADDRESS])), word));
                writes.push(((root, join(path, &[BOX_ID])), incarnation));
            }
            Value::BoxOp(crate::compiler::operations::BoxOp::Weaken, _, args) => {
                let (word, incarnation) = self.pointer_parts(&args[0])?;
                writes.push(((root, join(path, &[BOX_ADDRESS])), word));
                writes.push(((root, join(path, &[BOX_ID])), incarnation));
            }
            Value::BoxOp(op @ (crate::compiler::operations::BoxOp::Ref | crate::compiler::operations::BoxOp::Mut | crate::compiler::operations::BoxOp::Take), _, args) => {
                let outcome = self.box_variant(*op, &args[0])?;
                if path.is_empty() && !self.lent.contains(&root) { *pending = Some(outcome); }
            }
            Value::BoxOp(crate::compiler::operations::BoxOp::New, _, args) => {
                let outcome = self.new_box(args)?;
                if path.is_empty() && !self.lent.contains(&root) { *pending = Some(outcome); }
            }
            Value::Aggregate(parts) if self.types.get(e.ty).is_some_and(|record| record.fields.len() == parts.len()) => for (index, part) in parts.iter().enumerate() {
                // An array's elements are keyed as an element read finds them (`ELEMENT | k`).
                let array = self.types.get(e.ty).is_some_and(|aggregate| matches!(aggregate.kind, crate::compiler::semantic_types::Kind::Array(..)));
                let step = if array { ELEMENT | index as u32 } else { index as u32 };
                let inner = join(path, &[step]);
                if tracked(part.ty) { let value = self.number(part)?; let value = self.bind(value); writes.push(((root, inner), value)); } else { self.gather(root, &inner, part, writes, pending)?; }
            },
            // A slice of a whole fixed array: its length is the array's (`let s = a.slice()`). What
            // making it does to the array (a `slice_mut` lends it) is as for any other value first.
            // A slice copied from another keeps its length through the arm below (every known part).
            Value::SliceOf(_, length) => {
                self.effects(e)?;
                writes.push(((root, join(path, &[SLICE_LENGTH])), Math::Literal(*length as u64)));
            }
            // The bytes of borrowed text (`text.as_bytes()`): the same bytes, so the same length.
            Value::Widen(inner) if matches!(e.ty, Ty::Borrow { mutable: false, pointee: crate::compiler::logical_body::Pointee::Slice(_) })
                && matches!(inner.ty, Ty::Borrow { mutable: false, pointee: crate::compiler::logical_body::Pointee::Slice(_) }) => {
                self.effects(e)?;
                if let Some(key) = self.slice_length(inner) {
                    let length = self.leaf(key, Ty::Unsigned(crate::compiler::scalar::Scalar::U64));
                    writes.push(((root, join(path, &[SLICE_LENGTH])), length));
                }
            }
            Value::Call(call) => {
                // A shared view of a proved initialized slot exposes the stored
                // value. The resolved body must read that very slot.
                if matches!(e.ty, Ty::Borrow { mutable: false, .. })
                    && let Some((owner, mut storage)) = self.loan_base(e, 0)
                    && !self.lent.contains(&owner) {
                    storage.push(BOX_VALUE);
                    for ((id, fields), v) in &self.state {
                        if *id == owner && let Some(rest) = fields.strip_prefix(storage.as_slice()) {
                            writes.push(((root, join(path, rest)), v.clone()));
                        }
                    }
                }
                let (_, outcome) = self.call(call)?;
                for ((arm, rest), value) in &outcome.values { if arm.is_none() { writes.push(((root, join(path, rest)), value.clone())); } }
                if path.is_empty() && !self.lent.contains(&root) { *pending = Some(outcome); }
            }
            Value::Move { place, .. } | Value::Extract { place, .. } | Value::ReadPlace(place) | Value::Address(place) if !matches!(e.ty, Ty::Borrow { mutable: true, .. }) && fields(place).is_some() && !self.lent.contains(&place.root) => {
                let source = fields(place).expect("guarded");
                let known = self.state.iter().filter(|((local, fields), _)| *local == place.root && fields.starts_with(&source))
                    .map(|((_, fields), value)| (fields[source.len()..].to_vec(), value.clone())).collect::<Vec<_>>();
                for (rest, value) in known {
                    if rest.first() == Some(&MEMBER_STAMP) && !matches!(e.ty, Ty::Borrow { mutable: false, .. }) { continue; }
                    writes.push(((root, join(path, &rest)), value));
                }
                if path.is_empty() && source.is_empty() && !place.pointer { if let Some(outcome) = self.pending.get(&place.root).cloned() { *pending = Some(outcome); } }
            }
            _ => self.effects(e)?,
        }
        Ok(())
    }
}
