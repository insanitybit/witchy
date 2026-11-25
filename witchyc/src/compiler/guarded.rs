//! Counters whose every `+= 1` is guarded by its loop's condition, so the increment cannot
//! overflow and needs no check (`Function::steps`, as a counting `for`'s own counter).
//!
//! `while i < limit && ...:` with `i += 1` in the body: each pass starts with `i < limit`
//! true, and when the body changes `i` only by that one statement, outside any loop inside it,
//! `i` is incremented at most once after that test, so `i + 1 <= limit` holds, and `limit`
//! fits the type. Anything that could change `i` another way disqualifies the name: a second
//! assignment, `&mut i`, or a method called on `i`, in the condition or anywhere in the body.
//! A name qualifies only if every `+= 1` of it in the function is guarded so; locals are never
//! shadowed, so a name is one variable within its scope, and two sibling variables of one name
//! each meet the rule or neither is taken.

use crate::ast::{self as a, Expression, Statement};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn counters(body: &[Statement]) -> BTreeSet<String> {
    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    walk(body, &BTreeSet::new(), &mut counts);
    counts.into_iter().filter(|(_, (all, guarded))| *all > 0 && all == guarded).map(|(name, _)| name).collect()
}

/// `i += 1` of a plain name.
fn increment(statement: &Statement) -> Option<&str> {
    match statement {
        Statement::Assign { target, operation: "+=", value } if matches!(value.kind, Expression::Integer(1)) => match &target.kind {
            Expression::Name(name) => Some(name),
            _ => None,
        },
        _ => None,
    }
}

fn walk(body: &[Statement], guards: &BTreeSet<String>, counts: &mut BTreeMap<String, (usize, usize)>) {
    for statement in body {
        // Every `name += value` counts, whatever the value: the emitter skips the check for any
        // whose value checks to the number 1 (`i += ONE` too, were constants folded), so each must
        // be a guarded literal `+= 1` for the name to qualify (review: the two tests of "1" differed).
        if let Statement::Assign { target, operation: "+=", .. } = statement && let Expression::Name(name) = &target.kind {
            let entry = counts.entry(name.clone()).or_default();
            entry.0 += 1;
            if increment(statement) == Some(name.as_str()) && guards.contains(name) { entry.1 += 1; }
        }
        match statement {
            Statement::While { condition, body, .. } => {
                let mut bounded = BTreeSet::new();
                conjuncts(condition, &mut bounded);
                bounded.retain(|name| only_written_by_increment(condition, body, name));
                // An outer loop's guard does not hold here: this body may run many times per pass.
                walk(body, &bounded, counts);
            }
            Statement::For { body, .. } => walk(body, &BTreeSet::new(), counts),
            Statement::If { yes, no, .. } => { walk(yes, guards, counts); walk(no, guards, counts); }
            Statement::Match { arms, .. } => for arm in arms { walk(&arm.body, guards, counts); },
            Statement::Guard { failure, .. } => walk(&failure.body, guards, counts),
            Statement::Using { body, .. } | Statement::Block(body) => walk(body, guards, counts),
            Statement::Choose { inner, .. } => walk(std::slice::from_ref(&**inner), guards, counts),
            _ => {}
        }
    }
}

/// Names `n` with `n < x` as a conjunct of a `&&` chain: each is true whenever the body runs.
fn conjuncts(condition: &a::Expr, found: &mut BTreeSet<String>) {
    match &condition.kind {
        Expression::Binary("&&", left, right) => { conjuncts(left, found); conjuncts(right, found); }
        Expression::Binary("<", left, _) => if let Expression::Name(name) = &left.kind { found.insert(name.clone()); },
        // The same test written the other way round (Astra audit: spelling should not matter).
        Expression::Binary(">", _, right) => if let Expression::Name(name) = &right.kind { found.insert(name.clone()); },
        _ => {}
    }
}

/// Whether the one statement in `body` that writes `name` is a `name += 1` outside any loop
/// within it, and nothing in `condition` or `body` can change `name` another way.
fn only_written_by_increment(condition: &a::Expr, body: &[Statement], name: &str) -> bool {
    let (mut writes, mut direct) = (0usize, 0usize);
    count_writes(body, name, true, &mut writes, &mut direct);
    if writes != 1 || direct != 1 { return false; }
    let mut changes = false;
    let mut check = |e: &mut a::Expr| {
        match &e.kind {
            Expression::Unary("&mut", inner) if roots_at(inner, name) => changes = true,
            Expression::Call(callee, _) if matches!(&callee.kind, Expression::Field(receiver, _) if roots_at(receiver, name)) => changes = true,
            _ => {}
        }
        false
    };
    condition.clone().visit_mut(&mut check);
    for statement in body { statement.clone().visit_exprs_mut(&mut check); }
    !changes
}

/// Statements that assign `name` or bind it again, anywhere in `body`; `direct` counts the
/// `name += 1` statements outside any loop inside `body`.
fn count_writes(body: &[Statement], name: &str, outside_loops: bool, writes: &mut usize, direct: &mut usize) {
    for statement in body {
        match statement {
            Statement::Assign { target, .. } if roots_at(target, name) => {
                *writes += 1;
                if outside_loops && increment(statement) == Some(name) { *direct += 1; }
            }
            Statement::Bind { name: bound, .. } | Statement::Using { name: bound, .. } | Statement::Choose { name: bound, .. } if bound == name => *writes += 1,
            Statement::Guard { binding, .. } if binding == name => *writes += 1,
            Statement::Open { names, .. } if names.iter().any(|n| n == name) => *writes += 1,
            Statement::OpenFields { fields, .. } if fields.iter().any(|(_, n)| n == name) => *writes += 1,
            _ => {}
        }
        match statement {
            Statement::While { body, .. } | Statement::For { body, .. } => count_writes(body, name, false, writes, direct),
            Statement::If { yes, no, .. } => { count_writes(yes, name, outside_loops, writes, direct); count_writes(no, name, outside_loops, writes, direct); }
            Statement::Match { arms, .. } => for arm in arms { count_writes(&arm.body, name, outside_loops, writes, direct); },
            Statement::Guard { failure, .. } => count_writes(&failure.body, name, outside_loops, writes, direct),
            Statement::Using { body, .. } | Statement::Block(body) => count_writes(body, name, outside_loops, writes, direct),
            Statement::Choose { inner, .. } => count_writes(std::slice::from_ref(&**inner), name, outside_loops, writes, direct),
            _ => {}
        }
    }
}

/// Whether `e` is `name`, or a field or element reached from it.
fn roots_at(e: &a::Expr, name: &str) -> bool {
    match &e.kind {
        Expression::Name(n) => n == name,
        Expression::Field(inner, _) | Expression::Index(inner, _) | Expression::Unary("*", inner) => roots_at(inner, name),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::counters;
    fn of(source: &str) -> Vec<String> {
        let program = crate::compiler::parse(source).expect("parses");
        let function = program.declarations.iter().find_map(|d| match d { crate::ast::Declaration::Function(f) if f.name == "f" => Some(f), _ => None }).expect("f");
        counters(&function.body).into_iter().collect()
    }
    #[test]
    fn a_while_counter_bounded_by_its_condition() {
        assert_eq!(of("fn f(n: U64) -> U64:\n    var i: U64 = 0\n    while i < n:\n        i += 1\n    return i\n"), ["i"]);
        assert_eq!(of("fn f(n: U64) -> U64:\n    var i: U64 = 0\n    while n > i:\n        i += 1\n    return i\n"), ["i"]);
        // `i > n` bounds nothing above.
        assert!(of("fn f(n: U64) -> U64:\n    var i: U64 = n\n    while i > n:\n        i += 1\n    return i\n").is_empty());
        // Any conjunct of the condition.
        assert_eq!(of("fn f(n: U64) -> U64:\n    var i: U64 = 0\n    while n > 3 && i < 50:\n        if n > 4:\n            i += 1\n        else:\n            i += 1\n    return i\n"), Vec::<String>::new(), "two writes");
        assert_eq!(of("fn f(n: U64) -> U64:\n    var i: U64 = 0\n    while n > 3 && i < 50:\n        if n > 4:\n            i += 1\n    return i\n"), ["i"]);
    }
    #[test]
    fn anything_else_that_could_change_it_disqualifies() {
        // Incremented twice a pass, inside a nested loop, or set as well.
        assert!(of("fn f(n: U64) -> U64:\n    var i: U64 = 0\n    while i < n:\n        i += 1\n        i += 1\n    return i\n").is_empty());
        assert!(of("fn f(n: U64) -> U64:\n    var i: U64 = 0\n    while i < n:\n        while n > 1:\n            i += 1\n    return i\n").is_empty());
        assert!(of("fn f(n: U64) -> U64:\n    var i: U64 = 0\n    while i < n:\n        i = n\n        i += 1\n    return i\n").is_empty());
        // Lent mutably, in the body or the condition.
        assert!(of("fn g(x: &mut U64):\n    *x = 7\nfn f(n: U64) -> U64:\n    var i: U64 = 0\n    while i < n:\n        g(&mut i)\n        i += 1\n    return i\n").is_empty());
        // Not guarded: an `||`, or `<=`, or an increment outside the loop as well.
        assert!(of("fn f(n: U64) -> U64:\n    var i: U64 = 0\n    while i < n || n > 9:\n        i += 1\n    return i\n").is_empty());
        assert!(of("fn f(n: U64) -> U64:\n    var i: U64 = 0\n    while i <= n:\n        i += 1\n    return i\n").is_empty());
        assert!(of("fn f(n: U64) -> U64:\n    var i: U64 = 0\n    while i < n:\n        i += 1\n    i += 1\n    return i\n").is_empty());
        // Any other `+=` of the name, even one whose value is 1 once checked.
        assert!(of("fn f(n: U64) -> U64:\n    var i: U64 = 0\n    while i < n:\n        i += 1\n    i += n / n\n    return i\n").is_empty());
        assert!(of("fn f(n: U64) -> U64:\n    var i: U64 = 0\n    while i < n:\n        i += 1\n    i += (2 - 1)\n    return i\n").is_empty());
    }
}
