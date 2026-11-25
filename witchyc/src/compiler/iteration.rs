//! `for` is a `while` over a hidden counter.
//!
//! `for i in a..b:` counts. `for item in &container:` asks the container for a reference
//! to each element with `container.at(index)` until it answers `Err`; `for item in &mut
//! container:` asks `container.at_mut(index)`. Any type with such a method can be walked;
//! nothing is taken out of it. A type with no `at` and a `next(&mut cursor)` is asked for
//! each value in turn, the cursor starting at zero.
use crate::ast as a;

/// The end of the hidden counter's name in a counting `for`.
pub(super) const STEP: &str = ".count";
/// `s[i]` read in `for i in a..s.len():`, written `s.#counted(i)`: on a slice the checker reads
/// it with no test, the counter being below the length read once, which a slice cannot change
/// (no source name contains `#`). On anything else it is `s[i]` again.
pub(super) const COUNTED: &str = "#counted";
/// `v[i]` in `for i in a..v.len():`, written `v.#counted_in(i, view)`, where `view` is the hidden
/// local `let for#N.view = v.#counted_view()` bound once after the range's end: the checker
/// makes that local a slice of `v`'s elements when a container's own methods say its `index`
/// reads it and it is as long as `v.len()` (check/methods.rs `counted_container`), and reads
/// `view[i]` with no test; otherwise the local is `()` and the read is `v.#counted(i)`.
pub(super) const COUNTED_IN: &str = "#counted_in";
/// The hidden local's value, written `v.#counted_view()` (see `COUNTED_IN`).
pub(super) const COUNTED_VIEW: &str = "#counted_view";

/// Marks the reads `slice[counter]` in `body` (`COUNTED`). Assignment targets and borrows
/// (`s[i] = v`, `&s[i]`) stay as written; so does any statement this does not know.
/// Reports whether it marked a read.
pub(super) fn counted(body: &mut [a::Statement], slice: &str, counter: &str) -> bool { counted_in(body, slice, counter, None) }
/// The same, each read written `v.#counted_in(i, view)` when the loop binds `view` (`COUNTED_IN`).
pub(super) fn counted_in(body: &mut [a::Statement], slice: &str, counter: &str, view: Option<&str>) -> bool {
    let mut changed = false;
    let mut mark = |e: &mut a::Expr| -> bool {
        match &e.kind {
            a::Expression::Unary("&" | "&mut", _) => true,
            a::Expression::Index(receiver, at) if matches!(&receiver.kind, a::Expression::Name(n) if n == slice)
                && matches!(&at.kind, a::Expression::Name(n) if n == counter) => {
                let span = e.span.clone();
                let (method, mut arguments) = match view { Some(_) => (COUNTED_IN, vec![(**at).clone()]), None => (COUNTED, vec![(**at).clone()]) };
                if let Some(view) = view { arguments.push(a::Expr { kind: a::Expression::Name(view.into()), span: span.clone() }); }
                let callee = a::Expr { kind: a::Expression::Field(receiver.clone(), method.into()), span: span.clone() };
                e.kind = a::Expression::Call(Box::new(callee), arguments);
                changed = true;
                true
            }
            _ => false,
        }
    };
    visit_counted(body, slice, &mut mark);
    changed
}

fn visit_counted(body: &mut [a::Statement], slice: &str, mark: &mut dyn FnMut(&mut a::Expr) -> bool) {
    for statement in body {
        match statement {
            a::Statement::Expr(e) | a::Statement::Assert(e) | a::Statement::Return(Some(e)) => e.visit_mut(mark),
            a::Statement::Bind { value, .. } | a::Statement::Open { value, .. } | a::Statement::OpenFields { value, .. } => value.visit_mut(mark),
            // A write through the same slice (`s[j] = s[j] + 1`) is a `&mut` slice: its reads keep
            // their test and their ordinary order with the target's loan (regression of
            // d35a5d05: marked, they were refused as lent; fixtures slices/counted-read-mut and
            // slices/array-lent-as-slice). A shared slice is never an assignment target.
            a::Statement::Assign { target, value, .. } if !writes_through(target, slice) => value.visit_mut(mark),
            a::Statement::Assign { .. } => {}
            a::Statement::Guard { value, failure, .. } => { value.visit_mut(mark); visit_counted(&mut failure.body, slice, mark); }
            a::Statement::If { condition, yes, no } => { condition.visit_mut(mark); visit_counted(yes, slice, mark); visit_counted(no, slice, mark); }
            a::Statement::While { condition, body, .. } => { condition.visit_mut(mark); visit_counted(body, slice, mark); }
            a::Statement::For { start, end, body, .. } => { start.visit_mut(mark); if let Some(end) = end { end.visit_mut(mark); } visit_counted(body, slice, mark); }
            a::Statement::Match { value, arms } => { value.visit_mut(mark); for arm in arms { visit_counted(&mut arm.body, slice, mark); } }
            a::Statement::Using { value, body, .. } => { value.visit_mut(mark); visit_counted(body, slice, mark); }
            a::Statement::Block(body) => visit_counted(body, slice, mark),
            _ => {}
        }
    }
}

/// A name, its fields, a dereferenced name, or an element at a fixed or named index: reading it again does nothing.
fn place(expr: &a::Expr) -> bool {
    match &expr.kind {
        a::Expression::Name(_) => true,
        a::Expression::Unary("*", value) => matches!(&value.kind, a::Expression::Name(_)),
        a::Expression::Field(value, _) => place(value),
        a::Expression::Index(value, index) => place(value) && matches!(&index.kind, a::Expression::Integer(_) | a::Expression::Name(_)),
        _ => false,
    }
}
/// Whether an assignment target writes into `slice` (`slice[...]`, a field of one, ...).
fn writes_through(target: &a::Expr, slice: &str) -> bool {
    match &target.kind {
        a::Expression::Name(name) => name == slice,
        a::Expression::Index(base, _) | a::Expression::Field(base, _) | a::Expression::Unary(_, base) => writes_through(base, slice),
        _ => false,
    }
}
/// The named receiver in a counting loop ending at `s.len()`.
fn counted_end(end: &a::Expr) -> Option<&str> {
    if let a::Expression::Call(callee, arguments) = &end.kind
        && arguments.is_empty()
        && let a::Expression::Field(receiver, method) = &callee.kind
        && method == "len"
        && let a::Expression::Name(name) = &receiver.kind
    {
        return Some(name);
    }
    None
}
/// Rewrites every counting `for` directly in `body`. Nested blocks are rewritten when they are
/// checked; a `for` over a container waits for `walk`, when the container's type is known.
pub(super) fn rewrite(body: &[a::Statement]) -> Option<Vec<a::Statement>> {
    if !body.iter().any(|s| matches!(s, a::Statement::For { end: Some(_), .. })) { return None; }
    let mut out = Vec::new();
    for statement in body {
        let a::Statement::For { binding, start, end: Some(end), invariants, body, .. } = statement else { out.push(statement.clone()); continue; };
        if let a::Expression::Unary("..=", last) = &end.kind {
            out.extend(inclusive(binding, start, last, body));
            continue;
        }
        let expr = |kind| a::Expr { kind, span: start.span.clone() };
        // No source name contains `#`, and the position tells loops apart.
        // The counter of `a..b` is `for#N.count` (`STEP` names it): it is written only by the step
        // below, which runs only once `counter < end` held, so the step cannot overflow and is
        // emitted without its check (emit.rs).
        let hidden = |part: &str| if part.is_empty() { format!("for#{}{STEP}", start.span.start) } else { format!("for#{}{part}", start.span.start) };
        let name = |n: String| expr(a::Expression::Name(n));
        let step = a::Statement::Assign { target: name(hidden("")), operation: "+=", value: expr(a::Expression::Integer(1)) };
        let u64 = Some(a::Type::Named("U64".into(), Vec::new()));
        out.push(a::Statement::Bind { mutable: true, name: hidden(""), ty: u64.clone(), value: start.clone(), proof: None });
        out.push(a::Statement::Bind { mutable: false, name: hidden(".end"), ty: u64, value: end.clone(), proof: None });
        // After the start and the end, in the order written: what the checker makes of
        // `v.#counted_view()` (`COUNTED_IN`), a slice or `0`.
        let view = counted_end(end).map(|receiver| {
            let value = a::Expr { kind: a::Expression::Call(Box::new(a::Expr { kind: a::Expression::Field(Box::new(name(receiver.to_string())), COUNTED_VIEW.into()), span: end.span.clone() }), Vec::new()), span: end.span.clone() };
            out.push(a::Statement::Bind { mutable: false, name: hidden(".view"), ty: None, value, proof: None });
            hidden(".view")
        });
        // The counter moves on before the body runs, so `continue` cannot skip the step.
        let mut inner = vec![a::Statement::Bind { mutable: false, name: binding.clone(), ty: None, value: name(hidden("")), proof: None }, step];
        let mut body = body.to_vec();
        // The checker has the receiver's type when it resolves each marked read.
        if let Some(slice) = counted_end(end) { counted_in(&mut body, slice, binding, view.as_deref()); }
        inner.extend(body);
        let condition = expr(a::Expression::Binary("<", Box::new(name(hidden(""))), Box::new(name(hidden(".end")))));
        // At the head of the loop the name stands for the counter: the index the next pass will have.
        let invariants = invariants.iter().map(|clause| renamed(clause, binding, &hidden(""))).collect();
        out.push(a::Statement::While { condition, invariants, body: inner });
    }
    Some(out)
}

/// `for i in a..=b:` runs `b` too, without ever computing `b + 1` (which overflows when `b` is
/// the largest U64): it stops by comparing the counter with `b`.
///
///     var counter = a; let end = b; var going = counter <= end
///     while going:
///         let i = counter
///         if counter == end: going = false
///         else: counter += 1
///         body
///
/// The step runs only when `counter < end`, so it is the unchecked step of a counting `for`
/// (`STEP`). It comes before the body, as there, so `continue` cannot skip it. The parser
/// refuses invariants here: at the loop's head the counter is not the next index once the
/// last pass has begun.
fn inclusive(binding: &str, start: &a::Expr, last: &a::Expr, body: &[a::Statement]) -> Vec<a::Statement> {
    let expr = |kind| a::Expr { kind, span: start.span.clone() };
    let counter = format!("for#{}{STEP}", start.span.start);
    let (end, going) = (format!("for#{}.end", start.span.start), format!("for#{}.going", start.span.start));
    let name = |n: &String| expr(a::Expression::Name(n.clone()));
    let u64 = Some(a::Type::Named("U64".into(), Vec::new()));
    let binary = |op: &'static str, l: a::Expr, r: a::Expr| expr(a::Expression::Binary(op, Box::new(l), Box::new(r)));
    let mut inner = vec![
        a::Statement::Bind { mutable: false, name: binding.to_owned(), ty: None, value: name(&counter), proof: None },
        a::Statement::If {
            condition: binary("==", name(&counter), name(&end)),
            yes: vec![a::Statement::Assign { target: name(&going), operation: "=", value: expr(a::Expression::Bool(false)) }],
            no: vec![a::Statement::Assign { target: name(&counter), operation: "+=", value: expr(a::Expression::Integer(1)) }],
        },
    ];
    inner.extend(body.iter().cloned());
    vec![
        a::Statement::Bind { mutable: true, name: counter.clone(), ty: u64.clone(), value: start.clone(), proof: None },
        a::Statement::Bind { mutable: false, name: end.clone(), ty: u64, value: last.clone(), proof: None },
        a::Statement::Bind { mutable: true, name: going.clone(), ty: Some(a::Type::Named("Bool".into(), Vec::new())), value: binary("<=", name(&counter), name(&end)), proof: None },
        a::Statement::While { condition: name(&going), invariants: Vec::new(), body: inner },
    ]
}

/// What a `for` over a container walks, as it was written without its `&` or `&mut`.
pub(super) fn walked(start: &a::Expr) -> &a::Expr {
    match &start.kind { a::Expression::Unary("&" | "&mut", inner) => inner, _ => start }
}

/// How a `for` reaches each element.
pub(super) enum Way {
    /// An array of this many elements, by index.
    Array(u64),
    /// `at(index)`, a reference to the element where it is, until it answers `Err`.
    At,
    /// `next(&mut cursor)`, the next value and the cursor moved past it, until it answers `Err`.
    /// The cursor starts at zero and means what the type says it means.
    Next,
    /// `next_mut(&mut cursor)` on a value that changes as it goes, as a generator does: the
    /// container is held as a `var` (or must be one, when it is a named place).
    NextMut,
    /// A shared slice (`&[T]`): bound once to a hidden `let`, its length read once, and each
    /// element reached at the counter, which counts below that length (`Reach::Walk`: no test,
    /// no `Result`; the counter's step is a counting `for`'s, unchecked). Nothing can change a
    /// shared slice or what it lends from while the loop holds it.
    /// `true`: a unique slice from the type's `elements_mut`, its elements given as `&mut`.
    Slice(bool),
}
/// The methods only `Way::Slice` calls: no source name contains `#`.
pub(super) const WALK: &str = "#walk";
pub(super) const WALK_MUT: &str = "#walk_mut";
/// One `for` over a container, as the statements of a block of its own.
pub(super) fn walk(index: Option<&str>, binding: &str, start: &a::Expr, invariants: &[a::Expr], body: &[a::Statement], way: Way) -> Vec<a::Statement> {
    let expr = |kind| a::Expr { kind, span: start.span.clone() };
    let hidden = |part: &str| format!("for#{}{part}", start.span.start);
    let name = |n: String| expr(a::Expression::Name(n));
    let bind = |mutable, n: String, value| a::Statement::Bind { mutable, name: n, ty: None, value, proof: None };
    let step = a::Statement::Assign { target: name(hidden("")), operation: "+=", value: expr(a::Expression::Integer(1)) };
    let u64 = Some(a::Type::Named("U64".into(), Vec::new()));
    let mut out = Vec::new();
    // `for index, item in …` names where the element is, for the body alone.
    let position: Vec<a::Statement> = index.map(|given| bind(false, given.to_owned(), name(hidden(".at")))).into_iter().collect();
    // `for item in &mut container:` gives `&mut` references.
    let changing = matches!(&start.kind, a::Expression::Unary("&mut", _)) || matches!(way, Way::NextMut);
    if let Way::Slice(unique) = way {
        let counter = hidden(STEP);
        let slice = hidden(".in");
        // Always a local of its own, even for a named slice: the walk reads one value.
        out.push(bind(false, slice.clone(), walked(start).clone()));
        out.push(a::Statement::Bind { mutable: true, name: counter.clone(), ty: u64.clone(), value: expr(a::Expression::Integer(0)), proof: None });
        let length = expr(a::Expression::Call(Box::new(expr(a::Expression::Field(Box::new(name(slice.clone())), "len".into()))), vec![]));
        out.push(a::Statement::Bind { mutable: false, name: hidden(".end"), ty: u64, value: length, proof: None });
        let element = expr(a::Expression::Call(Box::new(expr(a::Expression::Field(Box::new(name(slice)), (if unique { WALK_MUT } else { WALK }).into()))), vec![name(hidden(".at"))]));
        let mut inner = vec![bind(false, hidden(".at"), name(counter.clone())),
            a::Statement::Assign { target: name(counter.clone()), operation: "+=", value: expr(a::Expression::Integer(1)) },
            bind(false, binding.to_owned(), element)];
        inner.extend(position.clone());
        inner.extend(body.iter().cloned());
        let condition = expr(a::Expression::Binary("<", Box::new(name(counter)), Box::new(name(hidden(".end")))));
        out.push(a::Statement::While { condition, invariants: invariants.to_vec(), body: inner });
        return out;
    }
    let written = walked(start).clone();
    // A container that is not a named place is evaluated once, before the loop.
    let container = if place(&written) { written } else {
        out.push(bind(changing, hidden(".in"), written));
        name(hidden(".in"))
    };
    out.push(a::Statement::Bind { mutable: true, name: hidden(""), ty: u64, value: expr(a::Expression::Integer(0)), proof: None });
    if let Way::Array(length) = way {
        // The index is below the length, so the element is there; the counter moves on first.
        let element = expr(a::Expression::Index(Box::new(container), Box::new(name(hidden(".at")))));
        let mut inner = vec![bind(false, hidden(".at"), name(hidden(""))), step,
            bind(false, binding.to_owned(), expr(a::Expression::Unary(if changing { "&mut" } else { "&" }, Box::new(element))))];
        inner.extend(position.clone());
        inner.extend(body.iter().cloned());
        let condition = expr(a::Expression::Binary("<", Box::new(name(hidden(""))), Box::new(expr(a::Expression::Integer(length)))));
        // The element has no name at the head of the loop, so the invariants cannot speak of it.
        out.push(a::Statement::While { condition, invariants: invariants.to_vec(), body: inner });
        return out;
    }
    out.push(bind(true, hidden(".more"), expr(a::Expression::Bool(true))));
    let cursor = matches!(way, Way::Next | Way::NextMut);
    let method = match (cursor, changing) { (false, false) => "at", (false, true) => "at_mut", (true, false) => "next", (true, true) => "next_mut" };
    let asked = if cursor { expr(a::Expression::Unary("&mut", Box::new(name(hidden(""))))) } else { name(hidden(".at")) };
    let at = expr(a::Expression::Call(Box::new(expr(a::Expression::Field(Box::new(container), method.into()))), vec![asked]));
    let stop = a::Statement::Assign { target: name(hidden(".more")), operation: "=", value: expr(a::Expression::Bool(false)) };
    let arms = vec![
        a::MatchArm { variant: "Ok".into(), binding: Some(binding.to_owned()), body: position.iter().chain(body).cloned().collect(), nested: None, open: vec![] },
        a::MatchArm { variant: "Err".into(), binding: Some(hidden(".stop")), body: vec![stop], nested: None, open: vec![] },
    ];
    // The counter stays below the largest U64, which no container reaches: a function with a
    // contract has to show that the step cannot overflow, and this is what shows it.
    let below = expr(a::Expression::Binary("<", Box::new(name(hidden(""))), Box::new(expr(a::Expression::Integer(u64::MAX)))));
    let condition = expr(a::Expression::Binary("&&", Box::new(below), Box::new(name(hidden(".more")))));
    // With a cursor the call itself moves on, and `.at` is where this value began.
    let mut inner = vec![bind(false, hidden(".at"), name(hidden("")))];
    if !cursor { inner.push(step); }
    inner.push(a::Statement::Match { value: at, arms });
    out.push(a::Statement::While { condition, invariants: invariants.to_vec(), body: inner });
    out
}

/// `clause` with every use of the name `from` reading `to` instead.
fn renamed(clause: &a::Expr, from: &str, to: &str) -> a::Expr {
    let again = |e: &a::Expr| Box::new(renamed(e, from, to));
    let kind = match &clause.kind {
        a::Expression::Name(name) if name == from => a::Expression::Name(to.to_owned()),
        a::Expression::Unary(op, value) => a::Expression::Unary(op, again(value)),
        a::Expression::Binary(op, left, right) => a::Expression::Binary(op, again(left), again(right)),
        a::Expression::Call(callee, args) => a::Expression::Call(again(callee), args.iter().map(|arg| renamed(arg, from, to)).collect()),
        a::Expression::Field(value, field) => a::Expression::Field(again(value), field.clone()),
        a::Expression::Index(value, index) => a::Expression::Index(again(value), again(index)),
        other => other.clone(),
    };
    a::Expr { kind, span: clause.span.clone() }
}
