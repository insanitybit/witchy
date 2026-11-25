use super::*;

impl Context<'_> {
    pub(super) fn block(&mut self, statements: &[Stmt]) -> Result<Flow> {
        for statement in statements { if self.statement(statement)? == Flow::Ends { return Ok(Flow::Ends); } }
        Ok(Flow::Falls)
    }
    pub(super) fn statement(&mut self, statement: &Stmt) -> Result<Flow> {
        // What this statement assigns, for the obligations its value makes; nested blocks set
        // their own, and this one's comes back after.
        let outer = self.near;
        self.near = match statement { Stmt::Set(id, _) => Some(*id), Stmt::StorePlace(place, _, _) => Some(place.root), Stmt::Return(..) => Some(RESULT), _ => None };
        let flow = self.statement_inner(statement);
        self.near = outer;
        flow
    }
    fn statement_inner(&mut self, statement: &Stmt) -> Result<Flow> {
        self.count()?;
        // Diagnostics: a local assigned again after a loop is no longer unknown because of it.
        if let (Stmt::Set(id, _), Some(d)) = (statement, self.diagnosis.as_mut()) && let Some(name) = self.function.local_names.get(id) {
            for note in &mut d.passed { note.names.retain(|n| n != name); }
        }
        match statement {
            Stmt::Return(e, _, at) if self.diagnosis.is_some() => {
                // Diagnostics: the obligations of this `return` say where it is written.
                let outer = self.diagnosis.as_mut().and_then(|d| std::mem::replace(&mut d.returning, *at));
                let flow = self.returning(e);
                if let Some(d) = self.diagnosis.as_mut() { d.returning = outer; }
                return flow;
            }
            Stmt::Return(e, _, _) => return self.returning(e),
            Stmt::Set(id, e) => {
                if tracked(e.ty) { let value = self.number(e)?; self.assign((*id, Vec::new()), value); }
                else { self.store(*id, Vec::new(), e)?; self.element_set(*id, e)?; }
            }
            Stmt::StorePlace(place, op, e) => match fields(place) {
                Some(path) if tracked(place.ty) => {
                    // The destination's value is read before the right side runs, as the emitted
                    // code reads it (emit.rs, StorePlace): a call on the right side that changes
                    // the destination does not change what is added to.
                    let current = op.map(|_| self.leaf((place.root, path.clone()), place.ty));
                    let mut value = self.number(e)?;
                    if let (Some(op), Some(current)) = (op, current) {
                        if self.proving && matches!(op, Binary::Add | Binary::Sub | Binary::Mul) && matches!(place.ty, Ty::Unsigned(_)) && matches!(scalar(place.ty), Some(Some(_))) {
                            self.covered.insert(statement as *const Stmt as usize);
                        }
                        value = self.arithmetic(*op, current, value, place.ty)?;
                    }
                    // A field an invariant over elements reads: the invariant is kept (elements.rs).
                    let key = (place.root, path);
                    let own = self.own_before(&key);
                    self.assign(key.clone(), value);
                    self.own_assigned(&key, own)?;
                }
                Some(path) => { let cell = self.element_cell(place); self.store(place.root, path, e)?; self.element_stored(cell, e); }
                // A store into an array element: the array's other parts are unknown too.
                // The target's indices are evaluated before the right side, as emitted (`locate`).
                None => { self.place(place)?; self.effects(e)?; self.forget(place.root, &prefix(place)); }
            },
            // A call that never returns ends the way here, as the `unreachable` it is.
            Stmt::Eval(e) if elements::diverges(self.functions, e) => { self.effects(e)?; return Ok(Flow::Ends); }
            Stmt::Eval(e) => self.effects(e)?,
            Stmt::Replace(local, place, e) => { self.place(place)?; self.effects(e)?; self.forget(*local, &[]); self.forget(place.root, &prefix(place)); }
            Stmt::Expire(_) => {}
            // A call's result waits in a local only along straight-line code.
            Stmt::If(condition, yes, no) => { let flow = self.branch(condition, yes, no)?; self.pending.clear(); return Ok(flow); }
            Stmt::While(condition, body, invariants) => { self.pending.clear(); self.repeat(condition, body, invariants)?; self.pending.clear(); }
            Stmt::Match { value, arms } => { let flow = self.select(value, arms)?; self.pending.clear(); return Ok(flow); }
            // A pass that ends early still ends with the invariants: `continue` goes back to the
            // head, and `break` leaves a loop after which they are all that is known.
            // Under a contract, a path that reaches `unreachable` must be shown never to be
            // taken: what is known on the way here has to be contradictory. Without one (a
            // function walked for its calls' goals) it is a defined trap. Either way nothing
            // after it runs.
            Stmt::Trap(site) => {
                if self.proving {
                    // The checker's own lowerings are reported as what the author wrote.
                    self.body(match site.why {
                        crate::ast::Why::Overwrite => OVERWRITE,
                        crate::ast::Why::Discard => DISCARD,
                        crate::ast::Why::Written | crate::ast::Why::Lowered => UNREACHED,
                    }, Prop::Bool(false))?;
                    if site.span != (0..0) && let Some(last) = self.obligations.last_mut() { last.site = Some((site.span.start, site.span.end)); }
                    self.checked_traps.insert(statement as *const Stmt as usize);
                }
                return Ok(Flow::Ends);
            }
            Stmt::Break | Stmt::Continue => {
                for clause in self.loops.last().cloned().unwrap_or_default() { let goal = self.invariant(&clause)?; self.body("a loop invariant at a `break` or `continue`", goal)?; }
                return Ok(Flow::Ends);
            }
        }
        Ok(Flow::Falls)
    }
    /// A `return` of this value: its value is the result, and every `ensures` is shown.
    fn returning(&mut self, e: &Expr) -> Result<Flow> {
        let mut variant = Returned::Unknown;
        let result = if tracked(e.ty) { Some(self.number(e)?) } else if self.parts {
            // A clause reads a field of the result: the record returned is followed like one written to a local.
            self.forget(RESULT, &[]);
            // A variant built here: what it holds, under its arm's step, and which it is.
            if let Value::Sum { arm, payload } = &e.kind {
                variant = Returned::Variant(*arm);
                self.remember((RESULT, vec![SUM_TAG]), Math::Literal(u64::from(*arm)));
                if tracked(payload.ty) { let value = self.number(payload)?; let value = self.bind(value); self.assign((RESULT, vec![ARM | *arm as u32]), value); }
                else { self.store(RESULT, vec![ARM | *arm as u32], payload)?; }
            } else if let Some(outcome) = self.handed_on(e)? {
                // A callee's Result handed on, directly (`return half(n)`) or through a
                // whole local that still holds it (`let answer = half(n)`, `return answer`;
                // Astra audit): what its contract says each variant holds is what this
                // result's variant holds, and which facts hold is by variant.
                for ((arm, rest), value) in &outcome.values {
                    let path = match arm { Some(arm) => join(&[ARM | *arm as u32], rest), None => rest.clone() };
                    self.remember((RESULT, path), value.clone());
                }
                variant = Returned::Outcome(outcome);
            } else { self.store(RESULT, Vec::new(), e)?; }
            None
        } else {
            match &e.kind {
                Value::Sum { arm, payload } => { self.effects(payload)?; variant = Returned::Variant(*arm); }
                Value::Call(call) => variant = Returned::Outcome(self.call(call)?.1),
                Value::Move { place, .. } | Value::ReadPlace(place) if place.projections.is_empty() && !place.pointer => {
                    if let Some(outcome) = self.pending.remove(&place.root) { variant = Returned::Outcome(outcome); }
                }
                _ => self.effects(e)?,
            }
            None
        };
        self.returned(result, variant)?;
        self.forget(RESULT, &[]);
        Ok(Flow::Ends)
    }
    /// What a clause's method call reads: the field, of the local as it is now or as it was on entry.
    pub(super) fn observe(&mut self, m: &Math) -> Result<Math> {
        let Some((root, path, ty, before)) = read(self.functions, m)? else { return Ok(m.clone()) };
        let key = (root, path);
        if before { return self.entry.get(&key).cloned().ok_or_else(|| error("function contracts old takes a method call on a parameter")); }
        if (root as usize) < self.function.params.len() && !matches!(self.function.params[root as usize], Ty::Borrow { .. }) {
            return Err(error("function contracts read a parameter taken by value as it was on entry: `old(value.method())`"));
        }
        Ok(self.leaf(key, ty))
    }
    /// A return shows every `ensures`: parameters at their entry values, fields behind `&mut` as they are now.
    /// A clause stated for one variant of the result is shown when that variant is returned: always
    /// when the variant is not known, never when it is another, and from what a callee's contract
    /// says of that variant when the callee's result is handed on.
    pub(super) fn returned(&mut self, result: Option<Math>, variant: Returned) -> Result<()> {
        let function = self.function;
        let Some(contract) = scalar_contract(function) else { return Ok(()); };
        if !contract.effects.is_empty() { self.effects_shown(&variant)?; }
        for clause in &contract.ensures {
            let mut given = Vec::new();
            match (clause.arm, &variant) {
                (Some(arm), Returned::Variant(returned)) if arm != *returned => continue,
                (Some(arm), Returned::Outcome(outcome)) => given.extend(outcome.facts.iter().filter(|(of, _)| *of == arm).map(|(_, fact)| fact.clone())),
                _ => {}
            }
            let goal = clause.value.rewrite(&mut |m| match m {
                Math::Result => result.clone().ok_or_else(|| error("function contracts ghost result was not bound to actual return")),
                Math::Observe { .. } | Math::Field { .. } => self.observe(m),
                other => Ok(other.clone()),
            }).map_err(|refusal| super::super::proof_advice::at_clause(refusal, &clause.span))?;
            let goal = if given.is_empty() { goal } else { implies(&all(given), goal) };
            self.body(ENSURES, goal)?;
            // Where the clause was written, for a refusal to tell a type's invariant from the
            // function's own `ensures` (for `proof::body_parts`).
            if clause.span != (0..0) && let Some(last) = self.obligations.last_mut() { last.site = Some((clause.span.start, clause.span.end)); }
        }
        Ok(())
    }
    /// The local's value after two ways of getting here: one name, equal to each side's value in its case.
    fn merge(&mut self, cases: Vec<BTreeMap<Key, Math>>) -> Vec<Vec<Prop>> {
        let mut equations = vec![Vec::new(); cases.len()];
        self.state.clear();
        let Some(first) = cases.first() else { return equations };
        for (key, value) in first {
            // A place one way has not read or written is whatever it was: it gets a name when next read.
            let Some(values) = cases.iter().map(|state| state.get(key).cloned()).collect::<Option<Vec<_>>>() else { continue };
            if values.iter().all(|other| other == value) { self.state.insert(key.clone(), value.clone()); continue; }
            let Some(ty) = self.ty(key) else { continue };
            let joined = self.fresh(ty);
            for (index, value) in values.into_iter().enumerate() { equations[index].push(equal(joined.clone(), value)); }
            self.state.insert(key.clone(), joined);
        }
        equations
    }
    pub(super) fn branch(&mut self, condition: &Expr, yes: &[Stmt], no: &[Stmt]) -> Result<Flow> {
        let taken = self.truth(condition)?;
        let skipped = not(&taken);
        let before = self.state.clone();
        let tagged = self.tags.clone();
        let waiting = self.pending.clone();
        let mark = self.hypotheses.len();
        let run = |context: &mut Self, guard: &Prop, body: &[Stmt]| -> Result<(Flow, Vec<Prop>, BTreeMap<Key, Math>, elements::Tags)> {
            context.pending = waiting.clone();
            context.guard(guard.clone());
            let flow = context.block(body)?;
            let added = context.hypotheses.split_off(mark + 1);
            context.hypotheses.pop();
            Ok((flow, added, std::mem::replace(&mut context.state, before.clone()), std::mem::replace(&mut context.tags, tagged.clone())))
        };
        // Diagnostics: the loops of a way that goes on are behind what follows.
        let walked = self.walked();
        let (flow_yes, facts_yes, state_yes, mut tags_yes) = run(self, &taken, yes)?;
        let mut passed = self.rewind(walked);
        if flow_yes == Flow::Ends { passed.clear(); }
        let (flow_no, facts_no, state_no, tags_no) = run(self, &skipped, no)?;
        let more = self.rewind(walked);
        if flow_no == Flow::Falls { passed.extend(more); }
        self.passed(passed);
        match (flow_yes, flow_no) {
            (Flow::Ends, Flow::Ends) => return Ok(Flow::Ends),
            (Flow::Falls, Flow::Ends) => { self.guard(taken); self.hypotheses.extend(facts_yes); self.state = state_yes; self.tags = tags_yes; }
            (Flow::Ends, Flow::Falls) => { self.guard(skipped); self.hypotheses.extend(facts_no); self.state = state_no; self.tags = tags_no; }
            (Flow::Falls, Flow::Falls) => {
                let stamp = self.stamp();
                tags_yes.join(&tags_no, stamp, &Some(taken.clone()), &Some(skipped.clone()));
                self.tags = tags_yes;
                for fact in facts_yes { self.hypotheses.push(implies(&taken, fact)); }
                for fact in facts_no { self.hypotheses.push(implies(&skipped, fact)); }
                let equations = self.merge(vec![state_yes, state_no]);
                for (guard, facts) in [taken, skipped].iter().zip(equations) { for fact in facts { self.hypotheses.push(implies(guard, fact)); } }
            }
        }
        Ok(Flow::Falls)
    }
    pub(super) fn invariant(&mut self, clause: &Prop) -> Result<Prop> {
        clause.rewrite(&mut |m| match m {
            Math::Binding(local) => {
                let key = (*local, Vec::new());
                let ty = self.ty(&key).ok_or_else(|| error("loop invariants name scalar locals"))?;
                Ok(self.leaf(key, ty))
            }
            Math::Observe { .. } | Math::Field { .. } => self.observe(m),
            other => Ok(other.clone()),
        })
    }
    /// Diagnostics: how many loops have been walked on the way here. Each way through a branch
    /// starts from there (`rewind`); afterwards the loops of every way are behind (`passed`).
    fn walked(&self) -> usize { self.diagnosis.as_ref().map_or(0, |d| d.passed.len()) }
    fn rewind(&mut self, walked: usize) -> Vec<LoopNote> { self.diagnosis.as_mut().map(|d| d.passed.split_off(walked.min(d.passed.len()))).unwrap_or_default() }
    fn passed(&mut self, loops: Vec<LoopNote>) { if let Some(d) = self.diagnosis.as_mut() { d.passed.extend(loops); } }
    /// A loop: the invariants hold on entry, each pass keeps them, and after the loop they hold
    /// with the condition false. Locals the body assigns are unknown at the head. A loop that
    /// `break` can leave is left with the invariants alone: the condition may still be true.
    fn repeat(&mut self, condition: &Expr, body: &[Stmt], invariants: &Invariants) -> Result<()> {
        let (assigned, jumps) = scan::loop_effects(condition, body);
        let noted = self.diagnosis.is_some().then(|| invariants.at.map(|at| LoopNote { at, invariants: !invariants.scalar.is_empty(),
            names: assigned.iter().filter_map(|id| self.function.local_names.get(id).cloned()).collect() })).flatten();
        if let (Some(d), Some(note)) = (self.diagnosis.as_mut(), &noted) { d.enclosing.push(note.clone()); }
        for clause in &invariants.scalar { let goal = self.invariant(clause)?; self.body("a loop invariant where the loop is entered", goal)?; }
        self.state.retain(|(local, _), _| !assigned.contains(local));
        self.element_loop(&assigned);
        for clause in &invariants.scalar { let fact = self.invariant(clause)?; self.hypotheses.push(fact); }
        let again = self.truth(condition)?;
        let head = self.state.clone();
        let tagged = self.tags.clone();
        let mark = self.hypotheses.len();
        self.guard(again.clone());
        self.loops.push(invariants.scalar.clone());
        let flow = self.block(body);
        self.loops.pop();
        if flow? == Flow::Falls {
            for clause in &invariants.scalar { let goal = self.invariant(clause)?; self.body("a loop invariant after a pass of the loop", goal)?; }
        }
        if let (Some(d), Some(note)) = (self.diagnosis.as_mut(), noted) { d.enclosing.pop(); d.passed.push(note); }
        self.hypotheses.truncate(mark);
        self.state = head;
        self.tags = tagged;
        if jumps {
            // A `break` leaves from the middle of a pass: what the pass assigned is known only
            // through the invariants, which every way out of the loop has shown.
            self.state.retain(|(local, _), _| !assigned.contains(local));
            for clause in &invariants.scalar { let fact = self.invariant(clause)?; self.hypotheses.push(fact); }
        } else { self.guard(not(&again)); }
        Ok(())
    }
    /// A match: each arm runs from the same state; afterwards one of the arms that continue was taken.
    pub(super) fn select(&mut self, value: &Expr, arms: &[Arm]) -> Result<Flow> {
        // Save the scrutinee before any arm can borrow or move its payload.
        let tag = match &value.kind {
            Value::Sum { arm, .. } => Some(Math::Literal(u64::from(*arm))),
            Value::Address(p) | Value::ReadPlace(p) | Value::Move { place: p, .. } => fields(p)
                .map(|path| self.leaf((p.root, join(&path, &[SUM_TAG])), Ty::U64)),
            _ => None,
        };
        let outcome = match &value.kind {
            Value::BoxOp(op @ (crate::compiler::operations::BoxOp::Ref | crate::compiler::operations::BoxOp::Mut | crate::compiler::operations::BoxOp::Take), _, args) => self.box_variant(*op, &args[0])?,
            Value::BoxOp(crate::compiler::operations::BoxOp::New, _, args) => self.new_box(args)?,
            Value::Call(call) => self.call(call)?.1,
            Value::Move { place, .. } | Value::ReadPlace(place) if place.projections.is_empty() && !place.pointer => self.pending.remove(&place.root).unwrap_or_default(),
            _ => { self.effects(value)?; Outcome::default() }
        };
        // A call matched where it is made is the call bound with `let` and that local matched
        // (`Stmt::Set`, then the `Move` arm above): what `store` would have written of the result
        // to the local (the parts of `outcome.values` that are of no one variant, `gather`) says
        // which variant it is and what each variant holds. Nothing the `let` form does not know.
        let direct: Vec<(Vec<u32>, Math)> = match &value.kind {
            Value::Call(_) if !matches!(value.ty, Ty::Borrow { .. }) && self.types.sum(value.ty).is_some() =>
                outcome.values.iter().filter(|((arm, _), _)| arm.is_none()).map(|((_, path), known)| (path.clone(), known.clone())).collect(),
            _ => Vec::new(),
        };
        let direct: Vec<(Vec<u32>, Math)> = direct.into_iter().map(|(path, known)| (path, self.bind(known))).collect();
        let tag = tag.or_else(|| direct.iter().find(|(path, _)| path.as_slice() == [SUM_TAG]).map(|(_, known)| known.clone()));
        let before = self.state.clone();
        let tagged = self.tags.clone();
        let waiting = self.pending.clone();
        let mark = self.hypotheses.len();
        let mut continuing = Vec::new();
        let (walked, mut passed) = (self.walked(), Vec::new());
        for (index, arm) in arms.iter().enumerate() {
            if let Some(tag) = &tag { self.guard(equal(tag.clone(), Math::Literal(index as u64))); }
            let variant = Some(index as u8);
            if let Some(binding) = arm.binding {
                self.forget(binding, &[]);
                // Matching a named sum projects exactly the selected payload.
                // Copy known subfields for a move or a shared reborrow. A unique
                // loan can subsequently change them through the binding.
                if !matches!(self.function.locals[binding as usize].ty, Ty::Borrow { mutable: true, .. })
                    && let Some(place) = box_place(value)
                    && !self.lent.contains(&place.root)
                    && let Some(mut path) = fields(place) {
                    path.push(ARM + index as u32);
                    for ((root, fields), v) in &before {
                        if *root == place.root && let Some(rest) = fields.strip_prefix(path.as_slice()) {
                            self.remember((binding, rest.to_vec()), v.clone());
                        }
                    }
                }
                // The same of a call matched where it is made (`direct`, above).
                if !matches!(self.function.locals[binding as usize].ty, Ty::Borrow { mutable: true, .. }) {
                    for (path, known) in &direct {
                        if let Some(rest) = path.strip_prefix(&[ARM + index as u32]) { self.remember((binding, rest.to_vec()), known.clone()); }
                    }
                }
                // What the callee's contract says of this variant's value.
                for ((of, path), known) in &outcome.values { if *of == variant { self.remember((binding, path.clone()), known.clone()); } }
            }
            for (of, fact) in &outcome.facts { if Some(*of) == variant { self.guard(fact.clone()); } }
            self.element_arm(&outcome.elements, variant, arm.binding, &arm.body)?;
            let flow = self.block(&arm.body)?;
            let facts = self.hypotheses.split_off(mark);
            let state = std::mem::replace(&mut self.state, before.clone());
            let tags = std::mem::replace(&mut self.tags, tagged.clone());
            self.pending = waiting.clone();
            let more = self.rewind(walked);
            // What held on this way, for what it did to a buffer the contract speaks of.
            let held = (flow == Flow::Falls && !self.declared.is_empty() && !facts.is_empty()).then(|| all(facts.clone()));
            if flow == Flow::Falls { continuing.push(((facts, state), (tags, held))); passed.extend(more); }
        }
        let (continuing, mut ways): (Vec<_>, Vec<_>) = continuing.into_iter().unzip();
        let mut continuing: Vec<(Vec<Prop>, BTreeMap<Key, Math>)> = continuing;
        if !ways.is_empty() {
            let (mut joined, mut mine) = ways.remove(0);
            if ways.is_empty() { mine = None; }
            for (way, theirs) in &ways { let stamp = self.stamp(); joined.join(way, stamp, &mine, theirs); mine = None; }
            self.tags = joined;
        }
        self.passed(passed);
        match continuing.len() {
            0 => return Ok(Flow::Ends),
            1 => { let (facts, state) = continuing.remove(0); self.hypotheses.extend(facts); self.state = state; }
            _ => {
                let (facts, states): (Vec<_>, Vec<_>) = continuing.into_iter().unzip();
                let equations = self.merge(states);
                let mut cases = facts.into_iter().zip(equations).map(|(mut facts, equations)| { facts.extend(equations); all(facts) });
                let mut any = cases.next().expect("two continuing arms");
                for case in cases { any = Prop::Binary("∨", Box::new(any), Box::new(case)); }
                // The names the merge introduced have their range facts ahead of this one.
                self.hypotheses.push(any);
            }
        }
        Ok(Flow::Falls)
    }
}
