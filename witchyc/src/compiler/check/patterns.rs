use super::*;

impl Checker<'_> {
    // Conservative target-depth bound: each source scope may introduce two
    // Wasm labels (while), each short-circuit expression one. Account two per
    // recursive visit; the emitted profile stays below 128 labels including the
    // function label. Calls are bounded separately by the engine stack.
    pub(super) fn nested<T>(&mut self, work: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.nesting >= 120 {
            return Err(error("scalar target nesting limit"));
        }
        self.nesting += 2;
        let result = work(self);
        self.nesting -= 2;
        result
    }
    /// A value that may go unnamed: nothing has to consume it.
    pub(super) fn unnamed(&self, ty: Ty) -> bool {
        ty == Ty::Unit || matches!(ty, Ty::Borrow { mutable: false, .. }) || self.discardable(ty).is_ok()
    }
    pub(super) fn match_arm(&mut self, arm: &a::MatchArm, payload: Ty, lent: &[u32], rest: bool) -> Result<(Arm, bool)> {
        self.count()?;
        let saved = self.scope.clone(); // PRE-binder scope; block restores only its own entry
        // `Variant(_)` leaves the value unnamed, which a value that must be consumed cannot be.
        let unnamed = arm.binding.as_deref() == Some("_");
        if unnamed && (self.modelled() || !self.unnamed(payload)) {
            return Err(error(if self.modelled() { "a function with a contract names the value of each variant" } else { "`_` would lose the owner this variant holds: name it and consume it" }));
        }
        let named = arm.binding.as_ref().filter(|_| !unnamed);
        let binding = if let Some(name) = named {
            if self.scope.contains_key(name) {
                // Arms carry no position of their own, so the message names the binder.
                return Err(error(format!("duplicate or shadowed match binder `{name}`: a local of that name is already in scope; choose another")));
            }
            let local = self.new_local(payload, false, None)?;
            self.local_names.insert(local.id, name.clone()); if self.at.start > 0 { self.bound_at.insert(name.clone(), self.at.start); } self.scope.insert(name.clone(), local);
            if self.layouts.lends(payload) && !lent.is_empty() {
                // The binder is the reference now: it keeps what the matched value borrowed.
                self.lend_again(lent);
                self.holds.insert(local.id, lent.to_vec());
                self.adopt.push((name.clone(), local.id));
            }
            Some(local.id)
        } else {
            if payload != Ty::Unit && !rest && !unnamed { return Err(error("an arm for a variant that holds a value names it: `Variant(name):`")); }
            None
        };
        let binder = named.and_then(|name| self.scope.get(name)).copied();
        let result = self.block(&arm.body);
        self.scope = saved;
        let (mut body, returns) = result?;
        if named.is_some() {
            // Scope was restored above, but binder identity must expire without
            // restoring or forgetting any OUTER move effects.
            let local = binder.expect("named arm binder");
            let expired = self.expire_locals(&[local]).map_err(|e| match named {
                // The arm's scope is gone, so say which binder it was.
                Some(name) if e.message.contains("no implicit discard") => Diagnostic::new("W0401", e.span.clone(), format!(
                    "`{name}` still holds an owner when its arm ends: hand it on, or close or discard it first")),
                _ => e,
            })?;
            if !expired.is_empty() { body.push(Stmt::Expire(expired)); }
            self.flow.remove(local.id);
        }
        Ok((Arm { payload, binding, body }, returns))
    }
    /// `place = value` for an owner in a field or behind `&mut` (docs/wasm-exchange.md).
    /// Nothing may be lost, so the value there now must be in a variant that holds no owner.
    /// That is a fact about a value, which only a proof knows; this is exactly what an author
    /// writes by hand, checked as if written so, with `value` already evaluated:
    /// `var held = value; memory.exchange(&mut place, &mut held); match move held:` with an
    /// empty arm for each owner-free variant and `unreachable` for each of the others. The
    /// exchange asks again, after the value, for all the authority it needs; each `unreachable`
    /// is an obligation of the function's proof, stated on the state after the value.
    pub(super) fn vacant_assign(&mut self, target: &a::Expr, place: &Place, value: Expr) -> Result<Vec<Stmt>> {
        // The place is found again after the value, so reaching it must do nothing else.
        let quiet = place.projections.iter().all(|step| match step { Projection::Field { .. } => true, Projection::Index { value, .. } => matches!(value.kind, Value::U64(_) | Value::ReadPlace(_)) });
        if !quiet { return Err(error("assigning an owner to an element needs an index that is a name or a number")); }
        let (names, payloads) = match self.layouts.sum(place.ty) {
            Some(sum) => (sum.names.iter().map(|name| if sum.anonymous { format!(".{name}") } else { name.clone() }).collect::<Vec<_>>(), sum.arms.clone()),
            None => (Vec::new(), Vec::new()),
        };
        let free = payloads.iter().map(|payload| *payload == Ty::Unit || self.discardable(*payload).is_ok()).collect::<Vec<_>>();
        if !free.contains(&true) {
            return Err(error("owned replacement requires a mutable local slot: this place always holds an owner (its type has no variant without one), which an assignment would lose; exchange the new value in with `memory.exchange` and consume the old one"));
        }
        // Never an unproved overwrite: the function is proved, or the assignment is refused.
        if !self.contracted {
            if self.entry { return Err(error("assigning an owner over a field needs a proof that the value there holds none, and `main` has no proof: do it in a function `main` calls")); }
            self.needs_contract.set(true);
        } else if !self.scalar_proof {
            return Err(error("a function whose contract speaks of memory cannot assign an owner over a field yet"));
        }
        let span = target.span.clone();
        let expr = |kind| a::Expr { kind, span: span.clone() };
        let held = format!("{OVERWRITTEN}{}", span.start);
        if self.scope.contains_key(&held) { return Err(error("unsupported repeated owner assignment")); }
        let local = self.new_local(place.ty, true, Some((span.start, span.end)))?;
        self.scope.insert(held.clone(), local);
        let name = || expr(a::Expression::Name(held.clone()));
        let exchange = expr(a::Expression::Call(
            Box::new(expr(a::Expression::Field(Box::new(expr(a::Expression::Name("memory".into()))), "exchange".into()))),
            vec![expr(a::Expression::Unary("&mut", Box::new(target.clone()))), expr(a::Expression::Unary("&mut", Box::new(name())))]));
        let arms = names.into_iter().zip(&payloads).zip(&free).map(|((variant, payload), free)| a::MatchArm {
            variant, nested: None, open: vec![],
            binding: if *payload == Ty::Unit { None } else if *free { Some("_".into()) } else { Some(format!("{held}.owner")) },
            body: if *free { vec![] } else { vec![a::Statement::Unreachable(a::Site { span: span.clone(), why: a::Why::Overwrite })] },
        }).collect();
        let checked = self.block(&[a::Statement::Expr(exchange), a::Statement::Match { value: expr(a::Expression::Unary("move", Box::new(name()))), arms }]);
        self.scope.remove(&held);
        let (body, _) = checked?;
        self.flow.remove(local.id);
        // Fail closed: exactly the exchange with this local and the match that consumes it.
        if !matches!(body.as_slice(), [Stmt::Eval(Expr { kind: Value::Exchange(_, right), .. }), Stmt::Match { value: Expr { kind: Value::Move { place: moved, .. }, .. }, .. }]
            if right.root == local.id && right.projections.is_empty() && moved.root == local.id && moved.projections.is_empty()) {
            return Err(error("unsupported owner assignment"));
        }
        // Marked as the compiler's own: only these three statements may become one store.
        self.overwrites.insert(local.id);
        let mut out = vec![Stmt::Set(local.id, value)];
        out.extend(body);
        Ok(out)
    }
    /// `memory.vacant(value)`: the end of a value that is proved to hold no owner
    /// (docs/wasm-exchange.md, "Discarding a value proved to hold no owner"). The rule is the
    /// owner assignment's, without the place: this is exactly the match an author writes by
    /// hand, checked as if written so, with nothing for each variant that holds no owner and
    /// `unreachable` for each of the others. Each `unreachable` is an obligation of the
    /// function's proof; nothing is emitted that rests on it but what `unreachable` already is.
    pub(super) fn vacant_discard(&mut self, call: &a::Expr, value: &a::Expr) -> Result<Vec<Stmt>> {
        let refuse = |message| Diagnostic::new("W0401", value.span.clone(), message);
        if self.modelled() { return Err(refuse("a function whose contract speaks of memory cannot use `memory.vacant` yet")); }
        // What is named when the proof fails: the value, without its `move`.
        let named = match &value.kind { a::Expression::Unary("move", inner) => inner.span.clone(), _ => value.span.clone() };
        let mut visits = 0;
        let Some(ty) = self.infer_hint(value, None, 0, &mut visits)? else {
            return Err(refuse("`memory.vacant` needs a value whose type is known here: bind it with `let` first"));
        };
        if matches!(ty, Ty::Borrow { .. }) { return Err(refuse("`memory.vacant` ends the value itself, not a reference to it: pass `move name`")); }
        let Some(sum) = self.layouts.sum(ty) else {
            return Err(refuse("`memory.vacant` ends a value of a type with variants, proved to be in one that holds no owner; this type has none: close or discard the value through its type"));
        };
        let (names, payloads) = (sum.names.iter().map(|name| if sum.anonymous { format!(".{name}") } else { name.clone() }).collect::<Vec<_>>(), sum.arms.clone());
        let free = payloads.iter().map(|payload| *payload == Ty::Unit || self.discardable(*payload).is_ok()).collect::<Vec<_>>();
        if !free.contains(&true) {
            return Err(refuse("`memory.vacant` ends a value proved to hold no owner, and this one always holds an owner (its type has no variant without one): hand it on, or close or discard it through its type"));
        }
        // Never an unproved trap in place of an owner's end: the function is proved, or this is refused.
        if free.contains(&false) {
            if !self.contracted {
                if self.entry { return Err(refuse("`memory.vacant` needs a proof that the value holds no owner, and `main` has no proof: do it in a function `main` calls")); }
                self.needs_contract.set(true);
            } else if !self.scalar_proof {
                return Err(refuse("a function whose contract speaks of memory cannot use `memory.vacant` yet"));
            }
        }
        let held = format!("{VACANT}{}", call.span.start);
        let arms = names.into_iter().zip(&payloads).zip(&free).map(|((variant, payload), free)| a::MatchArm {
            variant, nested: None, open: vec![],
            binding: if *payload == Ty::Unit { None } else if *free { Some("_".into()) } else { Some(format!("{held}.owner")) },
            body: if *free { vec![] } else { vec![a::Statement::Unreachable(a::Site { span: named.clone(), why: a::Why::Discard })] },
        }).collect();
        let (body, _) = self.block(&[a::Statement::Match { value: value.clone(), arms }])?;
        // Fail closed: exactly the one match, on a value of the type the arms were written for.
        if !matches!(body.as_slice(), [Stmt::Match { value: matched, .. }] if matched.ty == ty) { return Err(refuse("unsupported `memory.vacant`")); }
        Ok(body)
    }
}
