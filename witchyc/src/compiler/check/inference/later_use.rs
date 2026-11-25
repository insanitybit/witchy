use super::*;

impl Checker<'_> {
    /// The type a `let` of an ambiguous `Type.constructor(...)` has, from the first later
    /// call of one of `Type`'s methods on it (`names.push_mut(&allocator, 7)`): that method's
    /// type arguments, solved from the arguments it can type now, give its receiver's type.
    pub(in crate::compiler::check) fn later_use(&mut self, name: &str, value: &a::Expr, rest: &[a::Statement]) -> Result<Option<Ty>> {
        // `Vec.new(...)?` is the value the later call is made on.
        let call = match &value.kind { a::Expression::Try(inner) => inner.as_ref(), _ => value };
        let a::Expression::Call(callee, _) = &call.kind else { return Ok(None) };
        let a::Expression::Field(owner, _) = &callee.kind else { return Ok(None) };
        let a::Expression::Name(type_name) = &owner.kind else { return Ok(None) };
        if self.scope.contains_key(type_name) { return Ok(None); }
        // `var numbers = move hidden` (how `var x = call()?` is written out): follow the name.
        let (mut name, mut rest) = (name.to_string(), rest);
        let named = |e: &a::Expr, name: &str| match &e.kind {
            a::Expression::Name(n) => n == name,
            a::Expression::Unary("move", inner) => matches!(&inner.kind, a::Expression::Name(n) if n == name),
            _ => false,
        };
        while let Some((a::Statement::Bind { name: next, ty, value, .. }, after)) = rest.split_first() && named(value, &name) {
            // `var map: HashMap<U64, U64> = move start`: the type is stated.
            if let Some(ty) = ty { return Ok(Some(self.layouts.ty_in(ty, self.definition, &self.generics)?)); }
            (name, rest) = (next.clone(), after);
        }
        let Some((method, arguments)) = first_method_call(rest, &name) else { return Ok(None) };
        let Some(sig) = self.signatures.get(&crate::compiler::methods::name(type_name, method)).cloned() else { return Ok(None) };
        if sig.params.is_empty() || arguments.len() + 1 != sig.params.len() { return Ok(None); }
        // The receiver's own argument says nothing yet: the others are what can be typed now.
        let mut solved = vec![None; sig.generics.len()];
        for (argument, pattern) in arguments.iter().zip(&sig.params[1..]) {
            let context = self.layouts.partial(*pattern, sig.definition, &solved)?;
            let mut visits = 0;
            let hint = match self.infer_hint(argument, context, 0, &mut visits) { Ok(hint) => hint, Err(_) => None };
            let literal = matches!(argument.kind, a::Expression::Integer(_)).then_some(Ty::U64);
            if let Some(t) = hint.or(literal) { let mut tried = solved.clone(); if self.layouts.unify(*pattern, t, sig.definition, &mut tried).is_ok() { solved = tried; } }
        }
        let Some(solved) = solved.into_iter().collect::<Option<Vec<_>>>() else { return Ok(None) };
        let receiver = self.layouts.substitute(sig.params[0], sig.definition, &solved)?;
        Ok(Some(match receiver { Ty::Borrow { pointee, .. } => pointee.ty(), other => other }))
    }
}

/// The first `name.method(arguments)` in these statements, in order, at any depth.
fn first_method_call<'b>(statements: &'b [a::Statement], name: &str) -> Option<(&'b str, &'b [a::Expr])> {
    fn in_expr<'b>(e: &'b a::Expr, name: &str) -> Option<(&'b str, &'b [a::Expr])> {
        match &e.kind {
            a::Expression::Call(callee, arguments) => {
                if let a::Expression::Field(receiver, method) = &callee.kind && matches!(&receiver.kind, a::Expression::Name(n) if n == name) { return Some((method.as_str(), arguments.as_slice())); }
                in_expr(callee, name).or_else(|| arguments.iter().find_map(|argument| in_expr(argument, name)))
            }
            a::Expression::Field(inner, _) | a::Expression::Unary(_, inner) => in_expr(inner, name),
            a::Expression::Binary(_, left, right) => in_expr(left, name).or_else(|| in_expr(right, name)),
            a::Expression::Conditional(c, yes, no) => in_expr(c, name).or_else(|| in_expr(yes, name)).or_else(|| in_expr(no, name)),
            _ => None,
        }
    }
    statements.iter().find_map(|statement| match statement {
        a::Statement::Expr(e) | a::Statement::Assert(e) | a::Statement::Return(Some(e)) => in_expr(e, name),
        a::Statement::Bind { value, .. } | a::Statement::Assign { value, .. } | a::Statement::Open { value, .. } | a::Statement::OpenFields { value, .. } => in_expr(value, name),
        a::Statement::Guard { value, failure, .. } => in_expr(value, name).or_else(|| first_method_call(&failure.body, name)),
        a::Statement::If { condition, yes, no } => in_expr(condition, name).or_else(|| first_method_call(yes, name)).or_else(|| first_method_call(no, name)),
        a::Statement::While { condition, body, .. } => in_expr(condition, name).or_else(|| first_method_call(body, name)),
        a::Statement::For { start, end, body, .. } => in_expr(start, name).or_else(|| end.as_ref().and_then(|e| in_expr(e, name))).or_else(|| first_method_call(body, name)),
        a::Statement::Match { value, arms } => in_expr(value, name).or_else(|| arms.iter().find_map(|arm| first_method_call(&arm.body, name))),
        a::Statement::Using { value, body, .. } => in_expr(value, name).or_else(|| first_method_call(body, name)),
        a::Statement::Block(body) => first_method_call(body, name),
        a::Statement::Choose { inner, .. } => first_method_call(std::slice::from_ref(inner), name),
        _ => None,
    })
}
