use super::*;

impl Checker<'_> {
    /// How each root's loan counts differ from `before`.
    pub(super) fn loan_change(&self, before: &BTreeMap<u32, (usize, usize)>) -> Vec<(u32, i64, i64)> {
        let roots = before.keys().chain(self.loans.keys()).copied().collect::<BTreeSet<_>>();
        roots.into_iter().filter_map(|root| {
            let (a, b) = before.get(&root).copied().unwrap_or_default();
            let (c, d) = self.loans.get(&root).copied().unwrap_or_default();
            let change = (c as i64 - a as i64, d as i64 - b as i64);
            (change != (0, 0)).then_some((root, change.0, change.1))
        }).collect()
    }
    pub(super) fn apply_loan_change(&mut self, change: &[(u32, i64, i64)]) {
        for (root, shared, unique) in change {
            let loan = self.loans.entry(*root).or_default();
            loan.0 = (loan.0 as i64 + shared).max(0) as usize;
            loan.1 = (loan.1 as i64 + unique).max(0) as usize;
            if *loan == (0, 0) { self.loans.remove(root); }
        }
    }
    pub(super) fn lend_again(&mut self, roots: &[u32]) {
        for root in roots {
            let loan = self.loans.entry(root & !UNIQUE).or_default();
            if root & UNIQUE != 0 { loan.1 += 1; } else { loan.0 += 1; }
        }
    }
    pub(super) fn release(&mut self, roots: &[u32]) {
        for root in roots {
            let id = root & !UNIQUE;
            if let Some(loan) = self.loans.get_mut(&id) {
                if root & UNIQUE != 0 { loan.1 = loan.1.saturating_sub(1); } else { loan.0 = loan.0.saturating_sub(1); }
                if *loan == (0, 0) { self.loans.remove(&id); }
            }
        }
    }
    /// A value read from a reference-holding local depends on the same loans.
    pub(super) fn inherit(&mut self, local: u32) {
        if let Some(held) = self.holds.get(&local).cloned() {
            self.lend_again(&held);
            self.carried.extend(held);
        }
    }
    /// A parameter origin denotes a caller's referent, never this frame's
    /// carrier slots. By-value sums can supply such origins too.
    /// A `return` of a function that says what each variant borrows: what is returned carries
    /// loans only on the parameters its variant names. A value that is not built here (a local, a
    /// call) may be any variant, so it is held to every variant's sources at once.
    pub(super) fn returned_sources(&self, value: &Expr) -> Result<()> {
        if self.own_borrows.is_empty() { return Ok(()); }
        // `return choose(x, y, flag)`: a call whose function says what each variant borrows, of
        // this function's own result type, is judged variant by variant against this one's clauses.
        if let (Value::Call(call), Some((at, definition, _, parts))) = (&value.kind, &self.split)
            && *at == self.returned_at && call.definition == *definition && call.result == self.result && parts.len() == self.own_borrows.len() {
            for (tag, part) in parts.iter().enumerate() {
                let Some(allowed) = &self.own_borrows[tag] else { continue };
                for root in part {
                    let id = self.decode(*root).0;
                    if id < self.parameters && !allowed.contains(&(id as usize)) {
                        let name = self.parameter_names.get(id as usize).cloned().unwrap_or_default();
                        return Err(error(format!("this returns a call whose variant may borrow from `{name}`, which this function's `borrows` clause for that variant does not name")));
                    }
                }
            }
            return Ok(());
        }
        let built = matches!(value.kind, Value::Sum { .. });
        let allowed: Vec<usize> = match &value.kind {
            Value::Sum { arm, .. } => match &self.own_borrows[*arm as usize] { Some(from) => from.clone(), None => return Ok(()) },
            _ => {
                let all = (0..self.parameters as usize).collect::<Vec<_>>();
                // A variant that holds no reference (an empty list) carries no loan to judge.
                all.into_iter().filter(|index| self.own_borrows.iter().all(|from| from.as_ref().is_none_or(|from| from.is_empty() || from.contains(index)))).collect()
            }
        };
        for root in &self.carried {
            let id = self.decode(*root).0;
            if id < self.parameters && !allowed.contains(&(id as usize)) {
                let name = self.parameter_names.get(id as usize).cloned().unwrap_or_default();
                let message = if built { format!("this returns a reference that borrows from `{name}`, which the function's `borrows` clause for this variant does not name") }
                    else { format!("this returns a value that borrows from `{name}` and is not built here, so it could be any variant: write the variant at the `return` (`return Ok(...)`), or name `{name}` in every `borrows` clause") };
                return Err(error(message));
            }
        }
        Ok(())
    }
    pub(super) fn external_origin(&self, root: u32) -> bool {
        // `&v` of a view: it points into this frame, which ends with the call (adversarial
        // review round 15: `return &s` of a view parameter). A plain loan on a view's id
        // stands for the references it carries instead.
        if self.decode(root).1 == [VIEW_ITSELF] { return false; }
        // What a reference parameter points through is the caller's.
        if self.decode(root).1 == [THROUGH] { return self.decode(root).0 < self.parameters; }
        let (id, path) = self.decode(root);
        // A part of a local or parameter (`&m.data`) is judged by the part's own type: a part
        // that holds no reference is storage of this frame however much the whole holds
        // (review: `return m.data.slice()` of a view `m` pointed into a dead frame).
        // A reference held by the root (`(*value).data` of `value: &Vec<T>`) is a path into the
        // caller's memory and is judged as before, by the root.
        let whole = self.locals[id as usize].ty;
        let by_value = !path.is_empty() && !matches!(whole, Ty::Borrow { .. });
        let ty = if by_value { match self.part_type(whole, path) { Some(part) => part, None => return false } } else { whole };
        if by_value && !self.layouts.lends(ty) { return false; }
        if id >= self.parameters {
            // A local that holds a reference is not where the reference points: what it
            // borrows is carried beside it and is judged on its own.
            return self.layouts.lends(ty);
        }
        if root & UNIQUE != 0 { self.layouts.lends_unique(ty) }
        else { self.layouts.lends(ty) && !matches!(ty, Ty::Borrow { mutable: true, .. }) }
    }
    /// The type of the part a loan path reaches in a value of type `ty`: field ordinals, and
    /// array elements at a literal index (`INDEX | k`). `None` when the path says no more than
    /// that (a marker, a reference on the way, a step this cannot follow).
    fn part_type(&self, mut ty: Ty, path: &[u32]) -> Option<Ty> {
        const INDEX: u32 = 1 << 31;
        for step in path {
            if *step == THROUGH || *step == VIEW_ITSELF { return None; }
            let layout = self.layouts.get(ty)?;
            ty = if step & INDEX != 0 {
                let Kind::Array(element, _) = layout.kind else { return None };
                element
            } else {
                if !matches!(layout.kind, Kind::Record(_)) { return None; }
                layout.fields.get(*step as usize)?.ty
            };
        }
        Some(ty)
    }
    /// After a call: a result that holds a reference keeps every loan its arguments took;
    /// any other result ends them.
    pub(super) fn keep(&mut self, saved: BTreeMap<u32, (usize, usize)>, mark: usize, result: Ty) {
        if !self.layouts.lends(result) { self.loans = saved; self.carried.truncate(mark); return; }
        self.carried.truncate(mark);
        let taken = self.loans.iter().flat_map(|(root, (shared, unique))| {
            let before = saved.get(root).copied().unwrap_or_default();
            std::iter::repeat_n(*root, shared.saturating_sub(before.0)).chain(std::iter::repeat_n(*root | UNIQUE, unique.saturating_sub(before.1)))
        }).collect::<Vec<_>>();
        self.carried.extend(taken);
    }
    /// Holders, from this block or one around it, that `body` never names. For a branch that
    /// leaves the function: nothing after the branch runs on its path, so their loans end there.
    pub(super) fn unread_holders(&self, body: &[a::Statement]) -> Vec<u32> {
        self.holds.keys().copied().filter(|local| {
            let mut names = self.scope.iter().filter(|(_, held)| held.id == *local).map(|(name, _)| name);
            names.next().is_some_and(|name| !crate::compiler::lending::block(body, name)) && names.next().is_none()
        }).collect()
    }
    pub(super) fn drop_holder(&mut self, local: u32) {
        if let Some(held) = self.holds.remove(&local) { self.release(&held); }
    }
    /// End of a statement: a `let` takes over its value's loans until the name's last use
    /// in this block; every other loan the statement took ends here.
    pub(super) fn settle(&mut self, watch: &mut Vec<(usize, u32)>, index: usize, body: &[a::Statement]) {
        let rest = std::mem::take(&mut self.carried);
        if let Some((name, local)) = self.bound.take() {
            if let Some((bound, kept, parts)) = self.pending_parts.take() && bound == local && same_loans(&kept, &rest) {
                self.local_parts.insert(local, (kept, parts));
            }
            self.holds.insert(local, rest);
            match crate::compiler::lending::last(body, index + 1, &name) {
                Some(last) => watch.push((last, local)),
                None => self.drop_holder(local),
            }
        } else { self.release(&rest); }
        // The names an opening gave references to: each took the value's loans for itself.
        for (name, local) in std::mem::take(&mut self.opened_views) {
            match crate::compiler::lending::last(body, index + 1, &name) {
                Some(last) => watch.push((last, local)),
                None => self.drop_holder(local),
            }
        }
        let due = watch.iter().filter(|(at, _)| *at == index).map(|(_, local)| *local).collect::<Vec<_>>();
        watch.retain(|(at, _)| *at != index);
        for local in due { self.drop_holder(local); }
    }
    /// Ends the loans of every name whose last use in its block is the statement at `index`
    /// and which none of that statement's inner `blocks` mentions: the part of the statement
    /// that used it has been evaluated, and nothing reads it again.
    pub(super) fn finish(&mut self, watch: &mut Vec<(usize, u32)>, index: usize, blocks: &[&[a::Statement]]) {
        let finished: Vec<u32> = watch.iter().filter(|(at, local)| *at == index && {
            let mut names = self.scope.iter().filter(|(_, held)| held.id == *local).map(|(name, _)| name);
            names.next().is_some_and(|name| !blocks.iter().any(|block| crate::compiler::lending::block(block, name))) && names.next().is_none()
        }).map(|(_, local)| *local).collect();
        watch.retain(|(_, local)| !finished.contains(local));
        for local in finished { self.drop_holder(local); }
    }
}
