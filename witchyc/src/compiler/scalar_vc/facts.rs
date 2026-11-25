use super::*;

impl Context<'_> {
    pub(super) fn count(&mut self) -> Result<()> {
        self.nodes += 1;
        if self.nodes > NODES { return Err(error("function contracts VC complexity limit")); }
        Ok(())
    }
    pub(super) fn fresh(&mut self, ty: Ty) -> Math {
        let symbol = self.next;
        self.next += 1;
        if let Some(Some(maximum)) = scalar(ty) { self.hypotheses.push(Prop::Compare("≤", Math::Binding(symbol), Math::Literal(maximum))); }
        if ty == Ty::I64 { self.integer(symbol); }
        Math::Binding(symbol)
    }
    /// This name is an I64: an integer between the least and the greatest.
    pub(super) fn integer(&mut self, symbol: u32) {
        self.signed.insert(symbol);
        self.hypotheses.push(Self::within(&Math::Binding(symbol)));
    }
    pub(super) fn within(value: &Math) -> Prop {
        Prop::Binary("∧", Box::new(Prop::Compare("≤", Math::Int(i64::MIN), value.clone())), Box::new(Prop::Compare("≤", value.clone(), Math::Int(i64::MAX))))
    }
    /// Whether a value is an integer: it is, or is built from, an I64 name or an integer literal.
    pub(super) fn is_integer(&self, value: &Math) -> bool {
        match value {
            Math::Int(_) => true,
            Math::Cast(integer, _) => *integer,
            Math::Binding(symbol) => self.signed.contains(symbol),
            Math::Add(a, b) | Math::Sub(a, b) | Math::Mul(a, b) | Math::Div(a, b) | Math::Mod(a, b) => self.is_integer(a) || self.is_integer(b),
            _ => false,
        }
    }
    /// I64 arithmetic. `+ - *` trap unless the exact result is in range: shown in a function with a
    /// contract, known afterwards in every function. `/` and `%` trap on a zero divisor, and `/` on
    /// the least I64 by -1; otherwise their values are Lean's `Int.tdiv` and `Int.tmod`, which
    /// round toward zero as `i64.div_s` and `i64.rem_s` do (and `tmod` of the least by -1 is 0,
    /// as `rem_s` gives).
    pub(super) fn signed(&mut self, op: Signed, a: Math, b: Math) -> Result<Math> {
        let pair = |make: fn(Box<Math>, Box<Math>) -> Math| make(Box::new(a.clone()), Box::new(b.clone()));
        let (value, check) = match op {
            Signed::Add => { let value = pair(Math::Add); let check = Self::within(&value); (Some(value), check) }
            Signed::Sub => { let value = pair(Math::Sub); let check = Self::within(&value); (Some(value), check) }
            Signed::Mul => {
                if self.proving { self.signed_product_bound(&a, &b)?; }
                let value = pair(Math::Mul); let check = Self::within(&value); (Some(value), check)
            }
            Signed::Div => (Some(pair(Math::Div)), Prop::Binary("∧", Box::new(Prop::Compare("≠", b.clone(), Math::Int(0))),
                Box::new(not(&Prop::Binary("∧", Box::new(equal(a.clone(), Math::Int(i64::MIN))), Box::new(equal(b.clone(), Math::Int(-1)))))))),
            Signed::Rem => (Some(pair(Math::Mod)), Prop::Compare("≠", b.clone(), Math::Int(0))),
            _ => return Ok(self.fresh(Ty::I64)),
        };
        if self.proving { self.body("an I64 operation that may overflow, or divide by zero", check.clone())?; }
        self.hypotheses.push(check);
        // A quotient or remainder that did not trap is an I64, as every I64 value is known to be
        // (`fresh`); `omega` does not see it through `Int.tdiv` or `Int.tmod`.
        if let (Signed::Div | Signed::Rem, Some(value)) = (op, &value) { self.hypotheses.push(Self::within(value)); }
        Ok(match value { Some(value) => value, None => self.fresh(Ty::I64) })
    }
    /// A compound value gets a name, so later facts stay small.
    pub(super) fn bind(&mut self, value: Math) -> Math {
        if matches!(value, Math::Literal(_) | Math::Int(_) | Math::Binding(_)) { return value; }
        let symbol = self.next;
        self.next += 1;
        if self.is_integer(&value) { self.signed.insert(symbol); }
        self.hypotheses.push(equal(Math::Binding(symbol), value));
        Math::Binding(symbol)
    }
    /// A condition for being here: a branch taken, a loop entered or left, a variant matched.
    pub(super) fn guard(&mut self, condition: Prop) {
        self.guards.retain(|index| *index < self.hypotheses.len());
        self.guards.push(self.hypotheses.len());
        self.hypotheses.push(condition);
    }
    pub(super) fn obligation(&mut self, goal: Prop, kind: Kind) -> Result<()> {
        self.count()?;
        self.guards.retain(|index| *index < self.hypotheses.len());
        let mut seeds = Vec::new();
        for index in &self.guards { self.hypotheses[*index].variables(&mut seeds); }
        let note = self.diagnosis.as_ref().map(Diagnosis::note).unwrap_or_default();
        self.obligations.push(Obligation { hypotheses: self.hypotheses.clone(), goal, kind, seeds, what: self.what, near: self.near, clause: None, site: None, note });
        self.what = BODY;
        Ok(())
    }
    /// A body obligation, saying what it checks.
    pub(super) fn body(&mut self, what: &'static str, goal: Prop) -> Result<()> {
        self.what = what;
        self.obligation(goal, Kind::Body)
    }
    /// The type of a scalar place, when it is one.
    pub(super) fn ty(&self, key: &Key) -> Option<Ty> {
        let mut ty = if key.0 == RESULT { self.function.result } else { self.function.locals.get(key.0 as usize)?.ty };
        if let Ty::Borrow { pointee, .. } = ty { ty = pointee.ty(); }
        for field in &key.1 {
            if *field == SUM_TAG && self.types.sum(ty).is_some() { ty = Ty::U64; continue; }
            if *field == BOX_VALUE {
                let (Ty::Boxed(id) | Ty::Buffer(id)) = ty else { return None };
                ty = self.types.buffers()[id as usize].element;
                continue;
            }
            if *field == BOX_ID && matches!(ty, Ty::Boxed(_) | Ty::Weak(_) | Ty::Buffer(_)) { ty = Ty::U64; continue; }
            if *field == BOX_ADDRESS && matches!(ty, Ty::Boxed(_) | Ty::Weak(_) | Ty::Buffer(_)) { ty = Ty::Unsigned(crate::compiler::scalar::Scalar::U32); continue; }
            if *field == SLICE_LENGTH { ty = Ty::Unsigned(crate::compiler::scalar::Scalar::U64); continue; }
            ty = if (ARM..OBSERVED_FLOOR).contains(field) { *self.types.sum(ty)?.arms.get((field - ARM) as usize)? }
                else { self.types.get(ty)?.fields.get(*field as usize)?.ty };
        }
        tracked(ty).then_some(ty)
    }
    /// Nothing is known any more about this local's parts under `path`.
    pub(super) fn forget(&mut self, root: u32, path: &[u32]) {
        self.element_forgotten(root, path);
        // What a pure trait method said of this value, or of any value it is part of, may
        // depend on the part written (review: `few.count = 0` kept `len` at 3, and a subtraction
        // proved from it wrapped).
        self.state.retain(|(local, fields), _| *local != root || !(fields.starts_with(path)
            || depends_on(fields, path)));
        self.pending.remove(&root);
    }
    /// `path` of `root` is lent with `&mut` to a reference that is kept: a store through it may
    /// come at any later point, so nothing under that path is followed again.
    pub(super) fn lend_under(&mut self, root: u32, path: Vec<u32>) {
        self.lent.insert(root);
        self.forget(root, &path);
        self.lent_under.entry(root).or_default().push(path);
    }
    /// Whether a kept `&mut` may change this scalar part: it is under a lent path, holds one
    /// (a whole value read as one number), or is something computed from a value that holds one.
    pub(super) fn unfollowed(&self, key: &Key) -> bool {
        self.lent_under.get(&key.0).is_some_and(|paths| paths.iter().any(|lent|
            key.1.starts_with(lent) || lent.starts_with(&key.1) || depends_on(&key.1, lent)))
    }
    pub(super) fn assign(&mut self, key: Key, value: Math) {
        // A part written: what a pure trait method said of the value it is part of goes (forget).
        let (root, path) = (key.0, key.1.clone());
        self.state.retain(|(local, fields), _| *local != root || !depends_on(fields, &path));
        self.remember(key, value);
    }
    // Several observations of a single evaluated value arrive simultaneously.
    // Installing one observation does not mutate the siblings it observes.
    pub(super) fn remember(&mut self, key: Key, value: Math) {
        if !self.unfollowed(&key) { let value = self.bind(value); self.state.insert(key, value); }
    }
    /// The current value of a scalar place: what was last written, or one unknown value until the next write.
    pub(super) fn leaf(&mut self, key: Key, ty: Ty) -> Math {
        if !self.unfollowed(&key) && let Some(saved) = self.state.get(&key) {
            return saved.clone();
        }
        let value = self.raw_leaf(key.clone(), ty);
        if self.unfollowed(&key) { return value; }
        if key.1.contains(&ROOT_STAMP) || key.1.contains(&MEMBER_STAMP) { return value; }
        let Some(at) = key.1.iter().position(|s| view::folded(*s)) else { return value };
        let prefix = &key.1[..at];
        let suffix = &key.1[at + 1..];
        let Some(cases) = view::cases(self.functions, key.1[at] - view::FOLD) else { return value };
        for case in cases {
            let mut guards = Vec::new();
            for (sum, index, count) in case.tags {
                let tag = self.raw_leaf((key.0, join(&join(prefix, &sum), &[SUM_TAG])), Ty::U64);
                // In an exhaustive match the final arm covers the complement.
                if index + 1 == count {
                    for prior in 0..index {
                        guards.push(not(&equal(tag.clone(), Math::Literal(prior as u64))));
                    }
                } else { guards.push(equal(tag, Math::Literal(index as u64))); }
            }
            let actual = self.raw_leaf((key.0, join(&join(prefix, &case.selected), suffix)), ty);
            self.hypotheses.push(implies(&all(guards), equal(value.clone(), actual)));
        }
        value
    }
    pub(super) fn raw_leaf(&mut self, key: Key, ty: Ty) -> Math {
        let lent = self.unfollowed(&key);
        if let (false, Some(value)) = (lent, self.state.get(&key)) { return value.clone(); }
        let value = self.fresh(ty);
        if !lent { self.state.insert(key, value.clone()); }
        value
    }
    /// Facts added while `body` runs hold only when `condition` does.
    pub(super) fn guarded<T>(&mut self, condition: &Prop, body: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let mark = self.hypotheses.len();
        self.guard(condition.clone());
        let result = body(self)?;
        let added = self.hypotheses.split_off(mark + 1);
        self.hypotheses.pop();
        for fact in added { self.hypotheses.push(implies(condition, fact)); }
        Ok(result)
    }
}
