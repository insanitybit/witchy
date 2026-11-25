//! Sum matching, including arm bindings, flow joins and borrowed payloads.
use super::*;

impl Checker<'_> {
    pub(super) fn match_statement(
        &mut self,
        value: &a::Expr,
        arms: &[a::MatchArm],
        watch: &mut Vec<(usize, u32)>,
        index: usize,
    ) -> Result<(Stmt, bool)> {
        let matched_at = value.span.clone();
        let try_site = value.span.clone();
        self.split = None;
        // `let name: Type = value?`: the written type says what `value` is a Result of.
        let wanted = arms.iter().find(|arm| arm.variant == "Ok").and_then(|arm| match (arm.binding.as_deref(), arm.body.first()) {
            (Some(held), Some(a::Statement::Bind { ty: Some(ty), value: a::Expr { kind: a::Expression::Unary("move", inner), .. }, .. }))
                if held.starts_with("?try") && matches!(&inner.kind, a::Expression::Name(name) if name == held) => Some(ty),
            _ => None,
        });
        let value = match (wanted, self.layouts.result_sum(self.result).map(|outer| outer.arms[1])) {
            (Some(annotation), Some(error)) => {
                let held = self.layouts.ty_in(annotation, self.definition, &self.generics)?;
                // A call whose own Result fails with another error keeps it: the `?`
                // below then converts it (`F.from`) or says the two differ.
                let mut visits = 0;
                let own = match self.infer_hint(value, None, 0, &mut visits) { Ok(hint) => hint, Err(_) => None }
                    .and_then(|t| self.layouts.result_sum(t).map(|sum| sum.arms))
                    .filter(|arms| arms[0] == held && arms[1] != error).map(|arms| arms[1])
                    // A generic constructor (`Vec.new(...)`) is typed only by what is
                    // expected of it; its error, when no type parameter is in it, is
                    // read from its signature (review: `using var v: Vec<U64> =
                    // Vec.new(...)?:` did not convert).
                    .or_else(|| {
                        let a::Expression::Call(callee, _) = &value.kind else { return None };
                        let a::Expression::Field(owner, method) = &callee.kind else { return None };
                        let a::Expression::Name(type_name) = &owner.kind else { return None };
                        if self.scope.contains_key(type_name) { return None; }
                        let sig = self.signatures.get(&crate::compiler::methods::name(type_name, method))?;
                        let arms = self.layouts.result_sum(sig.result)?.arms;
                        (!matches!(arms[1], Ty::Param { .. }) && self.layouts.nominal(arms[1]).is_some() && arms[1] != error).then_some(arms[1])
                    });
                let expected = self.layouts.result([held, own.unwrap_or(error)])?;
                self.expect(value, expected)?
            }
            _ => {
                // `let name: Type = match call(): Variant(x): move x ...` says what `Variant` holds.
                self.held = if matches!(&value.kind, a::Expression::Call(..)) { arms.iter().find_map(|arm| {
                    let (Some(held), None, Some(a::Statement::Yield { name, value })) = (arm.binding.as_deref(), arm.nested.as_ref(), arm.body.last()) else { return None };
                    let given = match &value.kind { a::Expression::Unary("move", inner) => &inner.kind, kind => kind };
                    if !matches!(given, a::Expression::Name(n) if n == held) || arm.body.len() != 1 { return None; }
                    let choice = self.yields.iter().rfind(|choice| choice.0 == *name)?;
                    Some((arm.variant.clone(), choice.3.map(|local| local.ty).or(choice.2)?))
                }) } else { None };
                // Or the first later method call on an arm's binder (`?` is such a
                // match: `let v = Vec.new(...)?` then `v.push_mut(...)`).
                if self.held.is_none() && matches!(&value.kind, a::Expression::Call(..)) {
                    for arm in arms {
                        let (Some(binder), None) = (arm.binding.as_deref(), arm.nested.as_ref()) else { continue };
                        if let Some(t) = self.later_use(binder, value, &arm.body)? { self.held = Some((arm.variant.clone(), t)); break; }
                    }
                }
                let value = self.expr(value);
                self.held = None;
                value?
            }
        };
        // What the matched value borrowed passes to the binders that hold a reference:
        // for a call whose function says what each variant borrows, only that variant's.
        let split = self.split.take();
        let lent = std::mem::take(&mut self.carried);
        let parts = match (&value.kind, split) {
            (Value::Call(call), Some((at, definition, kept, parts))) if at == matched_at && call.definition == definition && same_loans(&kept, &lent) => Some(parts),
            // A local bound to such a call, matched whole.
            (Value::ReadPlace(place) | Value::Move { place, .. }, _) if place.projections.is_empty() && !place.pointer
                && self.local_parts.get(&place.root).is_some_and(|(kept, _)| same_loans(kept, &lent)) => self.local_parts.get(&place.root).map(|(_, parts)| parts.clone()),
            _ => None,
        };
        self.release(&lent);
        // As for an `if`: a reference last used in the matched value is in use in no arm.
        // A binder that is a reference into the matched value lends again for itself, below.
        self.finish(watch, index, &arms.iter().map(|arm| arm.body.as_slice()).collect::<Vec<_>>());
        let handed_back = arms.iter().any(|arm| arm.binding.as_deref() == Some(crate::compiler::early_exit::ERROR));
        // Through a reference the sum stays where it is: each binder is a reference to its variant's
        // value, `&mut` when the matched reference is. A `&mut` binder keeps the matched value lent
        // uniquely while it is in use, so the variant cannot be replaced under it.
        let (through, unique) = match value.ty { Ty::Borrow { mutable, pointee } if self.layouts.sum(pointee.ty()).is_some() => (Some(pointee.ty()), mutable), _ => (None, false) };
        let matched = through.unwrap_or(value.ty);
        if unique && (!self.sealed_reach(matched) || self.layouts.invariants(matched).is_some()) {
            return Err(error("a match through `&mut` cannot reach inside a sealed type"));
        }
        let levelled = crate::compiler::nested_patterns::rewrite(arms, &a::Expr { kind: a::Expression::Bool(true), span: matched_at }, through.is_some(), unique, self.locals.len())?;
        let arms = levelled.as_deref().unwrap_or(arms);
        if self.layouts.sum(matched).is_none() && !handed_back {
            // Say what the value is (`v.cell_mut(k)` gives the element itself, not a Result).
            let shown = self.layouts.type_name(value.ty).map(|t| format!(": this value is a `{t}`, which has no variants to match")).unwrap_or_default();
            return Err(error(format!("match requires a value of a sum type{shown}")));
        }
        let mut payloads = self.layouts.sum(matched).ok_or_else(|| error(if handed_back { "`?` follows a Result value" } else { "match requires a value of a sum type" }))?.arms.clone();
        if through.is_some() {
            if handed_back { return Err(error("`?` follows a Result value, not a reference to one")); }
            if self.modelled() { return Err(error("a function with a contract cannot match through a reference yet")); }
            if self.layouts.lends(matched) { return Err(error("a match through a reference needs a sum that can be stored: this one holds a reference")); }
            for payload in &mut payloads {
                if *payload != Ty::Unit { *payload = Ty::Borrow { mutable: unique, pointee: Pointee::from_ty(*payload).ok_or_else(|| error("a match through a reference needs variants that can be stored"))? }; }
            }
        }
        if handed_back && self.layouts.result_sum(matched).is_none() { return Err(error("`?` follows a Result value")); }
        let mut converting: Option<String> = None;
        if handed_back {
            // The error leaves as it is, so it must already be the function's own error type.
            let Some(outer) = self.layouts.result_sum(self.result) else { return Err(error("`?` hands the error back: the function must return a Result")); };
            if outer.arms[1] != payloads[1] && self.layouts.widening(payloads[1], outer.arms[1]).is_none() {
                // The function's error type says how to make one of this error:
                // `pub fn from(error: E) -> F`, which `?` calls (docs/decisions.md).
                let target = self.layouts.nominal(outer.arms[1]).map(str::to_string);
                let converts = target.as_ref().and_then(|owner| self.signatures.get(&crate::compiler::methods::name(owner, "from"))).cloned()
                    .is_some_and(|sig| if sig.generics.is_empty() { sig.params == [payloads[1]] && sig.result == outer.arms[1] } else {
                        // A generic `from` (`impl<E> Wrapped<E>`): one solution of its type
                        // parameters makes it take this error and make the function's.
                        let mut solved = vec![None; sig.generics.len()];
                        sig.params.len() == 1
                            && self.layouts.unify(sig.params[0], payloads[1], sig.definition, &mut solved).is_ok()
                            && self.layouts.unify(sig.result, outer.arms[1], sig.definition, &mut solved).is_ok()
                            && solved.iter().all(Option::is_some)
                    });
                if !converts {
                    let name = |t: Ty| self.layouts.type_name(t).unwrap_or_else(|_| "?".into());
                    return Err(error(format!("`?` hands the error back as it is: the function must return the same error type, and this fails with a `{}` where the function's errors are `{}`; handle this one where it happens (`match`, or `let Ok(x) = ... else Err(e):`), or give `{}` a `pub fn from(error: {}) -> {}` for `?` to call", name(payloads[1]), name(outer.arms[1]), name(outer.arms[1]), name(payloads[1]), name(outer.arms[1]))));
                }
                converting = target;
            }
            // A value that owns nothing may be left unused; an owner needs a name so it can be closed.
            if payloads[0] != Ty::Unit && matches!(arms[0].binding.as_deref(), None | Some("_")) && !self.layouts.properties(payloads[0]).copy { return Err(error("`?` on a Result that holds an owner needs a `let` to hold it")); }
        }
        // The tags of an anonymous sum are written with their dot.
        let anonymous = self.layouts.sum(matched).expect("a sum").anonymous;
        let names = self.layouts.sum(matched).expect("a sum").names.iter().map(|name| if anonymous { format!(".{name}") } else { name.clone() }).collect::<Vec<_>>();
        let mut selected = vec![None; names.len()];
        // `_:` is the arm of every variant that has none of its own.
        let rest = arms.iter().position(|arm| arm.variant == "_" || arm.variant == crate::compiler::nested_patterns::REST);
        if let Some(at) = rest {
            if at + 1 != arms.len() { return Err(error("the `_` arm comes last: it takes the variants no other arm names")); }
            if arms[at].binding.is_some() { return Err(error("the `_` arm holds no value: write `_:`")); }
            if handed_back || self.modelled() { return Err(error("a function with a contract gives every variant its own arm")); }
        }
        for arm in &arms[..rest.unwrap_or(arms.len())] {
            // An anonymous union's tags are written with their dot (`.Line:`): say so when
            // the arm is one of them without it.
            let dotted = format!(".{}", arm.variant);
            let hint = if names.iter().any(|name| *name == dotted) { format!("; this type's tags are written with a dot: `{dotted}:`") } else { String::new() };
            let tag = names.iter().position(|name| *name == arm.variant).ok_or_else(|| error(format!(
                "match arm names a variant the type does not have: `{}` is none of {}{hint}", arm.variant.split('@').next().unwrap_or(&arm.variant),
                names.iter().map(|n| format!("`{}`", n.split('@').next().unwrap_or(n))).collect::<Vec<_>>().join(", "))))?;
            if selected[tag].replace(arm).is_some() { return Err(error(format!("duplicate match arm: `{}` has two arms; one arm takes each variant", arm.variant.split('@').next().unwrap_or(&arm.variant)))); }
        }
        // `?` with an error the function's own type makes from: the failure arm returns
        // `Err(F.from(error))`, checked as if written so.
        let converted = converting.as_ref().map(|owner| {
            // Only the `return` changes: what runs before it (a `using` block's discards)
            // stays.
            let mut arm = selected[1].expect("the Err arm of a `?`").clone();
            if let Some(a::Statement::Return(Some(value))) = arm.body.last_mut() {
                let at = value.span.clone();
                let expr = |kind| a::Expr { kind, span: at.clone() };
                let error = expr(a::Expression::Unary("move", Box::new(expr(a::Expression::Name(crate::compiler::early_exit::ERROR.into())))));
                // Called by the function's own key, `F.from`, which no local can be named
                // (review: a local named `AppError` took a written `AppError.from`).
                let from = expr(a::Expression::Call(Box::new(expr(a::Expression::Name(crate::compiler::methods::name(owner, "from")))), vec![error]));
                *value = expr(a::Expression::Call(Box::new(expr(a::Expression::Name("Err".into()))), vec![from]));
            }
            arm
        });
        if let Some(arm) = &converted { selected[1] = Some(arm); }
        let mut covered = vec![false; names.len()];
        if let Some(at) = rest {
            if selected.iter().all(Option::is_some) && arms[at].variant == "_" { return Err(error("the `_` arm is never taken: every variant has an arm of its own")); }
            for tag in 0..names.len() {
                if selected[tag].is_some() { continue; }
                if !self.unnamed(payloads[tag]) {
                    return Err(error(format!("the `_` arm would lose the owner that `{}` holds: give `{}` an arm of its own", names[tag], names[tag])));
                }
                selected[tag] = Some(&arms[at]);
                covered[tag] = true;
            }
        }
        // Every variant has its own arm: a new variant makes each match that must change an error.
        let missing = names.iter().zip(&selected).filter(|(_, arm)| arm.is_none()).map(|(name, _)| name.as_str()).collect::<Vec<_>>();
        if !missing.is_empty() {
            // The message lives as long as the compiler run that reports it.
            // The arms to write, each with a binder when its variant carries a value.
            let written = names.iter().zip(&selected).zip(payloads.iter()).filter(|((_, arm), _)| arm.is_none())
                .map(|((name, _), payload)| if *payload == Ty::Unit { format!("`{name}:`") } else { format!("`{name}(value):`") }).collect::<Vec<_>>();
            return Err(error(format!("match must have an arm for every variant; missing: {}; add {}", missing.join(", "), written.join(" and "))));
        }
        let starting = self.flow.clone();
        let (mut lowered, mut flows, mut all_return) = (Vec::new(), Vec::new(), true);
        // Each arm starts from the loans in force before it; what each kept or released
        // is combined after all of them.
        let loans = self.loans.clone();
        let mut changes = Vec::new();
        let holds = self.holds.clone();
        let mut falling: Option<BTreeMap<u32, Vec<u32>>> = None;
        for (tag, arm) in selected.into_iter().enumerate() {
            self.flow = starting.clone();
            self.loans = loans.clone();
            self.holds = holds.clone();
            // A name last used in this match that this arm never reads holds nothing in
            // it: in a `?`'s failure arm, a span into a `using` value, read only on the
            // success path, no longer keeps that value lent for its discard.
            let body = &arm.expect("checked above").body;
            let unmentioned = |this: &Self, local: u32| {
                let mut names = this.scope.iter().filter(|(_, held)| held.id == local).map(|(name, _)| name);
                names.next().is_some_and(|name| !crate::compiler::lending::block(body, name)) && names.next().is_none()
            };
            let mut unread: Vec<u32> = watch.iter().filter(|(at, local)| *at == index && unmentioned(self, *local)).map(|(_, local)| *local).collect();
            // An arm that leaves the function on every path runs nothing after this
            // match: what any holder lends, from this block or one around it, is used
            // there only if the arm itself names the holder.
            let leaves = leaves_function(body);
            if leaves { unread.extend(self.unread_holders(body)); }
            for local in unread { self.drop_holder(local); }
            let own = parts.as_ref().map(|parts| parts[tag].as_slice()).unwrap_or(&lent);
            let outer_try = self.try_exit.clone();
            if arm.expect("checked above").binding.as_deref() == Some(crate::compiler::early_exit::ERROR) { self.try_exit = Some(try_site.clone()); }
            let checked = self.match_arm(arm.expect("checked above"), payloads[tag], own, covered[tag]);
            self.try_exit = outer_try;
            let change = self.loan_change(&loans);
            let (arm, arm_returns) = checked?;
            // An arm that leaves the function shapes nothing after the match: its loans
            // and holders are not carried on (they may have been ended early, above).
            // (`leaves`, not `arm_returns`: a `break` or `continue` goes on after the loop.)
            if !leaves { changes.push(change); falling = Some(self.holds.clone()); }
            lowered.push(arm);
            flows.push((self.flow.clone(), arm_returns));
            all_return &= arm_returns;
        }
        self.loans = loans;
        self.holds = falling.unwrap_or(holds);
        for change in &changes { self.apply_loan_change(change); }
        let joined = flows.iter().map(|(flow, returned)| (flow, *returned)).collect::<Vec<_>>();
        self.flow = Flow::join(&starting, &joined);
        Ok((Stmt::Match { value, arms: lowered }, all_return))
    }
}
