mod assignments;
mod matches;

use super::diagnostics::shadowed;
use super::*;

impl Checker<'_> {
    pub(super) fn block(&mut self, body: &[a::Statement]) -> Result<(Vec<Stmt>, bool)> {
        self.nested(|this| this.block_inner(body)).map_err(|mut e| {
            // Statement-level refusals name the nearest expression already visited.
            if e.span == (0..0) { e.span = self.at.clone(); }
            e
        })
    }
    fn block_inner(&mut self, body: &[a::Statement]) -> Result<(Vec<Stmt>, bool)> {
        let expanded = crate::compiler::using::expand(body, &self.written_result)?;
        let body = expanded.as_deref().unwrap_or(body);
        let counted = crate::compiler::iteration::rewrite(body);
        let body = counted.as_deref().unwrap_or(body);
        let rewritten = crate::compiler::early_exit::rewrite(body)?;
        let body = rewritten.as_deref().unwrap_or(body);
        let saved = self.scope.clone();
        let mut out = Vec::new();
        let mut returns = false;
        let mut watch: Vec<(usize, u32)> = Vec::new();
        for (name, local) in std::mem::take(&mut self.adopt) {
            match crate::compiler::lending::last(body, 0, &name) {
                Some(last) => watch.push((last, local)),
                None => self.drop_holder(local),
            }
        }
        for (index, stmt) in body.iter().enumerate() {
            if index > 0 { self.settle(&mut watch, index - 1, body); }
            self.count()?;
            // A refusal of the statement itself (a type, a name) is placed at its own first
            // expression, not at the last one checked before it.
            if let Some(span) = leading(stmt) { self.at = span; }
            let assigned;
            // `*(&mut v[i]) = x`: a unique borrow taken and dereferenced at once is the place itself
            // (a shared one stays refused: nothing is written through `&`).
            let unwrapped;
            let stmt = match stmt {
                a::Statement::Assign { target, operation, value } if matches!(&target.kind, a::Expression::Unary("*", inner) if matches!(&inner.kind, a::Expression::Unary("&mut", _))) => {
                    let a::Expression::Unary(_, inner) = &target.kind else { unreachable!() };
                    let a::Expression::Unary(_, place) = &inner.kind else { unreachable!() };
                    unwrapped = a::Statement::Assign { target: (**place).clone(), operation, value: value.clone() };
                    &unwrapped
                }
                other => other,
            };
            // `total += price` on a record or sum: `total = total + price`, so the `+` is the type's
            // `add` (library/wasm-ops.wy). The target is a name or fields of one, read and then
            // written, which runs nothing twice.
            let combined;
            let stmt = match stmt {
                a::Statement::Assign { target, operation: op @ ("+=" | "-=" | "*="), value }
                    if Self::place_syntax(target) && !matches!(target.kind, a::Expression::Index(..))
                    && matches!(self.hint(target), Some(Ty::Aggregate(_) | Ty::Sum(_))) => {
                    let binary = match *op { "+=" => "+", "-=" => "-", _ => "*" };
                    let span = target.span.start..value.span.end;
                    combined = a::Statement::Assign { target: target.clone(), operation: "=", value: a::Expr { kind: a::Expression::Binary(binary, Box::new(target.clone()), Box::new(value.clone())), span } };
                    &combined
                }
                _ => stmt,
            };
            // `points[i].x = v` (any field chain) on a Vec, not a fixed array: the value first, then
            // the field written through the element's `&mut`, as by hand:
            // `let v# = v; match points.at_mut(i): Ok(e): (*e).x = v#; Err: unreachable`.
            let fielded;
            let stmt = match stmt {
                a::Statement::Assign { target, operation, value } if matches!(target.kind, a::Expression::Field(..)) => {
                    let mut root = target;
                    let mut path = Vec::new();
                    while let a::Expression::Field(inner, name) = &root.kind { path.push(name.clone()); root = inner; }
                    match &root.kind {
                        a::Expression::Index(receiver, at) if !self.array_receiver(receiver)? => {
                            let span = target.span.clone();
                            // Written order: the Vec (a place, which runs nothing), the position,
                            // then the value (red-team review: the value ran first).
                            fn place(e: &a::Expr) -> bool { match &e.kind { a::Expression::Name(_) => true, a::Expression::Field(inner, _) => place(inner), _ => false } }
                            if !place(receiver) { return Err(Diagnostic::new("W0401", span, "a field of a Vec element is written through a name or a field of one: bind the Vec with `let` first")); }
                            let position = format!("position#{}", span.start);
                            let early = format!("value#{}", span.start);
                            let element = format!("element#{}", span.start);
                            let mut place = a::Expr { kind: a::Expression::Unary("*", Box::new(a::Expr { kind: a::Expression::Name(element.clone()), span: span.clone() })), span: span.clone() };
                            for name in path.into_iter().rev() { place = a::Expr { kind: a::Expression::Field(Box::new(place), name), span: span.clone() }; }
                            let callee = a::Expr { kind: a::Expression::Field(receiver.clone(), "at_mut".into()), span: span.clone() };
                            let cell = a::Expr { kind: a::Expression::Call(Box::new(callee), vec![a::Expr { kind: a::Expression::Name(position.clone()), span: at.span.clone() }]), span: span.clone() };
                            let arms = vec![
                                a::MatchArm { variant: "Ok".into(), binding: Some(element), nested: None, open: vec![],
                                    body: vec![a::Statement::Assign { target: place, operation, value: a::Expr { kind: a::Expression::Name(early.clone()), span: value.span.clone() } }] },
                                a::MatchArm { variant: "Err".into(), binding: Some("_".into()), nested: None, open: vec![], body: vec![a::Statement::Unreachable(a::Site { span: span.clone(), why: a::Why::Lowered })] },
                            ];
                            fielded = a::Statement::Block(vec![
                                a::Statement::Bind { mutable: false, name: position, ty: Some(a::Type::Named("U64".into(), Vec::new())), value: (**at).clone(), proof: None },
                                a::Statement::Bind { mutable: false, name: early, ty: None, value: value.clone(), proof: None },
                                a::Statement::Match { value: cell, arms },
                            ]);
                            &fielded
                        }
                        _ => stmt,
                    }
                }
                _ => stmt,
            };
            let rewritten;
            let stmt = if let Some(slice_store) = self.slice_store(stmt) {
                rewritten = slice_store;
                &rewritten
            } else {
                stmt
            };
            let stmt = match stmt {
                a::Statement::Assign { target, operation, value } => match self.index_assignment(target, operation, value)? {
                    Some(call) => { assigned = a::Statement::Expr(call); &assigned }
                    None => stmt,
                },
                _ => stmt,
            };
            match stmt {
                a::Statement::Proof(steps)=>{
                    if self.proof_requests!=0 || steps.as_slice()!=[a::ProofStep::Auto] {
                        return Err(error("an explicit function proof takes one auto step and may appear only once"));
                    }
                    if !self.contracted {
                        return Err(self.proof_required("a proof block requires a checkable function body"));
                    }
                    self.proof_requests+=1;
                }
                a::Statement::Bind {
                    mutable,
                    name,
                    ty: annotation,
                    value,
                    proof,
                } => {
                    if proof.is_some() {
                        return Err(error("binding proof obligations are unsupported"));
                    }
                    // `let _ = value` names nothing: the value is evaluated and dropped, said
                    // outright (a dropped `Result` is otherwise refused). What holds an owner
                    // cannot be dropped this way either.
                    let ignored = name == "_";
                    if ignored && *mutable { return Err(error("`let _ = ...` drops a value; `var _` would name a variable nothing can read")); }
                    if !ignored && self.scope.contains_key(name) {
                        return Err(error(shadowed(name, self.bound_at.get(name.as_str()).copied())));
                    }
                    let span = Some((value.span.start, value.span.end));
                    let written = value.span.clone();
                    self.split = None;
                    self.pending_parts = None;
                    let value = if let Some(annotation) = annotation {
                        {
                            let expected = self.layouts.ty_in(annotation,self.definition,&self.generics)?;
                            self.expect(value, expected)?
                        }
                    } else {
                        match self.expr(value) {
                            Ok(value) => value,
                            // `var names = Vec.new(&allocator, 4)` with `names.push_mut(&allocator, 7)`
                            // later: the first later method call says what the type arguments are.
                            Err(e) if e.message == "generic call has ambiguous type arguments" || e.message.contains("is already a reference: pass") => match self.later_use(name, value, &body[index + 1..])? {
                                Some(expected) => self.expect(value, expected)?,
                                None => return Err(e),
                            },
                            Err(e) => return Err(e),
                        }
                    };
                    if value.ty == Ty::Unit {
                        return Err(error(if ignored { "this value is `()`: there is nothing to ignore, so write the call as a statement, without `let _ =`" } else { "unit locals are unsupported" }));
                    }
                    if ignored {
                        if !self.unnamed(value.ty) { return Err(error("`let _ = ...` would lose the owner this value holds: name it and hand it on, or close or discard it")); }
                        self.split = None;
                        out.push(Stmt::Eval(value));
                    } else {
                    let local = self.new_local(value.ty, *mutable, span)?;
                    self.local_names.insert(local.id, name.clone()); if self.at.start > 0 { self.bound_at.insert(name.clone(), self.at.start); } self.scope.insert(name.clone(), local);
                    if self.layouts.lends(value.ty) { self.bound = Some((name.clone(), local.id)); }
                    // `let got = choose(&a, &b, flag)`: what each variant borrows, for a later `match got`.
                    if let (false, Value::Call(call), Some((at, definition, kept, parts))) = (*mutable, &value.kind, self.split.take())
                        && at == written && call.definition == definition { self.pending_parts = Some((local.id, kept, parts)); }
                    out.push(Stmt::Set(local.id, value));
                    }
                }
                a::Statement::Open { record, names, value } => {
                    let span = Some((value.span.start, value.span.end));
                    let value = self.expr(value)?;
                    if self.layouts.record_name(value.ty) != Some(record.as_str()) || self.layouts.is_anonymous(value.ty) {
                        return Err(error("opening a record requires a value of the named record type"));
                    }
                    // Taking a value apart reads it: nothing to show.
                    self.unsealed_covered(value.ty, true)?;
                    if self.layouts.get(value.ty).unwrap().fields.len() != names.len() { return Err(error("opening a record binds every field, in declaration order")); }
                    self.open(&mut out, value, names.iter().cloned().enumerate().collect(), span, false)?;
                }
                a::Statement::OpenFields { fields: bound, value, mutable, ty: annotation } => {
                    let span = Some((value.span.start, value.span.end));
                    // `for (a, b) in pairs` walks the array by reference: a reference to a tuple
                    // of copies is opened by reading it (`*item`).
                    let read;
                    let value = match self.hint(value) {
                        // A `let` through a shared reference only: `var (a, b) = r` or a `&mut`
                        // would look as if it changed `*r` (review).
                        Some(Ty::Borrow { pointee, mutable: false }) if annotation.is_none() && !*mutable && self.layouts.is_anonymous(pointee.ty()) && self.layouts.properties(pointee.ty()).copy => {
                            read = a::Expr { kind: a::Expression::Unary("*", Box::new(value.clone())), span: value.span.clone() };
                            &read
                        }
                        _ => value,
                    };
                    let value = match annotation {
                        Some(annotation) => { let expected = self.layouts.ty_in(annotation, self.definition, &self.generics)?; self.expect(value, expected)? }
                        None => self.expr(value)?,
                    };
                    let mutable = *mutable;
                    if matches!(value.ty, Ty::Borrow { .. }) { return Err(error("opening by label requires an anonymous record, and this is a reference to one: read it first, `var (a, b) = *r`, so the copies are plainly copies")); }
                    if !self.layouts.is_anonymous(value.ty) { return Err(error("opening by label requires an anonymous record; a named record is opened with `let Name(a, b) = value`")); }
                    let fields = self.layouts.get(value.ty).unwrap().fields.clone();
                    if fields.len() != bound.len() {
                        // Labels that are positions come from `Variant(a, b)`, which no `let` can write.
                        let positional = bound.first().is_some_and(|(label, _)| label.starts_with(|c: char| c.is_ascii_digit()));
                        return Err(error(if positional { "a pattern names each value its variant holds, or `_`" } else { "opening a record binds every field" }));
                    }
                    let mut names = Vec::new();
                    for (label, name) in bound {
                        let ordinal = fields.iter().position(|field| field.name == *label).ok_or_else(|| error("opening a record names a field it does not have"))?;
                        if names.iter().any(|(seen, _)| *seen == ordinal) { return Err(error("opening a record binds every field once")); }
                        names.push((ordinal, name.clone()));
                    }
                    self.open(&mut out, value, names, span, mutable)?;
                }
                a::Statement::Assign { target, operation, value } => {
                    self.assign(&mut out, target, operation, value)?;
                }
                a::Statement::Return(value) => {
                    // Where the `return` is written, as far as its value says (a statement has no place of its own).
                    let written_at = value.as_ref().map(|value| value.span.clone()).filter(|span| *span != (0..0));
                    let value = match value {
                        Some(value) => { self.split = None; self.returned_at = value.span.clone();
                            self.returned_payload_at = match &value.kind { a::Expression::Call(callee, args) if matches!(&callee.kind, a::Expression::Name(n) if n == "Ok") && args.len() == 1 => Some(args[0].span.clone()), _ => None }; self.returning = true; let value = self.expect(value, self.result); self.returning = false; value? }
                        None if self.result == Ty::Unit => Expr {
                            ty: Ty::Unit,
                            kind: Value::Unit,
                        },
                        None => return Err(error("missing return value")),
                    };
                    // A variant whose clause names a `&mut` parameter may hand out a `&` into what it
                    // points at (`return Decoded(&*scratch)`): its caller keeps that parameter's
                    // argument lent uniquely while the reference is in use.
                    let named_mut = match &value.kind {
                        Value::Sum { arm, .. } => self.own_borrows.get(*arm as usize).cloned().flatten().unwrap_or_default().into_iter()
                            .filter(|index| matches!(self.locals[*index].ty, Ty::Borrow { mutable: true, .. })).collect::<Vec<_>>(),
                        _ => Vec::new(),
                    };
                    let through_named = |this: &Self, root: u32| { let (id, path) = this.decode(root); path.is_empty() && named_mut.contains(&(id as usize)) };
                    if self.layouts.lends(self.result) && self.carried.iter().any(|root| !self.external_origin(*root) && !through_named(self, *root)) {
                        return Err(error("a returned reference must borrow from a reference parameter or a reference carried by a value parameter, not from a value this function owns"));
                    }
                    self.returned_sources(&value)?;
                    returns = true;
                    let expired = self.expire_visible().map_err(|e| match &self.try_exit {
                        Some(at) if e.message.contains("still holds an owner when it goes out of scope") => {
                            let what = e.message.split(" still holds").next().unwrap_or("a value");
                            Diagnostic::new(e.code, at.clone(), format!("this `?` returns early while {what} still holds an owner: bind it with `using var name: Type = value:` so it is discarded on every way out, or match the error and close it first"))
                        }
                        // A `return` the author wrote: the refusal is at it, and says where the owner was bound.
                        None if e.message.contains("still holds an owner when it goes out of scope") && e.span != (0..0) => match &written_at {
                            Some(at) if *at != e.span && e.message.starts_with('`') => {
                                let what = e.message.split(" still holds").next().unwrap_or("a value");
                                Diagnostic::new(e.code, at.clone(), format!("this `return` leaves while {what} (bound at ⟪at {}⟫) still holds an owner: hand it on, or close or discard it before returning, or bind it with `using var name: Type = value:` so it is discarded on every way out", e.span.start))
                            }
                            _ => e,
                        },
                        _ => e,
                    })?;
                    out.push(Stmt::Return(value, expired, written_at.map(|at| (at.start, at.end))));
                }
                leave @ (a::Statement::Break | a::Statement::Continue) => {
                    let leaving = matches!(leave, a::Statement::Break);
                    let Some((before, exit, outer)) = self.loops.last().cloned() else {
                        return Err(error(if leaving { "`break` is written inside a loop" } else { "`continue` is written inside a loop" }));
                    };
                    if self.modelled() { return Err(error("a function with a contract cannot use `break` or `continue` yet: its loops are proved by their conditions")); }
                    // What the loop body made must be consumed, as at the body's end.
                    let inside = self.scope.values().filter(|local| !outer.contains(&local.id)).copied().collect::<Vec<_>>();
                    let expired = self.expire_locals(&inside)?;
                    if !expired.is_empty() { out.push(Stmt::Expire(expired)); }
                    // And what was there before must be as the loop expects to find it.
                    let expected = if leaving { exit } else { before };
                    if self.flow.within(&expected) != expected {
                        return Err(error("loop backedge changes outer ownership availability"));
                    }
                    returns = true; // nothing after it in this block runs
                    out.push(if leaving { Stmt::Break } else { Stmt::Continue });
                }
                a::Statement::Block(inner) => {
                    let (inner, inner_returns) = self.block(inner)?;
                    out.extend(inner);
                    returns |= inner_returns;
                }
                a::Statement::Using { .. } => unreachable!("expanded at the start of the block"),
                a::Statement::Dispose { name, call } => {
                    let local = *self.scope.get(name).ok_or_else(|| error("a `using` name is not in scope at its exit"))?;
                    let held = if self.layouts.properties(local.ty).copy { Some(true) } else { self.flow.held(local.id) };
                    match held {
                        Some(true) => {
                            let value = self.expr(call)?;
                            self.discardable(value.ty)?;
                            out.push(Stmt::Eval(value));
                        }
                        // Handed on before this exit: nothing to discard.
                        Some(false) => {}
                        None => return Err(Diagnostic::new("W0401", call.span.clone(),
                            "a `using` value is moved away on some paths to this exit and not on others: it would be discarded on some and not others")),
                    }
                }
                a::Statement::Unreachable(site) => {
                    if self.nopanic_body && !self.contracted { self.needs_contract.set(true); }
                    // On this path no owner is released. A scalar or record body must
                    // discharge False under the actual branch conditions.
                    if self.modelled() && self.record_proof.is_none() { return Err(error("this memory contract model cannot prove an unreachable path")); }
                    // Every owner in sight ends here as if discarded; no code is emitted for it.
                    for local in self.scope.values().copied().collect::<Vec<_>>() {
                        if self.layouts.properties(local.ty).copy || matches!(local.ty, Ty::Borrow { .. }) { continue; }
                        self.flow.expire(local.id, true)?;
                    }
                    returns = true;
                    out.push(Stmt::Trap(site.clone()));
                }
                // `memory.vacant(value)`: a statement, written out as the match it stands for.
                a::Statement::Expr(a::Expr { kind: a::Expression::Call(callee, args), span })
                    if matches!(&callee.kind, a::Expression::Field(receiver, method) if method == "vacant" && matches!(&receiver.kind, a::Expression::Name(name) if name == "memory"))
                        && !self.scope.contains_key("memory") && !self.signatures.contains_key("memory") && !self.layouts.has_record("memory") => {
                    let [value] = args.as_slice() else { return Err(Diagnostic::new("W0401", span.clone(), "`memory.vacant` takes one value: the one that is proved to hold no owner")); };
                    out.extend(self.vacant_discard(&a::Expr { kind: a::Expression::Unit, span: span.clone() }, value)?);
                }
                a::Statement::Expr(expression) => {
                    let value = self.expr(expression)?;
                    if self.assertion_result == Some(value.ty) {
                        return Err(Diagnostic::new("W0401", expression.span.clone(), "an assertion result must be returned, propagated or handled"));
                    }
                    self.discardable(value.ty)?;
                    // A Result left as a statement loses whether the call failed, silently
                    // (docs/decisions.md, 2026-11-22). `let _ = ...` says that is meant.
                    if self.layouts.result_sum(value.ty).is_some() {
                        return Err(Diagnostic::new("W0401", expression.span.clone(), "this `Result` is dropped, and with it whether the call failed: handle it (`match`, or `let Ok(value): T = ... else Err(e):`), pass it on with `?`, or write `let _ = ...` to say it is ignored"));
                    }
                    out.push(Stmt::Eval(value));
                }
                a::Statement::If { condition, yes, no } => {
                    let condition = self.expect(condition, Ty::Bool)?;
                    let ended = std::mem::take(&mut self.carried); self.release(&ended);
                    // A reference last used in the condition is in use in neither branch: what it
                    // borrowed is free there, so a branch may close the container and leave.
                    self.finish(&mut watch, index, &[yes, no]);
                    let starting = self.flow.clone();
                    // Each branch starts from the loans in force before it; what each kept or
                    // released is combined after both.
                    // A branch that leaves the function ends the loans of holders it never names
                    // and shapes nothing after the `if` (as for a `match` arm).
                    let loans = self.loans.clone();
                    let holds = self.holds.clone();
                    let (yes_leaves, no_leaves) = (leaves_function(yes), leaves_function(no));
                    if yes_leaves { for local in self.unread_holders(yes) { self.drop_holder(local); } }
                    let (yes, yes_returns) = self.block(yes)?;
                    let yes_loans = self.loan_change(&loans);
                    let yes_holds = self.holds.clone();
                    self.loans = loans.clone();
                    self.holds = holds.clone();
                    let yes_flow = self.flow.clone();
                    self.flow = starting.clone();
                    if no_leaves { for local in self.unread_holders(no) { self.drop_holder(local); } }
                    let (no, no_returns) = self.block(no)?;
                    let no_loans = self.loan_change(&loans);
                    let no_holds = self.holds.clone();
                    self.loans = loans;
                    if !yes_leaves { self.apply_loan_change(&yes_loans); }
                    if !no_leaves { self.apply_loan_change(&no_loans); }
                    self.holds = if !no_leaves { no_holds } else if !yes_leaves { yes_holds } else { holds };
                    self.flow = Flow::join(&starting, &[(&yes_flow, yes_returns), (&self.flow, no_returns)]);
                    returns |= yes_returns && no_returns;
                    out.push(Stmt::If(condition, yes, no));
                }
                a::Statement::Choose { mutable, name, ty, inner } => {
                    if self.modelled() { return Err(error("a function whose contract speaks of memory cannot bind a `match` or `if` as a value yet")); }
                    // `let _ = if ...:` and `let Ok(_): T = ... else ...:` name nothing, as the
                    // plain `let _ = value` does: the value is computed and nothing can read it.
                    let ignored = name == "_";
                    if ignored && *mutable { return Err(error("`let _ = ...` drops a value; `var _` would name a variable nothing can read")); }
                    if !ignored && self.scope.contains_key(name) { return Err(error(shadowed(name, self.bound_at.get(name.as_str()).copied()))); }
                    let ty = match ty { Some(ty) => Some(self.layouts.ty_in(ty, self.definition, &self.generics)?), None => None };
                    self.yields.push((name.clone(), *mutable, ty, None));
                    self.chosen.push(Vec::new());
                    let outside = self.locals.len() as u32;
                    let checked = self.block(std::slice::from_ref(&**inner));
                    let (_, _, _, local) = self.yields.pop().expect("the choice being checked");
                    let lent = self.chosen.pop().expect("the choice being checked");
                    let (inner, inner_returns) = checked?;
                    out.extend(inner);
                    returns |= inner_returns;
                    let Some(local) = local else { return Err(error("no path of this `match` or `if` gives a value")); };
                    if ignored {
                        // Only what is copied freely and owns nothing: no flow state to settle,
                        // and what the paths borrowed is given back here (`lent` is dropped).
                        if !self.unnamed(local.ty) || !self.layouts.properties(local.ty).copy { return Err(error("`let _ = ...` would lose the owner this value holds: name it and hand it on, or close or discard it")); }
                        continue;
                    }
                    // Every path that reaches here went through a `Yield`: the parser made each one end in it or leave.
                    if !self.layouts.properties(local.ty).copy { self.flow.introduce(local.id); }
                    // A value that holds a reference: whichever path gave it, the name keeps what
                    // every path borrowed, to its last use, as a plain `let` keeps its value's loans.
                    if self.layouts.lends(local.ty) {
                        // A local of one path is gone when the path ends; the name would point at nothing.
                        if lent.iter().any(|root| self.decode(*root).0 >= outside) { return Err(error("a `match` or `if` gives a reference only into what was there before it, not into a local of one of its paths")); }
                        self.carried.extend(lent);
                        self.bound = Some((name.clone(), local.id));
                    }
                    self.local_names.insert(local.id, name.clone()); if self.at.start > 0 { self.bound_at.insert(name.clone(), self.at.start); } self.scope.insert(name.clone(), local);
                }
                a::Statement::Yield { name, value } => {
                    let at = self.yields.iter().rposition(|choice| choice.0 == *name).expect("a yield inside its choice");
                    let span = Some((value.span.start, value.span.end));
                    let wanted = self.yields[at].3.map(|local| local.ty).or(self.yields[at].2);
                    let value = match wanted { Some(ty) => self.expect(value, ty)?, None => self.expr(value)? };
                    if value.ty == Ty::Unit { return Err(error("this path gives no value: end it with the value, or leave with `return`, `break` or `continue`")); }
                    // The loans this value depends on stay taken: the chosen name will hold them.
                    if self.layouts.lends(value.ty) { let taken = std::mem::take(&mut self.carried); self.chosen[at].extend(taken); }
                    let local = match self.yields[at].3 {
                        Some(local) => { if !self.layouts.properties(local.ty).copy { self.flow.introduce(local.id); } local }
                        None => { let local = self.new_local(value.ty, self.yields[at].1, span)?; self.yields[at].3 = Some(local); local }
                    };
                    out.push(Stmt::Set(local.id, value));
                }
                a::Statement::Match { value, arms } => {
                    let (matched, all_return) = self.match_statement(value, arms, &mut watch, index)?;
                    returns |= all_return;
                    out.push(matched);
                }
                // A `for` over a container: an array is walked by index, anything else through `at`.
                a::Statement::For { index, binding, start, end: None, invariants, body } if let Some((key, statements)) = self.inlined_generator(index.as_deref(), binding, start, invariants, body) => {
                    self.inlining.push(key);
                    let checked = self.block(&statements);
                    self.inlining.pop();
                    out.extend(checked?.0);
                }
                a::Statement::For { index, binding, start, end: None, invariants, body } => {
                    let walked = crate::compiler::iteration::walked(start);
                    // An array written out has its length on the page, whatever its elements are.
                    let written = match &walked.kind { a::Expression::Array(values) => Some(values.len() as u64), _ => None };
                    let ty = if written.is_some() { None } else { self.infer_hint(walked, None, 0, &mut 0)?.map(|ty| match ty { Ty::Borrow { pointee, .. } => pointee.ty(), other => other }) };
                    let array = if written.is_some() { written } else { ty.and_then(|ty| self.layouts.get(ty)).and_then(|layout| if let Kind::Array(_, length) = layout.kind { Some(length as u64) } else { None }) };
                    // A shared slice, not walked through `&mut`: reached at a counter below its length.
                    let walked_ty = self.infer_hint(walked, None, 0, &mut 0)?;
                    let shared_slice = written.is_none() && !matches!(start.kind, a::Expression::Unary("&mut", _))
                        && matches!(walked_ty, Some(Ty::Borrow { pointee: Pointee::Slice(_), mutable: false }));
                    // A type with no `at` and a `next` is walked with a cursor: its values are made, not stored.
                    let has = |method: &str| ty.and_then(|ty| self.layouts.nominal(ty)).is_some_and(|owner| self.signatures.contains_key(&crate::compiler::methods::name(owner, method)));
                    // A generator (`gen fn`) has only `next_mut`: it changes as it goes.
                    // A type with `elements(&self) -> &[T]`, its elements in order as one slice, walked
                    // shared: the walk of that slice (the type is lent while the loop holds it).
                    // Only when re-reading the container each step would read the same thing: the body
                    // names none of the names in it (review: `for x in &arr[i]` with `i` written in the
                    // body, `for x in w.s` with `w` reassigned, were re-read before). A body that names
                    // the container may also change it once done with the element, as `at` asked again
                    // each step allows (decisions: "`for` re-asks the container").
                    let named = {
                        let mut names = std::collections::BTreeSet::new();
                        walked.clone().visit_mut(&mut |e| { if let a::Expression::Name(n) = &e.kind { names.insert(n.clone()); } false });
                        let mut found = false;
                        for statement in body { statement.clone().visit_exprs_mut(&mut |e| { if matches!(&e.kind, a::Expression::Name(n) if names.contains(n)) { found = true; } false }); }
                        found
                    };
                    let shared_slice = shared_slice && !named;
                    // A type with `elements(&self) -> &[T]`, its elements in order as one slice, walked
                    // shared where it would be walked by `at`: the walk of that slice (the type is lent
                    // while the loop holds it). The receiver is the type itself; a type walked by
                    // `next` keeps `next` (review).
                    let sliced = if array.is_none() && !shared_slice && !named && has("at") && !matches!(start.kind, a::Expression::Unary("&mut", _)) {
                        let owner = ty.and_then(|ty| self.layouts.nominal(ty));
                        owner.and_then(|owner| self.signatures.get(&crate::compiler::methods::name(owner, "elements")))
                            .is_some_and(|sig| sig.params.len() == 1
                                && matches!(sig.params[0], Ty::Borrow { mutable: false, pointee } if self.layouts.nominal(pointee.ty()) == owner)
                                && matches!(sig.result, Ty::Borrow { pointee: Pointee::Slice(_), mutable: false }))
                    } else { false };
                    // The same through `&mut`: `elements_mut(&mut self) -> &mut [T]` where `at_mut` walks it.
                    let sliced_mut = array.is_none() && !named && has("at_mut") && matches!(start.kind, a::Expression::Unary("&mut", _)) && {
                        let owner = ty.and_then(|ty| self.layouts.nominal(ty));
                        owner.and_then(|owner| self.signatures.get(&crate::compiler::methods::name(owner, "elements_mut")))
                            .is_some_and(|sig| sig.params.len() == 1
                                && matches!(sig.params[0], Ty::Borrow { mutable: true, pointee } if self.layouts.nominal(pointee.ty()) == owner)
                                && matches!(sig.result, Ty::Borrow { pointee: Pointee::Slice(_), mutable: true }))
                    };
                    // A unique slice walked by name (`for x in s` with `s: &mut [T]`): the unique
                    // walk of `s.part(0, s.len())`, a reborrow, so `s` serves again after the loop;
                    // it was walked by `at_mut`, an `Err` test each step (2x slower measured).
                    let unique_slice = written.is_none() && array.is_none() && !named && matches!(walked.kind, a::Expression::Name(_))
                        && matches!(walked_ty, Some(Ty::Borrow { pointee: Pointee::Slice(_), mutable: true }));
                    // Its length read first, into a local of its own: `s.part(0, s.len())` would read
                    // `s` while lending it.
                    let whole = format!("for#{}.whole", start.span.start);
                    let measured = unique_slice.then(|| a::Statement::Bind { mutable: false, name: whole.clone(), ty: Some(a::Type::Named("U64".into(), Vec::new())),
                        value: a::Expr { kind: a::Expression::Call(Box::new(a::Expr { kind: a::Expression::Field(Box::new(walked.clone()), "len".into()), span: start.span.clone() }), vec![]), span: start.span.clone() }, proof: None });
                    let start = &if unique_slice {
                        let len = a::Expr { kind: a::Expression::Name(whole.clone()), span: start.span.clone() };
                        let zero = a::Expr { kind: a::Expression::Integer(0), span: start.span.clone() };
                        let callee = a::Expr { kind: a::Expression::Field(Box::new(walked.clone()), "part".into()), span: start.span.clone() };
                        a::Expr { kind: a::Expression::Call(Box::new(callee), vec![zero, len]), span: start.span.clone() }
                    } else if sliced || sliced_mut {
                        let callee = a::Expr { kind: a::Expression::Field(Box::new(walked.clone()), (if sliced_mut { "elements_mut" } else { "elements" }).into()), span: start.span.clone() };
                        a::Expr { kind: a::Expression::Call(Box::new(callee), vec![]), span: start.span.clone() }
                    } else { start.clone() };
                    let way = match array { Some(length) => crate::compiler::iteration::Way::Array(length), None if shared_slice || sliced => crate::compiler::iteration::Way::Slice(false), None if sliced_mut || unique_slice => crate::compiler::iteration::Way::Slice(true), None if !has("at") && has("next") => crate::compiler::iteration::Way::Next,
                        None if !has("at") && !has("next") && has("next_mut") => crate::compiler::iteration::Way::NextMut, None => crate::compiler::iteration::Way::At };
                    let statements = measured.into_iter().chain(crate::compiler::iteration::walk(index.as_deref(), binding, start, invariants, body, way)).collect::<Vec<_>>();
                    let (inner, _) = self.block(&statements)?;
                    out.extend(inner);
                }
                a::Statement::While {
                    condition,
                    invariants,
                    body,
                } => {
                    let at = (condition.span != (0..0)).then(|| (condition.span.start, condition.span.end));
                    let mut invariants = if invariants.is_empty() { Invariants::default() } else {
                        let scope=self.scope.iter().map(|(n,l)|(n.clone(),(l.id,l.ty))).collect();
                        if let Some((parameters,element))=self.record_proof {
                            Invariants { memory: Vec::new(), record: crate::compiler::record_contract::resolve_loop(invariants,&scope,self.layouts,element,parameters)?, scalar: Vec::new(), at: None }
                        } else if self.memory_proof {
                            Invariants { memory: crate::compiler::memory_contract::resolve_loop(invariants,&scope,self.layouts,self.result,self.memory_element.expect("memory contract element"))?, record: Vec::new(), scalar: Vec::new(), at: None }
                        } else if self.contracted {
                            // A scalar contract: the invariants are clauses over the scalar locals in scope here.
                            let names=self.scope.iter().map(|(n,l)|(n.clone(),l.id)).collect();
                            let integers=self.scope.values().filter(|l|l.ty==Ty::I64).map(|l|l.id).collect();
                            let scalars=self.scope.values().filter(|l|matches!(l.ty,Ty::Unsigned(_)|Ty::Bool|Ty::I64)).map(|l|l.id).collect();
                            let bools=self.scope.values().filter(|l|l.ty==Ty::Bool).map(|l|l.id).collect();
                            Invariants { memory: Vec::new(), record: Vec::new(), scalar: crate::compiler::contract::resolve_loop(invariants,&names,&bools,&scalars,&integers,&|local,method|self.observer(local,method),&|local,fields,starred|self.clause_field(local,fields,starred))?, at: None }
                        } else { return Err(error("loop invariants require a function with a contract and proof auto")); }
                    };
                    invariants.at = at;
                    // `while i < s.len() ...:`: reads `s[i]` in the body's first statements, before any
                    // that could change `i`, come right after the test held, so they have no test
                    // (iteration.rs `COUNTED`; docs/wasm-iteration.md).
                    let mut marked_body = None;
                    let mut changed = false;
                    for (counter, slice) in self.slice_guards(condition) {
                        if Self::lends_counter(condition, &counter) { continue; }
                        let prefix = marked_body.get_or_insert_with(|| body.clone());
                        // Stop before a statement that could change the counter or bind or assign
                        // the slice's name (review 34).
                        let until = prefix.iter().position(|s| touches(s, &counter) || touches_name(s, &slice, false)).unwrap_or(prefix.len());
                        changed |= crate::compiler::iteration::counted(&mut prefix[..until], &slice, &counter);
                    }
                    let body = marked_body.as_deref().filter(|_| changed).unwrap_or(body);
                    let before = self.flow.clone();
                    let condition = self.expect(condition, Ty::Bool)?;
                    let ended = std::mem::take(&mut self.carried); self.release(&ended);
                    let exit = self.flow.clone(); // includes condition effects even at zero iterations
                    self.loops.push((before.clone(), exit.clone(), self.scope.values().map(|local| local.id).collect()));
                    let checked = self.block(body);
                    self.loops.pop();
                    let (body, body_returns) = checked?;
                    if !body_returns && self.flow != before {
                        return Err(error("loop backedge changes outer ownership availability"));
                    }
                    self.flow = exit;
                    out.push(Stmt::While(condition, body, invariants));
                }
                _ => {
                    return Err(error(
                        "proofs, assertions and guards require unsupported checking; refusing even unreachable code",
                    ));
                }
            }
        }
        if !body.is_empty() { self.settle(&mut watch, body.len() - 1, body); }
        for (_, local) in watch { self.drop_holder(local); }
        let fresh = self.scope.values().filter(|local| !saved.values().any(|outer| outer.id == local.id)).copied().collect::<Vec<_>>();
        let expired = self.expire_locals(&fresh)?;
        if !expired.is_empty() { out.push(Stmt::Expire(expired)); }
        for local in fresh { self.flow.remove(local.id); }
        self.scope = saved; // restore NAMES only; outer ownership effects survive
        Ok((out, returns))
    }
}
