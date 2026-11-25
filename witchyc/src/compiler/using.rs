//! `using name = value:` binds an owner for the indented block and discards it on every way
//! out of the block: falling off its end, `return` (a `?` that hands back an error is one),
//! and `break` or `continue` of a loop outside it. An exit that moves the owner away
//! (`return Ok(move name)`) hands it on instead. Witchy 1's structured resource (RFC 0167,
//! section 8), with `Discard` as the disposer.
//!
//! This is a rewrite of the source before checking: the block becomes a scope holding
//! `let name = value`, the body, and a `Dispose` before each exit. The checker turns each
//! `Dispose` into `name.discard()` where the owner is still held, into nothing where every path
//! has moved it away, and refuses one where only some paths have. It then checks the result as
//! any code, so a type without `Discard` is refused there.
use crate::{ast as a, diagnostic::Diagnostic};
use std::collections::BTreeSet;

fn refuse(at: &a::Expr, message: &'static str) -> Diagnostic { Diagnostic::new("W0401", at.span.clone(), message) }

/// The block with each `using` in it expanded, or `None` when it has none.
pub(super) fn expand(body: &[a::Statement], result: &a::Type) -> Result<Option<Vec<a::Statement>>, Diagnostic> {
    if !body.iter().any(|s| matches!(s, a::Statement::Using { .. })) { return Ok(None); }
    let mut out = Vec::with_capacity(body.len());
    for statement in body {
        let a::Statement::Using { mutable, name, ty, value, body: inner } = statement else { out.push(statement.clone()); continue };
        if inner.iter().any(|s| matches!(s, a::Statement::Bind { name: n, .. } | a::Statement::Choose { name: n, .. } if n == name)) {
            return Err(refuse(value, "a `using` block cannot bind its own name again"));
        }
        let hands_on = inner.last().is_some_and(|last| statement_moves(last, name));
        let mut scoped = vec![a::Statement::Bind { mutable: *mutable, name: name.clone(), ty: ty.clone(), value: value.clone(), proof: None }];
        let walked = within(inner, name, value, 0, &BTreeSet::new(), result)?;
        let ends = leaves(&walked);
        scoped.extend(walked);
        if !ends && !hands_on { scoped.push(dispose(name, value)); }
        out.push(a::Statement::Block(scoped));
    }
    Ok(Some(out))
}

/// `name.discard()` if `name` is still held at this point; the checker decides.
fn dispose(name: &str, at: &a::Expr) -> a::Statement {
    let span = at.span.clone();
    let receiver = a::Expr { kind: a::Expression::Name(name.to_string()), span: span.clone() };
    let callee = a::Expr { kind: a::Expression::Field(Box::new(receiver), "discard".into()), span: span.clone() };
    a::Statement::Dispose { name: name.to_string(), call: a::Expr { kind: a::Expression::Call(Box::new(callee), Vec::new()), span } }
}

/// Whether no path falls off the end of the block. A `Yield` at the end of a path gives the
/// value of a `let x = match` outside the block (one inside it is a whole `Choose`), so it leaves.
fn leaves(body: &[a::Statement]) -> bool {
    match body.last() {
        Some(a::Statement::Return(_) | a::Statement::Break | a::Statement::Continue | a::Statement::Unreachable(_) | a::Statement::Yield { .. }) => true,
        Some(a::Statement::If { yes, no, .. }) => !no.is_empty() && leaves(yes) && leaves(no),
        Some(a::Statement::Match { arms, .. }) => !arms.is_empty() && arms.iter().all(|arm| leaves(&arm.body)),
        Some(a::Statement::Block(inner)) => leaves(inner),
        _ => false,
    }
}

/// The block as the checker will see it (`for` and `?` written out, inner `using` expanded),
/// with the discard put before each exit from the `using` block. `loops` counts the loops
/// entered inside the block: a `break` within one of them does not leave it. `chosen` names the
/// `let x = match` inside it, whose values stay inside.
fn within(body: &[a::Statement], name: &str, at: &a::Expr, loops: u32, chosen: &BTreeSet<String>, result: &a::Type) -> Result<Vec<a::Statement>, Diagnostic> {
    let expanded = expand(body, result)?;
    let body = expanded.as_deref().unwrap_or(body);
    let counted = super::iteration::rewrite(body);
    let body = counted.as_deref().unwrap_or(body);
    let rewritten = super::early_exit::rewrite(body)?;
    let body = rewritten.as_deref().unwrap_or(body);
    let mut out = Vec::with_capacity(body.len() + 1);
    for statement in body { one(statement, name, at, loops, chosen, result, &mut out)?; }
    Ok(out)
}

/// One statement of such a block, with the discard put before it if it leaves.
fn one(statement: &a::Statement, name: &str, at: &a::Expr, loops: u32, chosen: &BTreeSet<String>, result: &a::Type, out: &mut Vec<a::Statement>) -> Result<(), Diagnostic> {
        match statement {
            a::Statement::Return(None) => { out.push(dispose(name, at)); out.push(statement.clone()); }
            a::Statement::Return(Some(value)) => {
                if moves(value, name) {
                    out.push(statement.clone());
                } else if super::lending::mentions(value, name) {
                    // The value reads the owner: work it out first, then discard, then return it.
                    let hidden = format!("?using{}.{}", value.span.start, value.span.end);
                    out.push(a::Statement::Bind { mutable: false, name: hidden.clone(), ty: Some(result.clone()), value: value.clone(), proof: None });
                    out.push(dispose(name, at));
                    let held = a::Expr { kind: a::Expression::Name(hidden), span: value.span.clone() };
                    out.push(a::Statement::Return(Some(a::Expr { kind: a::Expression::Unary("move", Box::new(held)), span: value.span.clone() })));
                } else {
                    out.push(dispose(name, at));
                    out.push(statement.clone());
                }
            }
            a::Statement::Break | a::Statement::Continue if loops == 0 => { out.push(dispose(name, at)); out.push(statement.clone()); }
            a::Statement::Yield { name: choice, value } if !chosen.contains(choice) => {
                if moves(value, name) {
                    out.push(statement.clone());
                } else if super::lending::mentions(value, name) {
                    return Err(refuse(value, "a value that leaves a `using` block reads what it holds: bind the value to a name inside the block first"));
                } else {
                    out.push(dispose(name, at));
                    out.push(statement.clone());
                }
            }
            a::Statement::If { condition, yes, no } => out.push(a::Statement::If {
                condition: condition.clone(),
                yes: within(yes, name, at, loops, chosen, result)?,
                no: within(no, name, at, loops, chosen, result)?,
            }),
            a::Statement::Match { value, arms } => {
                let mut walked = Vec::with_capacity(arms.len());
                for arm in arms { walked.push(a::MatchArm { body: within(&arm.body, name, at, loops, chosen, result)?, ..arm.clone() }); }
                out.push(a::Statement::Match { value: value.clone(), arms: walked });
            }
            a::Statement::While { condition, invariants, body } => out.push(a::Statement::While {
                condition: condition.clone(), invariants: invariants.clone(), body: within(body, name, at, loops + 1, chosen, result)?,
            }),
            a::Statement::For { index, binding, start, end, invariants, body } => out.push(a::Statement::For {
                index: index.clone(), binding: binding.clone(), start: start.clone(), end: end.clone(), invariants: invariants.clone(),
                body: within(body, name, at, loops + 1, chosen, result)?,
            }),
            a::Statement::Choose { mutable, name: choice, ty, inner } => {
                let mut inside = chosen.clone();
                inside.insert(choice.clone());
                // The `match` or `if` itself is walked as it stands: the checker writes out a `?` in it.
                let mut walked = Vec::with_capacity(1);
                one(inner, name, at, loops, &inside, result, &mut walked)?;
                let inner = if walked.len() == 1 { walked.remove(0) } else { a::Statement::Block(walked) };
                out.push(a::Statement::Choose { mutable: *mutable, name: choice.clone(), ty: ty.clone(), inner: Box::new(inner) });
            }
            a::Statement::Block(inner) => out.push(a::Statement::Block(within(inner, name, at, loops, chosen, result)?)),
            other => out.push(other.clone()),
        }
    Ok(())
}

/// Whether the statement itself (not a block inside it) moves `name` away.
fn statement_moves(statement: &a::Statement, name: &str) -> bool {
    match statement {
        a::Statement::Bind { value, .. } | a::Statement::Expr(value) | a::Statement::Assign { value, .. } | a::Statement::Return(Some(value))
            | a::Statement::Open { value, .. } | a::Statement::OpenFields { value, .. } => moves(value, name),
        _ => false,
    }
}

/// Whether the expression contains `move name`.
fn moves(e: &a::Expr, name: &str) -> bool {
    super::lending::any(e, &mut |part| matches!(&part.kind, a::Expression::Unary("move", inner) if matches!(&inner.kind, a::Expression::Name(n) if n == name)))
}

#[cfg(test)]
mod tests {
    use crate::ast as a;

    /// The body of `work` after expansion, written as one line per statement kind, nested blocks indented.
    fn shape(source: &str) -> String {
        let program = crate::parser::parse(source).expect("parses");
        let f = program.declarations.iter().find_map(|d| match d { a::Declaration::Function(f) if f.name == "work" => Some(f), _ => None }).expect("work");
        let body = super::expand(&f.body, &f.result).expect("expands").expect("has a using");
        let mut out = String::new();
        fn write(body: &[a::Statement], depth: usize, out: &mut String) {
            for s in body {
                let pad = "  ".repeat(depth);
                match s {
                    a::Statement::Block(inner) => { out.push_str(&format!("{pad}block\n")); write(inner, depth + 1, out); }
                    a::Statement::If { yes, no, .. } => { out.push_str(&format!("{pad}if\n")); write(yes, depth + 1, out); out.push_str(&format!("{pad}else\n")); write(no, depth + 1, out); }
                    a::Statement::While { body, .. } => { out.push_str(&format!("{pad}while\n")); write(body, depth + 1, out); }
                    a::Statement::Bind { name, .. } => out.push_str(&format!("{pad}let {name}\n")),
                    a::Statement::Expr(a::Expr { kind: a::Expression::Call(callee, _), .. }) | a::Statement::Dispose { call: a::Expr { kind: a::Expression::Call(callee, _), .. }, .. } => match &callee.kind {
                        a::Expression::Field(receiver, method) => match &receiver.kind {
                            a::Expression::Name(n) => out.push_str(&format!("{pad}{n}.{method}\n")),
                            _ => out.push_str(&format!("{pad}call\n")),
                        },
                        _ => out.push_str(&format!("{pad}call\n")),
                    },
                    a::Statement::Return(_) => out.push_str(&format!("{pad}return\n")),
                    a::Statement::Break => out.push_str(&format!("{pad}break\n")),
                    a::Statement::Continue => out.push_str(&format!("{pad}continue\n")),
                    _ => out.push_str(&format!("{pad}other\n")),
                }
            }
        }
        write(&body, 0, &mut out);
        out
    }

    #[test]
    fn discards_before_each_exit_and_at_the_end() {
        let source = "fn work(a: U64) -> U64:\n    using t = make():\n        if a == 1:\n            return 1\n        t.look()\n    return 0\n";
        assert_eq!(shape(source), "block\n  let t\n  if\n    t.discard\n    return\n  else\n  t.look\n  t.discard\nreturn\n");
    }

    #[test]
    fn a_return_that_moves_the_owner_hands_it_on() {
        let source = "fn work(a: U64) -> T:\n    using t = make():\n        return move t\n";
        assert_eq!(shape(source), "block\n  let t\n  return\n");
    }

    #[test]
    fn a_return_that_reads_the_owner_is_worked_out_first() {
        let source = "fn work(a: U64) -> U64:\n    using t = make():\n        return t.size()\n";
        assert_eq!(shape(source), "block\n  let t\n  let ?using\n  t.discard\n  return\n".replace("?using", &format!("?using{}.{}", source.find("t.size()").unwrap(), source.find("t.size()").unwrap() + 8)));
    }

    #[test]
    fn a_break_of_an_inner_loop_stays_and_one_of_an_outer_loop_leaves() {
        let source = "fn work(a: U64) -> U64:\n    while true:\n        using t = make():\n            while true:\n                break\n            break\n    return 0\n";
        let program = crate::parser::parse(source).expect("parses");
        let a::Declaration::Function(f) = &program.declarations[0] else { panic!() };
        let a::Statement::While { body, .. } = &f.body[0] else { panic!() };
        let expanded = super::expand(body, &f.result).unwrap().unwrap();
        let a::Statement::Block(inner) = &expanded[0] else { panic!() };
        // let t; inner loop with its break alone; then the discard before the outer break.
        assert!(matches!(&inner[1], a::Statement::While { body, .. } if matches!(body[..], [a::Statement::Break])));
        assert!(matches!(&inner[2], a::Statement::Dispose { .. }));
        assert!(matches!(&inner[3], a::Statement::Break));
        assert_eq!(inner.len(), 4);
    }
}
