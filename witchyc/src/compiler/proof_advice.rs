//! What a proof refusal says: the place responsible, what was not proved there, and one next
//! step. Every W0402 a refused proof ends with is made by `refused` from one `Unproved` kind,
//! so each kind's next step is written once (`Unproved::next`) and a test reads every place
//! that names a kind (`every_refusal_site_has_a_place_and_a_next_step`). Nothing here decides
//! what is accepted: it runs after a refusal, on the source and the checked program only.
use crate::ast as a;
use std::{borrow::Cow, ops::Range};

/// The kinds of proof refusal. A W0402 that explains a refused proof is made only by naming
/// one of these variants in `refused`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unproved {
    /// A function's `ensures` clause, not shown at a `return`.
    Ensures,
    /// A sealed type's invariant, not shown at a `return` of a function that changes the value.
    Invariant,
    /// A loop with no invariant changes what a later check reads.
    LoopNeedsInvariant,
    /// A loop's invariants do not say enough of what it changes for a later check.
    LoopSaysTooLittle,
    /// A loop invariant not shown on entry, after a pass, or at a `break` or `continue`.
    LoopInvariant,
    /// An `unreachable` some path reaches.
    Unreachable,
    /// An owner assigned over a place that may still hold one.
    Overwrite,
    /// A value discarded that may still hold an owner.
    Discard,
    /// A run-time check of the body: arithmetic in range, an index below a length, a shift.
    Check,
    /// A call that does not show its callee's `requires`.
    CallRequires,
    /// A construct the function's proof model does not read (a call with no contract in its
    /// vocabulary), or the operations of a body that model proves as one statement.
    Modelled,
    /// The proof ran out of time: Lean's step budget, or the checker's deadline.
    Timeout,
    /// The proof was refused and the parts of it could not be checked one at a time.
    Unnarrowed,
    /// Each `ensures` clause proves alone; together they do not.
    Combined,
    /// An authored lemma.
    Lemma,
}
impl Unproved {
    #[cfg(test)]
    pub(super) const ALL: [Self; 15] = [Self::Ensures, Self::Invariant, Self::LoopNeedsInvariant, Self::LoopSaysTooLittle, Self::LoopInvariant,
        Self::Unreachable, Self::Overwrite, Self::Discard, Self::Check, Self::CallRequires, Self::Modelled, Self::Timeout, Self::Unnarrowed,
        Self::Combined, Self::Lemma];
    /// One concrete thing to do next.
    pub(super) fn next(self) -> &'static str {
        match self {
            Self::Ensures => "state the fact this `return` relies on where the proof can see it: a `requires` on the function, an `ensures` on the call that made the value, or an `assert` just before the `return`",
            Self::Invariant => "make the invariant true again before this `return`, or test it and leave another way first (`if ...: return Err(...)`), so that every `return` of a changed value keeps it",
            Self::LoopNeedsInvariant => "add an `invariant` line right under the loop's first line that bounds or relates those names (for example `invariant i <= n`), so each pass and the code after the loop can rely on it",
            Self::LoopSaysTooLittle => "add an `invariant` line under the loop's first line that states what this check needs of those names",
            Self::LoopInvariant => "check that the invariant holds before the loop and that every pass (and every `break` and `continue`) restores it; weaken it, or add the fact a pass relies on as another `invariant`",
            Self::Unreachable => "rule the case out where the value is made (an `ensures` on that call saying when it happens, a `requires`, or a type invariant), or handle the case instead of writing `unreachable`",
            Self::Overwrite => "take the old value out first (`replace`, `take`, or `memory.exchange`) and close or hand it on, or state with a `requires` or an invariant that the place is empty here",
            Self::Discard => "close or hand on what the value holds before this point, or state with a `requires` or an invariant that it holds nothing here",
            Self::Check => "show what this check needs before this point: test it with `if` or `match`, state it with `requires` (or a loop `invariant`), or use the checked form of the operation, which returns a `Result`",
            Self::CallRequires => "show the callee's `requires` before the call: test it with `if`, add it to this function's own `requires`, or have the call that made the argument `ensures` it",
            Self::Modelled => "keep the claim on a function the model reads whole: move the call into a helper with no contract and build or change the value there, or give the callee a contract in the model's vocabulary",
            Self::Timeout => "make the proof smaller: compute a value once with `let x = if ...:` instead of writing it in each branch, split the function into helpers with their own contracts, or add an intermediate `assert` of the fact the later steps need",
            Self::Unnarrowed => "split the function into helpers with their own contracts, so that each proof is small enough to check and to explain part by part",
            Self::Combined => "state the fact the clauses share where the proof can see it (a `requires`, or an `assert` before the `return`), or put them into one `ensures`",
            Self::Lemma => "add the `requires` the lemma needs, or prove a smaller step first as a lemma of its own",
        }
    }
}

/// A W0402 of a refused proof, made by `refused`.
pub(super) struct Refused { pub kind: Unproved, pub at: Range<usize>, pub message: Cow<'static, str>,
    /// A likely place rather than a certain one: a proof model that checks a body as one statement.
    pub tentative: bool }
/// The W0402 of a refused proof: where, what was not proved there, and the kind's next step.
pub(super) fn refused(kind: Unproved, at: Range<usize>, said: String) -> Refused {
    Refused { kind, tentative: false, at, message: format!("this function's proof did not go through: {said}; next: {}", kind.next()).into() }
}

/// The source text at a place, on one line.
pub(super) fn written(source: &str, at: &Range<usize>) -> String {
    source.get(at.clone()).unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A function as written, by its full name (`Queue.push`).
pub(super) fn function<'a>(program: &'a a::Program, name: &str) -> Option<&'a a::Function> {
    program.declarations.iter().find_map(|d| match d { a::Declaration::Function(f) if f.name == name => Some(f), _ => None })
}

/// What must hold for a path to reach the `unreachable` written at `site`: the innermost arm or
/// branch around it, said as a condition on the source (`` `items.at_mut(last)` is `Err(code)` ``).
pub(super) fn unreachable_condition(source: &str, body: &[a::Statement], site: &Range<usize>) -> Option<String> {
    fn arm(arm: &a::MatchArm) -> String {
        let mut shown = arm.variant.clone();
        if let Some(binding) = &arm.binding { shown.push_str(&format!("({binding})")); }
        else if !arm.open.is_empty() { shown.push_str(&format!("({})", arm.open.join(", "))); }
        else if arm.nested.is_some() { shown.push_str("(...)"); }
        shown
    }
    fn walk(source: &str, statements: &[a::Statement], site: &Range<usize>, around: &mut Vec<String>) -> Option<String> {
        for statement in statements {
            let found = match statement {
                a::Statement::Unreachable(s) if s.span == *site => return Some(around.last().cloned().unwrap_or_default()),
                a::Statement::If { condition, yes, no } => {
                    let shown = written(source, &condition.span);
                    around.push(format!("`{shown}` holds"));
                    let found = walk(source, yes, site, around);
                    around.pop();
                    found.or_else(|| {
                        around.push(format!("`{shown}` does not hold"));
                        let found = walk(source, no, site, around);
                        around.pop();
                        found
                    })
                }
                a::Statement::Match { value, arms } => {
                    let shown = written(source, &value.span);
                    arms.iter().find_map(|one| {
                        around.push(format!("`{shown}` is `{}`", arm(one)));
                        let found = walk(source, &one.body, site, around);
                        around.pop();
                        found
                    })
                }
                a::Statement::Guard { value, variant, failure, .. } => {
                    around.push(format!("`{}` is `{}`, not `{variant}`", written(source, &value.span), arm(failure)));
                    let found = walk(source, &failure.body, site, around);
                    around.pop();
                    found
                }
                a::Statement::While { body, .. } | a::Statement::For { body, .. } | a::Statement::Using { body, .. } | a::Statement::Block(body) => walk(source, body, site, around),
                a::Statement::Choose { inner, .. } => walk(source, std::slice::from_ref(&**inner), site, around),
                _ => None,
            };
            if found.is_some() { return found; }
        }
        None
    }
    walk(source, body, site, &mut Vec::new()).filter(|c| !c.is_empty())
}

/// The names a part of the source mentions, as paths (`queue.count`, `i`), in order, once each.
pub(super) fn mentioned(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i < bytes.len() {
        if (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') && (i == 0 || !word(bytes[i - 1])) {
            let start = i;
            while i < bytes.len() && (word(bytes[i]) || (bytes[i] == b'.' && bytes.get(i + 1).is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_'))) { i += 1; }
            // A method call is the path before it (`items.len()` reads `items`).
            let mut path = &text[start..i];
            if bytes.get(i) == Some(&b'(') { path = path.rsplit_once('.').map_or("", |(p, _)| p); }
            if !path.is_empty() && !matches!(path, "old" | "result" | "true" | "false" | "and" | "or" | "not") && !out.iter().any(|seen| seen == path) {
                out.push(path.to_owned());
            }
        } else { i += 1; }
    }
    out
}

/// The paths of `text` whose first name is one of `names` (the locals a loop assigns).
pub(super) fn changed_by(text: &str, names: &[String]) -> Vec<String> {
    mentioned(text).into_iter().filter(|path| names.iter().any(|name| path.split('.').next() == Some(name.as_str()))).collect()
}

/// Where a body check was written, from what its label says: the value given to `name` that
/// holds this operator, when exactly one `let`, `var` or assignment of `name` in the body does.
pub(super) fn assigned_with(body: &[a::Statement], name: &str, operator: &str) -> Option<Range<usize>> {
    fn has(e: &a::Expr, operator: &str) -> bool {
        let mut found = false;
        let mut e = e.clone();
        e.visit_mut(&mut |x| { if let a::Expression::Binary(op, ..) = &x.kind && *op == operator { found = true; } false });
        found
    }
    fn walk(statements: &[a::Statement], name: &str, operator: &str, out: &mut Vec<Range<usize>>) {
        for statement in statements {
            match statement {
                a::Statement::Bind { name: bound, value, .. } if bound == name && has(value, operator) => out.push(value.span.clone()),
                a::Statement::Assign { target, operation, value } if matches!(&target.kind, a::Expression::Name(n) if n == name)
                    && (has(value, operator) || operation.starts_with(operator)) => out.push(if *operation == "=" { value.span.clone() } else { target.span.start..value.span.end }),
                a::Statement::If { yes, no, .. } => { walk(yes, name, operator, out); walk(no, name, operator, out); }
                a::Statement::Match { arms, .. } => for arm in arms { walk(&arm.body, name, operator, out); },
                a::Statement::Guard { failure, .. } => walk(&failure.body, name, operator, out),
                a::Statement::While { body, .. } | a::Statement::For { body, .. } | a::Statement::Using { body, .. } | a::Statement::Block(body) => walk(body, name, operator, out),
                a::Statement::Choose { inner, .. } => walk(std::slice::from_ref(&**inner), name, operator, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(body, name, operator, &mut out);
    match out.as_slice() { [one] if *one != (0..0) => Some(one.clone()), _ => None }
}

/// The operator a body check's label names (`` a `+` that may overflow ``), and the local its
/// value is assigned to (`` in what is assigned to `left` ``).
pub(super) fn check_of(label: &str) -> (Option<&str>, Option<&str>) {
    let operator = label.strip_prefix("a `").and_then(|rest| rest.split_once('`')).map(|(op, _)| op).filter(|op| op.len() <= 3);
    let local = label.split_once("in what is assigned to `").and_then(|(_, rest)| rest.split_once('`')).map(|(name, _)| name);
    (operator, local)
}

/// The message of a Lean refusal that ran out of time or steps.
pub(super) fn timed_out(message: &str) -> bool {
    message.contains("heartbeats") || message.contains("(deterministic) timeout") || message.contains("deadline")
}

/// The next step for a contract the clauses' vocabulary refuses (W0401), by the refusal's
/// family and what it says; every such message reaches an author with one (`advised`).
pub(crate) fn contract_next(message: &str) -> Option<&'static str> {
    let family = ["function contracts", "record contracts", "memory contracts", "storage contracts", "contracts"].into_iter().find(|f| message.starts_with(f))?;
    let has = |words: &str| message.contains(words);
    Some(if has("complexity limit") || has("budget exceeded") || has("at most 32") || has("expansion limit") {
        "split the contract or the function: fewer clauses here, or a helper with its own contract for part of the work"
    } else if has("acyclic") {
        "break the cycle of contracted calls: one function of the cycle has no contract, or the recursion becomes a loop"
    } else if has("old") {
        "write `old(...)` only in an `ensures` or a loop invariant, around a parameter's field or argument-free method, as `old(items.len())`"
    } else if has("result") && !has("Result") {
        "name `result` only in an `ensures`, and read a record result through its number fields (`result.count`)"
    } else if has("Bool is a proposition") || has("compare") {
        "compare numbers of one kind (`x <= limit`), and use a Bool as a condition by itself (`flag`, `!flag`)"
    } else if has("not a parameter") || has("unknown parameter") || has("unknown field") || has("not a parameter or active result binder") {
        "name a parameter of this function (or `result`, or a binder of this `match result:` arm), and a field of it the clause may read"
    } else if has("method") {
        "read a number through an argument-free method whose body returns one field (`items.len()`), or through the field itself"
    } else if has("match") || has("Ok") || has("Err") || has("arm") {
        "write `ensures match result:` with one arm per variant of the returned sum, each binding fresh names"
    } else if has("loop") {
        "give each loop of this function `invariant` lines, or move the loop into a helper with no contract"
    } else if has("require") || has("support") || has("unsupported") || has("only") || has("requires") {
        if family == "function contracts" { "rewrite the clause with parameters, their number fields, literals, arithmetic, comparisons, `&&`, `||`, `!` and `old(...)`, or move this part of the claim to a helper with its own contract" }
        else { "keep this function to what its proof model reads (see docs/proof-architecture.md), or move the part it does not into a helper with no contract" }
    } else if family == "function contracts" {
        "rewrite the clause with parameters, their number fields, literals, arithmetic, comparisons, `&&`, `||`, `!` and `old(...)`"
    } else {
        "keep this function to what its proof model reads (see docs/proof-architecture.md), or move the part it does not into a helper with no contract"
    })
}

/// Whether a refusal already says what to write instead (`...; bind the argument with `let``).
pub(crate) fn says_next(message: &str) -> bool {
    message.contains("; next: ") || [": bind ", "; bind ", ": write ", "; write ", ": use ", "; use ", ": add ", "; add ", ": give ", "; give ", ": rewrite ", ": move ", "; move ", ": compare it", ", as in `"]
        .iter().any(|step| message.contains(step))
}
/// A contract refusal with its next step said, unless it already says one.
pub(crate) fn advised(message: Cow<'static, str>) -> Cow<'static, str> {
    match contract_next(&message) {
        Some(next) if !says_next(&message) => format!("{message}; next: {next}").into(),
        _ => message,
    }
}

/// The invariants of the sealed types a function takes or returns, where each is written.
pub(super) fn invariants_of(program: &a::Program, f: &a::Function) -> Vec<Range<usize>> {
    fn names(ty: &a::Type, out: &mut Vec<String>) {
        match ty {
            a::Type::Named(name, arguments) => { out.push(name.clone()); for t in arguments { names(t, out); } }
            a::Type::Borrow { inner, .. } | a::Type::Array { element: inner, .. } | a::Type::Slice(inner) => names(inner, out),
            a::Type::Record(fields) => for (_, t) in fields { names(t, out); },
            a::Type::Union(tags) => for t in tags.iter().filter_map(|(_, t)| t.as_ref()) { names(t, out); },
            a::Type::Unit => {}
        }
    }
    let mut types = Vec::new();
    for (_, ty) in &f.parameters { names(ty, &mut types); }
    names(&f.result, &mut types);
    program.declarations.iter().filter_map(|d| match d {
        a::Declaration::Record { name, conditions, sealed: true, .. } if types.contains(name) => Some(conditions.iter().map(|c| c.span.clone()).filter(|at| *at != (0..0))),
        _ => None,
    }).flatten().collect()
}

/// Where a function returns, as written: each `return` with a value.
pub(super) fn returns_of(f: &a::Function) -> Vec<Range<usize>> {
    fn walk(statements: &[a::Statement], out: &mut Vec<Range<usize>>) {
        for statement in statements {
            match statement {
                a::Statement::Return(Some(value)) if value.span != (0..0) => out.push(value.span.clone()),
                a::Statement::If { yes, no, .. } => { walk(yes, out); walk(no, out); }
                a::Statement::Match { arms, .. } => for arm in arms { walk(&arm.body, out); },
                a::Statement::Guard { failure, .. } => walk(&failure.body, out),
                a::Statement::While { body, .. } | a::Statement::For { body, .. } | a::Statement::Using { body, .. } | a::Statement::Block(body) => walk(body, out),
                a::Statement::Choose { inner, .. } => walk(std::slice::from_ref(&**inner), out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&f.body, &mut out);
    out
}

/// A refusal of a function's contract with no place of its own, placed at the function's name.
pub(super) fn at_function(mut refusal: crate::diagnostic::Diagnostic, f: &super::logical_body::Function) -> crate::diagnostic::Diagnostic {
    if refusal.span == (0..0) && f.span != (0, 0) { refusal.span = f.span.0..f.span.1; }
    refusal
}
/// A refusal with no place of its own, placed at the clause it arose in.
pub(super) fn at_clause(mut refusal: crate::diagnostic::Diagnostic, clause: &Range<usize>) -> crate::diagnostic::Diagnostic {
    if refusal.span == (0..0) && *clause != (0..0) { refusal.span = clause.clone(); }
    refusal
}
