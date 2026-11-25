//! `?` and a refutable `let` are a match whose success arm holds the rest of the block.
use crate::{ast as a, diagnostic::Diagnostic};

/// The name of the error a `?` hands back. No source name can spell it.
pub(super) const ERROR: &str = "?error";

fn refuse(at: &a::Expr, message: &'static str) -> Diagnostic { Diagnostic::new("W0401", at.span.clone(), message) }

/// Reading it runs nothing: a name, a literal, a field, a borrow or a move of one.
fn quiet(expr: &a::Expr) -> bool {
    match &expr.kind {
        a::Expression::Name(_) | a::Expression::Integer(_) | a::Expression::Bool(_) | a::Expression::Unit => true,
        a::Expression::Field(value, _) | a::Expression::Unary("&" | "&mut" | "*" | "move", value) => quiet(value),
        _ => false,
    }
}

/// The first `?` that runs in `expr`, taken out: its place holds a move of a hidden name, returned with its operand.
/// The name carries the position of that `?`, so two in one statement do not meet.
/// `ran` says what runs before this point that is not quiet: `.0` something with an effect (a
/// call), `.1` arithmetic that has none but may trap. A `?` after either would be moved ahead
/// of it; an `if` value only after an effect, or after a possible trap when the `if` itself
/// calls something (otherwise moving it changes at most which trap stops the run).
fn lift(expr: &mut a::Expr, ran: &mut (bool, bool)) -> Result<Option<(String, a::Expr)>, Diagnostic> {
    // An inline `if`: its condition runs first, then one of its values. It is taken out whole
    // and becomes a `Choose` of the hidden name before the statement.
    if let a::Expression::Conditional(condition, _, _) = &mut expr.kind {
        let before = *ran;
        if let Some(found) = lift(condition, ran)? { return Ok(Some(found)); }
        *ran = before;
        if ran.0 { return Err(refuse(expr, "this `if` value runs after an earlier call in the same statement: bind that call to a name first")); }
        if ran.1 && calls(expr) { return Err(refuse(expr, "this `if` value calls something or uses `?` after arithmetic that may trap earlier in the same statement: bind one of them to a name first")); }
        let hidden = format!("?if{}.{}", expr.span.start, expr.span.end);
        let name = a::Expr { kind: a::Expression::Name(hidden.clone()), span: expr.span.clone() };
        let taken = std::mem::replace(expr, a::Expr { kind: a::Expression::Unary("move", Box::new(name)), span: expr.span.clone() });
        return Ok(Some((hidden, taken)));
    }
    if let a::Expression::Try(inner) = &mut expr.kind {
        // What runs inside the operand moves with it, so only what ran before the
        // operand began could be overtaken.
        let before = *ran;
        if let Some(found) = lift(inner, ran)? { return Ok(Some(found)); }
        *ran = before;
        if ran.0 { return Err(refuse(expr, "this `?` runs after an earlier call in the same statement: bind that call to a name first")); }
        if ran.1 { return Err(refuse(expr, "this `?` runs after arithmetic that may trap earlier in the same statement: bind that to a name first")); }
        let hidden = format!("?try{}.{}", expr.span.start, expr.span.end);
        let name = a::Expr { kind: a::Expression::Name(hidden.clone()), span: expr.span.clone() };
        let taken = std::mem::replace(expr, a::Expr { kind: a::Expression::Unary("move", Box::new(name)), span: expr.span.clone() });
        let a::Expression::Try(inner) = taken.kind else { unreachable!() };
        return Ok(Some((hidden, *inner)));
    }
    let mut parts: Vec<&mut a::Expr> = Vec::new();
    match &mut expr.kind {
        a::Expression::Call(callee, args) => {
            if let a::Expression::Field(receiver, _) = &mut callee.kind { parts.push(receiver); }
            parts.extend(args.iter_mut());
        }
        a::Expression::Binary(op, left, right) => {
            parts.push(left);
            // The right side of `and` and `or` may not run at all.
            if matches!(*op, "and" | "or" | "&&" | "||") {
                if holds(right) { return Err(refuse(right, "`?` or an `if` value on the right of `and` or `or` may not run: use an `if` statement")); }
            } else { parts.push(right); }
        }
        a::Expression::Unary(_, value) | a::Expression::Field(value, _) => parts.push(value),
        a::Expression::Index(value, index) => { parts.push(value); parts.push(index); }
        a::Expression::Record(fields) => parts.extend(fields.iter_mut().map(|(_, value)| value)),
        a::Expression::Array(values) => parts.extend(values.iter_mut()),
        a::Expression::Inject(_, Some(value)) => parts.push(value),
        _ => {}
    }
    for part in parts {
        if let Some(found) = lift(part, ran)? { return Ok(Some(found)); }
        if !quiet(part) { if pure(part) { ran.1 = true; } else { ran.0 = true; } }
    }
    Ok(None)
}

/// Arithmetic, comparison and indexing over quiet values: no effect, though it may trap.
fn pure(expr: &a::Expr) -> bool {
    match &expr.kind {
        a::Expression::Binary(_, left, right) | a::Expression::Index(left, right) => pure(left) && pure(right),
        a::Expression::Unary("-" | "!" | "~", value) | a::Expression::Field(value, _) => pure(value),
        a::Expression::Float(_) => true,
        _ => quiet(expr),
    }
}

/// Whether anything that acts runs anywhere in `expr`: a call, or a `?`, which may return
/// (red-team review: a `?` in an `if` moved ahead of a division by zero returned instead of
/// trapping).
fn calls(expr: &a::Expr) -> bool {
    let mut found = false;
    let mut probe = expr.clone();
    probe.visit_mut(&mut |e| { if matches!(e.kind, a::Expression::Call(..) | a::Expression::Try(..)) { found = true; } false });
    found
}

/// Whether every way through `body` leaves the enclosing block.
fn leaves(body: &[a::Statement]) -> bool {
    match body.last() {
        Some(a::Statement::Return(_) | a::Statement::Break | a::Statement::Continue | a::Statement::Unreachable(_)) => true,
        Some(a::Statement::If { yes, no, .. }) => leaves(yes) && leaves(no),
        Some(a::Statement::Match { arms, .. }) => !arms.is_empty() && arms.iter().all(|arm| leaves(&arm.body)),
        Some(a::Statement::Block(inner)) => leaves(inner),
        _ => false,
    }
}

fn holds(expr: &a::Expr) -> bool {
    match &expr.kind {
        a::Expression::Try(_) | a::Expression::Conditional(..) => true,
        // A callee is looked into only as a method's receiver, as `lift` and `spill` take it: a
        // `?` or `if` that computes the function itself is left to the checker to refuse
        // (red-team review: it reached `lift` and found nothing to take out).
        a::Expression::Call(callee, args) => matches!(&callee.kind, a::Expression::Field(receiver, _) if holds(receiver)) || args.iter().any(holds),
        a::Expression::Binary(_, left, right) | a::Expression::Index(left, right) => holds(left) || holds(right),
        a::Expression::Unary(_, value) | a::Expression::Field(value, _) | a::Expression::Inject(_, Some(value)) | a::Expression::Labelled(_, value) => holds(value),
        a::Expression::Record(fields) => fields.iter().any(|(_, value)| holds(value)),
        a::Expression::Array(values) => values.iter().any(holds),
        _ => false,
    }
}

/// Rewrites the first early exit in `body`; later ones are met when the success arm is checked.
pub(super) fn rewrite(body: &[a::Statement]) -> Result<Option<Vec<a::Statement>>, Diagnostic> {
    let exits = |s: &a::Statement| match s {
        a::Statement::Guard { .. } => true,
        a::Statement::Yield { value, .. } => holds(value),
        a::Statement::Choose { inner, .. } => matches!(&**inner, a::Statement::Match { value, .. } | a::Statement::If { condition: value, .. } if holds(value)),
        a::Statement::Bind { value, .. } | a::Statement::Expr(value) | a::Statement::Assign { value, .. } | a::Statement::Return(Some(value))
            | a::Statement::Match { value, .. } | a::Statement::If { condition: value, .. } | a::Statement::OpenFields { value, .. } | a::Statement::Open { value, .. } => holds(value),
        a::Statement::While { condition, .. } => holds(condition),
        _ => false,
    };
    let Some(at) = body.iter().position(exits) else { return Ok(None) };
    // What runs before the `?` or `if` value taken out, bound to hidden names first (`spill`).
    let mut spilled: Vec<a::Statement> = Vec::new();
    // `while condition?:` asks again before each pass, so the `?` cannot move ahead of the loop:
    // the loop runs until an `if` at its head, which holds the `?`, leaves it.
    if let a::Statement::While { condition, invariants, body: inner } = &body[at] {
        let span = condition.span.clone();
        let done = a::Expr { kind: a::Expression::Unary("!", Box::new(condition.clone())), span: span.clone() };
        let mut passes = vec![a::Statement::If { condition: done, yes: vec![a::Statement::Break], no: vec![] }];
        passes.extend(inner.iter().cloned());
        let mut out = body[..at].to_vec();
        out.push(a::Statement::While { condition: a::Expr { kind: a::Expression::Bool(true), span }, invariants: invariants.clone(), body: passes });
        // What follows the loop is in no arm of it: its own first exit is written out now, so a
        // walk of this block (a `using` placing its discards) sees it (review: the `?` after a
        // `while condition?:` in a `using` returned without the discard).
        let rest = &body[at + 1..];
        out.extend(rewrite(rest)?.unwrap_or_else(|| rest.to_vec()));
        return Ok(Some(out));
    }
    let rest = body[at + 1..].to_vec();
    let arm = |binding: Option<String>, body| a::MatchArm { variant: "Ok".into(), binding, body, nested: None, open: vec![] };
    let (value, success, failure) = match &body[at] {
        // With its type written, a guard is `let name: Type = match value: Variant(x): x ...`,
        // so the type reaches a generic call in `value`; the rest of the block follows it.
        a::Statement::Guard { value, variant, binding, ty: Some(ty), failure } => {
            // The checker of `let name = match` takes every arm to give the value or leave.
            if !leaves(&failure.body) { return Err(refuse(value, "the failure arm of a refutable `let` leaves and cannot fall through: end it with `return`, `break`, `continue` or `unreachable`")); }
            let hidden = format!("{binding}#guard");
            let named = a::Expr { kind: a::Expression::Name(hidden.clone()), span: value.span.clone() };
            if matches!(ty, a::Type::Borrow { .. }) {
                // A reference payload (`let Ok(node): &mut Node = nodes.at_mut(i) else ...`): a
                // `match` cannot give a reference bound in one of its arms, and `move` of a
                // reference is refused; so the rest of the block is the success arm, opening with
                // the name bound at its written type.
                let rest = rewrite(&rest)?.unwrap_or(rest);
                let mut inner = vec![a::Statement::Bind { mutable: false, name: binding.clone(), ty: Some(ty.clone()), value: named, proof: None }];
                inner.extend(rest);
                let success = a::MatchArm { variant: variant.clone(), binding: Some(hidden), body: inner, nested: None, open: vec![] };
                let mut out = body[..at].to_vec();
                out.push(a::Statement::Match { value: value.clone(), arms: vec![success, failure.clone()] });
                return Ok(Some(out));
            }
            let yielded = a::Expr { kind: a::Expression::Unary("move", Box::new(named)), span: value.span.clone() };
            let success = a::MatchArm { variant: variant.clone(), binding: Some(hidden), body: vec![a::Statement::Yield { name: binding.clone(), value: yielded }], nested: None, open: vec![] };
            let chosen = a::Statement::Choose { mutable: false, name: binding.clone(), ty: Some(ty.clone()),
                inner: Box::new(a::Statement::Match { value: value.clone(), arms: vec![success, failure.clone()] }) };
            let mut out = body[..at].to_vec();
            out.push(chosen);
            // The rest stays in this block, so its own early exits are rewritten here too.
            let rest = rewrite(&rest)?.unwrap_or(rest);
            out.extend(rest);
            return Ok(Some(out));
        }
        a::Statement::Guard { value, failure, ty: None, .. } if !leaves(&failure.body) =>
            return Err(refuse(value, "the failure arm of a refutable `let` leaves and cannot fall through: end it with `return`, `break`, `continue` or `unreachable`")),
        a::Statement::Guard { value, variant, binding, ty: None, failure } =>
            (value.clone(), a::MatchArm { variant: variant.clone(), binding: Some(binding.clone()), body: rest, nested: None, open: vec![] }, failure.clone()),
        // `let name = value?` and `value?` alone: the success arm binds the name itself.
        a::Statement::Bind { mutable: false, name, ty: None, value, proof: None } if matches!(&value.kind, a::Expression::Try(inner) if !holds(inner)) => {
            let a::Expression::Try(inner) = &value.kind else { unreachable!() };
            ((**inner).clone(), arm(Some(name.clone()), rest), hand_back(value))
        }
        a::Statement::Expr(value) if matches!(&value.kind, a::Expression::Try(inner) if !holds(inner)) => {
            let a::Expression::Try(inner) = &value.kind else { unreachable!() };
            // The value is not used: `_` takes it, and the checker refuses an owner there.
            ((**inner).clone(), arm(Some("_".into()), rest), hand_back(value))
        }
        // Anywhere else the `?` that runs first is taken out: its value gets a hidden name, and the
        // statement, reading that name, starts the success arm.
        statement => {
            let mut statement = statement.clone();
            if let a::Statement::Assign { target, operation, value } = &mut statement { spill_target(target, operation, value, &mut spilled); }
            let value = match &mut statement {
                a::Statement::Bind { value, proof, .. } => {
                    if proof.is_some() { return Err(refuse(value, "`?` or an `if` value has no place in a binding with a proof: bind it first")); }
                    value
                }
                a::Statement::Choose { inner, .. } => match &mut **inner {
                    a::Statement::Match { value, .. } | a::Statement::If { condition: value, .. } => value,
                    _ => unreachable!(),
                },
                a::Statement::Expr(value) | a::Statement::Yield { value, .. } | a::Statement::Assign { value, .. } | a::Statement::Return(Some(value)) | a::Statement::Match { value, .. }
                    | a::Statement::If { condition: value, .. } | a::Statement::OpenFields { value, .. } | a::Statement::Open { value, .. } => value,
                _ => unreachable!(),
            };
            spill(value, &mut spilled)?;
            let at_value = value.clone();
            let (hidden, inner) = lift(value, &mut (false, false))?.expect("a `?` was found in this statement");
            // An inline `if`: the hidden name is chosen by an `if` statement just before this one.
            if let a::Expression::Conditional(condition, yes, no) = inner.kind {
                let given = |value: a::Expr| a::Statement::Yield { name: hidden.clone(), value };
                // `if let`: a match of that arm, giving `yes`, and a `_` arm giving `no`.
                let choice = match condition.kind {
                    a::Expression::Matches(value, arm) => a::Statement::Match { value: *value, arms: vec![
                        a::MatchArm { body: vec![given(*yes)], ..*arm },
                        a::MatchArm { variant: "_".into(), binding: None, body: vec![given(*no)], nested: None, open: vec![] },
                    ] },
                    _ => a::Statement::If { condition: *condition, yes: vec![given(*yes)], no: vec![given(*no)] },
                };
                // `let name: T = if c: a else: b`: the `if` is the whole value, so each path is
                // read at the written type (a literal is then a `Text`, not a borrowed `&str`).
                let written = match &statement {
                    a::Statement::Bind { ty: Some(ty), value, .. } if matches!(&value.kind, a::Expression::Unary("move", inner) if matches!(&inner.kind, a::Expression::Name(name) if *name == hidden)) => Some(ty.clone()),
                    _ => None,
                };
                let chosen = a::Statement::Choose { mutable: false, name: hidden.clone(), ty: written, inner: Box::new(choice) };
                let mut out = body[..at].to_vec();
                out.extend(spilled);
                out.push(chosen);
                let mut tail = vec![statement];
                tail.extend(rest);
                let tail = rewrite(&tail)?.unwrap_or(tail);
                out.extend(tail);
                return Ok(Some(out));
            }
            let mut body = vec![statement];
            body.extend(rest);
            (inner, arm(Some(hidden), body), hand_back(&at_value))
        }
    };
    let mut out = body[..at].to_vec();
    out.extend(spilled);
    out.push(a::Statement::Match { value, arms: vec![success, failure] });
    Ok(Some(out))
}

/// Before the first `?` or `if` value that runs in `expr` is taken out ahead of its statement,
/// what is written before it there is bound to hidden names, in order, so taking it out moves
/// nothing ahead of anything written earlier: `f(g(x), if c: a else: b)` is `let s# = g(x)` and
/// then the rest. Values read are bound too (`x + (if c: bump(&mut x) else: 0)` reads `x`
/// first: red-team review, it once read it after `bump`), and `move x` (moved first, so the
/// `if` cannot use `x`). What stays: literals and arithmetic of them (no effect, and they keep
/// the type their place gives them), references `&x`/`&mut x` (read where they are used, as
/// any argument's are), and a method's receiver that is a place (borrowed at the call). The
/// right side of `&&` and `||` may not run, and is left to `lift` to refuse.
/// Whether `expr` holds a `?` or an `if` value.
fn spill(expr: &mut a::Expr, out: &mut Vec<a::Statement>) -> Result<bool, Diagnostic> {
    if matches!(expr.kind, a::Expression::Try(_) | a::Expression::Conditional(..)) { return Ok(true); }
    let method = matches!(&expr.kind, a::Expression::Call(callee, _) if matches!(callee.kind, a::Expression::Field(..)));
    let mut parts: Vec<&mut a::Expr> = Vec::new();
    match &mut expr.kind {
        a::Expression::Call(callee, args) => {
            if let a::Expression::Field(receiver, _) = &mut callee.kind { parts.push(receiver); }
            parts.extend(args.iter_mut());
        }
        a::Expression::Binary(op, left, right) => {
            parts.push(left);
            if !matches!(*op, "and" | "or" | "&&" | "||") { parts.push(right); }
        }
        a::Expression::Unary(_, value) | a::Expression::Field(value, _) | a::Expression::Labelled(_, value) => parts.push(value),
        a::Expression::Index(value, index) => { parts.push(value); parts.push(index); }
        a::Expression::Record(fields) => parts.extend(fields.iter_mut().map(|(_, value)| value)),
        a::Expression::Array(values) => parts.extend(values.iter_mut()),
        a::Expression::Inject(_, Some(value)) => parts.push(value),
        _ => return Ok(false),
    }
    let Some(first) = parts.iter().position(|part| holds(part)) else { return Ok(false) };
    // A method's receiver that is a place stays (lent or read at the call, as without an `if`)
    // when what is taken out never names it: with no globals nothing else can change it. When it
    // does, whether the receiver is read before (taken by value) or after (lent) depends on the
    // method, which is not known here; either reading would be wrong for one kind (red-team
    // review: `c.plus(if t: bump(&mut c) else: 0)`), so it is refused.
    let named = names_in(parts[first]);
    for (index, part) in parts.iter_mut().take(first).enumerate() {
        if stays(part) { continue; }
        if index == 0 && method && receiver_place(part) {
            if names_in(part).iter().any(|name| named.contains(name)) {
                return Err(refuse(part, "an argument's `if` or `?` uses this method's receiver: bind one of them with `let` first"));
            }
            continue;
        }
        bind(part, out);
    }
    spill(parts.swap_remove(first), out)?;
    Ok(true)
}

/// `part` bound to a hidden name before the statement, read in its place by moving that name.
fn bind(part: &mut a::Expr, out: &mut Vec<a::Statement>) {
    let span = part.span.clone();
    let name = format!("?spill{}.{}", span.start, span.end);
    let value = std::mem::replace(part, a::Expr { kind: a::Expression::Unary("move", Box::new(a::Expr { kind: a::Expression::Name(name.clone()), span: span.clone() })), span });
    out.push(a::Statement::Bind { mutable: true, name, ty: None, value, proof: None });
}

/// Left in place by `spill`: literals and arithmetic of them, and references to places.
fn stays(expr: &a::Expr) -> bool {
    match &expr.kind {
        a::Expression::Integer(_) | a::Expression::Float(_) | a::Expression::Bool(_) | a::Expression::Unit => true,
        a::Expression::Binary(_, left, right) => stays(left) && stays(right) && !matches!(left.kind, a::Expression::Unary("&" | "&mut", _)),
        a::Expression::Unary("-" | "!" | "~", value) => stays(value),
        a::Expression::Unary("&" | "&mut", value) => place(value),
        // A hidden name this rewrite made for a value already taken out in order (`move ?try#`):
        // nothing a program writes can change it, and binding it again would bind it twice.
        a::Expression::Name(name) => name.starts_with('?'),
        a::Expression::Unary("move", value) => matches!(&value.kind, a::Expression::Name(name) if name.starts_with('?')),
        _ => false,
    }
}

/// Every name read in `expr`.
fn names_in(expr: &a::Expr) -> Vec<String> {
    let mut names = Vec::new();
    let mut probe = expr.clone();
    probe.visit_mut(&mut |e| { if let a::Expression::Name(name) = &e.kind { names.push(name.clone()); } false });
    names
}

/// A receiver whose naming runs nothing: a name, its fields, an element at a name or a number,
/// or `*r`.
fn receiver_place(expr: &a::Expr) -> bool {
    match &expr.kind {
        a::Expression::Name(_) => true,
        a::Expression::Field(inner, _) => receiver_place(inner),
        a::Expression::Index(inner, at) => receiver_place(inner) && matches!(at.kind, a::Expression::Name(_) | a::Expression::Integer(_)),
        a::Expression::Unary("*", inner) => matches!(inner.kind, a::Expression::Name(_)),
        _ => false,
    }
}

/// A name or a field of one: a place, which naming runs nothing.
fn place(expr: &a::Expr) -> bool {
    match &expr.kind { a::Expression::Name(_) => true, a::Expression::Field(inner, _) => place(inner), _ => false }
}

/// `target = value` where `value` holds a `?` or an `if` value: the target's positions run
/// first, as written (red-team review: `a[noisy(3)] = if ...` ran the `if` first), and for
/// `x op= value` the target's value is read first: `x += if c: bump(&mut x) else: 1` adds to
/// the `x` before `bump`.
fn spill_target(target: &mut a::Expr, operation: &mut &'static str, value: &mut a::Expr, out: &mut Vec<a::Statement>) {
    fn positions(expr: &mut a::Expr, out: &mut Vec<a::Statement>) {
        match &mut expr.kind {
            // A position that is a name is bound too: the `if` may change it (security review).
            a::Expression::Index(base, index) => { positions(base, out); if !stays(index) { bind(index, out); } }
            a::Expression::Field(base, _) | a::Expression::Unary("*", base) => positions(base, out),
            _ => {}
        }
    }
    positions(target, out);
    let combined = match *operation { "+=" => "+", "-=" => "-", "*=" => "*", "/=" => "/", "%=" => "%", "&=" => "&", "|=" => "|", "^=" => "^", "<<=" => "<<", ">>=" => ">>", _ => return };
    let span = target.span.clone();
    let old = format!("?old{}.{}", span.start, span.end);
    out.push(a::Statement::Bind { mutable: false, name: old.clone(), ty: None, value: target.clone(), proof: None });
    let read = a::Expr { kind: a::Expression::Name(old), span: span.clone() };
    let rest = std::mem::replace(value, a::Expr { kind: a::Expression::Unit, span: span.clone() });
    *value = a::Expr { span: span.start..rest.span.end, kind: a::Expression::Binary(combined, Box::new(read), Box::new(rest)) };
    *operation = "=";
}

/// `Err(e): return Err(move e)`.
fn hand_back(at: &a::Expr) -> a::MatchArm {
    let expr = |kind| a::Expr { kind, span: at.span.clone() };
    let error = expr(a::Expression::Unary("move", Box::new(expr(a::Expression::Name(ERROR.into())))));
    let result = expr(a::Expression::Call(Box::new(expr(a::Expression::Name("Err".into()))), vec![error]));
    a::MatchArm { variant: "Err".into(), binding: Some(ERROR.into()), body: vec![a::Statement::Return(Some(result))], nested: None, open: vec![] }
}
