use super::*;

impl Context<'_> {
    /// The value of an unsigned or Bool expression.
    pub(super) fn pointer_parts(&mut self, owner: &Expr) -> Result<(Math, Math)> {
        let kind = Ty::Unsigned(crate::compiler::scalar::Scalar::U32);
        if matches!(owner.ty, Ty::Boxed(_) | Ty::Weak(_)) && matches!(owner.kind, Value::U64(0)) {
            return Ok((Math::Literal(0), Math::Literal(0)));
        }
        if let Some(place) = box_place(owner) && let Some(path) = fields(place) {
            self.place(place)?;
            let address = self.leaf((place.root, join(&path, &[BOX_ADDRESS])), kind);
            let incarnation = self.leaf((place.root, join(&path, &[BOX_ID])), Ty::U64);
            return Ok((address, incarnation));
        }
        self.effects(owner)?;
        Ok((self.fresh(kind), self.fresh(Ty::U64)))
    }
    /// Recover the live allocation behind an element loan at index zero. Follow
    /// only fixed field projections of matching mutability and transparent returns.
    /// The actual storage borrow has a separate initialization and bounds proof.
    pub(super) fn shared_path(&self, e: &Expr, depth: usize) -> Option<(u32, Vec<u32>)> {
        if depth > 32 || !matches!(e.ty, Ty::Borrow { mutable: false, .. }) { return None; }
        if let Some(p) = box_place(e) {
            return p.projections.iter().all(|v| matches!(v, Projection::Field { .. })).then(|| (p.root, prefix(p)));
        }
        let Value::Call(call) = &e.kind else { return None };
        let [arg] = call.args.as_slice() else { return None };
        let (root, mut path) = self.shared_path(arg, depth + 1)?;
        let f = self.functions.get(call.definition.0 as usize)?;
        if let Some(selected) = view::direct(f) { path.extend(selected); }
        else { view::cases(self.functions, call.definition.0)?; path.push(view::FOLD + call.definition.0); }
        Some((root, path))
    }
    pub(super) fn loan_base(&self, loan: &Expr, depth: usize) -> Option<(u32, Vec<u32>)> {
        let Ty::Borrow { mutable, .. } = loan.ty else { return None };
        if depth > 32 { return None; }
        if !mutable && let Some((root, mut path)) = self.shared_path(loan, depth)
            && path.pop() == Some(BOX_VALUE) {
            return Some((root, path));
        }
        match &loan.kind {
            Value::Storage { op, receiver, index, .. } if matches!((op, mutable), (StorageOp::RefUnchecked, false) | (StorageOp::MutUnchecked, true))
                && matches!(index.kind, Value::U64(0)) => {
                let buffer = box_place(receiver)?;
                buffer.projections.iter().all(|p| matches!(p, Projection::Field { .. })).then(|| (buffer.root, prefix(buffer)))
            }
            Value::Call(call) => {
                let callee = self.functions.get(call.definition.0 as usize)?;
                let [argument] = call.args.as_slice() else { return None };
                let [Ty::Borrow { mutable: arg_mut, .. }] = callee.params.as_slice() else { return None };
                if *arg_mut != mutable { return None; }
                let [Stmt::Return(result, _, _)] = callee.body.as_slice() else { return None };
                let (0, projection) = self.loan_base(result, depth + 1)? else { return None };
                let base = box_place(argument)?;
                base.projections.iter().all(|p| matches!(p, Projection::Field { .. })).then(|| (base.root, join(&prefix(base), &projection)))
            }
            _ => None,
        }
    }
    pub(super) fn box_contents(&self, owner: &Expr) -> Vec<(Vec<u32>, Math)> {
        let Some(place) = box_place(owner) else { return Vec::new() };
        let Some(path) = fields(place) else { return Vec::new() };
        if self.lent.contains(&place.root) { return Vec::new(); }
        let inside = join(&path, &[BOX_VALUE]);
        self.state.iter().filter_map(|((root, path), value)| {
            (*root == place.root).then(|| path.strip_prefix(inside.as_slice())).flatten()
                .map(|rest| (rest.to_vec(), value.clone()))
        }).collect()
    }
    pub(super) fn forget_box_contents(&mut self, owner: &Expr) {
        if let Some(place) = box_place(owner) {
            if let Some(path) = fields(place) { self.forget(place.root, &join(&path, &[BOX_VALUE])); }
            else { self.forget(place.root, &prefix(place)); }
        } else {
            self.state.retain(|(_, path), _| !path.contains(&BOX_VALUE));
        }
    }
    pub(super) fn box_address(&mut self, owner: &Expr) -> Result<Math> { Ok(self.pointer_parts(owner)?.0) }
    pub(super) fn box_variant(&mut self, op: crate::compiler::operations::BoxOp, owner: &Expr) -> Result<Outcome> {
        let word = self.box_address(owner)?;
        let mut out = Outcome::default();
        out.facts.push((0, Prop::Compare(">", word.clone(), Math::Literal(0))));
        out.facts.push((1, equal(word, Math::Literal(0))));
        if matches!(op, crate::compiler::operations::BoxOp::Take) {
            // The emitter loads the complete initialized T before releasing the
            // allocation. Its transferred fields keep their own incarnations.
            for (path, value) in self.box_contents(owner) {
                out.values.insert((Some(0), path), value);
            }
        }
        if !matches!(op, crate::compiler::operations::BoxOp::Ref) { self.forget_box_contents(owner); }
        Ok(out)
    }
    pub(super) fn new_box(&mut self, args: &[Expr]) -> Result<Outcome> {
        self.lend(&args[0])?;
        let input = &args[1];
        let mut fields = Vec::new();
        if tracked(input.ty) {
            fields.push(((RESULT, Vec::new()), self.number(input)?));
        } else {
            self.gather(RESULT, &[], input, &mut fields, &mut None)?;
        }
        // A successful allocation of one storable T has a nonzero heap base.
        let word = self.fresh(Ty::Unsigned(crate::compiler::scalar::Scalar::U32));
        let mut out = Outcome::default();
        out.values.insert((Some(0), vec![BOX_ADDRESS]), word.clone());
        out.values.insert((Some(0), vec![BOX_ID]), self.fresh(Ty::U64));
        for ((_, path), value) in fields {
            // Retain the fields of this payload, excluding cached contents of further
            // boxes. Proof state stays bounded for recursively boxed data.
            if path.contains(&BOX_VALUE) { continue; }
            // The actual constructor stores the input on success and hands the
            // whole input back on failure. A mutable loan invalidates this snapshot.
            out.values.insert((Some(0), join(&[BOX_VALUE], &path)), value.clone());
            out.values.insert((Some(1), path), value);
        }
        out.facts.push((0, Prop::Compare(">", word, Math::Literal(0))));
        Ok(out)
    }
}
