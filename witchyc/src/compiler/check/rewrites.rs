use super::*;

impl Checker<'_> {
    pub(super) fn array_receiver(&mut self, receiver: &a::Expr) -> Result<bool> {
        let mut visits = 0;
        let ty = self.infer_hint(receiver, None, 0, &mut visits)?;
        let ty = match ty { Some(Ty::Borrow { pointee, .. }) => Some(pointee.ty()), other => other };
        Ok(ty.is_none_or(|ty| self.layouts.get(ty).is_some_and(|a| matches!(a.kind, Kind::Array(..)))))
    }
    /// `value[at] = new` on anything but a fixed array is `value.index_set(at, new)`. `+=` and `-=`
    /// read the element first with `value[at]`, so the value and the position must run nothing.
    pub(super) fn index_assignment(&mut self, target: &a::Expr, operation: &str, value: &a::Expr) -> Result<Option<a::Expr>> {
        let a::Expression::Index(receiver, at) = &target.kind else { return Ok(None) };
        if self.array_receiver(receiver)? { return Ok(None); }
        fn quiet(e: &a::Expr) -> bool {
            match &e.kind {
                a::Expression::Name(_) | a::Expression::Integer(_) => true,
                a::Expression::Field(v, _) | a::Expression::Unary("*", v) => quiet(v),
                _ => false,
            }
        }
        let new = match operation {
            "=" => value.clone(),
            "+=" | "-=" => {
                if !quiet(receiver) || !quiet(at) {
                    return Err(Diagnostic::new("W0401", target.span.clone(), "`+=` and `-=` on `value[at]` read it first: the value and the position must be names, fields or literals"));
                }
                let span = target.span.start..value.span.end;
                a::Expr { kind: a::Expression::Binary(if operation == "+=" { "+" } else { "-" }, Box::new(target.clone()), Box::new(value.clone())), span }
            }
            _ => return Ok(None),
        };
        let callee = a::Expr { kind: a::Expression::Field(receiver.clone(), "index_set".into()), span: target.span.clone() };
        Ok(Some(a::Expr { kind: a::Expression::Call(Box::new(callee), vec![(**at).clone(), new]), span: target.span.start..value.span.end }))
    }
    /// `&v[i]` of a container that is no array or slice is its `cell` (`cell_mut` for `&mut`):
    /// a reference to the element, past the end a trap, as `v[i]` reads one.
    /// The method must be an element borrow (Astra audit: a `cell` returning a U64 made
    /// `&shelf[0]` a number): it takes the receiver by `&` (`&mut` for `cell_mut`), returns a
    /// reference of the same kind, and takes no other reference, so what it returns can only
    /// borrow from the receiver, which stays lent while the reference is used.
    /// `for item in g(arguments):` over a `gen fn` that can run in place, as the statements that
    /// run it (inline_generator.rs); None to step a generator as before. Not in a function with a
    /// proof (its obligations would change), and only where every name the generator's body uses
    /// is one this function may use.
    pub(super) fn inlined_generator(&self, index: Option<&str>, binding: &str, start: &a::Expr, invariants: &[a::Expr], body: &[a::Statement]) -> Option<(String, Vec<a::Statement>)> {
        if index.is_some() || !invariants.is_empty() || self.contracted || self.memory_proof || self.scalar_proof || self.record_proof.is_some() { return None; }
        let a::Expression::Call(callee, arguments) = &start.kind else { return None };
        let (key, given, method) = match &callee.kind {
            a::Expression::Name(g) if !self.scope.contains_key(g) => (g.clone(), arguments.clone(), false),
            a::Expression::Field(receiver, name) => {
                let held = self.hint(receiver)?;
                let (owner_ty, borrowed) = match held { Ty::Borrow { pointee, mutable: false } => (pointee.ty(), true), Ty::Borrow { .. } => return None, other => (other, false) };
                let owner = self.layouts.nominal(owner_ty)?;
                let receiver_arg = if borrowed { (**receiver).clone() } else { a::Expr { kind: a::Expression::Unary("&", receiver.clone()), span: receiver.span.clone() } };
                (crate::compiler::methods::name(owner, name), std::iter::once(receiver_arg).chain(arguments.iter().cloned()).collect(), true)
            }
            _ => return None,
        };
        // The generator the call means: of the file the name resolves to, as a call resolves it
        // (review: keyed by name alone, another file's private `gen fn` of that name was run).
        let module = if method { self.signatures.get(&key)?.module } else { self.signatures.get(&self.function_named(&key))?.module };
        let generator = self.inline_generators.get(&key)?.iter().find(|g| g.module == module)?;
        // Not inside its own inlining: a generator that loops over itself steps (review).
        let marked = format!("{key}@{module}");
        if self.inlining.contains(&marked) { return None; }
        // A method's receiver: a shared reference, or `Text`, which is passed as it is.
        let given = if method && let Some((_, a::Type::Named(t, _))) = generator.parameters.first() && t == "Text" {
            let a::Expression::Call(callee, _) = &start.kind else { return None };
            let a::Expression::Field(receiver, _) = &callee.kind else { return None };
            if self.hint(receiver).and_then(|ty| self.layouts.nominal(ty)) != Some("Text") { return None; }
            std::iter::once((**receiver).clone()).chain(given.into_iter().skip(1)).collect()
        } else {
            if method && !matches!(generator.parameters.first(), Some((_, a::Type::Borrow { mutable: false, .. }))) { return None; }
            given
        };
        let same = generator.module == self.module;
        // From another file, only its public functions and `Ok`/`Err`: a type has rules of its own
        // there (review: a sealed type's construction moved out of its file).
        // And the function a name means here must be the one it means in the generator's file
        // (eighth review: the generator's `bump(i)` run in place called this file's private `bump`).
        let free = |n: &str| n != binding && !self.scope.contains_key(n) && (same || matches!(n, "Ok" | "Err")
            || (self.function_named(n) == self.function_named_from(n, generator.module)
                && self.signatures.get(&self.function_named(n)).is_some_and(|sig| sig.public || sig.module == self.module)));
        // No signature: one the compiler gives (`U8.to_u64`, `Text.byte_at`), private to no file; the
        // generator itself was checked, so the name means something.
        // The owner type too must be one this file may name (review: `Hidden.seven(...)` of a type
        // private to the generator's file was run in place where `Hidden` cannot be named).
        // A module's name means the file each file imports by it: run in place only where this
        // file and the generator's import the same file by it, or neither imports one (ninth
        // review: `util.pick` of the generator ran as this file's own `util`).
        let imported = |file: u32, owner: &str| self.imports.iter().find(|(from, name, _)| *from == file && name == owner).map(|(_, _, to)| *to);
        let static_call = |owner: &str, m: &str| !self.scope.contains_key(owner) && imported(self.module, owner) == imported(generator.module, owner)
            && (same || (!self.private.get(owner).is_some_and(|file| *file != self.module)
            && self.signatures.get(&crate::compiler::methods::name(owner, m)).is_none_or(|sig| sig.public || sig.module == self.module)));
        crate::compiler::inline_generator::inline(generator, given, binding, body, &start.span, &crate::compiler::inline_generator::Allowed { free: &free, static_call: &static_call, value_calls: same }).map(|s| (marked, s))
    }
    /// A local holding a shared reference to a Copy value.
    pub(super) fn copied_through(&self, e: &a::Expr) -> bool {
        matches!(&e.kind, a::Expression::Name(n) if self.scope.get(n).is_some_and(|local| matches!(local.ty, Ty::Borrow { mutable: false, pointee } if !matches!(pointee.ty(), Ty::Slice(_)) && self.layouts.properties(pointee.ty()).copy)))
    }
    pub(super) fn celled(&self, expr: &a::Expr) -> Result<Option<a::Expr>> {
        let a::Expression::Unary(lend @ ("&" | "&mut"), inner) = &expr.kind else { return Ok(None) };
        let a::Expression::Index(receiver, at) = &inner.kind else { return Ok(None) };
        let given = self.hint(receiver);
        // A slice lends its element with its own `cell(i)`: `&s[i]` of a shared slice, `&mut s[i]`
        // of a unique one (as the slice is, it lends; the other pairing stays refused).
        if let Some(Ty::Borrow { pointee: Pointee::Slice(_), mutable }) = given {
            if !mutable && *lend == "&mut" { return Ok(None); }
            if mutable && *lend == "&" {
                return Err(error("`&s[i]` of a `&mut [T]`: its element is lent uniquely, as `&mut s[i]`; for a copy, `s[i]`"));
            }
            let callee = a::Expr { kind: a::Expression::Field(receiver.clone(), "cell".into()), span: inner.span.clone() };
            return Ok(Some(a::Expr { kind: a::Expression::Call(Box::new(callee), vec![(**at).clone()]), span: expr.span.clone() }));
        }
        let Some(held) = given.map(|ty| match ty { Ty::Borrow { pointee, .. } => pointee.ty(), other => other }) else { return Ok(None) };
        if self.layouts.get(held).is_some_and(|layout| matches!(layout.kind, Kind::Array(..))) { return Ok(None); }
        let mutable = *lend == "&mut";
        let method = if mutable { "cell_mut" } else { "cell" };
        let Some(owner) = self.layouts.nominal(held) else { return Ok(None) };
        let Some(sig) = self.signatures.get(&crate::compiler::methods::name(owner, method)) else { return Ok(None) };
        let borrows = |ty: &Ty| matches!(ty, Ty::Borrow { mutable: m, .. } if *m == mutable);
        let shaped = sig.params.len() == 2 && borrows(&sig.params[0]) && !matches!(sig.params[1], Ty::Borrow { .. }) && borrows(&sig.result);
        if !shaped {
            let spaced = if mutable { "&mut " } else { "&" };
            return Err(error(format!("`{spaced}v[i]` lends an element through the type's `{method}`, which must take `{spaced}self` and an index and return a `{spaced}` reference to the element; this `{method}` does not, so call it by name")));
        }
        let callee = a::Expr { kind: a::Expression::Field(receiver.clone(), method.into()), span: inner.span.clone() };
        Ok(Some(a::Expr { kind: a::Expression::Call(Box::new(callee), vec![(**at).clone()]), span: expr.span.clone() }))
    }
    /// `i < s.len() && ... s[i] ...`: the right side runs only once the left held, and an
    /// expression assigns nothing, so on a shared slice of Copy elements each read `s[i]` there is
    /// below the length and is read untested (iteration.rs `COUNTED`). An expression that lends
    /// `i` uniquely anywhere is left as written: it may change `i` after the test. `None` when
    /// there is nothing to mark.
    pub(super) fn guarded_reads(&self, expr: &a::Expr) -> Option<a::Expr> {
        let a::Expression::Binary("&&", left, right) = &expr.kind else { return None };
        let found = self.slice_guards(left);
        if found.is_empty() { return None; }
        let mut right = (**right).clone();
        let mut changed = false;
        for (counter, slice) in &found {
            if Self::lends_counter(expr, counter) { continue; }
            right.visit_mut(&mut |e| match &e.kind {
                a::Expression::Unary("&" | "&mut", _) => true,
                a::Expression::Index(receiver, at) if matches!(&receiver.kind, a::Expression::Name(n) if n == slice)
                    && matches!(&at.kind, a::Expression::Name(n) if n == counter) => {
                    let callee = a::Expr { kind: a::Expression::Field(receiver.clone(), crate::compiler::iteration::COUNTED.into()), span: e.span.clone() };
                    e.kind = a::Expression::Call(Box::new(callee), vec![(**at).clone()]);
                    changed = true;
                    true
                }
                _ => false,
            });
        }
        changed.then(|| a::Expr { kind: a::Expression::Binary("&&", left.clone(), Box::new(right)), span: expr.span.clone() })
    }
    /// Whether `e` takes `&mut counter` or calls a method on it anywhere: it may change it.
    pub(super) fn lends_counter(e: &a::Expr, counter: &str) -> bool {
        let mut lent = false;
        // A method called on it may take it as `&mut self` (guarded.rs refuses the same).
        e.clone().visit_mut(&mut |e| {
            match &e.kind {
                a::Expression::Unary("&mut", inner) if matches!(&inner.kind, a::Expression::Name(n) if n == counter) => lent = true,
                a::Expression::Call(callee, _) if matches!(&callee.kind, a::Expression::Field(receiver, _) if matches!(&receiver.kind, a::Expression::Name(n) if n == counter)) => lent = true,
                _ => {}
            }
            false
        });
        lent
    }
    /// `i < s.len()` (or `s.len() > i`) among the `&&` conjuncts of `e`, with `i` a U64 local and
    /// `s` a shared slice of Copy elements in scope: the pairs (counter, slice).
    pub(super) fn slice_guards(&self, e: &a::Expr) -> Vec<(String, String)> {
        fn guards(e: &a::Expr, out: &mut Vec<(String, String)>) {
            let len_of = |e: &a::Expr| match &e.kind {
                a::Expression::Call(callee, arguments) if arguments.is_empty() => match &callee.kind {
                    a::Expression::Field(receiver, method) if method == "len" => match &receiver.kind { a::Expression::Name(n) => Some(n.clone()), _ => None },
                    _ => None,
                },
                _ => None,
            };
            match &e.kind {
                a::Expression::Binary("&&", l, r) => { guards(l, out); guards(r, out); }
                a::Expression::Binary("<", i, s) if let (a::Expression::Name(i), Some(s)) = (&i.kind, len_of(s)) => out.push((i.clone(), s)),
                a::Expression::Binary(">", s, i) if let (a::Expression::Name(i), Some(s)) = (&i.kind, len_of(s)) => out.push((i.clone(), s)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        guards(e, &mut found);
        found.retain(|(i, slice)| self.scope.get(i).is_some_and(|l| l.ty == Ty::U64)
            && matches!(self.scope.get(slice).map(|l| l.ty), Some(Ty::Borrow { pointee: Pointee::Slice(id), mutable: false }) if self.layouts.properties(self.layouts.slice_element(id)).copy));
        found
    }
}
