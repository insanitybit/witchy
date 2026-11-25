use super::*;

impl Checker<'_> {
    /// A slice element is written through the unique reference from `cell`.
    pub(super) fn slice_store(&self, statement: &a::Statement) -> Option<a::Statement> {
        let a::Statement::Assign { target, operation, value } = statement else { return None };
        let a::Expression::Index(receiver, index) = &target.kind else { return None };
        if !matches!(self.hint(receiver), Some(Ty::Borrow { pointee: Pointee::Slice(_), .. })) {
            return None;
        }
        let span = &target.span;
        let callee = a::Expr {
            kind: a::Expression::Field(receiver.clone(), "cell".into()),
            span: span.clone(),
        };
        let cell = a::Expr {
            kind: a::Expression::Call(Box::new(callee), vec![(**index).clone()]),
            span: span.clone(),
        };
        let name = format!("cell#{}", span.start);
        let named = a::Expr { kind: a::Expression::Name(name.clone()), span: span.clone() };
        let mut statements = Vec::new();
        // Read the same slice before taking its unique loan, provided that moving
        // the read ahead of the target does not reorder any effects.
        let value = if matches!(&receiver.kind, a::Expression::Name(name)
            if crate::compiler::lending::mentions(value, name))
            && self.only_reads(receiver) && self.only_reads(index) && self.only_reads(value)
        {
            let early = format!("value#{}", span.start);
            statements.push(a::Statement::Bind {
                mutable: false, name: early.clone(), ty: None, value: value.clone(), proof: None,
            });
            a::Expr { kind: a::Expression::Name(early), span: value.span.clone() }
        } else {
            value.clone()
        };
        statements.push(a::Statement::Bind {
            mutable: false, name, ty: None, value: cell, proof: None,
        });
        statements.push(a::Statement::Assign {
            target: a::Expr {
                kind: a::Expression::Unary("*", Box::new(named)),
                span: span.clone(),
            },
            operation,
            value,
        });
        Some(a::Statement::Block(statements))
    }

    pub(super) fn assign(&mut self, out: &mut Vec<Stmt>, target: &a::Expr, operation: &str, value: &a::Expr) -> Result<()> {
        // A refusal here is about the target (adversarial review round 13).
        self.at = target.span.clone();
        let place = self.place(target)?;
        // A field is written beside loans of the value's other fields (adversarial
        // review round 13); the whole value, or a field it is part of, still conflicts.
        let id = self.loan_id(&place);
        self.access(id, true)?;
        if !place.mutable {
            // Through a shared reference (`*x = v` where `x: &T`, as `for x in v` lends):
            // no `var` mends that.
            let mut root = target;
            while let a::Expression::Field(inner, _) | a::Expression::Index(inner, _) = &root.kind { root = inner; }
            // Written `*x` or by the name alone (`x.field`, read through): the same refusal.
            let named = match &root.kind { a::Expression::Unary("*", inner) => match &inner.kind { a::Expression::Name(n) => Some(n), _ => None }, a::Expression::Name(n) => Some(n), _ => None };
            if let Some(name) = named
                && matches!(self.scope.get(name).map(|l| l.ty), Some(Ty::Borrow { mutable: false, .. })) {
                return Err(error(format!("assignment requires a mutable root or unique referent: `{name}` is a shared reference (`&`), and nothing is written through one; a `for` lends `&mut` elements as `for {name} in &mut container` (`&mut *v` for a `&mut` parameter)")));
            }
            return Err(error("assignment requires a mutable root or unique referent: a `let` is never changed; declare it with `var` (or pass `&mut` to change a caller's value)"));
        }
        // Through a `&mut` to a view, only what holds no reference changes: a callee
        // could otherwise plant a reference that lives shorter than the caller's view.
        if place.pointer && self.layouts.lends(place.ty) && matches!(self.locals[place.root as usize].ty, Ty::Borrow { mutable: true, pointee } if self.layouts.lends(pointee.ty())) {
            return Err(error("through a `&mut` to a value that holds a reference, only its fields that hold none are changed"));
        }
        // A whole record that holds `&` references may be given a new value, but only one
        // that borrows from what the variable already borrows. Its loans then never
        // change, so a loop that assigns it lends on every pass what it lent on the first.
        let rebound = self.layouts.lends(place.ty);
        if rebound && !(matches!(place.ty, Ty::Aggregate(_)) && place.projections.is_empty() && !place.pointer
            && !self.layouts.lends_unique(place.ty) && operation == "=" && !self.modelled()) {
            return Err(error("a reference is bound once with `let`; it cannot be assigned"));
        }
        self.unsealed_write(&place, false)?;
        if self.layouts.properties(place.ty).copy { self.available(&place)?; }
        let mark = self.carried.len();
        let value = self.expect(value, place.ty)?;
        if rebound {
            let held = self.holds.get(&place.root).cloned().unwrap_or_default();
            if self.carried[mark..].iter().any(|root| root & UNIQUE != 0 || !held.iter().any(|kept| kept & !UNIQUE == *root)) {
                return Err(error("a record that holds a reference is assigned only a value that borrows from what it already borrows: bind the new value with `let`"));
            }
        }
        if place.ty == Ty::F64 && matches!(operation, "+=" | "-=") {
            let quiet = place.projections.iter().all(|step| match step { Projection::Field { .. } => true, Projection::Index { value, .. } => matches!(value.kind, Value::U64(_) | Value::ReadPlace(_)) });
            if !quiet { return Err(error("`+=` on an F64 element needs an index that is a name or a number: write `place = place + value`")); }
            let old = Expr { ty: Ty::F64, kind: Value::ReadPlace(place.clone()) };
            let (_, kind) = self.float(if operation == "+=" { crate::compiler::logical_body::Float::Add } else { crate::compiler::logical_body::Float::Sub }, vec![old, value])?;
            out.push(Stmt::StorePlace(place, None, Expr { ty: Ty::F64, kind }));
            return Ok(());
        }
        if place.ty == Ty::I64 && matches!(operation, "+=" | "-=") {
            // The place is read again for the old value, so reaching it must do nothing else.
            let quiet = place.projections.iter().all(|step| match step { Projection::Field { .. } => true, Projection::Index { value, .. } => matches!(value.kind, Value::U64(_) | Value::ReadPlace(_)) });
            if !quiet { return Err(error("`+=` on an I64 element needs an index that is a name or a number: write `place = place + value`")); }
            let old = Expr { ty: Ty::I64, kind: Value::ReadPlace(place.clone()) };
            let (_, kind) = self.signed(if operation == "+=" { Signed::Add } else { Signed::Sub }, vec![old, value])?;
            out.push(Stmt::StorePlace(place, None, Expr { ty: Ty::I64, kind }));
            return Ok(());
        }
        if !self.layouts.properties(place.ty).copy {
            if operation == "=" && (place.pointer || !place.projections.is_empty()) && !self.layouts.properties(place.ty).implicit_discard {
                out.extend(self.vacant_assign(target, &place, value)?);
                return Ok(());
            }
            if place.pointer || !place.projections.is_empty() || operation != "=" {
                return Err(error("owned replacement requires a mutable local slot"));
            }
            self.flow.reinitialize(place.identity, self.layouts.properties(place.ty).implicit_discard)?; // ONLY after checked RHS succeeds
            out.push(Stmt::Replace(place.identity, place, value));
            return Ok(());
        }
        // RHS evaluation may consume the enclosing owner even when
        // this projected field is Copy. The final store still needs it.
        self.available(&place)?;
        let operation = match operation {
            "=" => None,
            "+=" | "-=" if matches!(place.ty, Ty::Unsigned(_)) => Some(
                if operation == "+=" { Binary::Add } else { Binary::Sub }),
            _ => return Err(error("unsupported place compound assignment")),
        };
        // The step of a counting `for` (iteration.rs): its own counter, by itself.
        if let a::Expression::Name(n) = &target.kind && ((n.starts_with("for#") && n.ends_with(crate::compiler::iteration::STEP)) || self.guarded.contains(n))
            && matches!(operation, Some(Binary::Add)) && place.projections.is_empty() && !place.pointer && matches!(value.kind, Value::U64(1)) {
            self.steps.insert(place.root);
        }
        out.push(Stmt::StorePlace(place, operation, value));
        Ok(())
    }
}
