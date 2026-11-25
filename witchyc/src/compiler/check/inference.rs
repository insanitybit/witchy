mod later_use;

use super::*;

impl Checker<'_> {
    pub(super) fn word_method_types(&mut self, op: Bits) -> Result<(Ty, Ty)> {
        if matches!(op, Bits::Packed(packed) if packed.both) {
            let output = self.layouts.anonymous(vec![("first".into(), Ty::F64), ("second".into(), Ty::F64)])?;
            Ok((Ty::F64, output))
        } else {
            Ok((Ty::U64, Ty::U64))
        }
    }

    pub(super) fn count(&mut self) -> Result<()> {
        self.nodes += 1;
        if self.nodes > 4096 {
            Err(error("a function is at most 4096 operations, and this one goes past that here: move part of it into a function of its own"))
        } else {
            Ok(())
        }
    }
    pub(super) fn expect(&mut self, expr: &a::Expr, expected: Ty) -> Result<Expr> {
        let value = self.expr_expected(expr, Some(expected))?;
        // More authority stands in for less: a full `Dir` is a `Dir<Read>` or a `Dir<Write>`, by
        // value or by shared reference. The handle is the same; the type allows fewer operations.
        if crate::compiler::host::narrows(value.ty, expected) { return Ok(Expr { ty: expected, kind: value.kind }); }
        if let Some(map) = self.layouts.widening(value.ty, expected) {
            // A smaller anonymous sum is a value of a wider one once its tag is renumbered.
            if self.modelled() { return Err(Diagnostic::new("W0402", expr.span.clone(), "a function with a contract cannot widen an anonymous sum yet")); }
            return Ok(Expr { ty: expected, kind: Value::Retag(Box::new(value), map) });
        }
        if value.ty != expected && self.layouts.sum(value.ty).is_some_and(|sum| sum.anonymous) && self.layouts.sum(expected).is_some_and(|sum| sum.anonymous) {
            return Err(Diagnostic::new("W0402", expr.span.clone(),
                "an anonymous sum becomes a wider one only: every tag it has must be in the expected type, holding the same type. Match it and build the other value by hand"));
        }
        if value.ty != expected {
            if std::env::var("WITCHY_DEBUG_MISMATCH").is_ok() { crate::compiler::compiler_diagnostic!("mismatch: {:?} vs {:?} sum {:?} / {:?}", value.ty, expected, self.layouts.sum(value.ty).map(|s| s.arms.clone()), self.layouts.sum(expected).map(|s| s.arms.clone())); }
            // Say what kind of mismatch it is, and what would mend it. Every message keeps the
            // words the controls look for.
            // A literal with `${...}` is a format, not a Text (parser/interpolation.rs).
            let named = |t: Ty| self.layouts.record_name(t).map(str::to_string);
            if named(value.ty).is_some_and(|n| n.starts_with("Format#")) && (named(expected).is_some_and(|n| n == "Text") || Some(expected) == self.layouts.known_str_borrow()) {
                return Err(Diagnostic::new("W0402", expr.span.clone(), "scalar expression type mismatch: a text with `${...}` is a format, not a `&str`; print it with `console.print(...)`, write it without an end of line with `console.write_format(...)`, or add it to a String with `push_format`"));
            }
            // Two Results that differ only in their error: name both (`?` passes an error on as
            // it is, which is where this is met most).
            if let (Some(given), Some(wanted)) = (self.layouts.result_sum(value.ty), self.layouts.result_sum(expected))
                && given.arms[0] == wanted.arms[0] && given.arms[1] != wanted.arms[1] {
                let name = |t: Ty| self.layouts.type_name(t).unwrap_or_else(|_| "?".into());
                let message = format!("scalar expression type mismatch: this fails with a `{}` where a `{}` is expected; `?` passes an error on as it is, so handle this one where it happens (`match`, or `let Ok(x) = ... else Err(e):`)",
                    name(given.arms[1]), name(wanted.arms[1]));
                return Err(Diagnostic::new("W0402", expr.span.clone(), message));
            }
            // The payload of a refutable `let` whose written type is a reference
            // (`early_exit`: `let node: &Node = node#guard`): the call gives a value, and no `&`
            // can be written there.
            if matches!(&expr.kind, a::Expression::Name(name) if name.ends_with("#guard")) && matches!(expected, Ty::Borrow { .. }) {
                let given = self.layouts.type_name(value.ty).unwrap_or_else(|_| "?".into());
                let message = format!("scalar expression type mismatch: the written type is a reference, and what this gives is a `{given}` value; write `{given}` as the type, or call the method that gives a reference (for a Vec, `at` or `at_mut` instead of `get`)");
                return Err(Diagnostic::new("W0402", expr.span.clone(), message));
            }
            let message: std::borrow::Cow<'static, str> = match (value.ty, expected) {
                (Ty::I64, Ty::Unsigned(_)) | (Ty::Unsigned(_), Ty::I64) => "scalar expression type mismatch: an I64 and an unsigned number do not mix; convert with `I64.from(value)` or `value.to_u64()`, which check, or keep to one kind".into(),
                (Ty::Unsigned(_), Ty::Unsigned(_)) => "scalar expression type mismatch: unsigned numbers of different widths; widen with `U8.to_u64(value)` or narrow with `U8.truncate(value)`".into(),
                (Ty::Borrow { pointee, .. }, wanted) if pointee.ty() == wanted => "scalar expression type mismatch: a value is expected and this is a reference to one; read it with `*`".into(),
                (given, Ty::Borrow { pointee, .. }) if pointee.ty() == given => "scalar expression type mismatch: a reference is expected and this is the value; lend it with `&` or `&mut`".into(),
                (Ty::Unsigned(_) | Ty::I64 | Ty::F64, Ty::Bool) => "scalar expression type mismatch: a Bool is expected here and this is a number; compare it (`n != 0`)".into(),
                // Older controls match this text as the start of the message.
                (given, wanted) => match (self.layouts.type_name(given), self.layouts.type_name(wanted)) {
                    (Ok(given), Ok(wanted)) => format!("scalar expression type mismatch: this is a `{given}` where a `{wanted}` is expected").into(),
                    _ => "scalar expression type mismatch".into(),
                },
            };
            return Err(Diagnostic::new("W0402", expr.span.clone(), message));
        }
        Ok(value)
    }
    pub(super) fn expr(&mut self, expr: &a::Expr) -> Result<Expr> {
        self.expr_expected(expr, None)
    }
    pub(super) fn expr_expected(&mut self, expr: &a::Expr, expected: Option<Ty>) -> Result<Expr> {
        self.at = expr.span.clone();
        self.nested(|this| this.expr_inner(expr, expected)).map_err(|mut e| {
            if e.span == (0..0) { e.span = expr.span.clone(); }
            e
        })
    }
    // An unsuffixed literal has no fixed width until its contextual scalar type
    // is known. Already-bound values never implicitly change widths.
    /// The type of a method call's result (`buffer.as_slice()`), as inference sees it, where
    /// `hint` knows only names and plain functions: a slice method may follow a call.
    pub(super) fn call_hint(&mut self, expr: &a::Expr) -> Option<Ty> {
        if !matches!(&expr.kind, a::Expression::Call(callee, _) if matches!(callee.kind, a::Expression::Field(..))) { return None; }
        let mut visits = 0;
        self.infer_hint(expr, None, 0, &mut visits).ok().flatten()
    }
    // The error of the innermost argument, among calls' arguments, that has no type and is refused.
    // A call is looked into even when it has a type: a format's record has one while its own
    // type parameter is what is open.
    fn untyped_error(&mut self, arg: &a::Expr) -> Option<Diagnostic> {
        if let a::Expression::Call(_, inner) = &arg.kind
            && let Some(own) = inner.iter().find_map(|value| self.untyped_error(value)) {
            return Some(own);
        }
        // Nothing inside is to blame: the call itself, when it has no type (`v[0].eq(&k)`).
        if self.hint(arg).is_some() { return None; }
        self.expr(arg).err()
    }

    pub(super) fn hint(&self, expr: &a::Expr) -> Option<Ty> {
        if let Some(explicit) = self.through(expr) { return self.hint(&explicit); }
        if let Some(inner) = self.collapsed(expr, None) { return self.hint(inner); }
        if let Some(text) = self.lent_literal(expr) { return Some(text); }
        match &expr.kind {
            a::Expression::Name(n) => self.scope.get(n).map(|local| local.ty),
            a::Expression::Unary("move", value) => self.hint(value),
            a::Expression::Unary("*", value) => self.borrowed(value).ok().and_then(|local| {
                if let Ty::Borrow { pointee, .. } = local.ty { Some(pointee.ty()) } else { None }
            }),
            a::Expression::Bool(_) | a::Expression::Unary("!", _) => Some(Ty::Bool),
            a::Expression::Unary("-", _) => Some(Ty::I64),
            a::Expression::Unit => Some(Ty::Unit),
            a::Expression::Field(value, field) => self
                .hint(value)
                .and_then(|t| self.layouts.get(t))
                .and_then(|l| l.fields.iter().find(|f| f.name == *field))
                .map(|f| f.ty),
            a::Expression::Index(value, _) => self
                .hint(value)
                .and_then(|t| self.layouts.get(t))
                .and_then(|l| {
                    if let Kind::Array(t, _) = l.kind {
                        Some(t)
                    } else {
                        None
                    }
                }),
            a::Expression::Binary(op, left, right) => {
                if matches!(*op, "+" | "-" | "*" | "/" | "%") {
                    self.hint(left).or_else(|| self.hint(right))
                } else {
                    Some(Ty::Bool)
                }
            }
            a::Expression::Call(..) if self.literal(expr) => self.layouts.known_str_borrow(),
            a::Expression::Call(callee, _) => match &callee.kind {
                a::Expression::Name(n) => self
                    .signatures
                    .get(n)
                    .filter(|sig|sig.generics.is_empty()).map(|sig| sig.result)
                    .or_else(|| self.layouts.named_record(n)),
                a::Expression::Field(receiver, method) if matches!(&receiver.kind, a::Expression::Name(name) if name == "I64") && !self.scope.contains_key("I64") =>
                    match method.as_str() { "from_bits" => Some(Ty::I64), "bits" => Some(Ty::U64), _ => None },
                a::Expression::Field(_, method) if method == "to_u64" => Some(Ty::U64),
                a::Expression::Field(receiver, method) if method == "truncate" => match &receiver.kind {
                    a::Expression::Name(name) => Scalar::named(name).map(Ty::Unsigned),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }
    pub(super) fn infer_call(&mut self,sig:&Signature,args:&[a::Expr],expected:Option<Ty>,depth:usize,visits:&mut usize)->Result<Vec<Ty>> {
        // An opening's brand is offered to its own call only: taken before the arguments are
        // read, so a call among them is not offered it.
        let fresh=self.fresh_for.take();
        if args.len()!=sig.params.len(){return Err(error("call argument count mismatch"));}
        if sig.generics.is_empty(){return Ok(vec![]);}
        let mut solved=vec![None;sig.generics.len()];
        if let Some(t)=expected{self.layouts.unify(sig.result,t,sig.definition,&mut solved).map_err(|e| {
            // `if !v.push_mut(...)`: the call gives a sum (a `Result`), and a plain value is wanted.
            let gives=self.layouts.nominal(sig.result).map(|n| n.split('@').next().unwrap_or(n).to_string());
            match (gives, matches!(t, Ty::Bool|Ty::Unsigned(_)|Ty::I64|Ty::F64)) {
                (Some(sum), true) if matches!(sig.result, Ty::Sum(_)) => {
                    let wanted = self.layouts.type_name(t).unwrap_or_else(|_| "value".to_string());
                    let arms = if sum == "Result" { " (`Ok:` and `Err(back):`)" } else { "" };
                    error(format!("this call gives a `{sum}`, where a `{wanted}` is wanted: `match` on it, with an arm for each variant{arms}"))
                }
                _ => e,
            }
        })?;}
        // Twice: first with a call among the arguments leaving a bare number open, so every
        // argument has its say; then, for what is still open, with the U64 default.
        for open in [true, false] {
            let saved=self.open_numbers;
            self.open_numbers=open;
            let read=(||->Result<()>{loop {let before=solved.clone();
                for (arg,pattern) in args.iter().zip(&sig.params){
                    // A parameter whose type names no type parameter settles none: its argument
                    // is read against that type when the call is checked, as a plain call's is
                    // (so a literal given for a `&str` is one, in a generic method too).
                    if !self.layouts.names_parameter(*pattern,sig.definition,sig.generics.len()) { continue; }
                    let context=self.layouts.partial(*pattern,sig.definition,&solved)?;
                    if let Some(t)=self.infer_hint(arg,context,depth+1,visits)? {
                        // A brand conflict is placed at the argument, named by its type.
                        self.layouts.unify(*pattern,t,sig.definition,&mut solved).map_err(|e| match e.message.strip_prefix("brand conflict: this comes from") {
                            Some(rest) => { let what=self.layouts.nominal(match t { Ty::Borrow{pointee,..}=>pointee.ty(), other=>other }).unwrap_or("value").to_string();
                                Diagnostic::new("W0402",arg.span.clone(),format!("this `{what}` comes from{rest}")) }
                            // Two of this function's own brands (`G`, `H`): name the argument's.
                            None if e.message.starts_with("generic type argument conflict") && self.brand_parameter_in(t).is_some() => {
                                let (what, name) = (self.layouts.nominal(match t { Ty::Borrow{pointee,..}=>pointee.ty(), other=>other }).unwrap_or("value").to_string(), self.brand_parameter_in(t).unwrap_or_default());
                                Diagnostic::new("W0402",arg.span.clone(),format!("this `{what}` is numbered by `{name}`, and what it is used with here by another brand: rows of different brands do not mix, even when the counts agree")) }
                            None => e })?;
                    }
                }
                if solved==before{return Ok(());}
            }})();
            self.open_numbers=saved;
            read?;
            if solved.iter().all(Option::is_some){break;}
        }
        // A `Fresh` parameter: the brand new at this opening, and nothing else. One the arguments
        // or the expected type gave would be a second issue of a brand already in use.
        for (at, bounds) in sig.bounds.iter().enumerate() {
            if !bounds.iter().any(|b| b == "Fresh") { continue; }
            match (solved[at], fresh) {
                (None, Some(brand)) => { solved[at] = Some(brand); self.opened = true; }
                (Some(given), Some(brand)) if given == brand => self.opened = true,
                (Some(_), _) => return Err(error("a `Fresh` type parameter is given by an opening alone, never by an argument or by what the result is used as")),
                (None, None) => return Err(error("this call makes a new numbering each time it runs: open it as `brand value.method() as name:` and use what it gives inside that block")),
            }
        }
        // What the arguments leave open (before a bare number's U64, the last resort below), the use of the result may settle: a `match` whose arm
        // hands its variant's value to a `let` of a known type says what that variant holds.
        // The matched call itself: depth 0, or 1 for `Type.method(...)`, which is rewritten into a
        // call first. The outermost call reaches here before any call among its arguments.
        if let Some((variant,held))=if depth<=1{self.held.take()}else{None}{
            if solved.iter().any(Option::is_none){
                let arm=self.layouts.sum(sig.result).and_then(|sum|sum.names.iter().position(|name|*name==variant).map(|at|sum.arms[at]));
                let mut tried=solved.clone();
                if arm.is_some_and(|arm|self.layouts.unify(arm,held,sig.definition,&mut tried).is_ok()){solved=tried;}
            }
        }
        // Inside another call's reading of its arguments, a bare number stays open (above).
        if self.open_numbers && depth > 0 && solved.iter().any(Option::is_none) {
            return Err(error("generic call has ambiguous type arguments"));
        }
        // A bare number settles nothing above; where a parameter is still open it is a U64, as a
        // number is everywhere no other type is asked for (`map.get(&7)`).
        if solved.iter().any(Option::is_none) {
            for (arg, pattern) in args.iter().zip(&sig.params) {
                let literal = match &arg.kind {
                    a::Expression::Integer(_) => Some(Ty::U64),
                    a::Expression::Unary("&", inner) if matches!(inner.kind, a::Expression::Integer(_)) =>
                        Pointee::from_ty(Ty::U64).map(|pointee| Ty::Borrow { mutable: false, pointee }),
                    _ => None,
                };
                let Some(literal) = literal else { continue };
                let mut tried = solved.clone();
                if self.layouts.unify(*pattern, literal, sig.definition, &mut tried).is_ok() { solved = tried; }
            }
        }
        // A type parameter fixed only by another's bound (`U` in `F: WitchyFn1<T, U>`): the
        // impl of that trait for the type given for `F` says what its arguments are.
        if solved.iter().any(Option::is_none) {
            for (at, bounds) in sig.bounds.iter().enumerate() {
                let Some(Some(given)) = solved.get(at).copied() else { continue };
                let Some(type_name) = self.layouts.nominal(given).map(str::to_string) else { continue };
                for bound in bounds {
                    let Some((trait_name, _)) = bound.split_once('<') else { continue };
                    let Some(arguments) = self.trait_arguments.get(&(trait_name.to_string(), type_name.clone())).cloned() else { continue };
                    let written = self.bound_arguments(bound, sig.definition, &sig.generics, None)?;
                    if written.len() != arguments.len() { continue; }
                    let mut tried = solved.clone();
                    if written.iter().zip(&arguments).all(|(pattern, t)| self.layouts.unify(*pattern, *t, sig.definition, &mut tried).is_ok()) { solved = tried; }
                }
            }
        }
        // A trait method's own type parameters are the trait's arguments. Where `Self` is one of
        // this function's type parameters, its bound gives them (`F: WitchyFn1<U64, U64>` says
        // what `f.call(x)` returns), whatever the call's result is used for.
        if solved.iter().any(Option::is_none) && let Some(Some(Ty::Param { owner, index })) = solved.first().copied()
            && owner == self.definition && let Some([bound]) = sig.bounds.first().map(Vec::as_slice) && let Some((trait_name, _)) = bound.split_once('<') {
            let (generics, mine) = (self.generics.clone(), self.bounds.get(index as usize).cloned().unwrap_or_default());
            let matching: Vec<&String> = mine.iter().filter(|b| b.split_once('<').is_some_and(|(n, _)| n == trait_name)).collect();
            // One bound of the trait: two (`F: T<U64> + T<Bool>`) leave the choice to the arguments.
            if let [only] = matching.as_slice() {
                let given = self.bound_arguments(only, self.definition, &generics, None)?;
                if given.len() + 1 == sig.generics.len() {
                    let mut tried = solved.clone();
                    let ok = given.iter().enumerate().all(|(at, t)| match tried[at + 1] { Some(found) => found == *t, None => { tried[at + 1] = Some(*t); true } });
                    if ok { solved = tried; }
                }
            }
        }
        if solved.iter().any(Option::is_none) && let Some(name) = args.iter().find_map(|arg| match &arg.kind { a::Expression::Name(name) if !self.scope.contains_key(name) && self.signatures.contains_key(name) => Some(name), _ => None }) {
            let shown = name.split('@').next().unwrap_or(name);
            return Err(error(format!("`{shown}` is a function, not a value: pass a closure that calls it, as in `fn(n: U64) -> U64: {shown}(n)`")));
        }
        // `map.get(&name)` with `name` already a shared reference: `&&T`, which nothing implements.
        if solved.iter().any(Option::is_none) && let Some(name) = args.iter().find_map(|arg| match &arg.kind {
            a::Expression::Unary("&", inner) => match &inner.kind { a::Expression::Name(n) if self.scope.get(n).is_some_and(|l| matches!(l.ty, Ty::Borrow { .. })) => Some(n.clone()), _ => None },
            _ => None }) {
            return Err(error(format!("`{name}` is already a reference: pass `{name}`, not `&{name}`")));
        }
        // An argument with no type is usually why: its own error says more than the ambiguity
        // (`print("${v[0].eq(&k)}")` said nothing fixed `F`).
        // A call among them is looked into, not checked whole: without what it is used as, a
        // format's own call reads wrongly ("a format, not a Text").
        if solved.iter().any(Option::is_none) {
            for arg in args {
                if let Some(own) = self.untyped_error(arg) {
                    return Err(own);
                }
            }
        }
        // Name the type parameters nothing fixed, and how to fix them.
        if solved.iter().any(Option::is_none) {
            let open = solved.iter().zip(&sig.generics).filter(|(t, _)| t.is_none()).map(|(_, g)| format!("`{g}`")).collect::<Vec<_>>();
            if !open.is_empty() {
                return Err(error(format!("generic call has ambiguous type arguments: nothing given here fixes {}; name the type where the result is bound (`let x: Vec<U64> = ...`)", open.join(", "))));
            }
        }
        let solved=solved.into_iter().collect::<Option<Vec<_>>>().ok_or_else(||error("generic call has ambiguous type arguments"))?;
        // A call trait's own `call`: `Self` bounded by exactly `WitchyFn<n><A0, .., R>` over the
        // method's other type parameters (closure.rs writes these traits).
        let call_trait=sig.generics.len()>=2&&sig.generics.first().is_some_and(|g|g=="Self")&&sig.bounds.first().is_some_and(|b|matches!(b.as_slice(),[only] if *only==format!("WitchyFn{}<{}>",sig.generics.len()-2,sig.generics[1..].join(", "))));
        for (i,(t,any)) in solved.iter().zip(&sig.any).enumerate(){
            // A function type's parameter, or any type argument of a call trait's `call` (`Self`
            // may hold authority; an argument may be a reference), may be given authority.
            // Its result `R` (the last) is no reference: through the trait an argument and the
            // result are different parameters, so a reference result could outlive its loan.
            let callable=sig.generics[i].starts_with("#F")||(call_trait&&i+1<sig.generics.len());
            // Work to run once (`W: WitchyOnce0<()>`, a task's): it takes over what it holds,
            // authority included, and the bound says so; `wasm-caps` follows it.
            if sig.bounds[i].iter().any(|b|crate::compiler::methods::once_bound(b)){self.layouts.work_argument(*t,*any)?;continue;}
            // A view (a Span) may be a `T: Any` the callee only ever holds as `&T` (semantic_types).
            if !callable && *any && self.layouts.shared_view(*t) && !self.layouts.authority_in(*t,0) && self.layouts.only_lent(&sig.params,sig.result,sig.definition,i as u32) { continue; }
            // `str` itself, held only as `&Q`: `map.get(text)` (Rust's `?Sized` key).
            if !callable && *any && self.layouts.unsized_str(*t) && self.layouts.only_lent(&sig.params,sig.result,sig.definition,i as u32) { continue; }
            if callable{self.layouts.callable_argument(*t,*any)?}else{self.layouts.function_argument(*t,*any)?}
        }
        Ok(solved)
    }
    /// The type of `e`, when it can be read without checking it. Within one inference (the
    /// outermost `infer_hint` or `method_call` call and everything under it) nothing it reads
    /// changes: no name comes into scope, nothing is moved or lent. So the answer for the same
    /// expression with the same expected type is the answer it gave before, and is given
    /// again without the work: `method_call` writes an operator on a program's type as a call
    /// and that call's arguments are read again at every level, which made a chain of n
    /// operators take about 3^n steps. The two counters that bound one inference are advanced
    /// by what the work would have advanced them, so every limit is met exactly where it was;
    /// only `hint_work`, the steps actually taken, is smaller.
    pub(super) fn infer_hint(&mut self,e:&a::Expr,expected:Option<Ty>,depth:usize,visits:&mut usize)->Result<Option<Ty>> {
        let chain=visits as *const usize;
        // An opening is found by the address of its expression: read as written, never recalled.
        // A name or a literal is read in one step: nothing to save, and no copy of it is kept.
        if self.opening.is_some() || self.fresh_for.is_some()
            || matches!(e.kind,a::Expression::Name(_)|a::Expression::Integer(_)|a::Expression::Float(_)|a::Expression::Bool(_)|a::Expression::Unit) {
            if self.hinting>0&&self.hint_chain==chain { self.hint_reach=self.hint_reach.max(depth); }
            return self.infer_hint_once(e,expected,depth,visits);
        }
        if self.hinting==0 { self.hint_memo.clear(); }
        let key=(e.span.start,e.span.end);
        if let Some(found)=self.hint_memo.get(&key).and_then(|answers|answers.iter().find(|answer|answer.expected==expected&&answer.expr==*e)) {
            let (result,steps,reach)=(found.result,found.steps,found.reach);
            #[cfg(debug_assertions)]
            if std::env::var_os("WITCHY_DEBUG_HINT_SHADOW").is_some() {
                // A development build can read every recalled answer again and compare.
                let saved=(std::mem::take(&mut self.hint_memo),self.hint_work,self.hinting,self.hint_chain,self.hint_reach,self.operands);
                let mut again=0;
                // It may take a sixty-fourth of a function's work, so a long chain is given up soon.
                (self.hinting,self.hint_work)=(u32::MAX/2,HINT_WORK-HINT_WORK/64);
                let fresh=self.infer_hint_plain(e,expected,0,&mut again);
                // Read again with nothing recalled, a long chain takes the steps it used to, and
                // meets the bound on a function's work: that is no answer to compare.
                let unread=matches!(&fresh,Err(e) if e.message.starts_with("type inference work limit"));
                assert!(unread||matches!(&fresh,Ok(ty) if *ty==result)&&again==steps,"recalled {result:?} in {steps} steps, read again {fresh:?} in {again}: {:?}",e.span);
                (self.hint_memo,self.hint_work,self.hinting,self.hint_chain,self.hint_reach,self.operands)=saved;
            }
            // The limits of one inference, exactly as the work would have met them.
            *visits+=steps;
            if depth+reach>120||*visits>4096{return Err(error("generic inference complexity limit"));}
            self.hint_work=self.hint_work.saturating_add(1);
            if self.hint_work>HINT_WORK{return Err(Diagnostic::new("W0401",e.span.clone(),HINT_WORK_MESSAGE));}
            if self.hinting>0&&self.hint_chain==chain { self.hint_reach=self.hint_reach.max(depth+reach); }
            self.operands=None;
            return Ok(result);
        }
        let outer=(self.hint_chain,self.hint_reach);
        let same=self.hinting>0&&outer.0==chain;
        self.hinting+=1;
        (self.hint_chain,self.hint_reach)=(chain,depth);
        let before=*visits;
        let result=self.infer_hint_once(e,expected,depth,visits);
        let (steps,reach)=(*visits-before,self.hint_reach.saturating_sub(depth));
        (self.hint_chain,self.hint_reach)=(outer.0,if same{outer.1.max(self.hint_reach)}else{outer.1});
        self.hinting-=1;
        // Only an answer: a refusal may be a limit, which depends on where it was asked.
        if let Ok(ty)=&result { self.hint_memo.entry(key).or_default().push(Hinted{expr:e.clone(),expected,result:*ty,steps,reach}); }
        result
    }
    /// `infer_hint` with nothing recalled, for the comparison a development build can make.
    #[cfg(debug_assertions)]
    fn infer_hint_plain(&mut self,e:&a::Expr,expected:Option<Ty>,depth:usize,visits:&mut usize)->Result<Option<Ty>> {
        let saved=self.opening;
        // With an opening set nothing is recalled or kept; this address is no expression's.
        self.opening=Some((std::ptr::NonNull::<a::Expr>::dangling().as_ptr() as *const a::Expr,Ty::Unit));
        let result=self.infer_hint_once(e,expected,depth,visits);
        self.opening=saved;
        result
    }
    /// Constraint gathering only: no Flow, loans, counters, source evaluation or argument recheck.
    fn infer_hint_once(&mut self,e:&a::Expr,expected:Option<Ty>,depth:usize,visits:&mut usize)->Result<Option<Ty>> {
        if let Some((call, brand)) = self.opening && std::ptr::eq(call, e) { self.fresh_for = Some(brand); }
        if let a::Expression::Call(callee, args) = &e.kind && matches!(&callee.kind, a::Expression::Name(n) if n == crate::parser::OPENING) {
            let [inner] = args.as_slice() else { return Err(error("an opening opens one value")); };
            let brand = self.opening_brand(e, inner);
            let saved = self.opening.replace((inner as *const a::Expr, brand));
            let hint = self.infer_hint(inner, expected, depth, visits);
            self.opening = saved; self.fresh_for = None;
            return hint;
        }
        *visits+=1;if depth>120||*visits>4096{return Err(error("generic inference complexity limit"));}
        // Every step on this function counts, whichever call started it: the counters above
        // start again inside `method_call`, so they do not bound the work as a whole.
        self.hint_work=self.hint_work.saturating_add(1);
        if self.hint_work>HINT_WORK{return Err(Diagnostic::new("W0401",e.span.clone(),HINT_WORK_MESSAGE));}
        self.operands=None;
        if let Some(call)=self.method_call(e)?{return self.infer_hint(&call,expected,depth+1,visits);}
        // Set by that call, for this expression, or not at all.
        let operands=self.operands.take().filter(|(of,_,_)|std::ptr::eq(*of,e));
        if let Some(explicit)=self.through(e){return self.infer_hint(&explicit,expected,depth+1,visits);}
        if let Some(inner)=self.collapsed(e,expected){return self.infer_hint(inner,expected,depth+1,visits);}
        if let Some(text)=self.lent_literal(e){return Ok(Some(text));}
        // Slice operations (docs/design-slices.md): `a.slice()`, `s.part(..)`, `s.at(..)`, `s.len()`.
        if let a::Expression::Call(callee,args)=&e.kind && let a::Expression::Field(receiver,method)=&callee.kind && matches!(method.as_str(),"slice"|"slice_mut"|"part"|"part_proved"|"at"|"cell"|"len"|"as_bytes"|"read_le64"|"index"|"#walk"|"#walk_mut"|"#counted") {
            let receiver_ty=self.infer_hint(receiver,None,depth+1,visits)?;
            let array=receiver_ty.and_then(|t| match self.layouts.get(t).map(|a| a.kind.clone()) { Some(Kind::Array(element,_))=>Some(element), _=>None });
            match (method.as_str(),receiver_ty,array) {
                ("slice"|"slice_mut",_,Some(element)) if args.is_empty() => { let Ty::Slice(id)=self.layouts.slice_of(element)? else { unreachable!() }; return Ok(Some(Ty::Borrow{mutable:method=="slice_mut",pointee:Pointee::Slice(id)})); }
                ("part"|"part_proved",Some(t @ Ty::Borrow{pointee:Pointee::Slice(_),..}),_) => return Ok(Some(t)),
                ("read_le64",Some(Ty::Borrow{pointee:Pointee::Slice(id),..}),_) if !self.layouts.is_str(id) && self.layouts.slice_element(id) == Ty::Unsigned(Scalar::U8) => return Ok(Some(Ty::U64)),
                ("len",Some(Ty::Borrow{pointee:Pointee::Slice(_),..}),_) => return Ok(Some(Ty::U64)),
                ("as_bytes", Some(Ty::Borrow { pointee: Pointee::Slice(id), .. }), _) if self.layouts.is_str(id) => {
                    let Ty::Slice(bytes) = self.layouts.slice_of(Ty::Unsigned(Scalar::U8))? else { unreachable!() };
                    return Ok(Some(Ty::Borrow { mutable: false, pointee: Pointee::Slice(bytes) }));
                }
                ("index"|"#counted",Some(Ty::Borrow{pointee:Pointee::Slice(id),..}),_) => return Ok(Some(self.layouts.slice_element(id))),
                ("#walk_mut",Some(Ty::Borrow{pointee:Pointee::Slice(id),mutable:true}),_) => { let Some(pointee)=Pointee::from_ty(self.layouts.slice_element(id)) else { return Ok(None) }; return Ok(Some(Ty::Borrow{mutable:true,pointee})); }
                ("#walk",Some(Ty::Borrow{pointee:Pointee::Slice(id),mutable:false}),_) => { let Some(pointee)=Pointee::from_ty(self.layouts.slice_element(id)) else { return Ok(None) }; return Ok(Some(Ty::Borrow{mutable:false,pointee})); }
                ("cell",Some(Ty::Borrow{pointee:Pointee::Slice(id),mutable}),_) => { let Some(pointee)=Pointee::from_ty(self.layouts.slice_element(id)) else { return Ok(None) }; return Ok(Some(Ty::Borrow{mutable,pointee})); }
                ("at",Some(Ty::Borrow{pointee:Pointee::Slice(id),mutable}),_) => { let Some(pointee)=Pointee::from_ty(self.layouts.slice_element(id)) else { return Ok(None) }; return Ok(Some(self.layouts.result([Ty::Borrow{mutable,pointee},Ty::U64])?)); }
                _ => {}
            }
        }
        Ok(match &e.kind {
            a::Expression::Integer(_)=>expected.filter(|t|matches!(t,Ty::Unsigned(_)|Ty::I64)),
            a::Expression::Float(_)=>Some(Ty::F64),
            // `F64.sqrt(x)` and the other F64 calls give an F64; `bits` a U64 and `to_i64` an I64.
            a::Expression::Call(callee,_) if matches!(&callee.kind, a::Expression::Field(r,m) if matches!(&r.kind, a::Expression::Name(n) if n=="F64") && matches!(m.as_str(),"from_bits"|"from_u64"|"from_i64"|"sqrt"|"abs"|"floor")) && !self.scope.contains_key("F64") => Some(Ty::F64),
            // (The comment named these two, but they had no line: `F64.to_i64(x).magnitude()` was
            // refused as a method on an unknown type.)
            a::Expression::Call(callee,_) if matches!(&callee.kind, a::Expression::Field(r,m) if matches!(&r.kind, a::Expression::Name(n) if n=="F64") && m=="to_i64") && !self.scope.contains_key("F64") => Some(Ty::I64),
            a::Expression::Call(callee,_) if matches!(&callee.kind, a::Expression::Field(r,m) if matches!(&r.kind, a::Expression::Name(n) if n=="F64") && m=="bits") && !self.scope.contains_key("F64") => Some(Ty::U64),
            a::Expression::Unary("-",v)=>if matches!(self.infer_hint(v,None,depth+1,visits)?,Some(Ty::F64)){Some(Ty::F64)}else{Some(Ty::I64)},
            a::Expression::Bool(_)=>Some(Ty::Bool),a::Expression::Unit=>Some(Ty::Unit),
            // A local, or a variant holding nothing (`Dot`) of one type.
            a::Expression::Name(n)=>self.scope.get(n).map(|l|l.ty).or_else(|| self.layouts.variant(n,expected).ok().map(|(t,_)|t)),
            a::Expression::Unary("move",v)=>self.infer_hint(v,expected,depth+1,visits)?,
            a::Expression::Unary("*",v)=>self.borrowed(v).ok().and_then(|l|if let Ty::Borrow{pointee,..}=l.ty{Some(pointee.ty())}else{None}),
            a::Expression::Unary(op @ ("&"|"&mut"),v)=>self.infer_hint(v,None,depth+1,visits)?.and_then(Pointee::from_ty).map(|pointee|Ty::Borrow{mutable:*op=="&mut",pointee}),
            a::Expression::Unary("!",_)=>Some(Ty::Bool),
            a::Expression::Field(v,n)=>{let t=self.infer_hint(v,None,depth+1,visits)?;t.and_then(|t|self.layouts.get(t)).and_then(|r|r.fields.iter().find(|f|f.name==*n)).map(|f|f.ty)},
            a::Expression::Index(v,_)=>{let t=self.infer_hint(v,None,depth+1,visits)?;t.and_then(|t|self.layouts.get(t)).and_then(|a|if let Kind::Array(t,_)=a.kind{Some(t)}else{None})},
            a::Expression::Binary(op,a,b)=>if matches!(*op,"+"|"-"|"*"|"/"|"%"|"&"|"|"|"^"){
                // With nothing expected these are the two types `method_call` read just now.
                if let (None,Some((_,left,right)))=(expected,operands) { left.or(right) }
                else { let a=self.infer_hint(a,expected,depth+1,visits)?;let b=self.infer_hint(b,expected,depth+1,visits)?;a.or(b) }
            }else if matches!(*op,"<<"|">>"){
                // The amount is a U64; the result has the shifted value's type.
                self.infer_hint(a,expected,depth+1,visits)?
            }else{Some(Ty::Bool)},
            a::Expression::Unary("~",v)=>self.infer_hint(v,expected,depth+1,visits)?,
            // A literal is a `&str` unless a `Text` is asked for (text_literal.rs).
            a::Expression::Call(..) if self.literal(e) && !expected.is_some_and(|t| self.layouts.nominal(t)==Some("Text")) => self.layouts.known_str_borrow(),
            a::Expression::Call(callee,args)=>match &callee.kind {
                a::Expression::Name(n) if !self.scope.contains_key(n)=>{
                    if let Some(sig)=self.signatures.get(&self.function_named(n)).cloned(){
                        match self.infer_call(&sig,args,expected,depth+1,visits){
                            Ok(subs)=>Some(self.layouts.substitute(sig.result,sig.definition,&subs)?),
                            Err(e) if expected.is_none()&&(e.message=="generic call has ambiguous type arguments"||e.message.contains("is already a reference: pass"))=>None,
                            Err(e)=>return Err(e),
                        }
                    }else if let Ok((t,_))=self.layouts.variant(n,expected){
                        // `Square(2)`: the sum a variant of one type builds (`Square(2).before(&a)`).
                        Some(t)
                    }else{match self.layouts.constructor(n,expected){
                        Ok(t)=>Some(t),
                        Err(_)=>{let mut fields=Vec::with_capacity(args.len());for arg in args{
                            let hint=self.infer_hint(arg,None,depth+1,visits)?;
                            if n.starts_with("Format#") && let a::Expression::Name(name)=&arg.kind && matches!(hint,Some(Ty::Borrow{pointee,..}) if !self.layouts.properties(pointee.ty()).copy) {
                                return Err(error(format!("`${{{name}}}`: a format holds a copy of each value, and `{name}` refers to one that is not copied (a String, a Vec): write it with `console.write_string({name})`, or show it on its own")));
                            }
                            // A format holds what a shared reference to a Copy value points at (`expr_inner`).
                            fields.push(if n.starts_with("Format#")&&self.copied_through(arg){hint.map(|t|match t{Ty::Borrow{pointee,..}=>pointee.ty(),other=>other})}else{hint});
                        }self.layouts.constructed(n,&fields)?}
                    }}
                },
                a::Expression::Field(receiver,method) if matches!(&receiver.kind,a::Expression::Name(n) if n=="memory")&&!self.scope.contains_key("memory")=>{
                    match method.as_str(){
                        "try_alloc"|"try_alloc_prefix"|"null_box"|"null_weak"|"try_box"|"buffer_pointer"=>expected,"box_address"|"weak_bits"=>Some(Ty::Unsigned(Scalar::U32)),"capacity"|"allocation_key"=>Some(Ty::U64),"initialized"=>Some(Ty::Bool),
                        "get_unchecked"|"take_unchecked"=>if let Some(arg)=args.first(){self.buffer_argument(arg).ok().map(|id|self.layouts.buffers()[id as usize].element)}else{None},
                        "init"=>Some(self.layouts.result([Ty::Unit,Ty::U64])?),
                        "try_init"=>if let Some(arg)=args.first(){if let Ok(id)=self.buffer_argument(arg){let t=self.layouts.buffers()[id as usize].element;Some(self.layouts.result([Ty::Unit,t])?)}else{None}}else{None},
                        "init_unchecked"|"exchange"|"move_prefix_unchecked"|"copy_into_unchecked"|"take_prefix_unchecked"|"fill_unchecked"|"init_word_unchecked"|"init_slice_unchecked"=>Some(Ty::Unit),
                        "read_word_unchecked"=>Some(Ty::U64),
                        "read"|"take"|"swap"=>if let Some(arg)=args.first(){let id=self.buffer_argument(arg).ok();
                            if let Some(id)=id{let t=self.layouts.buffers()[id as usize].element;Some(self.layouts.result([t,Ty::U64])?)}else{None}}else{None},
                        "try_release"=>if let Some(arg)=args.first(){if let Some(t@Ty::Buffer(_))=self.infer_hint(arg,None,depth+1,visits)?{Some(self.layouts.result([Ty::Unit,t])?)}else{None}}else{None},_=>None,
                    }
                },
                a::Expression::Field(receiver,method) if matches!(&receiver.kind,a::Expression::Name(n) if n=="U64") && !self.scope.contains_key("U64") && Bits::word_method(method).is_some()=>Some(self.word_method_types(Bits::word_method(method).unwrap().0)?.1),
                a::Expression::Field(receiver,method) if matches!(&receiver.kind,a::Expression::Name(n) if n=="I64")&&!self.scope.contains_key("I64")=>match method.as_str(){"from_bits"=>Some(Ty::I64),"bits"=>Some(Ty::U64),_=>None},
                a::Expression::Field(_,method) if method=="to_u64"=>Some(Ty::U64),
                a::Expression::Field(receiver,method) if method=="truncate"=>match &receiver.kind{a::Expression::Name(name)=>Scalar::named(name).map(Ty::Unsigned),_=>None},
                a::Expression::Field(_,method) if method=="print_int"||method=="print_byte"||method=="print_word"=>Some(Ty::Unit),
                a::Expression::Field(receiver,method) if matches!(&receiver.kind,a::Expression::Name(n) if n=="provider")&&!self.scope.contains_key("provider")&&crate::compiler::host::HostOp::public(method).is_some()=>Some(Ty::U64),
                a::Expression::Field(receiver,method) if method.starts_with("raw_")||matches!(method.as_str(),"now_ms"|"next_u64")=>{
                    let on=match self.infer_hint(receiver,None,depth+1,visits)? { Some(Ty::Host(h)|Ty::Borrow{pointee:Pointee::Host(h),..})=>Some(h), _=>None };
                    on.and_then(|h|crate::compiler::host::HostOp::method(method,h)).map(|(_,_,out)|match out {
                        crate::compiler::host::Out::Nothing=>Ty::Unit, crate::compiler::host::Out::Number=>Ty::U64, crate::compiler::host::Out::Handle(h)=>Ty::Host(h) })
                },
                a::Expression::Field(_,method) if (method=="next_byte"||method=="next_run")&&args.is_empty()=>Some(Ty::U64),_=>None,
            },
            a::Expression::Record(fields)=>{let mut found=vec![];
                for(label,value)in fields{
                    // A bare literal takes the type it would have as a binding.
                    let literal=match &value.kind{a::Expression::Integer(_)=>Some(Ty::U64),a::Expression::Bool(_)=>Some(Ty::Bool),_=>None};
                    match self.infer_hint(value,None,depth+1,visits)?.or(literal){Some(t)=>found.push((label.clone(),t)),None=>return Ok(None)}}
                self.layouts.anonymous(found).ok()},
            a::Expression::Array(values)=>{let context=expected.and_then(|t|self.layouts.get(t)).and_then(|a|if let Kind::Array(t,_) =a.kind{Some(t)}else{None});
                let mut element=context;for v in values{element=element.or(self.infer_hint(v,context,depth+1,visits)?.map(|t|self.stored_text(t)));}
                if let Some(t)=element{Some(self.layouts.array(t,values.len() as u64)?)}else{None}},
            a::Expression::Repeat(v,n)=>{let context=expected.and_then(|t|self.layouts.get(t)).and_then(|a|if let Kind::Array(t,_) =a.kind{Some(t)}else{None});
                if let Some(t)=self.infer_hint(v,context,depth+1,visits)?{Some(self.layouts.array(t,*n)?)}else{None}},_=>None,
        })
    }
}
