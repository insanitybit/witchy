//! `for item in g(arguments):` over a `gen fn` that can run in place: the generator's body as
//! written, its names made hidden ones, each `yield value` the loop's body with `item` bound to
//! the value. No record is made and nothing is saved between steps: the generator's locals are
//! the loop's locals (docs/wasm-generators.md, "A `for` over a generator").
//!
//! The shape, with `stop` a hidden Bool:
//!
//!     let parameters = arguments; var stop = false
//!     while true:
//!         <the generator's body, where `yield value` is
//!             let item = value
//!             while true:  <the loop's body: `continue` is `break`, `break` is `stop = true; break`>
//!                          break
//!             if stop: break
//!          a `return` of the generator is `stop = true; break`, and after each loop of the
//!          generator `if stop: break` leaves the next one out>
//!         break
//!
//! Nothing here is trusted: the result is checked as code the program wrote. The checker
//! decides whether to use it (check.rs: not in a function with a proof, and every name the
//! generator's body calls or reads is one the loop's function may call or read).
use crate::ast::{self as a, Expression, Statement};
use std::collections::BTreeSet;

/// What the checker allows of names the generator's body does not bind.
pub(super) struct Allowed<'a> {
    /// A free name (a function, a constant, a type) read or called.
    pub free: &'a dyn Fn(&str) -> bool,
    /// `Type.method(...)`.
    pub static_call: &'a dyn Fn(&str, &str) -> bool,
    /// `value.method(...)` on a value: allowed only when the generator is the loop's module's.
    pub value_calls: bool,
}

pub(super) fn inline(generator: &a::Function, arguments: Vec<a::Expr>, binding: &str, body: &[Statement], span: &std::ops::Range<usize>, allowed: &Allowed) -> Option<Vec<Statement>> {
    if arguments.len() != generator.parameters.len() { return None; }
    let prefix = format!("gen#{}.", span.start);
    let mut bound: BTreeSet<String> = generator.parameters.iter().map(|(n, _)| n.clone()).collect();
    if !binds(&generator.body, &mut bound) { return None; }
    // The declared type of each parameter, by name: a call on one is judged as `Type.method(...)`.
    let typed: std::collections::BTreeMap<String, String> = generator.parameters.iter().filter_map(|(n, ty)| {
        let named = match ty { a::Type::Borrow { inner, .. } => &**inner, other => other };
        match named { a::Type::Named(t, args) if args.is_empty() => Some((n.clone(), t.clone())), _ => None }
    }).collect();
    if !names_allowed(&generator.body, &bound, &typed, allowed) { return None; }
    // Another module's types may be private to it: written types are numbers and Bools then.
    if !allowed.value_calls && !typed_plainly(&generator.body) { return None; }
    let rename = |n: &str| format!("{prefix}{n}");
    // Not `prefix` + a name: a generator name renamed is that, and `stop` is one (review).
    let stop = format!("gen#{}#stop", span.start);
    let e = |kind| a::Expr { kind, span: span.clone() };
    let mut out = Vec::new();
    for ((name, ty), value) in generator.parameters.iter().zip(arguments) {
        out.push(Statement::Bind { mutable: false, name: rename(name), ty: Some(ty.clone()), value, proof: None });
    }
    out.push(Statement::Bind { mutable: true, name: stop.clone(), ty: Some(a::Type::Named("Bool".into(), Vec::new())), value: e(Expression::Bool(false)), proof: None });
    let context = Context { bound: &bound, prefix: &prefix, stop: &stop, binding, body, element: &generator.result, span };
    let mut inner = context.generator_block(&generator.body)?;
    inner.push(Statement::Break);
    out.push(Statement::While { condition: e(Expression::Bool(true)), invariants: Vec::new(), body: inner });
    Some(out)
}

/// Every name the statements bind, into `out`; false for a form not run in place.
fn binds(statements: &[Statement], out: &mut BTreeSet<String>) -> bool {
    for statement in statements {
        let ok = match statement {
            Statement::Bind { name, .. } => { out.insert(name.clone()); true }
            Statement::Assign { .. } | Statement::Return(_) | Statement::Expr(_) | Statement::Break | Statement::Continue | Statement::Yield { .. } => true,
            Statement::If { yes, no, .. } => binds(yes, out) && binds(no, out),
            Statement::While { body, invariants, .. } => invariants.is_empty() && binds(body, out),
            Statement::For { index, binding, body, invariants, .. } => {
                out.insert(binding.clone());
                if let Some(index) = index { out.insert(index.clone()); }
                invariants.is_empty() && binds(body, out)
            }
            Statement::Match { arms, .. } => arms.iter().all(|arm| {
                if let Some(b) = &arm.binding { out.insert(b.clone()); }
                arm.nested.is_none() && arm.open.is_empty() && binds(&arm.body, out)
            }),
            Statement::Block(body) => binds(body, out),
            // Anything else (proofs, `using`, guards, openings, ...) stays a generator.
            _ => false,
        };
        if !ok { return false; }
    }
    true
}

fn names_allowed(statements: &[Statement], bound: &BTreeSet<String>, typed: &std::collections::BTreeMap<String, String>, allowed: &Allowed) -> bool {
    fn expr(e: &a::Expr, bound: &BTreeSet<String>, typed: &std::collections::BTreeMap<String, String>, allowed: &Allowed) -> bool {
        let mut ok = true;
        e.clone().visit_mut(&mut |e| {
            match &e.kind {
                Expression::Call(callee, arguments) => if let Expression::Field(receiver, method) = &callee.kind {
                    match &receiver.kind {
                        // `Type.method(...)`: the owner is judged as the call, not as a free name.
                        Expression::Name(owner) if !bound.contains(owner) => {
                            if !(allowed.static_call)(owner, method) || !arguments.iter().all(|argument| expr(argument, bound, typed, allowed)) { ok = false; }
                            return true;
                        }
                        // `parameter.method(...)` on a parameter of a declared type: as that type's call.
                        Expression::Name(n) if typed.contains_key(n) => {
                            if !allowed.value_calls && !(allowed.static_call)(&typed[n], method) { ok = false; }
                        }
                        _ => { if !allowed.value_calls { ok = false; } }
                    }
                },
                Expression::Name(n) if !bound.contains(n) => { if !(allowed.free)(n) { ok = false; } }
                _ => {}
            }
            false
        });
        ok
    }
    let mut ok = true;
    for statement in statements {
        statement.clone().visit_exprs_mut(&mut |e| { if !expr(e, bound, typed, allowed) { ok = false; } true });
    }
    ok
}

struct Context<'a> {
    bound: &'a BTreeSet<String>,
    prefix: &'a str,
    stop: &'a str,
    binding: &'a str,
    body: &'a [Statement],
    element: &'a a::Type,
    span: &'a std::ops::Range<usize>,
}

impl Context<'_> {
    fn e(&self, kind: Expression) -> a::Expr { a::Expr { kind, span: self.span.clone() } }
    fn hidden(&self, n: &str) -> String { if self.bound.contains(n) { format!("{}{n}", self.prefix) } else { n.to_owned() } }
    fn expr(&self, e: &a::Expr) -> a::Expr {
        let mut e = e.clone();
        e.visit_mut(&mut |e| { if let Expression::Name(n) = &mut e.kind && self.bound.contains(n.as_str()) { *n = format!("{}{n}", self.prefix); } false });
        e
    }
    /// `stop = true; break`.
    fn leave(&self) -> Vec<Statement> {
        vec![Statement::Assign { target: self.e(Expression::Name(self.stop.to_owned())), operation: "=", value: self.e(Expression::Bool(true)) }, Statement::Break]
    }
    fn stopped(&self) -> Statement {
        Statement::If { condition: self.e(Expression::Name(self.stop.to_owned())), yes: vec![Statement::Break], no: Vec::new() }
    }
    fn generator_block(&self, statements: &[Statement]) -> Option<Vec<Statement>> {
        let mut out = Vec::new();
        for statement in statements {
            match statement {
                Statement::Yield { value, .. } => {
                    // Bound inside the one-pass loop: two `yield`s in a block bind it in two scopes.
                    let mut once = vec![Statement::Bind { mutable: false, name: self.binding.to_owned(), ty: Some(self.element.clone()), value: self.expr(value), proof: None }];
                    once.extend(self.user_block(self.body)?);
                    once.push(Statement::Break);
                    out.push(Statement::While { condition: self.e(Expression::Bool(true)), invariants: Vec::new(), body: once });
                    out.push(self.stopped());
                }
                Statement::Return(_) => out.extend(self.leave()),
                Statement::Bind { mutable, name, ty, value, proof } => out.push(Statement::Bind { mutable: *mutable, name: self.hidden(name), ty: ty.clone(), value: self.expr(value), proof: proof.clone() }),
                Statement::Assign { target, operation, value } => out.push(Statement::Assign { target: self.expr(target), operation, value: self.expr(value) }),
                Statement::Expr(value) => out.push(Statement::Expr(self.expr(value))),
                Statement::Break => out.push(Statement::Break),
                Statement::Continue => out.push(Statement::Continue),
                Statement::If { condition, yes, no } => out.push(Statement::If { condition: self.expr(condition), yes: self.generator_block(yes)?, no: self.generator_block(no)? }),
                Statement::While { condition, body, .. } => {
                    out.push(Statement::While { condition: self.expr(condition), invariants: Vec::new(), body: self.generator_block(body)? });
                    out.push(self.stopped());
                }
                Statement::For { index, binding, start, end, body, .. } => {
                    out.push(Statement::For { index: index.as_ref().map(|i| self.hidden(i)), binding: self.hidden(binding), start: self.expr(start), end: end.as_ref().map(|end| self.expr(end)), invariants: Vec::new(), body: self.generator_block(body)? });
                    out.push(self.stopped());
                }
                Statement::Match { value, arms } => {
                    let mut lowered = Vec::new();
                    for arm in arms {
                        lowered.push(a::MatchArm { variant: arm.variant.clone(), binding: arm.binding.as_ref().map(|b| self.hidden(b)), body: self.generator_block(&arm.body)?, nested: None, open: Vec::new() });
                    }
                    out.push(Statement::Match { value: self.expr(value), arms: lowered });
                }
                Statement::Block(body) => out.push(Statement::Block(self.generator_block(body)?)),
                _ => return None,
            }
        }
        Some(out)
    }
    /// The loop's own body, inside a one-pass `while`: its `continue` ends the pass, its `break`
    /// stops the generator too. Loops of its own keep their `break` and `continue`.
    /// None when a `break` or `continue` of the loop sits in a form not rewritten here.
    fn user_block(&self, statements: &[Statement]) -> Option<Vec<Statement>> {
        let mut out = Vec::new();
        for statement in statements {
            match statement {
                Statement::Continue => out.push(Statement::Break),
                Statement::Break => out.extend(self.leave()),
                Statement::If { condition, yes, no } => out.push(Statement::If { condition: condition.clone(), yes: self.user_block(yes)?, no: self.user_block(no)? }),
                Statement::Match { value, arms } => {
                    let mut lowered = Vec::new();
                    for arm in arms { lowered.push(a::MatchArm { body: self.user_block(&arm.body)?, ..arm.clone() }); }
                    out.push(Statement::Match { value: value.clone(), arms: lowered });
                }
                Statement::Block(body) => out.push(Statement::Block(self.user_block(body)?)),
                Statement::Using { mutable, name, ty, value, body } => out.push(Statement::Using { mutable: *mutable, name: name.clone(), ty: ty.clone(), value: value.clone(), body: self.user_block(body)? }),
                Statement::Guard { value, variant, binding, ty, failure } => out.push(Statement::Guard { value: value.clone(), variant: variant.clone(), binding: binding.clone(), ty: ty.clone(), failure: a::MatchArm { body: self.user_block(&failure.body)?, ..failure.clone() } }),
                // Loops of the body's own keep their `break` and `continue`.
                Statement::While { .. } | Statement::For { .. } => out.push(statement.clone()),
                other => { if exits(other) { return None; } out.push(other.clone()); }
            }
        }
        Some(out)
    }
}

/// Whether a `break` or `continue` outside any loop is somewhere in this statement.
fn exits(statement: &Statement) -> bool {
    match statement {
        Statement::Break | Statement::Continue => true,
        Statement::While { .. } | Statement::For { .. } => false,
        Statement::If { yes, no, .. } => yes.iter().chain(no).any(exits),
        Statement::Match { arms, .. } => arms.iter().any(|arm| arm.body.iter().any(exits)),
        Statement::Block(body) | Statement::Using { body, .. } => body.iter().any(exits),
        Statement::Guard { failure, .. } => failure.body.iter().any(exits),
        Statement::Choose { inner, .. } => exits(inner),
        _ => false,
    }
}

fn typed_plainly(statements: &[Statement]) -> bool {
    let plain = |ty: &Option<a::Type>| ty.as_ref().is_none_or(|t| matches!(t, a::Type::Named(n, args) if args.is_empty() && matches!(n.as_str(), "U8" | "U16" | "U32" | "U64" | "I64" | "F64" | "Bool")));
    statements.iter().all(|statement| match statement {
        Statement::Bind { ty, .. } => plain(ty),
        Statement::If { yes, no, .. } => typed_plainly(yes) && typed_plainly(no),
        Statement::While { body, .. } | Statement::For { body, .. } | Statement::Block(body) => typed_plainly(body),
        Statement::Match { arms, .. } => arms.iter().all(|arm| typed_plainly(&arm.body)),
        _ => true,
    })
}
