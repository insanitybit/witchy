//! Where a name is last used in a block. A reference keeps its source lent until then.
use crate::ast as a;

/// The last statement at or after `from` that names `name`, if any.
pub(super) fn last(body: &[a::Statement], from: usize, name: &str) -> Option<usize> {
    (from..body.len()).rev().find(|index| statement(&body[*index], name))
}
/// Whether a function's body or clauses use `name` anywhere.
pub(super) fn function_names(f: &a::Function, name: &str) -> bool {
    fn clause(p: &a::Postcondition, name: &str) -> bool {
        match &p.kind {
            a::PostconditionKind::Predicate(e) => expr(e, name),
            a::PostconditionKind::Match { value, arms } => expr(value, name) || arms.iter().any(|arm| arm.clauses.iter().any(|inner| clause(inner, name))),
        }
    }
    block(&f.body, name) || f.requires.iter().any(|e| expr(e, name)) || f.ensures.iter().any(|p| clause(p, name))
}
/// Whether a clause of the function (`requires`, `ensures`, a loop's `invariant`, an `assert`)
/// uses `name`. An `assert` is not read here, so it is taken to name everything.
pub(super) fn clauses_name(f: &a::Function, name: &str) -> bool {
    fn clause(p: &a::Postcondition, name: &str) -> bool {
        match &p.kind {
            a::PostconditionKind::Predicate(e) => expr(e, name),
            a::PostconditionKind::Match { value, arms } => expr(value, name) || arms.iter().any(|arm| arm.clauses.iter().any(|inner| clause(inner, name))),
        }
    }
    fn within(body: &[a::Statement], name: &str) -> bool {
        use a::Statement as S;
        body.iter().any(|s| match s {
            S::While { invariants, body, .. } => invariants.iter().any(|e| expr(e, name)) || within(body, name),
            S::For { invariants, body, .. } => invariants.iter().any(|e| expr(e, name)) || within(body, name),
            S::Using { body, .. } | S::Block(body) => within(body, name),
            S::Guard { failure, .. } => within(&failure.body, name),
            S::If { yes, no, .. } => within(yes, name) || within(no, name),
            S::Match { arms, .. } => arms.iter().any(|arm| within(&arm.body, name)),
            S::Choose { inner, .. } => within(std::slice::from_ref(inner), name),
            S::Assert(_) => true,
            S::Bind { .. } | S::Open { .. } | S::OpenFields { .. } | S::Expr(_) | S::Assign { .. } | S::Return(_) | S::Yield { .. }
            | S::Proof(_) | S::Break | S::Continue | S::Unreachable(_) | S::Dispose { .. } => false,
        })
    }
    f.requires.iter().any(|e| expr(e, name)) || f.ensures.iter().any(|p| clause(p, name)) || within(&f.body, name)
}
/// Whether every use of `name` in the function's body hands it on or reads a field of it:
/// `name` is an argument of a call, the receiver of a method call, or the value a field is
/// read from. Bound to another name, taken apart, matched, returned, borrowed or walked by a
/// `for`, it is not. Any use of the name counts, whatever it means there (a shadowing local
/// is held to the same rule).
pub(super) fn only_handed_on(f: &a::Function, name: &str) -> bool {
    fn ok(e: &a::Expr, name: &str, here: bool) -> bool {
        use a::Expression as E;
        match &e.kind {
            E::Name(n) => n != name || here,
            E::Unit | E::Integer(_) | E::Float(_) | E::Bool(_) | E::SourceSite(_) => true,
            E::Call(callee, args) => (match &callee.kind { E::Field(receiver, _) => ok(receiver, name, true), _ => ok(callee, name, false) })
                && args.iter().all(|e| ok(e, name, true)),
            E::Field(e, _) => ok(e, name, true),
            E::Forall { lower, upper, body, .. } => ok(lower, name, false) && ok(upper, name, false) && ok(body, name, false),
            E::SpecCase { value, arms, .. } => ok(value, name, false) && arms.iter().all(|(_, _, e)| ok(e, name, false)),
            E::Array(items) => items.iter().all(|e| ok(e, name, false)),
            E::Repeat(e, _) | E::Unary(_, e) | E::Try(e) | E::Labelled(_, e) | E::Matches(e, _) => ok(e, name, false),
            E::Binary(_, a, b) | E::Index(a, b) => ok(a, name, false) && ok(b, name, false),
            E::Conditional(c, a, b) => ok(c, name, false) && ok(a, name, false) && ok(b, name, false),
            E::Record(fields) => fields.iter().all(|(_, e)| ok(e, name, false)),
            E::Inject(_, payload) => payload.as_ref().is_none_or(|e| ok(e, name, false)),
        }
    }
    fn all(body: &[a::Statement], name: &str) -> bool {
        use a::Statement as S;
        body.iter().all(|s| match s {
            S::Bind { value, .. } | S::Open { value, .. } | S::OpenFields { value, .. } | S::Expr(value) | S::Yield { value, .. } => ok(value, name, false),
            S::Guard { value, failure, .. } => ok(value, name, false) && all(&failure.body, name),
            S::Assign { target, value, .. } => ok(target, name, false) && ok(value, name, false),
            S::Return(value) => value.as_ref().is_none_or(|v| ok(v, name, false)),
            S::While { condition, invariants, body } => ok(condition, name, false) && invariants.iter().all(|e| ok(e, name, false)) && all(body, name),
            S::For { start, end, invariants, body, .. } => ok(start, name, false) && end.as_ref().is_none_or(|e| ok(e, name, false))
                && invariants.iter().all(|e| ok(e, name, false)) && all(body, name),
            S::If { condition, yes, no } => ok(condition, name, false) && all(yes, name) && all(no, name),
            S::Match { value, arms } => ok(value, name, false) && arms.iter().all(|arm| all(&arm.body, name)),
            S::Choose { inner, .. } => all(std::slice::from_ref(inner), name),
            S::Using { value, body, .. } => ok(value, name, false) && all(body, name),
            S::Block(body) => all(body, name),
            S::Dispose { name: held, call } => held != name && ok(call, name, false),
            S::Proof(_) | S::Break | S::Continue | S::Unreachable(_) => true,
            S::Assert(_) => false,
        })
    }
    all(&f.body, name)
}
/// Whether any statement of the block uses `name`.
pub(super) fn block(body: &[a::Statement], name: &str) -> bool { body.iter().any(|s| statement(s, name)) }
fn statement(s: &a::Statement, name: &str) -> bool {
    use a::Statement as S;
    match s {
        S::Bind { value, .. } | S::Open { value, .. } | S::OpenFields { value, .. } | S::Expr(value) => expr(value, name),
        S::Guard { value, failure, .. } => expr(value, name) || block(&failure.body, name),
        S::Assign { target, value, .. } => expr(target, name) || expr(value, name),
        S::Return(value) => value.as_ref().is_some_and(|v| expr(v, name)),
        S::While { condition, invariants, body } => expr(condition, name) || invariants.iter().any(|e| expr(e, name)) || block(body, name),
        S::For { start, end, body, .. } => expr(start, name) || end.as_ref().is_some_and(|e| expr(e, name)) || block(body, name),
        S::If { condition, yes, no } => expr(condition, name) || block(yes, name) || block(no, name),
        S::Match { value, arms } => expr(value, name) || arms.iter().any(|arm| block(&arm.body, name)),
        S::Choose { inner, .. } => statement(inner, name),
        S::Yield { value, .. } => expr(value, name),
        S::Proof(_) | S::Break | S::Continue | S::Unreachable(_) => false,
        S::Using { value, body, .. } => expr(value, name) || block(body, name),
        S::Block(body) => block(body, name),
        S::Dispose { name: held, .. } => held == name,
        // Unread here, so assumed to name everything: the loan only lasts longer.
        S::Assert(_) => true,
    }
}
/// Whether the expression uses `name`.
pub(super) fn mentions(e: &a::Expr, name: &str) -> bool { expr(e, name) }
/// Whether any part of the expression, itself included, satisfies `test`.
pub(super) fn any(e: &a::Expr, test: &mut dyn FnMut(&a::Expr) -> bool) -> bool {
    use a::Expression as E;
    if test(e) { return true; }
    match &e.kind {
        E::Name(_) | E::Unit | E::Integer(_) | E::Float(_) | E::Bool(_) | E::SourceSite(_) => false,
        E::Forall { lower, upper, body, .. } => any(lower, test) || any(upper, test) || any(body, test),
        E::SpecCase { value, arms, .. } => any(value,test) || arms.iter().any(|(_,_,e)|any(e,test)),
        E::Array(items) => items.iter().any(|e| any(e, test)),
        E::Repeat(e, _) | E::Unary(_, e) | E::Try(e) | E::Field(e, _) | E::Labelled(_, e) | E::Matches(e, _) => any(e, test),
        E::Binary(_, a, b) | E::Index(a, b) => any(a, test) || any(b, test),
        E::Conditional(c, a, b) => any(c, test) || any(a, test) || any(b, test),
        E::Call(callee, args) => any(callee, test) || args.iter().any(|e| any(e, test)),
        E::Record(fields) => fields.iter().any(|(_, e)| any(e, test)),
        E::Inject(_, payload) => payload.as_ref().is_some_and(|e| any(e, test)),
    }
}
fn expr(e: &a::Expr, name: &str) -> bool {
    use a::Expression as E;
    match &e.kind {
        E::Name(n) => n == name,
        E::Unit | E::Integer(_) | E::Float(_) | E::Bool(_) | E::SourceSite(_) => false,
        E::Forall { lower, upper, body, .. } => expr(lower, name) || expr(upper, name) || expr(body, name),
        E::SpecCase { value, arms, .. } => expr(value,name) || arms.iter().any(|(_,b,e)|b.as_deref()!=Some(name) && expr(e,name)),
        E::Array(items) => items.iter().any(|e| expr(e, name)),
        E::Repeat(e, _) | E::Unary(_, e) | E::Try(e) | E::Field(e, _) | E::Labelled(_, e) | E::Matches(e, _) => expr(e, name),
        E::Binary(_, a, b) | E::Index(a, b) => expr(a, name) || expr(b, name),
        E::Conditional(c, a, b) => expr(c, name) || expr(a, name) || expr(b, name),
        E::Call(callee, args) => expr(callee, name) || args.iter().any(|e| expr(e, name)),
        E::Record(fields) => fields.iter().any(|(_, e)| expr(e, name)),
        E::Inject(_, payload) => payload.as_ref().is_some_and(|e| expr(e, name)),
    }
}
