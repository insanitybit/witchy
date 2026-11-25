use super::*;

impl Context<'_> {
    /// The run-time operation traps unless `check` holds: shown in a function with a contract,
    /// and known afterwards in every function.
    pub(super) fn arithmetic(&mut self, op: Binary, a: Math, b: Math, ty: Ty) -> Result<Math> {
        let Some(Some(maximum)) = scalar(ty) else { return Ok(self.fresh(ty)); };
        let pair = |make: fn(Box<Math>, Box<Math>) -> Math| make(Box::new(a.clone()), Box::new(b.clone()));
        let (value, check) = match op {
            Binary::Add => (pair(Math::Add), None),
            Binary::Sub => (pair(Math::Sub), Some(Prop::Compare("≤", b.clone(), a.clone()))),
            Binary::Mul => {
                if self.proving { self.product_bound(&a, &b)?; }
                (pair(Math::Mul), None)
            }
            Binary::Div => {
                if self.proving { self.quotient_bound(&a, &b)?; }
                (pair(Math::Div), Some(Prop::Compare("<", Math::Literal(0), b.clone())))
            }
            Binary::Rem => (pair(Math::Mod), Some(Prop::Compare("<", Math::Literal(0), b.clone()))),
            _ => return Ok(self.fresh(ty)),
        };
        let check = check.unwrap_or_else(|| Prop::Compare("≤", value.clone(), Math::Literal(maximum)));
        let what = match op {
            Binary::Add => "a `+` that may overflow",
            Binary::Mul => "a `*` that may overflow",
            Binary::Sub => "a `-` that may go below zero",
            Binary::Div => "a `/` whose divisor may be zero",
            Binary::Rem => "a `%` whose divisor may be zero",
            _ => BODY,
        };
        if self.proving { self.body(what, check.clone())?; }
        self.hypotheses.push(check);
        // By a variable, `omega` knows nothing of a quotient or remainder; these facts, each proved
        // by its core lemma once the divisor is known above zero, give it the usual bounds.
        if self.proving && !matches!(b, Math::Literal(_)) {
            let facts: &[(Prop, &'static str)] = match &value {
                Math::Div(..) => &[(Prop::Compare("≤", value.clone(), a.clone()), "  intros\n  apply Nat.div_le_self\n")],
                Math::Mod(..) => &[(Prop::Compare("<", value.clone(), b.clone()), "  intros\n  apply Nat.mod_lt\n  witchy_inline_omega\n"),
                    (Prop::Compare("≤", value.clone(), a.clone()), "  intros\n  apply Nat.mod_le\n")],
                _ => &[],
            };
            for (fact, prover) in facts {
                self.obligation(fact.clone(), Kind::Lemma(prover))?;
                self.hypotheses.push(fact.clone());
            }
        }
        Ok(value)
    }
    /// The least literal bound known for a value: `x <= c`, or `x < c` as `c - 1`.
    pub(super) fn upper(&self, x: &Math) -> Option<u64> { self.upper_within(x, 4) }
    /// `upper`, following a name's defining equation (`v = i + j`) at most `depth` deep. The
    /// prover's `omega` checks the bound from the same facts, so this only proposes it.
    fn upper_within(&self, x: &Math, depth: u32) -> Option<u64> {
        if let Math::Literal(n) = x { return Some(*n); }
        // A sum is at most the sum of its parts' bounds (`(i + j) * (i + j + 1)`).
        if let Math::Add(a, b) = x {
            if let (Some(a), Some(b)) = (self.upper_within(a, depth), self.upper_within(b, depth)) { return a.checked_add(b); }
        }
        // A difference of naturals is at most what it takes from (`100 - w`); a quotient by a
        // number above zero, the dividend's bound over it; a product, the bounds' product.
        if let Math::Sub(a, _) = x && let Some(a) = self.upper_within(a, depth) { return Some(a); }
        if let Math::Div(a, b) = x && let Math::Literal(k) = **b && k > 0 && let Some(a) = self.upper_within(a, depth) { return Some(a / k); }
        if let Math::Mul(a, b) = x && let (Some(a), Some(b)) = (self.upper_within(a, depth), self.upper_within(b, depth)) { return a.checked_mul(b); }
        // A stated bound, or one conjunct of a fact (`requires w <= 1000 && h <= 1000`).
        fn bound(fact: &Prop, x: &Math) -> Option<u64> {
            match fact {
                Prop::Compare("≤", y, Math::Literal(c)) | Prop::Compare("≥", Math::Literal(c), y) if y == x => Some(*c),
                Prop::Compare("<", y, Math::Literal(c)) | Prop::Compare(">", Math::Literal(c), y) if y == x && *c > 0 => Some(c - 1),
                Prop::Binary("∧", l, r) => match (bound(l, x), bound(r, x)) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) },
                _ => None,
            }
        }
        let stated = self.hypotheses.iter().filter_map(|fact| bound(fact, x)).min();
        if stated.is_some() || depth == 0 { return stated; }
        self.hypotheses.iter().filter_map(|fact| match fact {
            Prop::Compare("=", y, value) if y == x && value != x => self.upper_within(value, depth - 1),
            Prop::Compare("=", value, y) if y == x && value != x => self.upper_within(value, depth - 1),
            _ => None,
        }).min()
    }
    /// A constant integer, as a clause or the walk writes one.
    fn constant(m: &Math) -> Option<i128> {
        match m {
            Math::Literal(n) => Some(*n as i128),
            Math::Int(n) => Some(*n as i128),
            Math::Sub(a, b) => Some(Self::constant(a)? - Self::constant(b)?),
            Math::Add(a, b) => Some(Self::constant(a)? + Self::constant(b)?),
            _ => None,
        }
    }
    /// The least `X` known with `-X <= x <= X`, from literal bounds on both sides.
    fn magnitude(&self, x: &Math) -> Option<u64> {
        if let Some(c) = Self::constant(x) { return u64::try_from(c.unsigned_abs()).ok(); }
        let (mut low, mut high): (Option<i128>, Option<i128>) = (None, None);
        for fact in &self.hypotheses {
            let Prop::Compare(op, l, r) = fact else { continue };
            let (bound_above, c) = match (*op, l == x, r == x) {
                ("≤", true, false) | ("≥", false, true) => (true, Self::constant(if l == x { r } else { l })),
                ("<", true, false) | (">", false, true) => (true, Self::constant(if l == x { r } else { l }).map(|c| c - 1)),
                ("≤", false, true) | ("≥", true, false) => (false, Self::constant(if r == x { l } else { r })),
                ("<", false, true) | (">", true, false) => (false, Self::constant(if r == x { l } else { r }).map(|c| c + 1)),
                _ => continue,
            };
            let Some(c) = c else { continue };
            if bound_above { high = Some(high.map_or(c, |h| h.min(c))); } else { low = Some(low.map_or(c, |l| l.max(c))); }
        }
        let most = low?.unsigned_abs().max(high?.unsigned_abs());
        u64::try_from(most).ok()
    }
    /// `-(X * Y) <= a * b <= X * Y` for two I64 values with known magnitudes, neither a constant:
    /// a goal proved through `Int.natAbs_mul` and `Nat.mul_le_mul`, then a fact.
    pub(super) fn signed_product_bound(&mut self, a: &Math, b: &Math) -> Result<()> {
        if Self::constant(a).is_some() || Self::constant(b).is_some() { return Ok(()); }
        let (Some(x), Some(y)) = (self.magnitude(a), self.magnitude(b)) else { return Ok(()) };
        let Some(most) = x.checked_mul(y).filter(|m| *m <= i64::MAX as u64) else { return Ok(()) };
        let product = Math::Mul(Box::new(a.clone()), Box::new(b.clone()));
        let bound = Prop::Binary("∧", Box::new(Prop::Compare("≤", Math::Int(-(most as i64)), product.clone())), Box::new(Prop::Compare("≤", product, Math::Int(most as i64))));
        self.obligation(bound.clone(), Kind::SignedProduct { a: a.clone(), b: b.clone(), x, y })?;
        self.hypotheses.push(bound);
        Ok(())
    }
    /// `a * b <= A * B` for two values with known literal bounds, neither a literal itself: a goal
    /// proved by `Nat.mul_le_mul`, then a fact (Kind::Product).
    fn product_bound(&mut self, a: &Math, b: &Math) -> Result<()> {
        if matches!(a, Math::Literal(_)) || matches!(b, Math::Literal(_)) { return Ok(()); }
        let (Some(x), Some(y)) = (self.upper(a), self.upper(b)) else { return Ok(()) };
        let Some(most) = x.checked_mul(y) else { return Ok(()) };
        let product = Math::Mul(Box::new(a.clone()), Box::new(b.clone()));
        // Proved with the bound as `A * B`, the shape `Nat.mul_le_mul` concludes; known after as
        // the one number it is.
        let bound = Prop::Compare("≤", product.clone(), Math::Mul(Box::new(Math::Literal(x)), Box::new(Math::Literal(y))));
        self.obligation(bound, Kind::Product)?;
        self.hypotheses.push(Prop::Compare("≤", product, Math::Literal(most)));
        Ok(())
    }
    /// `m * L / y <= L` where a fact says `m <= y` (or `m < y`) and `y` is not a literal: a goal
    /// proved by `Nat.div_le_of_le_mul`, then a fact (Kind::Quotient).
    fn quotient_bound(&mut self, a: &Math, b: &Math) -> Result<()> {
        if matches!(b, Math::Literal(_)) { return Ok(()); }
        let (part, scale) = match a {
            Math::Mul(m, k) if let Math::Literal(l) = **k => (&**m, l),
            Math::Mul(k, m) if let Math::Literal(l) = **k => (&**m, l),
            _ => return Ok(()),
        };
        // A stated fact, or a conjunct of one (`requires part <= whole && whole > 0`).
        fn states(fact: &Prop, part: &Math, whole: &Math) -> bool {
            match fact {
                Prop::Compare("≤" | "<", x, y) | Prop::Compare("≥" | ">", y, x) => x == part && y == whole,
                Prop::Binary("∧", l, r) => states(l, part, whole) || states(r, part, whole),
                _ => false,
            }
        }
        let below = self.hypotheses.iter().any(|fact| states(fact, part, b));
        if !below { return Ok(()); }
        let bound = Prop::Compare("≤", Math::Div(Box::new(a.clone()), Box::new(b.clone())), Math::Literal(scale));
        self.obligation(bound.clone(), Kind::Quotient)?;
        self.hypotheses.push(bound);
        Ok(())
    }
    pub(super) fn reify(&mut self, truth: Prop) -> Math {
        match truth {
            Prop::Bool(value) => Math::Literal(u64::from(value)),
            Prop::Truth(value) => value,
            other => {
                let symbol = self.fresh(Ty::Bool);
                self.hypotheses.push(Prop::Binary("↔", Box::new(Prop::Truth(symbol.clone())), Box::new(other)));
                symbol
            }
        }
    }
}
