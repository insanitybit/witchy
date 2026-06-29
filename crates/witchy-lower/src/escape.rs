//! RFC-0024 (seed): escape / confinement analysis.
//!
//! The first consumer this is built for is escape-driven SROA (RFC-0027): a
//! fixed-shape aggregate (record or tuple) bound to an immutable local and used
//! ONLY through field/index access never needs to exist as a heap object — its
//! fields can live in WASM locals. The enabling fact is confinement: the
//! aggregate's identity must never leave the frame.
//!
//! Under witchy's value semantics the only way a value's *identity* escapes a
//! function is to be used as a WHOLE value — returned, stored into another
//! aggregate, captured by a closure, passed to a call, compared, or rendered.
//! Reading a field or element (`p.x`, `xs[i]`) is not a whole-value use. So a
//! binding is SROA-eligible exactly when every occurrence is the base of a
//! field/index access. This is deliberately conservative (a single whole-value
//! use disqualifies it) and SOUND by construction; later increments sharpen it
//! and consolidate the other escape computations here per RFC-0024.
//!
//! Additive for now — no consumer is wired yet; the analysis is unit-tested in
//! isolation. The `sroa` lever in [`witchy_syntax::opt`] will gate the eventual
//! codegen consumer.
//!
//! # Codegen consumer (next increment, atomic)
//!
//! Wire scalar replacement into `codegen.rs` storing each field as a UNIFORM i64
//! slot in a local (mirroring the heap field layout), so every synthetic local is
//! i64 — no per-field valtype inference needed. Four edits:
//!
//! 1. `begin_unit` (~codegen.rs:2102): when `opt::enabled(Opt::Sroa)` and not
//!    `force_copy_mode()`, compute `escape::sroa_candidates` and stash the set.
//!    Reset a per-unit `sroa_active: HashSet<String>` (names actually replaced).
//! 2. `Stmt::Let` lowering (~2214): if `name` is a candidate and `value` is a
//!    `Ctor`/`Tuple` whose shape codegen can resolve, emit
//!    `SetLocal(name$i, ToSlot(lower(arg_i), kind_i))` per field instead of the
//!    `mk` allocation; record `name` in `sroa_active` and `name$i` in a
//!    declared-locals set. (If the shape can't be resolved, fall through to the
//!    normal alloc — do NOT add to `sroa_active`.)
//! 3. `Expr::Field { base: Var(p), field }` lowering (~4016): if `p ∈ sroa_active`,
//!    return `FromSlot(GetLocal(p$idx), field_kind)` — same `idx`/`kind` the
//!    existing arm computes — instead of the `Load`. Consistency holds because the
//!    `Let` precedes its uses in statement order, so `sroa_active` is populated
//!    first; an unresolved `Let` never enters `sroa_active`, so its fields stay
//!    heap loads.
//! 4. `assemble_wir_func` (~2000): declare each `name$i` as an i64 local.
//!
//! DoD: the differential sweep already walks `-sroa`, so `sroa == -sroa == none`
//! falls out; add a `witchy stats` assertion that a frame-confined aggregate's
//! `allocs`/`heap_bytes` drop with `sroa` on. Keep it conservative — record/tuple
//! field-assignment (`p.x = …`, a whole-value `RecordUpdate` read) already
//! disqualifies a candidate, so SROA only ever sees read-only aggregates.

use std::collections::HashSet;
use witchy_syntax::ast::{Block, Expr, Function, Stmt};

/// Immutable locals bound to a fixed-shape aggregate (record `Ctor` or tuple)
/// whose every use is a field/index access — so the aggregate never escapes the
/// frame as a whole value and is a candidate for scalar replacement.
pub fn sroa_candidates(f: &Function) -> HashSet<String> {
    sroa_candidates_block(&f.body)
}

/// As [`sroa_candidates`], over a body block directly (what codegen has in hand).
pub fn sroa_candidates_block(body: &Block) -> HashSet<String> {
    let mut potential = HashSet::new();
    collect_aggregate_lets(body, &mut potential);
    if potential.is_empty() {
        return potential;
    }
    let mut whole = HashSet::new();
    collect_whole_uses_block(body, &mut whole);
    potential.retain(|n| !whole.contains(n));
    potential
}

/// `let x = Ctor(..)` / `let x = (a, b, ..)` — immutable aggregate bindings. A
/// `var` (reassignable) binding is excluded: scalar-replacing a reassigned
/// aggregate is a later refinement.
fn collect_aggregate_lets(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        if let Stmt::Let { name, mutable: false, value, .. } = s {
            if matches!(value, Expr::Ctor { .. } | Expr::Tuple(_)) {
                out.insert(name.clone());
            }
        }
        each_block_in_stmt(s, &mut |blk| collect_aggregate_lets(blk, out));
    }
}

fn collect_whole_uses_block(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        each_expr_in_stmt(s, &mut |e| collect_whole_uses(e, out));
    }
}

/// Record every binding used as a WHOLE value. A bare `Var(n)` is a whole use;
/// `Var(n).field` and `Var(n)[i]` are NOT (the base var is read structurally).
fn collect_whole_uses(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Var(n) => {
            out.insert(n.clone());
        }
        // Field/index access: a Var base is a STRUCTURED (non-whole) use, so it is
        // not recorded; a compound base recurses normally. The index expression of
        // a subscript is an ordinary sub-expression.
        Expr::Field { base, .. } => {
            if !matches!(base.as_ref(), Expr::Var(_)) {
                collect_whole_uses(base, out);
            }
        }
        Expr::Index { base, index } => {
            if !matches!(base.as_ref(), Expr::Var(_)) {
                collect_whole_uses(base, out);
            }
            collect_whole_uses(index, out);
        }
        _ => each_subexpr(e, &mut |s| collect_whole_uses(s, out)),
    }
}

/// Apply `f` to each immediate sub-expression of `e` (no Field/Index special
/// casing — used for the generic recursion above). Mirrors the AST shape.
fn each_subexpr(e: &Expr, f: &mut impl FnMut(&Expr)) {
    match e {
        Expr::If { cond, then_block, else_block } => {
            f(cond);
            each_expr_in_block(then_block, f);
            if let Some(b) = else_block {
                each_expr_in_block(b, f);
            }
        }
        Expr::Match { scrutinee, arms } => {
            f(scrutinee);
            for a in arms {
                if let Some(g) = &a.guard {
                    f(g);
                }
                f(&a.body);
            }
        }
        Expr::Block(b) => each_expr_in_block(b, f),
        Expr::While { cond, body } => {
            f(cond);
            each_expr_in_block(body, f);
        }
        Expr::For { iter, body, .. } => {
            f(iter);
            each_expr_in_block(body, f);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            f(scrutinee);
            each_expr_in_block(body, f);
        }
        Expr::Lambda { body, .. } => each_expr_in_block(body, f),
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::List(args) | Expr::Tuple(args) => {
            for a in args {
                f(a);
            }
        }
        Expr::Apply { func, args } => {
            f(func);
            for a in args {
                f(a);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        Expr::Range { lo, hi, .. } => {
            f(lo);
            f(hi);
        }
        Expr::Index { base, index } => {
            f(base);
            f(index);
        }
        Expr::Field { base, .. } => f(base),
        Expr::MethodCall { receiver, args, .. } => {
            f(receiver);
            for a in args {
                f(a);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => f(expr),
        Expr::RecordUpdate { base, fields } => {
            f(base);
            for (_, v) in fields {
                f(v);
            }
        }
        _ => {}
    }
}

fn each_expr_in_block(b: &Block, f: &mut impl FnMut(&Expr)) {
    for s in &b.stmts {
        each_expr_in_stmt(s, f);
    }
}

fn each_expr_in_stmt(s: &Stmt, f: &mut impl FnMut(&Expr)) {
    match s {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetTuple { value, .. }
        | Stmt::Yield(value)
        | Stmt::Expr(value) => f(value),
        Stmt::Return(Some(e)) => f(e),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
    }
}

/// Apply `g` to each `Block` reachable from a statement's expressions.
fn each_block_in_stmt(s: &Stmt, g: &mut impl FnMut(&Block)) {
    each_expr_in_stmt(s, &mut |e| each_block_in_expr(e, g));
}

fn each_block_in_expr(e: &Expr, g: &mut impl FnMut(&Block)) {
    match e {
        Expr::If { then_block, else_block, .. } => {
            g(then_block);
            if let Some(b) = else_block {
                g(b);
            }
        }
        Expr::Block(b) | Expr::While { body: b, .. } | Expr::For { body: b, .. }
        | Expr::WhileLet { body: b, .. } => g(b),
        // A lambda is a separate function; its aggregates are scalar-replaced when
        // it is compiled, not by the enclosing function — don't descend.
        Expr::Lambda { .. } => {}
        Expr::Match { arms, .. } => {
            for a in arms {
                each_block_in_expr(&a.body, g);
            }
        }
        _ => each_subexpr(e, &mut |s| each_block_in_expr(s, g)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::parser;

    fn func(src: &str) -> Function {
        let m = parser::parse_module(src).expect("parse");
        m.items
            .into_iter()
            .find_map(|it| match it {
                witchy_syntax::ast::Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("a function")
    }

    #[test]
    fn field_only_aggregate_is_a_candidate() {
        let f = func(
            "type P:\n    x: Int\n    y: Int\nfn d(a: Int) -> Int:\n    let p = P(a, a)\n    p.x + p.y\n",
        );
        assert!(sroa_candidates(&f).contains("p"), "field-only record is SROA-eligible");
    }

    #[test]
    fn returned_aggregate_escapes() {
        let f = func("type P:\n    x: Int\n    y: Int\nfn mk(a: Int) -> P:\n    let p = P(a, a)\n    p\n");
        assert!(!sroa_candidates(&f).contains("p"), "a returned aggregate escapes whole");
    }

    #[test]
    fn aggregate_passed_to_a_call_escapes() {
        let f = func(
            "type P:\n    x: Int\n    y: Int\nfn use_it(q: P) -> Int:\n    q.x\nfn d(a: Int) -> Int:\n    let p = P(a, a)\n    use_it(p)\n",
        );
        assert!(!sroa_candidates(&f).contains("p"), "passing the whole value escapes");
    }

    #[test]
    fn tuple_field_only_is_a_candidate() {
        let f = func("fn d(a: Int) -> Int:\n    let t = (a, a)\n    t.0 + t.1\n");
        assert!(sroa_candidates(&f).contains("t"));
    }

    #[test]
    fn interpolating_the_whole_value_escapes() {
        let f = func(
            "type P:\n    x: Int\n    y: Int\nfn d(a: Int) -> String:\n    let p = P(a, a)\n    \"${p}\"\n",
        );
        assert!(!sroa_candidates(&f).contains("p"), "rendering the whole value escapes");
    }
}
