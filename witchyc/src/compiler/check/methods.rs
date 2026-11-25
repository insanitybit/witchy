use super::diagnostics::read_only;
use super::*;

impl Checker<'_> {
    /// What `expr` is written out as, when it is a method call or an operator that is one. An
    /// inference of its own when nothing else is open (`infer_hint`): what it reads of one
    /// operand it need not read again for the call it writes.
    pub(super) fn method_call(&mut self, expr: &a::Expr) -> Result<Option<a::Expr>> {
        if self.hinting == 0 { self.hint_memo.clear(); }
        self.hinting += 1;
        let written = self.method_call_once(expr);
        self.hinting -= 1;
        written
    }
    /// `v[i]` in `for i in a..v.len():` (iteration.rs `COUNTED`) on a container `v` that is not a
    /// slice: the method `m` such that `v[i]` may be read as `v.m()[i]` with no test, or None,
    /// and `v[i]` is the container's `index` as before. All of these must hold, each read from
    /// what is written (`Shape`), never from a name of a type:
    ///
    /// - `v` is a name bound once (a parameter or a `let`), of the container's type or a `&` to
    ///   it: in the loop it is the value whose `len()` ended the range, unchanged, since a
    ///   shared reference or an immutable binding lets nothing write it;
    /// - its `index(c, i)` is `return c.m()[i]` and nothing else: `v[i]` *is* the element `i`
    ///   of the slice `v.m()` returns, read with a test against that slice's length;
    /// - its `len(c)` is `return c.p` for a path `p` of fields and methods called with no
    ///   argument (`(*c).length`, `c.items.len()`), the value that ended the range;
    /// - `m` takes `&` to the container, requires nothing, returns a `&[T]` (read as copies),
    ///   may be called from here, and has an `ensures result.len() == c.p` (or `== c.len()`),
    ///   proved with `m` like any other `ensures`: a clause reads a method only as the field
    ///   its one-line body returns, so `c.p` there is the number `len` returned.
    ///
    /// So at the read, `i` is below the end the range read, which is `v`'s `f` then and now,
    /// which is the length of the slice `v.m()` gives: the slice read's test cannot fail, and
    /// the read is the one `index` makes, written in place. A user type named like the
    /// library's `Vec` with another `index` keeps its own (`slices/counted-renamed.wy`).
    ///
    /// `hoisted`: `v` is a `&` (so making the slice lends nothing new) and `m`'s body is one
    /// `return` of fields, literals and `memory.` operations only, under a proof, or forwards to
    /// such a method of a field (`straight`): it runs no loop and no other call, so it ends;
    /// under its proof it cannot trap; given only `&` it changes nothing; and `v` is the same
    /// value at every read. So `v.m()` made once, after the
    /// range's start and end, is the slice each read would make (`COUNTED_IN`).
    fn counted_container(&self, receiver: &a::Expr) -> Option<(String, bool)> {
        let a::Expression::Name(name) = &receiver.kind else { return None };
        let local = self.scope.get(name)?;
        if local.mutable { return None; }
        let owner_ty = match local.ty { Ty::Borrow { pointee, mutable: false } => pointee.ty(), Ty::Borrow { .. } => return None, other => other };
        let owner = self.layouts.nominal(owner_ty)?;
        let method = |name: &str| self.signatures.get(&crate::compiler::methods::name(owner, name));
        let shared = |sig: &Signature| matches!(sig.params.first(), Some(Ty::Borrow { mutable: false, pointee }) if self.layouts.nominal(pointee.ty()) == Some(owner));
        let (index, len) = (method("index")?, method("len")?);
        let slice = index.shape.slice_index.clone()?;
        let field = len.shape.projection.clone()?;
        let made = method(&slice)?;
        if !shared(index) || !shared(len) || !shared(made) || made.params.len() != 1 || made.requires { return None; }
        if !made.public && !self.may_reach(made.module, Some(owner)) { return None; }
        let Ty::Borrow { mutable: false, pointee } = made.result else { return None };
        // Its elements are read as copies, as `index` returns them: the slice's own counted read
        // asks that of the element type at this call.
        let Ty::Slice(_) = pointee.ty() else { return None };
        let measured = made.shape.result_length.as_ref()?;
        let hoisted = matches!(local.ty, Ty::Borrow { mutable: false, .. }) && self.straight(owner_ty, &slice, 0);
        (*measured == field || measured.as_slice() == ["len()"]).then_some((slice, hoisted))
    }
    /// Whether the method of this name on `owner` (taking `&` to it and nothing else) runs to its
    /// end without a trap, a write or a loop, on any value: its body is `Shape::straight` under
    /// its own proof, or forwards (`Shape::forwards`) to such a method of one of its fields.
    fn straight(&self, owner: Ty, method: &str, depth: u32) -> bool {
        if depth > 4 { return false; }
        let Some(name) = self.layouts.nominal(owner) else { return false };
        let Some(sig) = self.signatures.get(&crate::compiler::methods::name(name, method)) else { return false };
        if sig.requires || !matches!(sig.params.as_slice(), [Ty::Borrow { mutable: false, .. }]) { return false; }
        if sig.shape.straight { return sig.contracted; }
        let Some((fields, next)) = &sig.shape.forwards else { return false };
        let mut ty = owner;
        for field in fields {
            let Some(record) = self.layouts.get(ty) else { return false };
            if !matches!(record.kind, Kind::Record(_)) { return false; }
            let Some(found) = record.fields.iter().find(|f| f.name == *field) else { return false };
            ty = found.ty;
        }
        self.straight(ty, next, depth + 1)
    }
    fn method_call_once(&mut self, expr: &a::Expr) -> Result<Option<a::Expr>> {
        if let a::Expression::Call(callee, arguments) = &expr.kind
            && let a::Expression::Field(receiver, method) = &callee.kind
            && matches!(&receiver.kind, a::Expression::Name(name) if name == "str")
            && method == "from_utf8_bytes"
            && !self.scope.contains_key("str")
        {
            let function = a::Expr {
                kind: a::Expression::Name("#str_from_bytes".into()),
                span: callee.span.clone(),
            };
            return Ok(Some(a::Expr {
                kind: a::Expression::Call(Box::new(function), arguments.clone()),
                span: expr.span.clone(),
            }));
        }
        if let a::Expression::Call(callee, arguments) = &expr.kind
            && matches!(&callee.kind, a::Expression::Name(name) if name == "str_from_bytes")
            && !self.scope.contains_key("str_from_bytes")
            && !self.signatures.contains_key("str_from_bytes")
        {
            let function = a::Expr {
                kind: a::Expression::Name("#str_from_bytes".into()),
                span: callee.span.clone(),
            };
            return Ok(Some(a::Expr {
                kind: a::Expression::Call(Box::new(function), arguments.clone()),
                span: expr.span.clone(),
            }));
        }
        // `v.#counted_view()` (iteration.rs `COUNTED_IN`): the slice `v.m()` when the counted read
        // may use one made before the loop (`hoisted`), else `0`, which runs nothing.
        if let a::Expression::Call(callee, arguments) = &expr.kind && arguments.is_empty()
            && let a::Expression::Field(receiver, method) = &callee.kind && method == crate::compiler::iteration::COUNTED_VIEW {
            let view = self.counted_container(receiver).filter(|(_, hoisted)| *hoisted).map(|(slice, _)|
                a::Expr { kind: a::Expression::Call(Box::new(a::Expr { kind: a::Expression::Field(receiver.clone(), slice), span: callee.span.clone() }), Vec::new()), span: expr.span.clone() });
            // No unit local: a number nothing reads.
            let view = view.unwrap_or(a::Expr { kind: a::Expression::Integer(0), span: expr.span.clone() });
            return Ok(Some(self.method_call(&view)?.unwrap_or(view)));
        }
        // `v.#counted_in(i, view)`: `view[i]`, read with no test, when the loop made `view` a slice
        // of `v`'s elements; else `v.#counted(i)`.
        if let a::Expression::Call(callee, arguments) = &expr.kind && let [at, view] = arguments.as_slice()
            && let a::Expression::Field(receiver, method) = &callee.kind && method == crate::compiler::iteration::COUNTED_IN {
            let sliced = match self.hint(view) { Some(Ty::Borrow { pointee: Pointee::Slice(id), mutable: false }) => self.layouts.properties(self.layouts.slice_element(id)).copy, _ => false };
            let target = if sliced { view.clone() } else { (**receiver).clone() };
            let read = a::Expr { kind: a::Expression::Call(Box::new(a::Expr { kind: a::Expression::Field(Box::new(target), crate::compiler::iteration::COUNTED.into()), span: callee.span.clone() }), vec![at.clone()]), span: expr.span.clone() };
            return Ok(Some(self.method_call(&read)?.unwrap_or(read)));
        }
        // `s.#counted(i)` (iteration.rs `COUNTED`) is read untested on a shared slice of Copy
        // elements; on anything else it is `s[i]` (a Vec could shrink; a `&mut` slice is written
        // through in the same statements).
        if let a::Expression::Call(callee, arguments) = &expr.kind && let [at] = arguments.as_slice()
            && let a::Expression::Field(receiver, method) = &callee.kind && method == crate::compiler::iteration::COUNTED {
            let shared = match self.hint(receiver).or_else(|| self.call_hint(receiver)) { Some(Ty::Borrow { pointee: Pointee::Slice(id), mutable: false }) => Some(id), _ => None };
            if shared.is_some_and(|id| self.layouts.properties(self.layouts.slice_element(id)).copy) { return Ok(None); }
            // A container whose `v[i]` is, as written, an element of a slice one of its methods
            // returns, a slice that method's proved `ensures` says is as long as `v.len()`:
            // `v[i]` is that slice's element, read untested (`counted_container`).
            if let Some((slice, _)) = self.counted_container(receiver) {
                let call = a::Expr { kind: a::Expression::Call(Box::new(a::Expr { kind: a::Expression::Field(receiver.clone(), slice), span: receiver.span.clone() }), Vec::new()), span: receiver.span.clone() };
                let read = a::Expr { kind: a::Expression::Call(Box::new(a::Expr { kind: a::Expression::Field(Box::new(call), crate::compiler::iteration::COUNTED.into()), span: callee.span.clone() }), vec![at.clone()]), span: expr.span.clone() };
                return Ok(Some(self.method_call(&read)?.unwrap_or(read)));
            }
            let index = a::Expr { kind: a::Expression::Index(receiver.clone(), Box::new(at.clone())), span: expr.span.clone() };
            return Ok(Some(self.method_call(&index)?.unwrap_or(index)));
        }
        // `map.get(&keys[at])`: an argument `&v[i]` of a container (not an array or a slice) is
        // `v.cell(i)` before the call's type parameters are inferred, as it is anywhere else; read
        // as an element first, it asked for a copy of a `String` (found writing a lookup benchmark).
        if let a::Expression::Call(callee, arguments) = &expr.kind && matches!(callee.kind, a::Expression::Field(..)) {
            let container = |this: &mut Self, arg: &a::Expr| matches!(&arg.kind, a::Expression::Unary("&" | "&mut", inner)
                if matches!(&inner.kind, a::Expression::Index(v, _) if !matches!(this.hint(v), Some(Ty::Borrow { pointee: Pointee::Slice(_), .. }))));
            if arguments.iter().any(|arg| container(self, arg)) {
                let mut changed = false;
                let mut rewritten = Vec::with_capacity(arguments.len());
                for arg in arguments {
                    match container(self, arg).then(|| self.celled(arg)).transpose()?.flatten() {
                        Some(call) => { changed = true; rewritten.push(call); }
                        None => rewritten.push(arg.clone()),
                    }
                }
                if changed {
                    let call = a::Expr { kind: a::Expression::Call(callee.clone(), rewritten), span: expr.span.clone() };
                    return Ok(Some(self.method_call(&call)?.unwrap_or(call)));
                }
            }
        }
        // `value[at]` on anything but a fixed array is `value.index(at)`: the type's own method
        // says what reading at a position means, and what happens when there is nothing there.
        if let a::Expression::Index(receiver, at) = &expr.kind {
            if self.array_receiver(receiver)? { return Ok(None); }
            let callee = a::Expr { kind: a::Expression::Field(receiver.clone(), "index".into()), span: expr.span.clone() };
            let call = a::Expr { kind: a::Expression::Call(Box::new(callee), vec![(**at).clone()]), span: expr.span.clone() };
            return Ok(Some(self.method_call(&call)?.unwrap_or(call)));
        }
        // `points[i].x`, `v[i].a.b`: the fields of what `index` gives, when the indexed value is
        // not a fixed array (whose element is a place, read in place).
        if let a::Expression::Field(base, name) = &expr.kind && matches!(base.kind, a::Expression::Field(..) | a::Expression::Index(..)) {
            let mut root = &**base;
            while let a::Expression::Field(inner, _) = &root.kind { root = inner; }
            if let a::Expression::Index(receiver, _) = &root.kind && !self.array_receiver(receiver)? {
                let inner = self.method_call(base)?.unwrap_or_else(|| (**base).clone());
                return Ok(Some(a::Expr { kind: a::Expression::Field(Box::new(inner), name.clone()), span: expr.span.clone() }));
            }
        }
        // `a == b` on anything but numbers and Bools is `a.eq(&b)`, from the type's `Eq`; `a != b`
        // is `!a.eq(&b)`. An operand that is a reference is passed as it is.
        if let a::Expression::Binary(op @ ("==" | "!="), left, right) = &expr.kind {
            let mut visits = 0;
            let left_ty = self.infer_hint(left, None, 0, &mut visits)?;
            let right_ty = self.infer_hint(right, None, 0, &mut visits)?;
            let plain = |ty: Option<Ty>| match ty { Some(Ty::Borrow { pointee, .. }) => Some(pointee.ty()), other => other }
                .is_none_or(|ty| matches!(ty, Ty::Unsigned(_) | Ty::Bool | Ty::I64 | Ty::F64 | Ty::Unit));
            if plain(left_ty) && plain(right_ty) { return Ok(None); }
            // A String beside a Text literal, either way round: `text.equivalent(&string)`, the
            // library's `Equivalent<String> for Text` (a String has no `eq` taking a Text).
            let named_as = |ty: Option<Ty>, name: &str| match ty { Some(Ty::Borrow { pointee, .. }) => Some(pointee.ty()), other => other }
                .is_some_and(|ty| self.layouts.record_name(ty) == Some(name) || (name == "str" && self.layouts.nominal(ty) == Some("str")));
            // A Text beside borrowed UTF-8: compared as two `&str`.
            let as_str = |e: &a::Expr| a::Expr { kind: a::Expression::Call(Box::new(a::Expr { kind: a::Expression::Field(Box::new(e.clone()), "as_str".into()), span: e.span.clone() }), vec![]), span: e.span.clone() };
            let deref = |e: &a::Expr, ty: Option<Ty>| if matches!(ty, Some(Ty::Borrow { .. })) { a::Expr { kind: a::Expression::Unary("*", Box::new(e.clone())), span: e.span.clone() } } else { e.clone() };
            let mixed = if named_as(left_ty, "Text") && named_as(right_ty, "str") { Some((as_str(&deref(left, left_ty)), (**right).clone())) }
                else if named_as(left_ty, "str") && named_as(right_ty, "Text") { Some(((**left).clone(), as_str(&deref(right, right_ty)))) } else { None };
            if let Some((l, r)) = mixed {
                let compared = a::Expr { kind: a::Expression::Binary(op, Box::new(l), Box::new(r)), span: expr.span.clone() };
                return Ok(Some(self.method_call(&compared)?.unwrap_or(compared)));
            }
            let textual = |ty: Option<Ty>| named_as(ty, "Text") || named_as(ty, "str");
            let text_and_string = if textual(left_ty) && named_as(right_ty, "String") { Some((left, right, right_ty)) }
                else if named_as(left_ty, "String") && textual(right_ty) { Some((right, left, left_ty)) } else { None };
            if let Some((text, string, string_ty)) = text_and_string {
                let argument = if matches!(string_ty, Some(Ty::Borrow { .. })) { (**string).clone() }
                    else { a::Expr { kind: a::Expression::Unary("&", string.clone()), span: string.span.clone() } };
                let callee = a::Expr { kind: a::Expression::Field(text.clone(), "equivalent".into()), span: expr.span.clone() };
                let call = a::Expr { kind: a::Expression::Call(Box::new(callee), vec![argument]), span: expr.span.clone() };
                let call = self.method_call(&call)?.unwrap_or(call);
                return Ok(Some(if *op == "==" { call } else { a::Expr { kind: a::Expression::Unary("!", Box::new(call)), span: expr.span.clone() } }));
            }
            // `Dot == a`: a variant written bare on the left would read as `Dot.eq`, a type's method
            // (review); compared the other way round, both being names that do nothing when read.
            let bare_variant = |e: &a::Expr| matches!(&e.kind, a::Expression::Name(n) if !self.scope.contains_key(n) && self.layouts.is_variant(n));
            // Both bare (`Dot == Dot`): nothing says the type but the variant, so the call names it.
            if bare_variant(left) && bare_variant(right) && let a::Expression::Name(n) = &left.kind && let Some(sum) = self.layouts.plain_sum_of_variant(n) {
                let reference = |e: &a::Expr| a::Expr { kind: a::Expression::Unary("&", Box::new(e.clone())), span: e.span.clone() };
                let callee = a::Expr { kind: a::Expression::Field(Box::new(a::Expr { kind: a::Expression::Name(sum), span: left.span.clone() }), "eq".into()), span: expr.span.clone() };
                let call = a::Expr { kind: a::Expression::Call(Box::new(callee), vec![reference(left), reference(right)]), span: expr.span.clone() };
                let call = self.method_call(&call)?.unwrap_or(call);
                return Ok(Some(if *op == "==" { call } else { a::Expr { kind: a::Expression::Unary("!", Box::new(call)), span: expr.span.clone() } }));
            }
            // Reading a bare variant does nothing, so which side runs first does not matter; nor does
            // reading a local, which says the type a left side with none of its own lacks
            // (`Just(3) == m`, a generic sum's value built with nothing to say its `T`).
            let swap = (bare_variant(left) && !bare_variant(right))
                || (left_ty.is_none() && right_ty.is_some() && matches!(&right.kind, a::Expression::Name(n) if self.scope.contains_key(n)));
            // The right side says the type: `Shape.eq(&left, &right)`, the operands in the order
            // written (a hand-written `Eq` need not be symmetric). Compared the other way round only
            // when the type's name does not find it.
            let typed = match right_ty { Some(Ty::Borrow { pointee, .. }) => Some(pointee.ty()), other => other };
            if swap && let Some(name) = typed.and_then(|t| self.layouts.declared_name(t)) {
                let reference = |e: &a::Expr, ty: Option<Ty>| if matches!(ty, Some(Ty::Borrow { .. })) { e.clone() } else { a::Expr { kind: a::Expression::Unary("&", Box::new(e.clone())), span: e.span.clone() } };
                let callee = a::Expr { kind: a::Expression::Field(Box::new(a::Expr { kind: a::Expression::Name(name), span: left.span.clone() }), "eq".into()), span: expr.span.clone() };
                let call = a::Expr { kind: a::Expression::Call(Box::new(callee), vec![reference(left, None), reference(right, right_ty)]), span: expr.span.clone() };
                let call = self.method_call(&call)?.unwrap_or(call);
                return Ok(Some(if *op == "==" { call } else { a::Expr { kind: a::Expression::Unary("!", Box::new(call)), span: expr.span.clone() } }));
            }
            let (left, right, right_ty) = if swap { (right, left, left_ty) } else { (left, right, right_ty) };
            let argument = if matches!(right_ty, Some(Ty::Borrow { .. })) { (**right).clone() }
                else { a::Expr { kind: a::Expression::Unary("&", right.clone()), span: right.span.clone() } };
            let callee = a::Expr { kind: a::Expression::Field(left.clone(), "eq".into()), span: expr.span.clone() };
            let call = a::Expr { kind: a::Expression::Call(Box::new(callee), vec![argument]), span: expr.span.clone() };
            let call = self.method_call(&call)?.unwrap_or(call);
            return Ok(Some(if *op == "==" { call } else { a::Expr { kind: a::Expression::Unary("!", Box::new(call)), span: expr.span.clone() } }));
        }
        // `a + b`, `a - b`, `a * b` on values that are not numbers: `a.add(b)` and so on, from the
        // type's `Add`, `Sub`, `Mul` (library/wasm-ops.wy). `a < b` is `a.before(&b)`, `a > b`
        // is `b.before(&a)`, `a <= b` is `!b.before(&a)` and `a >= b` is `!a.before(&b)`, from
        // `Before`. Numbers keep their own operators; a reference is read through first.
        if let a::Expression::Binary(op @ ("+" | "-" | "*" | "<" | "<=" | ">" | ">="), left, right) = &expr.kind {
            let mut visits = 0;
            let left_ty = self.infer_hint(left, None, 0, &mut visits)?;
            let right_ty = self.infer_hint(right, None, 0, &mut visits)?;
            let own = |ty: Option<Ty>| ty.is_some_and(|ty| matches!(ty, Ty::Aggregate(_) | Ty::Sum(_)));
            if !own(left_ty) && !own(right_ty) {
                if matches!(*op, "+" | "-" | "*") { self.operands = Some((expr as *const a::Expr, left_ty, right_ty)); }
                return Ok(None);
            }
            // A number beside a program's type (`30s + 5`): the trait takes two of the type.
            let number = |ty: Option<Ty>, e: &a::Expr| matches!(e.kind, a::Expression::Integer(_) | a::Expression::Float(_)) || ty.is_some_and(|ty| matches!(ty, Ty::Unsigned(_) | Ty::I64 | Ty::F64));
            // `250ms * 4`: a value of the type scaled by a number, through `Scale`.
            if *op == "*" && own(left_ty) && number(right_ty, right) {
                let span = expr.span.clone();
                let callee = a::Expr { kind: a::Expression::Field(Box::new(a::Expr { kind: a::Expression::Name("Scale".into()), span: span.clone() }), "scale".into()), span: span.clone() };
                let call = a::Expr { kind: a::Expression::Call(Box::new(callee), vec![(**left).clone(), (**right).clone()]), span };
                self.operator = true;
                let written = self.method_call(&call);
                self.operator = false;
                return Ok(Some(written?.unwrap_or(call)));
            }
            // A number beside a `Result` (`total - I64.from(n)`): the number is inside it.
            for (ty, side, other, operand) in [(left_ty, "left", right_ty, left), (right_ty, "right", left_ty, right)] {
                if let Some(ty) = ty && number(other, if side == "left" { right } else { left }) && self.layouts.result_sum(ty).is_some() {
                    let shown = self.layouts.type_name(ty).unwrap_or_else(|_| "Result".into());
                    return Err(Diagnostic::new("W0401", operand.span.clone(), format!("the {side} operand of `{op}` is a `{shown}`, not a number: take the value out first, with `?` where the function returns a `Result` with the same error type (`let n = ...?`), or with `match`")));
                }
            }
            if number(left_ty, left) || number(right_ty, right) {
                return Err(Diagnostic::new("W0401", expr.span.clone(), "an operator on a program's type takes two values of that type, not a number: write the number as one (`5ms`, `Money(cents: 5)`), or scale with `value * n`"));
            }
            let span = expr.span.clone();
            // Through the trait by name (`Add.add(a, b)`), so only the trait's method, with the
            // trait's signature, is called (red-team review: any method named `add`, even one
            // taking `&mut self`, was), and with the operands in the order written.
            let named = |n: &str| a::Expr { kind: a::Expression::Name(n.into()), span: span.clone() };
            let call = |callee: a::Expr, arguments: Vec<a::Expr>| a::Expr { kind: a::Expression::Call(Box::new(callee), arguments), span: span.clone() };
            let through = |t: &str, m: &str| a::Expr { kind: a::Expression::Field(Box::new(named(t)), m.into()), span: span.clone() };
            let lend = |e: &a::Expr| a::Expr { kind: a::Expression::Unary("&", Box::new(e.clone())), span: e.span.clone() };
            let not = |e: a::Expr| a::Expr { kind: a::Expression::Unary("!", Box::new(e)), span: span.clone() };
            // `>` and `<=` go through the standard library's `sort_after`, named as its file's
            // (`name@file`, which no source spells): the operator's, not a name this file wrote.
            // Whichever files declare `sort_after`, the operator's is the standard library's
            // (review: a package's own `sort_after` changed what `>` meant).
            let after = std::iter::once("sort_after").chain(self.others("sort_after").map(String::as_str))
                .find_map(|key| self.signatures.get(key).filter(|sig| sig.public && (self.reach.packages.is_empty() || self.reach.standard(sig.module))).map(|sig| format!("sort_after@{}", sig.module)))
                .unwrap_or_else(|| "sort_after@".to_string());
            let call = match *op {
                "+" => call(through("Add", "add"), vec![(**left).clone(), (**right).clone()]),
                "-" => call(through("Sub", "sub"), vec![(**left).clone(), (**right).clone()]),
                "*" => call(through("Mul", "mul"), vec![(**left).clone(), (**right).clone()]),
                "<" | ">=" => call(through("Before", "before"), vec![lend(left), lend(right)]),
                _ => call(named(&after), vec![lend(left), lend(right)]),
            };
            // The trait's call is written out here, at once, so its name is read as the operator's.
            self.operator = !matches!(*op, ">" | "<=");
            let written = self.method_call(&call);
            self.operator = false;
            let call = written?.unwrap_or(call);
            return Ok(Some(if matches!(*op, ">=" | "<=") { not(call) } else { call }));
        }
        let a::Expression::Call(callee, args) = &expr.kind else { return Ok(None) };
        // `add10(5)` where `add10` is a local: a value is called through its `call` method, which
        // a closure has (docs/wasm-closures.md), including one a function returned.
        if let a::Expression::Name(local) = &callee.kind && self.scope.contains_key(local) {
            // A closure that took over an owner has `call_once` alone: called once, consumed.
            let record = self.layouts.record_name(self.scope[local].ty).map(str::to_string);
            let once = record.as_ref().is_some_and(|r| self.signatures.contains_key(&format!("{r}.call_once")) && !self.signatures.contains_key(&format!("{r}.call")));
            let callee = a::Expr { kind: a::Expression::Field(callee.clone(), if once { "call_once" } else { "call" }.into()), span: callee.span.clone() };
            let call = a::Expr { kind: a::Expression::Call(Box::new(callee), args.clone()), span: expr.span.clone() };
            return Ok(Some(self.method_call(&call)?.unwrap_or(call)));
        }
        let a::Expression::Field(receiver, method) = &callee.kind else { return Ok(None) };
        // `Right.Io(7)` builds a variant; it is no call.
        if self.constructor(callee).is_some() { return Ok(None); }
        let call = |function: String, first: Option<a::Expr>| {
            let callee = a::Expr { kind: a::Expression::Name(function), span: callee.span.clone() };
            a::Expr { kind: a::Expression::Call(Box::new(callee), first.into_iter().chain(args.iter().cloned()).collect()), span: expr.span.clone() }
        };
        if let a::Expression::Name(owner) = &receiver.kind {
            // `shapes.area(x)`: the function `area` of the file this one imports as `shapes`.
            // A local of that name comes first.
            if !self.scope.contains_key(owner) && let Some(file) = self.imports.iter().find(|(from, name, _)| *from == self.module && name == owner).map(|(_, _, file)| *file) {
                let own = format!("{method}@{file}");
                let function = if self.signatures.contains_key(&own) { own }
                    else if self.signatures.get(method.as_str()).is_some_and(|sig| sig.module == file) { method.clone() }
                    else if matches!(owner.as_str(), "console" | "allocator" | "clock" | "net" | "dir" | "env" | "random") {
                        return Err(Diagnostic::new("W0401", callee.span.clone(), format!("the module `{owner}` has no function `{method}`: no `{owner}` value is in scope, so the name is the imported module; a function uses a capability only when it is given one as a parameter (`{owner}: &...`), which `main` passes on")));
                    }
                    else { return Err(Diagnostic::new("W0401", callee.span.clone(), format!("the module `{owner}` has no function `{method}`"))) };
                return Ok(Some(call(function, None)));
            }
            if !self.scope.contains_key(owner) && self.imports.iter().any(|(_, name, _)| name == owner) {
                return Err(Diagnostic::new("W0401", receiver.span.clone(), format!("`{owner}` is a module this file does not import; another file importing it does not make it visible here")));
            }
            if !self.scope.contains_key(owner) {
                if !std::mem::take(&mut self.operator) { self.visible(owner, &receiver.span)?; }
                let function = crate::compiler::methods::name(owner, method);
                return Ok(self.signatures.contains_key(&function).then(|| call(function, None)));
            }
        }
        // A bare number is a U64, as it is everywhere a type is not asked for.
        let literal = matches!(receiver.kind, a::Expression::Integer(_)).then_some(Ty::Unsigned(Scalar::U64));
        let Some(ty) = self.hint(receiver).or_else(|| self.infer_hint(receiver, None, 0, &mut 0).ok().flatten()).or(literal) else { return Ok(None) };
        let (owner, lent) = match ty { Ty::Borrow { pointee, .. } => (pointee.ty(), true), owner => (owner, false) };
        let function = match owner {
            // A value of a type parameter has the methods of the traits the parameter is bound by.
            Ty::Param { owner: declared, index } if declared == self.definition => {
                let mut found = self.bounds[index as usize].iter().map(|bound| crate::compiler::methods::name(crate::compiler::methods::bound_name(bound), method)).filter(|f| self.signatures.contains_key(f));
                let Some(first) = found.next() else {
                    return Err(Diagnostic::new("W0401", callee.span.clone(), "no trait this type parameter is bound by has a method of this name"));
                };
                if found.next().is_some() {
                    return Err(Diagnostic::new("W0401", callee.span.clone(), "two traits this type parameter is bound by have a method of this name: call the one meant as `Trait.method(value)`"));
                }
                first
            }
            _ => {
                let Some(nominal) = self.layouts.nominal(owner) else { return Ok(None) };
                let mut own = crate::compiler::methods::name(nominal, method);
                // A full `Dir` has its views' methods too: first `Dir<Read>`'s, then `Dir<Write>`'s.
                // The receiver narrows to the view's type where it is passed.
                if let Ty::Host(host) = owner && !self.signatures.contains_key(&own) {
                    if let Some(view) = host.views().iter().map(|view| crate::compiler::methods::name(view, method)).find(|f| self.signatures.contains_key(f)) { own = view; }
                }
                // A method of a trait the type has from an impl for every type.
                let blanket = (!self.signatures.contains_key(&own)).then(|| self.blankets.iter().map(|(t, _, _, _)| crate::compiler::methods::name(&format!("#{t}"), method))
                    .find(|f| self.signatures.contains_key(f))).flatten();
                blanket.unwrap_or(own)
            }
        };
        // `f(x)` of a closure that took over an owner: it has `call_once` alone, and is consumed.
        // A Text has borrowed UTF-8's methods: `label.byte_at(0)` is `label.as_str().byte_at(0)`.
        if self.layouts.nominal(owner) == Some("Text") && !self.signatures.contains_key(&function)
            && (self.signatures.contains_key(&crate::compiler::methods::name("str", method)) || self.signatures.contains_key(&crate::compiler::methods::name("&str", method)))
            && let a::Expression::Call(_, args) = &expr.kind {
            let held = if lent { a::Expr { kind: a::Expression::Unary("*", receiver.clone()), span: receiver.span.clone() } } else { (**receiver).clone() };
            let text = a::Expr { kind: a::Expression::Call(Box::new(a::Expr { kind: a::Expression::Field(Box::new(held), "as_str".into()), span: receiver.span.clone() }), vec![]), span: receiver.span.clone() };
            let call = a::Expr { kind: a::Expression::Call(Box::new(a::Expr { kind: a::Expression::Field(Box::new(text), method.clone()), span: callee.span.clone() }), args.clone()), span: expr.span.clone() };
            return Ok(Some(self.method_call(&call)?.unwrap_or(call)));
        }
        // Borrowed UTF-8 has the methods of `impl str` and of traits for `&str` (`show_on`).
        let function = if matches!(owner, Ty::Slice(_)) && !self.signatures.contains_key(&function) && self.signatures.contains_key(&crate::compiler::methods::name("&str", method)) && self.layouts.nominal(owner) == Some("str") { crate::compiler::methods::name("&str", method) } else { function };
        let function = if method == "call" && !self.signatures.contains_key(&function)
            && let Some(owner) = function.strip_suffix(".call") && self.signatures.contains_key(&format!("{owner}.call_once")) { format!("{owner}.call_once") } else { function };
        let Some(signature) = self.signatures.get(&function) else {
            // A console's built-in operations are read below.
            // So are a slice's and borrowed UTF-8's (`len`, `part`, `as_bytes`: check/calls.rs).
            if matches!(owner, Ty::Console | Ty::Input | Ty::Host(_) | Ty::Slice(_)) { return Ok(None); }
            // `using` asks for this one by itself, so say where it comes from.
            // An operator on a Result: the value is inside it (`text.byte_at(i) == 32`).
            if matches!(method.as_str(), "eq" | "add" | "sub" | "mul" | "before") && self.layouts.result_sum(owner).is_some() {
                let shown = self.layouts.type_name(owner).unwrap_or_else(|_| "Result".into());
                return Err(Diagnostic::new("W0401", callee.span.clone(), format!("this value is a `{shown}`, not a number: take the value out first (`let Ok(b): ... = ... else Err(e):`, or `match`)")));
            }
            if method == "eq" {
                return Err(Diagnostic::new("W0401", callee.span.clone(), "the receiver's type has no method of this name: `==` on a value that is not a number or a Bool calls `eq`, from implementing `Eq`"));
            }
            if method == "discard" {
                return Err(Diagnostic::new("W0401", callee.span.clone(), "the receiver's type has no method of this name: `discard` comes from implementing `Discard`, which `using` needs"));
            }
            if let Some(message) = match method.as_str() {
                "add" | "sub" | "mul" => Some("the receiver's type has no method of this name: `+`, `-` and `*` on a value that is not a number call `add`, `sub` and `mul`, from implementing `Add`, `Sub` and `Mul` (`import ops`)"),
                "before" => Some("the receiver's type has no method of this name: `<`, `<=`, `>` and `>=` on a value that is not a number call `before`, from implementing `Before` (`derive(Before)`, `import sort`)"),
                _ => None,
            } { return Err(Diagnostic::new("W0401", callee.span.clone(), message)); }
            // A method a standard module adds to the type, that module not imported
            // (`text.find_text(...)` without `import text`).
            if let Ok(shown) = self.layouts.type_name(owner) {
                if let Some(message) = read_only(shown.split('@').next().unwrap_or(&shown), method) { return Err(Diagnostic::new("W0401", callee.span.clone(), message)); }
                let base = shown.split('<').next().unwrap_or(&shown).to_string();
                if let Some(module) = crate::compiler::files::module_declaring_method(&base, method) {
                    return Err(Diagnostic::new("W0401", callee.span.clone(), format!("the receiver's type has no method of this name: `{base}.{method}` is in the standard library's `{module}`: add `import {module}`")));
                }
            }
            // The methods it does have that are nearest what was written: a few letters apart,
            // or a word of the same family (`close` and `release`).
            // `.slice()` is how an array is sliced; a type with its own slice says `.as_slice()`.
            if let Some((owner_name, _)) = function.split_once('.') && matches!(method.as_str(), "slice" | "slice_mut")
                && let Some(has) = (if method == "slice" { vec!["as_slice"] } else { vec!["as_mut_slice", "as_slice_mut"] }).into_iter().find(|has| self.signatures.contains_key(&crate::compiler::methods::name(owner_name, has))) {
                return Err(Diagnostic::new("W0401", callee.span.clone(), format!("the receiver's type has no method of this name: `{owner_name}` has `{has}` (`.{method}()` is a fixed array's)")));
            }
            if let Some((owner_name, _)) = function.split_once('.') {
                let near = self.near_methods(owner_name, method);
                if !near.is_empty() {
                    let listed = near.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", ");
                    let shown = if owner_name.starts_with("Closure#") { "this closure".to_string() } else { format!("`{owner_name}`") };
                    return Err(Diagnostic::new("W0401", callee.span.clone(), format!("the receiver's type has no method of this name: {shown} has {listed}")));
                }
            }
            // Say which type and which method, and the field when the receiver is one (a derived
            // impl's tokens all carry the type's span: `derive(Hash)` over an F64 field said only
            // the first sentence).
            if let Ok(shown) = self.layouts.type_name(owner) {
                let field = match &receiver.kind { a::Expression::Field(_, f) => format!(" (field `{f}`)"), _ => String::new() };
                let trait_note = match method.as_str() {
                    "hash" => format!(": `{shown}` does not implement `Hash`{}", if shown == "F64" { " (a NaN is not equal to itself, so equal keys could hash apart)" } else { "" }),
                    "show_on" | "push_on" => format!(": `{shown}` does not implement `Show`"),
                    _ => String::new(),
                };
                return Err(Diagnostic::new("W0401", callee.span.clone(), format!("the receiver's type has no method of this name: `{shown}`{field} has no `{method}`{trait_note}")));
            }
            return Err(Diagnostic::new("W0401", callee.span.clone(), "the receiver's type has no method of this name"));
        };
        // A view receiver: `c.bump()` lends only the fields `bump` lists, each as it says.
        if !signature.view.is_empty() {
            if !lent && !Self::place_syntax(receiver) {
                return Err(Diagnostic::new("W0401", receiver.span.clone(), "this method borrows fields of its receiver, which must be a named value: bind it with `let` or `var` first"));
            }
            let view = signature.view.clone();
            // A field that is itself a reference is passed as it is (the method takes its type).
            let reference_fields: Vec<String> = self.layouts.get(owner).map(|layout| layout.fields.iter().filter(|f| matches!(f.ty, Ty::Borrow { .. })).map(|f| f.name.clone()).collect()).unwrap_or_default();
            let lends = view.iter().map(|(field, mutable)| {
                let place = a::Expr { kind: a::Expression::Field(receiver.clone(), field.clone()), span: receiver.span.clone() };
                if reference_fields.contains(field) { return place; }
                a::Expr { kind: a::Expression::Unary(if *mutable { "&mut" } else { "&" }, Box::new(place)), span: receiver.span.clone() }
            });
            let callee = a::Expr { kind: a::Expression::Name(function), span: callee.span.clone() };
            return Ok(Some(a::Expr { kind: a::Expression::Call(Box::new(callee), lends.chain(args.iter().cloned()).collect()), span: expr.span.clone() }));
        }
        let Some(wanted) = signature.params.first().copied() else {
            return Err(Diagnostic::new("W0401", callee.span.clone(), "this function takes no receiver: call it on the type, `Type.name(...)`"));
        };
        if matches!(wanted, Ty::Borrow { mutable: true, .. }) && !lent && !Self::place_syntax(receiver) {
            return Err(Diagnostic::new("W0401", receiver.span.clone(), "this method changes its receiver, which must be a named value: bind it with `var` first"));
        }
        let wrap = |operator: &'static str| a::Expr { kind: a::Expression::Unary(operator, receiver.clone()), span: receiver.span.clone() };
        let first = match wanted {
            Ty::Borrow { mutable, .. } if !lent => wrap(if mutable { "&mut" } else { "&" }),
            Ty::Borrow { .. } => (**receiver).clone(),
            _ if !lent && Self::place_syntax(receiver) && !self.layouts.properties(owner).copy => wrap("move"),
            _ => (**receiver).clone(),
        };
        Ok(Some(call(function, Some(first))))
    }
}
