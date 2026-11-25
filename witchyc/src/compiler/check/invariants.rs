//! Invariant clauses, guarded type reach, and restrictions on lending during an update.
use super::{Ty, Types, a};
use std::collections::{BTreeSet, HashSet};

/// The named types `ty` is or holds (`reached_types`) that the file `by` guards (sealed, or with
/// an invariant), by name: an open type's fields are
/// anyone's to write, so a recipe's authority over it is not narrowed (review round 6: private
/// helpers and methods over an open `Point` were refused).
pub(super) fn guarded_names(layouts: &Types, ty: Ty, by: u32) -> BTreeSet<String> {
    // Guarded by the recipe's own file (`by`): another file's sealed type (the library's String)
    // gives the recipe no authority to narrow (review round 7: a recipe's private trait method on
    // a String field was refused).
    reached_types(layouts, ty).into_iter().filter(|t| layouts.sealed_in(*t) == Some(by) || layouts.invariants(*t).is_some_and(|(m, _)| m == by))
        .filter_map(|t| layouts.nominal(t).map(str::to_string)).collect()
}

/// The declared records and sums `ty` is or holds: through references, buffers, slices, a
/// record's fields and type arguments, and a sum's variants (everything a value of it leads to).
pub(super) fn reached_types(layouts: &Types, ty: Ty) -> Vec<Ty> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    let mut pending = vec![ty];
    while let Some(t) = pending.pop() {
        if !seen.insert(t) { continue; }
        // Declared records and sums only: a number or a capability is no one's to seal.
        if matches!(t, Ty::Aggregate(_) | Ty::Sum(_)) && layouts.nominal(t).is_some() { names.push(t); }
        match t {
            Ty::Borrow { pointee, .. } => pending.push(pointee.ty()),
            Ty::Slice(id) => pending.push(layouts.slice_element(id)),
            Ty::Boxed(id) | Ty::Buffer(id) => if let Some(buffer) = layouts.buffers().get(id as usize) { pending.push(buffer.element) },
            Ty::Aggregate(_) => if let Some(aggregate) = layouts.get(t) {
                pending.extend(aggregate.fields.iter().map(|f| f.ty));
                if let Some((_, args)) = &aggregate.origin { pending.extend(args.iter().copied()); }
            },
            Ty::Sum(_) => if let Some(sum) = layouts.sum(t) { pending.extend(sum.arms.iter().copied()); },
            _ => {}
        }
    }
    names
}

// The pointer projections in scalar contracts are leaves. Their resolver
// accepts only typed, visible field paths; all other memory clauses need the
// heap proof model.
pub(super) fn has_invariant_view(invariants: &[a::Expr]) -> bool {
    invariants.iter().any(|invariant| {
        let mut view = false;
        invariant.clone().visit_mut(&mut |expr| {
            if let a::Expression::Call(callee, _) = &expr.kind
                && let a::Expression::Field(base, _) = &callee.kind
                && !matches!(&base.kind, a::Expression::Name(n) if n == "memory") {
                view = true;
            }
            view
        });
        view
    })
}

/// Whether no invariant of a type reads the field `field`, decided on the clauses as they are
/// kept (a `when` case is by then a test of `variant_index(field)`). A clause has no name for
/// the whole value, so it reads a field only where it writes that field's name: as a value, as
/// what a function or a method is given or called on, or as the subject of a case. A name bound
/// by a case's pattern that is spelled like the field counts as the field, and so does a member
/// of that name; a clause of a form not listed here counts as reading every field.
pub(super) fn invariant_free_field(invariants: &[a::Expr], field: &str) -> bool {
    fn free(e: &a::Expr, field: &str) -> bool {
        match &e.kind {
            a::Expression::Integer(_) | a::Expression::Bool(_) => true,
            a::Expression::Name(n) => n != field,
            a::Expression::Unary(_, inner) => free(inner, field),
            a::Expression::Binary(_, left, right) | a::Expression::Index(left, right) => free(left, field) && free(right, field),
            a::Expression::Field(inner, member) => member != field && free(inner, field),
            a::Expression::Call(callee, arguments) => free(callee, field) && arguments.iter().all(|argument| free(argument, field)),
            a::Expression::SpecCase { value, arms, .. } => free(value, field)
                && arms.iter().all(|(_, binding, clause)| binding.as_deref() != Some(field) && free(clause, field)),
            _ => false,
        }
    }
    invariants.iter().all(|invariant| free(invariant, field))
}

/// Whether every invariant reads the field `field`, if at all, only as what a method with no
/// other argument is called on (`items.len()`): a number the proof over numbers reads as the
/// field that method's one-line body returns, and forgets when a kept `&mut` can reach it. Any
/// other reading, and any form `invariant_free_field` does not list, counts as reading it whole.
pub(super) fn invariant_observes_field(invariants: &[a::Expr], field: &str) -> bool {
    fn free(e: &a::Expr, field: &str) -> bool {
        match &e.kind {
            a::Expression::Call(callee, arguments) if arguments.is_empty()
                && matches!(&callee.kind, a::Expression::Field(receiver, _) if matches!(&receiver.kind, a::Expression::Name(n) if n == field)) => true,
            a::Expression::Integer(_) | a::Expression::Bool(_) => true,
            a::Expression::Name(n) => n != field,
            a::Expression::Unary(_, inner) => free(inner, field),
            a::Expression::Binary(_, left, right) | a::Expression::Index(left, right) => free(left, field) && free(right, field),
            a::Expression::Field(inner, member) => member != field && free(inner, field),
            a::Expression::Call(callee, arguments) => free(callee, field) && arguments.iter().all(|argument| free(argument, field)),
            a::Expression::SpecCase { value, arms, .. } => free(value, field)
                && arms.iter().all(|(_, binding, clause)| binding.as_deref() != Some(field) && free(clause, field)),
            _ => false,
        }
    }
    invariants.iter().all(|invariant| free(invariant, field))
}

pub(super) fn heap_invariant(invariants: &[a::Expr]) -> bool {
    let mut heap = false;
    for invariant in invariants {
        invariant.clone().visit_mut(&mut |e| {
            if matches!(&e.kind, a::Expression::Call(c, _) if matches!(&c.kind, a::Expression::Field(base, op)
                if matches!(&base.kind, a::Expression::Name(n) if n == "memory") && matches!(op.as_str(), "pointer_bits" | "box_id"))) {
                return true;
            }
            heap |= matches!(&e.kind, a::Expression::Name(n) if n == "memory");
            false
        });
    }
    heap
}

/// A record a proof over numbers may be given by value and hand on unread: it holds only `&`
/// references (nothing can be changed through it), no part of it is a sum (a proof follows a
/// sum's variant), and neither it nor a record in it has an invariant (a proof assumes one).
pub(super) fn opaque_view(layouts: &Types, ty: Ty) -> bool {
    if !matches!(ty, Ty::Aggregate(_)) || layouts.lends_unique(ty) { return false; }
    let (mut pending, mut seen) = (vec![ty], Vec::new());
    while let Some(ty) = pending.pop() {
        if seen.contains(&ty) { continue; }
        seen.push(ty);
        match ty {
            Ty::Aggregate(_) => {
                if layouts.invariants(ty).is_some() { return false; }
                let Some(record) = layouts.get(ty) else { return false };
                pending.extend(record.fields.iter().map(|field| field.ty));
            }
            Ty::Sum(_) | Ty::Param { .. } => return false,
            _ => {}
        }
    }
    true
}
pub(super) fn invariant_clauses(f: &a::Function, params: &[Ty], result: Ty, layouts: &Types) -> (Vec<a::Expr>, Vec<a::Postcondition>) {
    let (mut assumed, mut shown) = (Vec::new(), Vec::new());
    let of = |ty: Ty| -> Option<(Vec<String>, Vec<a::Expr>)> {
        let (module, invariants) = layouts.invariants(ty)?;
        if module != f.module { return None; }
        let memory = heap_invariant(invariants);
        if memory { return None; }
        let fields = layouts.get(ty)?.fields.iter().map(|field| field.name.clone()).collect();
        // An invariant over a container's elements is kept another way (`element_clauses`).
        Some((fields, invariants.iter().filter(|invariant| !matches!(invariant.kind, a::Expression::Forall { .. })).cloned().collect()))
    };
    // Each bare field name of the invariant, read from `holder`.
    let at = |holder: &str, fields: &[String], invariant: &a::Expr| {
        let mut e = invariant.clone();
        e.visit_mut(&mut |e| {
            if let a::Expression::Name(n) = &e.kind && fields.contains(n) {
                let name = a::Expr { kind: a::Expression::Name(holder.to_string()), span: e.span.clone() };
                e.kind = a::Expression::Field(Box::new(name), n.clone());
                return true;
            }
            false
        });
        e
    };
    for ((name, _), ty) in f.parameters.iter().zip(params) {
        let (pointee, mutable) = match ty { Ty::Borrow { pointee, mutable } => (pointee.ty(), *mutable), other => (*other, false) };
        let Some((fields, invariants)) = of(pointee) else { continue };
        for invariant in &invariants {
            assumed.push(at(name, &fields, invariant));
            if mutable { shown.push(a::Postcondition { kind: a::PostconditionKind::Predicate(at(name, &fields, invariant)), span: invariant.span.clone() }); }
        }
    }
    // A value with an invariant over a container's elements is built with that container empty:
    // true of no elements (docs/proposal-element-invariants.md).
    let empty = |ty: Ty| -> Vec<a::Expr> {
        let Some((module, invariants)) = layouts.invariants(ty) else { return Vec::new() };
        if module != f.module || heap_invariant(invariants) { return Vec::new(); }
        invariants.iter().filter_map(|invariant| {
            let a::Expression::Forall { upper, .. } = &invariant.kind else { return None };
            let zero = a::Expr { kind: a::Expression::Integer(0), span: invariant.span.clone() };
            Some(a::Expr { kind: a::Expression::Binary("==", upper.clone(), Box::new(zero)), span: invariant.span.clone() })
        }).collect()
    };
    if let Some((fields, mut invariants)) = of(result) {
        invariants.extend(empty(result));
        for invariant in &invariants { shown.push(a::Postcondition { kind: a::PostconditionKind::Predicate(at("result", &fields, invariant)), span: invariant.span.clone() }); }
    }
    // `Result<T, E>`: what the `Ok` variant holds, named by a binder no program can write.
    if let Some(sum) = layouts.result_sum(result) && let Some(&made) = sum.arms.first() && let Some((fields, mut invariants)) = of(made) {
        invariants.extend(empty(made));
        let clauses = invariants.iter().map(|invariant| a::Postcondition { kind: a::PostconditionKind::Predicate(at("made#", &fields, invariant)), span: invariant.span.clone() }).collect();
        let result_name = a::Expr { kind: a::Expression::Name("result".into()), span: f.span.clone() };
        shown.push(a::Postcondition { kind: a::PostconditionKind::Match { value: result_name, arms: vec![a::PostconditionArm { variant: "Ok".into(), binding: Some("made#".into()), clauses }] }, span: f.span.clone() });
    }
    (assumed, shown)
}

/// One `forall i in (0, v.len()): P` invariant of a parameter's type, for the resolver: how many
/// elements (`holder.v.len()`), `P` with each `v[i]` written `holder.v.#element` and the type's
/// own fields read from `holder`, and where it was written.
pub(super) struct ElementClause { pub upper: a::Expr, pub body: a::Expr, pub span: std::ops::Range<usize> }
/// The element invariants of the types `f` receives (docs/proposal-element-invariants.md). An
/// invariant not of the supported form is refused where it is written: the lower bound is 0,
/// the upper `v.len()` of a field `v`, and the index is used only as `v[i]`.
pub(super) fn element_clauses(f: &a::Function, params: &[Ty], layouts: &Types) -> Result<Vec<ElementClause>, crate::diagnostic::Diagnostic> {
    let refused = |e: &a::Expr, what: &str| crate::diagnostic::Diagnostic::new("W0401", e.span.clone(), format!("a type invariant over elements is `forall i in (0, field.len()): clause`, the clause reading `field[i]` and the type's own number fields: {what}"));
    let mut out = Vec::new();
    for ((name, _), ty) in f.parameters.iter().zip(params) {
        let pointee = match ty { Ty::Borrow { pointee, .. } => pointee.ty(), other => *other };
        let Some((module, invariants)) = layouts.invariants(pointee) else { continue };
        if module != f.module || heap_invariant(invariants) { continue; }
        let Some(record) = layouts.get(pointee) else { continue };
        let fields: Vec<String> = record.fields.iter().map(|field| field.name.clone()).collect();
        for invariant in invariants {
            let a::Expression::Forall { variable, lower, upper, body } = &invariant.kind else { continue };
            if !matches!(lower.kind, a::Expression::Integer(0)) { return Err(refused(lower, "the range starts at 0")); }
            // `v.len()`, `v` a field.
            let container = match &upper.kind {
                a::Expression::Call(callee, arguments) if arguments.is_empty() => match &callee.kind {
                    a::Expression::Field(base, method) if method == "len" => match &base.kind { a::Expression::Name(v) if fields.contains(v) => Some(v.clone()), _ => None },
                    _ => None,
                },
                _ => None,
            };
            let Some(container) = container else { return Err(refused(upper, "the range ends at the length of a field, `field.len()`")) };
            let held = |field: &str, span: &std::ops::Range<usize>| a::Expr { kind: a::Expression::Field(Box::new(a::Expr { kind: a::Expression::Name(name.clone()), span: span.clone() }), field.to_string()), span: span.clone() };
            let mut body = (**body).clone();
            let mut problem = None;
            body.visit_mut(&mut |e| {
                match &e.kind {
                    a::Expression::Index(base, at) => {
                        if matches!(&base.kind, a::Expression::Name(v) if *v == container) && matches!(&at.kind, a::Expression::Name(i) if i == variable) {
                            e.kind = a::Expression::Field(Box::new(held(&container, &e.span)), "#element".into());
                        } else if problem.is_none() { problem = Some(refused(e, "an element is read as `field[i]`, of the field whose length ends the range")); }
                        true
                    }
                    a::Expression::Name(n) if n == variable => { if problem.is_none() { problem = Some(refused(e, "the index is used only to read `field[i]`")); } true }
                    a::Expression::Name(n) if fields.contains(n) => { e.kind = held(n, &e.span).kind; true }
                    a::Expression::Forall { .. } => { if problem.is_none() { problem = Some(refused(e, "one `forall` is not written inside another")); } true }
                    _ => false,
                }
            });
            if let Some(problem) = problem { return Err(problem); }
            let length = a::Expr { kind: a::Expression::Call(Box::new(a::Expr { kind: a::Expression::Field(Box::new(held(&container, &upper.span)), "len".into()), span: upper.span.clone() }), Vec::new()), span: upper.span.clone() };
            out.push(ElementClause { upper: length, body, span: invariant.span.clone() });
        }
    }
    Ok(out)
}

/// In a function that writes the fields of a `&mut` parameter of an invariant type, the value may
/// break its invariant between those writes, which is shown only at each return. So there the
/// parameter is used only through its fields (`(*p).f`, `p.f`, read or written): never lent,
/// called a method on, read whole or copied, which would hand a broken value to code that
/// assumes the invariant (review: a proved `room()` called mid-update computed 5 - 7 unchecked).
/// The name of the first parameter used another way.
pub(super) fn invariant_escape(f: &a::Function, params: &[Ty], covered: &dyn Fn(Ty) -> bool) -> Option<String> {
    let is = |e: &a::Expr, name: &str| match &e.kind {
        a::Expression::Name(n) => n == name,
        a::Expression::Unary("*", inner) => matches!(&inner.kind, a::Expression::Name(n) if n == name),
        _ => false,
    };
    for ((name, _), ty) in f.parameters.iter().zip(params) {
        let Ty::Borrow { pointee, mutable: true } = ty else { continue };
        if !covered(pointee.ty()) { continue; }
        let mut body = f.body.clone();
        // Written through: an assignment whose target is a field of the parameter.
        let mut writes = false;
        fn targets<'a>(statements: &'a [a::Statement], out: &mut Vec<&'a a::Expr>) {
            for statement in statements {
                match statement {
                    a::Statement::Assign { target, .. } => out.push(target),
                    a::Statement::If { yes, no, .. } => { targets(yes, out); targets(no, out); }
                    a::Statement::While { body, .. } | a::Statement::For { body, .. } | a::Statement::Using { body, .. } | a::Statement::Block(body) => targets(body, out),
                    a::Statement::Match { arms, .. } => for arm in arms { targets(&arm.body, out); },
                    a::Statement::Guard { failure, .. } => targets(&failure.body, out),
                    a::Statement::Choose { inner, .. } => targets(std::slice::from_ref(&**inner), out),
                    _ => {}
                }
            }
        }
        let mut written = Vec::new();
        targets(&f.body, &mut written);
        for target in written {
            let mut e = target;
            while let a::Expression::Field(inner, _) | a::Expression::Index(inner, _) = &e.kind { if is(inner, name) { writes = true; break; } e = inner; }
            if is(target, name) { writes = true; }
        }
        // A field lent with `&mut` (to `memory.exchange`, or to any call) may be written there
        // (review: a swap and a proved setter changed the fields with no assignment in sight).
        let rooted = |mut e: &a::Expr| loop {
            if is(e, name) { return true; }
            match &e.kind { a::Expression::Field(inner, _) | a::Expression::Index(inner, _) => e = inner, _ => return false }
        };
        for statement in &mut body {
            statement.visit_exprs_mut(&mut |e| {
                match &e.kind {
                    a::Expression::Unary("&mut", inner) if rooted(inner) => writes = true,
                    // A method called on a field may take it `&mut` without saying so.
                    a::Expression::Call(callee, _) if matches!(&callee.kind, a::Expression::Field(base, _) if rooted(base) && !is(base, name)) => writes = true,
                    _ => {}
                }
                false
            });
        }
        // A `for` over a field steps it with a method once desugared (`next_mut` of an iterator).
        fn walked<'a>(statements: &'a [a::Statement], out: &mut Vec<&'a a::Expr>) {
            for statement in statements {
                match statement {
                    a::Statement::For { start, body, .. } => { out.push(start); walked(body, out); }
                    a::Statement::If { yes, no, .. } => { walked(yes, out); walked(no, out); }
                    a::Statement::While { body, .. } | a::Statement::Using { body, .. } | a::Statement::Block(body) => walked(body, out),
                    a::Statement::Match { arms, .. } => for arm in arms { walked(&arm.body, out); },
                    a::Statement::Guard { failure, .. } => walked(&failure.body, out),
                    a::Statement::Choose { inner, .. } => walked(std::slice::from_ref(&**inner), out),
                    _ => {}
                }
            }
        }
        let mut starts = Vec::new();
        walked(&f.body, &mut starts);
        if starts.iter().any(|start| rooted(start) && !is(start, name)) { writes = true; }
        if !writes { continue; }
        let mut other = false;
        for statement in &mut body {
            statement.visit_exprs_mut(&mut |e| {
                match &e.kind {
                    // A method called on it lends it whole.
                    a::Expression::Call(callee, _) if matches!(&callee.kind, a::Expression::Field(base, _) if is(base, name)) => { other = true; true }
                    a::Expression::Field(base, _) if is(base, name) => true,
                    a::Expression::Name(n) if n == name => { other = true; true }
                    _ => false,
                }
            });
        }
        if other { return Some(name.clone()); }
    }
    None
}
