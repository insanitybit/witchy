use super::*;

impl Context<'_> {
    pub(super) fn push(&mut self, out: &mut String, text: &str) -> Result<()> {
        if text.len() > self.bytes { return Err(error("function contracts generated statement budget exceeded")); }
        self.bytes -= text.len();
        out.push_str(text);
        Ok(())
    }
    pub(super) fn symbol(&self, id: u32) -> String { format!("{}{id}", if (id as usize) < self.function.params.len() { "p" } else { "v" }) }
    /// `integer`: the comparison this value stands in is between integers, so its literals are too.
    pub(super) fn math(&mut self, out: &mut String, value: &Math, integer: bool) -> Result<()> {
        self.count()?;
        let (symbol, a, b) = match value {
            Math::Literal(n) if integer => {
                if *n > i64::MAX as u64 { return Err(error("function contracts integer literal exceeds the range of I64")); }
                return self.push(out, &format!("({n} : Int)"));
            }
            Math::Literal(n) => return self.push(out, &format!("({n} : Nat)")),
            Math::Int(n) => return self.push(out, &format!("({n} : Int)")),
            // Two's complement over 64 bits, in the forms `omega` reads: `emod` by a literal,
            // `Int.toNat` and the cast of a natural number.
            Math::Cast(to, inner) => {
                if *to != integer { return Err(error("function contracts compare an I64 with an I64, and an unsigned number with an unsigned number")); }
                if *to {
                    self.push(out, "((((")?; self.math(out, inner, false)?;
                    return self.push(out, " : Nat) : Int) + 9223372036854775808) % 18446744073709551616 - 9223372036854775808)");
                }
                self.push(out, "(Int.toNat (")?; self.math(out, inner, true)?;
                return self.push(out, " % 18446744073709551616))");
            }
            Math::Binding(id) => {
                if integer != self.signed.contains(id) { return Err(error("function contracts compare an I64 with an I64, and an unsigned number with an unsigned number")); }
                let name = self.symbol(*id); return self.push(out, &name);
            }
            // I64 division rounds toward zero: Lean's `Int.tdiv` and `Int.tmod`, not `/` and `%`.
            Math::Div(a, b) | Math::Mod(a, b) if integer => {
                self.push(out, if matches!(value, Math::Div(..)) { "(Int.tdiv " } else { "(Int.tmod " })?;
                self.math(out, a, integer)?; self.push(out, " ")?; self.math(out, b, integer)?; return self.push(out, ")");
            }
            Math::Result | Math::Observe { .. } | Math::Field { .. } => return Err(error("function contracts ghost result was not bound to actual return")),
            Math::Add(a, b) => (" + ", a, b), Math::Sub(a, b) => (" - ", a, b), Math::Mul(a, b) => (" * ", a, b),
            Math::Div(a, b) => (" / ", a, b), Math::Mod(a, b) => (" % ", a, b),
        };
        self.push(out, "(")?; self.math(out, a, integer)?; self.push(out, symbol)?; self.math(out, b, integer)?; self.push(out, ")")
    }
    fn prop(&mut self, out: &mut String, value: &Prop) -> Result<()> {
        self.count()?;
        match value {
            Prop::Bool(b) => self.push(out, if *b { "True" } else { "False" }),
            Prop::Truth(m) => { self.push(out, "(")?; self.math(out, m, false)?; self.push(out, " = (1 : Nat))") }
            Prop::Compare(op, a, b) => {
                let integer = self.is_integer(a) || self.is_integer(b);
                self.push(out, "(")?; self.math(out, a, integer)?; self.push(out, " ")?; self.push(out, op)?; self.push(out, " ")?; self.math(out, b, integer)?; self.push(out, ")")
            }
            Prop::Not(inner) => { self.push(out, "(¬ ")?; self.prop(out, inner)?; self.push(out, ")") }
            Prop::Binary(op, a, b) => { self.push(out, "(")?; self.prop(out, a)?; self.push(out, " ")?; self.push(out, op)?; self.push(out, " ")?; self.prop(out, b)?; self.push(out, ")") }
        }
    }
    /// One sequent as a closed statement: the facts that share a name with the goal, directly or
    /// through other facts, then the goal. Leaving a fact out can only make the statement harder to prove.
    pub(super) fn sequent(&mut self, obligation: &Obligation) -> Result<(String, Option<String>)> {
        // A repeated hypothesis needs only that premise. Reflexive comparisons
        // and True need none. Emit a stronger statement for Lean to check.
        let hypotheses = match (&obligation.kind, &obligation.goal) {
            (Kind::Body, Prop::Bool(true)) => &[][..],
            (Kind::Body, Prop::Compare("=" | "≤" | "≥", left, right)) if left == right => &[],
            (Kind::Body, goal) => match obligation.hypotheses.iter().position(|fact| fact == goal) {
                Some(index) => &obligation.hypotheses[index..=index],
                None => &obligation.hypotheses,
            },
            _ => &obligation.hypotheses,
        };
        let mut names = BTreeSet::new();
        let mut found = Vec::new();
        obligation.goal.variables(&mut found);
        names.extend(found.drain(..));
        if hypotheses.len() == obligation.hypotheses.len() {
            names.extend(obligation.seeds.iter().copied());
        }
        let mut facts_by_variable = BTreeMap::<u32, Vec<usize>>::new();
        let mut selected = vec![false; hypotheses.len()];
        let mut variables = Vec::with_capacity(selected.len());
        for (index, fact) in hypotheses.iter().enumerate() {
            let mut used = Vec::new();
            fact.variables(&mut used);
            selected[index] = used.is_empty();
            for &name in &used {
                facts_by_variable.entry(name).or_default().push(index);
            }
            variables.push(used);
        }
        let mut pending: Vec<_> = names.iter().copied().collect();
        while let Some(name) = pending.pop() {
            for &index in facts_by_variable.get(&name).into_iter().flatten() {
                if std::mem::replace(&mut selected[index], true) {
                    continue;
                }
                for &dependency in &variables[index] {
                    if names.insert(dependency) {
                        pending.push(dependency);
                    }
                }
            }
        }
        let mut out = String::new();
        if !names.is_empty() {
            self.push(&mut out, "∀ ")?;
            for name in &names { let text = format!("({} : {}) ", self.symbol(*name), if self.signed.contains(name) { "Int" } else { "Nat" }); self.push(&mut out, &text)?; }
            self.push(&mut out, ", ")?;
        }
        let mut discriminants = BTreeMap::new();
        zero_discriminants(&obligation.goal, &mut discriminants);
        let mut emitted = std::collections::HashSet::new();
        for (index, fact) in hypotheses.iter().enumerate() {
            if selected[index] && emitted.insert(fact) {
                zero_discriminants(fact, &mut discriminants);
                self.prop(&mut out, fact)?;
                self.push(&mut out, " → ")?;
            }
        }
        self.prop(&mut out, &obligation.goal)?;
        let split = discriminants.into_iter()
            .filter(|(id, count)| *count >= 2 && !self.signed.contains(id))
            .map(|(id, count)| (count, self.symbol(id)))
            .max()
            .map(|(_, tag)| {
                let binders = names.iter().map(|id| self.symbol(*id)).collect::<Vec<_>>().join(" ");
                format!("  intro {binders}\n  intros\n  first\n  | with_reducible assumption\n  | solve | by_cases h : {tag} = 0 <;> simp_all\n  | witchy_inline_omega\n")
            });
        Ok((out, split))
    }
}

fn zero_discriminants(prop: &Prop, counts: &mut BTreeMap<u32, usize>) {
    match prop {
        Prop::Compare("=" | "≠", Math::Binding(id), Math::Literal(0)) => {
            *counts.entry(*id).or_default() += 1;
        }
        Prop::Binary(_, left, right) => {
            zero_discriminants(left, counts);
            zero_discriminants(right, counts);
        }
        Prop::Not(inner) => zero_discriminants(inner, counts),
        _ => {}
    }
}
