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

use std::collections::{HashMap, HashSet};
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

/// (RFC-0028 confined Views) Locals bound to a slice of another list — `let w =
/// list.slice(xs, lo, hi)` where `xs` is a plain variable — that are used ONLY via
/// view-safe reads (`w[i]` / `w.length()`, i.e. `list.at`/`list.length`), never as
/// a whole value, and whose source `xs` is never reassigned or mutated while `w` is
/// live. Such a `w` is a confined, read-only borrow that can be a zero-copy slice
/// (its source pointer + offset + length in locals) instead of a copied list.
///
/// Conservative and SOUND: any whole-value use of `w` (passing it, returning it,
/// storing it, rendering it), or any assignment to `xs` (including an in-place
/// `xs.push`/`set_at`, which can reallocate the buffer `w` borrows), disqualifies it.
pub fn confined_slice_candidates(f: &Function) -> HashSet<String> {
    confined_slice_candidates_block(&f.body)
}

/// As [`confined_slice_candidates`], over a body block directly.
pub fn confined_slice_candidates_block(body: &Block) -> HashSet<String> {
    let mut src_of: HashMap<String, String> = HashMap::new();
    collect_slice_lets(body, &mut src_of);
    if src_of.is_empty() {
        return HashSet::new();
    }
    let mut whole = HashSet::new();
    collect_whole_uses_block(body, &mut whole);
    let mut reassigned = HashSet::new();
    collect_assigned_targets(body, &mut reassigned);
    src_of
        .into_iter()
        // `w` is read-only-by-view (never whole), and the source is neither
        // reassigned/mutated nor used as a whole value anywhere else (so no alias
        // can mutate the buffer the view borrows). The slice binding's own use of
        // the source is exempted in `collect_whole_uses`, so a whole `src` here is
        // a genuine second use.
        .filter(|(w, src)| {
            !whole.contains(w) && !reassigned.contains(src) && !whole.contains(src)
        })
        .map(|(w, _)| w)
        .collect()
}

/// Whether a call name is `list.slice` — possibly with a monomorphization suffix
/// (`list.slice__Int`) appended by generic instantiation.
pub fn is_list_slice(callee: &str) -> bool {
    callee == "list.slice" || callee.starts_with("list.slice__")
}

/// (RFC-0027 packed, inferred case) Immutable locals bound to a LIST LITERAL of
/// record constructors (`let xs = [P(..), P(..), ..]`) whose every use is either
/// `list.length(xs)` or a `list.at(xs, i).field` read — the list is read only
/// element-field-wise, never escapes as a whole value, no element is taken whole,
/// and it is never reassigned. Such a list can be stored as a flat PACKED buffer
/// (fields inline) instead of an array of pointers to boxed records, so each
/// `list.at(xs, i).field` lowers to a direct slot read — the cache-density win of
/// RFC-0027's `packed`, delivered by inference for the confined case (the dual of
/// how confined views deliver zero-copy borrows; see [`confined_slice_candidates`]).
///
/// Conservative + SOUND: binding an element whole (`let p = list.at(xs, i)`), using
/// the list whole (passing/returning/rendering it), or any reassignment disqualifies
/// it. The element record must also be PACKABLE (fixed-size, all-scalar fields) —
/// a type-level check this AST pass leaves to the codegen consumer.
pub fn confined_record_list_candidates(f: &Function) -> HashSet<String> {
    confined_record_list_candidates_block(&f.body)
}

/// As [`confined_record_list_candidates`], over a body block directly.
pub fn confined_record_list_candidates_block(body: &Block) -> HashSet<String> {
    let mut lets = HashSet::new();
    collect_record_list_lets(body, &mut lets);
    if lets.is_empty() {
        return lets;
    }
    let mut disqualified = HashSet::new();
    mark_non_packed_uses_block(body, &mut disqualified);
    let mut reassigned = HashSet::new();
    collect_assigned_targets(body, &mut reassigned);
    lets.retain(|n| !disqualified.contains(n) && !reassigned.contains(n));
    lets
}

/// (RFC-0016 never-OOM, in-place reuse rung) A `var` bound to a list LITERAL of a
/// fixed length L, every reassignment to which is ALSO a list literal of length L,
/// and which is NEVER used as a whole value (only element reads — `x[i]`,
/// `list.at(x, i)`, `list.length(x)` — and the reassignments themselves). The
/// escape oracle proves such a var's buffer is never aliased, so each reassignment
/// may OVERWRITE the existing L-slot buffer IN PLACE instead of allocating a fresh
/// list. This bounds a build-and-drop loop (`var x = [..]; while …: x = [..]`) that
/// otherwise leaks O(n): the arena/watermark cannot reclaim a value that escaped
/// the loop body and only LATER became dead (measured in
/// `stats::escaping_reassignment_leaks_without_rc_floor`). Parity-safe — with no
/// alias, an in-place overwrite is indistinguishable from a fresh value.
///
/// Conservative + SOUND: a whole-value use (pass / return / store / render /
/// compare / `let y = x`), a reassignment that is not a same-length list literal,
/// or a non-literal binding disqualifies it. A reassignment whose RHS reads the var
/// (`x = [x.at(0), …]`) is left to the codegen consumer, which allocates for that
/// one site rather than corrupt a slot mid-write.
pub fn confined_inplace_reuse_vars(f: &Function) -> HashSet<String> {
    confined_inplace_reuse_vars_block(&f.body)
}

/// As [`confined_inplace_reuse_vars`], over a body block directly.
pub fn confined_inplace_reuse_vars_block(body: &Block) -> HashSet<String> {
    let mut len_of: HashMap<String, usize> = HashMap::new();
    collect_list_literal_lets(body, &mut len_of);
    if len_of.is_empty() {
        return HashSet::new();
    }
    // Every reassignment must be a list literal of the SAME length, or the fixed
    // L-slot buffer the in-place overwrite relies on would not hold.
    let mut disq = HashSet::new();
    mark_nonuniform_reassigns(body, &len_of, &mut disq);
    // Never used as a whole value (so the buffer has no other reference).
    let mut whole = HashSet::new();
    mark_reuse_escapes_block(body, &mut whole);
    len_of
        .into_iter()
        .filter(|(x, _)| !disq.contains(x) && !whole.contains(x))
        .map(|(x, _)| x)
        .collect()
}

/// `var x = [..]` / `let x = [..]` — a binding whose value is a list literal;
/// record `x -> length`. (A non-literal binding is not a fixed-size buffer.)
fn collect_list_literal_lets(b: &Block, out: &mut HashMap<String, usize>) {
    for s in &b.stmts {
        if let Stmt::Let { name, value: Expr::List(items), .. } = s {
            out.insert(name.clone(), items.len());
        }
        each_block_in_stmt(s, &mut |blk| collect_list_literal_lets(blk, out));
    }
}

/// Mark a candidate if any reassignment `x = <v>` is NOT a list literal of x's
/// recorded length (a different length, or a non-literal — neither preserves the
/// fixed L-slot buffer the in-place overwrite needs).
fn mark_nonuniform_reassigns(b: &Block, len_of: &HashMap<String, usize>, out: &mut HashSet<String>) {
    for s in &b.stmts {
        if let Stmt::Assign { name, value } = s {
            if let Some(&l) = len_of.get(name) {
                match value {
                    Expr::List(items) if items.len() == l => {}
                    _ => {
                        out.insert(name.clone());
                    }
                }
            }
        }
        each_block_in_stmt(s, &mut |blk| mark_nonuniform_reassigns(blk, len_of, out));
    }
}

/// Like [`collect_whole_uses_block`] but a reassignment `x = <v>` does NOT mark `x`
/// (a same-length list-literal reassignment is exactly what the reuse rung wants,
/// not an escape) — only a genuine whole-value use of `x` in some expression marks
/// it. Recurses nested blocks through itself (NOT `collect_whole_uses_block`, which
/// would re-introduce the assign-target marking).
fn mark_reuse_escapes_block(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        match s {
            // Skip the assign TARGET; scan only the RHS (a fresh literal won't
            // mention the target unless it reads it, which the consumer handles).
            Stmt::Assign { value, .. } => mark_reuse_escapes_expr(value, out),
            _ => each_expr_in_stmt(s, &mut |e| mark_reuse_escapes_expr(e, out)),
        }
    }
}

/// The expression-level companion of [`mark_reuse_escapes_block`]: identical to
/// [`collect_whole_uses`] (bare `Var` is a whole use; `x[i]`/`x.field`/`list.at`/
/// `list.length`/`list.slice` element reads are exempt; a closure capture marks
/// everything it mentions) EXCEPT that control-flow blocks recurse through
/// `mark_reuse_escapes_block` so nested reassignments stay non-escaping.
fn mark_reuse_escapes_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Var(n) => {
            out.insert(n.clone());
        }
        Expr::Field { base, .. } => {
            if !matches!(base.as_ref(), Expr::Var(_)) {
                mark_reuse_escapes_expr(base, out);
            }
        }
        Expr::Index { base, index } => {
            if !matches!(base.as_ref(), Expr::Var(_)) {
                mark_reuse_escapes_expr(base, out);
            }
            mark_reuse_escapes_expr(index, out);
        }
        Expr::If { cond, then_block, else_block } => {
            mark_reuse_escapes_expr(cond, out);
            mark_reuse_escapes_block(then_block, out);
            if let Some(b) = else_block {
                mark_reuse_escapes_block(b, out);
            }
        }
        Expr::While { cond, body } => {
            mark_reuse_escapes_expr(cond, out);
            mark_reuse_escapes_block(body, out);
        }
        Expr::For { iter, body, .. } => {
            mark_reuse_escapes_expr(iter, out);
            mark_reuse_escapes_block(body, out);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            mark_reuse_escapes_expr(scrutinee, out);
            mark_reuse_escapes_block(body, out);
        }
        Expr::Block(b) => mark_reuse_escapes_block(b, out),
        Expr::Match { scrutinee, arms } => {
            mark_reuse_escapes_expr(scrutinee, out);
            for a in arms {
                if let Some(g) = &a.guard {
                    mark_reuse_escapes_expr(g, out);
                }
                mark_reuse_escapes_expr(&a.body, out);
            }
        }
        Expr::Lambda { body, .. } => {
            for s in &body.stmts {
                each_expr_in_stmt(s, &mut |x| mark_all_vars(x, out));
            }
        }
        Expr::Call { name, args }
            if (name == "list.at" || name == "list.length" || is_list_slice(name))
                && matches!(args.first(), Some(Expr::Var(_))) =>
        {
            for a in &args[1..] {
                mark_reuse_escapes_expr(a, out);
            }
        }
        _ => each_subexpr(e, &mut |s| mark_reuse_escapes_expr(s, out)),
    }
}

/// `let xs = [Ctor(..), Ctor(..), ..]` — a non-empty list literal whose every
/// element is a record constructor. Records the binding name.
fn collect_record_list_lets(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        if let Stmt::Let { name, value: Expr::List(items), .. } = s {
            if !items.is_empty() && items.iter().all(|e| matches!(e, Expr::Ctor { .. })) {
                out.insert(name.clone());
            }
        }
        each_block_in_stmt(s, &mut |blk| collect_record_list_lets(blk, out));
    }
}

fn mark_non_packed_uses_block(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        each_expr_in_stmt(s, &mut |e| mark_non_packed_uses(e, out));
    }
}

/// Mark every variable used in a position that is NOT packed-safe. A packed-safe
/// list use is exactly `list.length(xs)` or `list.at(xs, i)` as the immediate base
/// of a `.field` access; the index `i` is an ordinary sub-expression. Any other
/// occurrence of a variable — used whole, or a `list.at` result taken whole (it
/// falls through to the generic arm, which marks the list var) — disqualifies it.
fn mark_non_packed_uses(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Var(n) => {
            out.insert(n.clone());
        }
        // `list.at(xs, i).field`: the `list.at` base is a packed-safe read of `xs`,
        // so do NOT mark `xs` — only the index is an ordinary sub-expression.
        Expr::Field { base, .. }
            if matches!(base.as_ref(),
                Expr::Call { name, args }
                    if name == "list.at" && matches!(args.first(), Some(Expr::Var(_)))) =>
        {
            if let Expr::Call { args, .. } = base.as_ref() {
                for a in &args[1..] {
                    mark_non_packed_uses(a, out);
                }
            }
        }
        // `list.length(xs)` is packed-safe; a bare `list.at(xs, i)` (NOT under a
        // field) is NOT — it falls to the generic arm and marks `xs`.
        Expr::Call { name, args }
            if name == "list.length" && matches!(args.first(), Some(Expr::Var(_))) =>
        {
            for a in &args[1..] {
                mark_non_packed_uses(a, out);
            }
        }
        _ => each_subexpr(e, &mut |s| mark_non_packed_uses(s, out)),
    }
}

/// `let w = list.slice(xs, lo, hi)` (xs a plain var) — record `w -> xs`.
fn collect_slice_lets(b: &Block, out: &mut HashMap<String, String>) {
    for s in &b.stmts {
        if let Stmt::Let { name, value: Expr::Call { name: callee, args }, .. } = s {
            if is_list_slice(callee) {
                if let Some(Expr::Var(src)) = args.first() {
                    out.insert(name.clone(), src.clone());
                }
            }
        }
        each_block_in_stmt(s, &mut |blk| collect_slice_lets(blk, out));
    }
}

/// Every name that is the target of a reassignment (`x = …`), at any block depth —
/// including the in-place sugar (`x.push`/`x[i] = …` desugar to `x = …`).
fn collect_assigned_targets(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        if let Stmt::Assign { name, .. } = s {
            out.insert(name.clone());
        }
        each_block_in_stmt(s, &mut |blk| collect_assigned_targets(blk, out));
    }
}

/// `let x = Ctor(..)` / `var x = (a, b, ..)` — aggregate bindings (immutable or
/// mutable). A mutable one stays a candidate as long as every write is a field
/// update or a whole-aggregate reassignment (checked in `collect_whole_uses_block`).
fn collect_aggregate_lets(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        if let Stmt::Let { name, value, .. } = s {
            if matches!(value, Expr::Ctor { .. } | Expr::Tuple(_)) {
                out.insert(name.clone());
            }
        }
        each_block_in_stmt(s, &mut |blk| collect_aggregate_lets(blk, out));
    }
}

fn collect_whole_uses_block(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        // A top-level write to an aggregate is SROA-compatible without escaping it
        // when it is a field update of itself (`p.x = v` desugars to
        // `p = RecordUpdate{ base: p, .. }`) or a whole reassignment to a fresh
        // aggregate (`p = Point(..)`): scan only the new field/element values, not
        // the `..p` spread base. Any OTHER assignment to `name` disqualifies it
        // (codegen can't scalar-replace it); a write nested in a sub-block falls
        // through to the generic walk below and disqualifies via the `..p` base.
        if let Stmt::Assign { name, value } = s {
            // The only SROA-compatible write is a SINGLE-field update of itself
            // (`p.x = v` desugars to `p = RecordUpdate{ base: p, [(x, v)] }`): one
            // field local changes, and its value is evaluated before the write, so
            // there is no cross-field read hazard. Scan only the field value (not
            // the `..p` base). Every other assignment to `name` — a whole
            // reassignment, a multi-field spread, an alias — disqualifies it.
            if let Expr::RecordUpdate { base, fields } = value {
                if fields.len() == 1 && matches!(base.as_ref(), Expr::Var(x) if x == name) {
                    collect_whole_uses(&fields[0].1, out);
                    continue;
                }
            }
            out.insert(name.clone());
            collect_whole_uses(value, out);
            continue;
        }
        each_expr_in_stmt(s, &mut |e| collect_whole_uses(e, out));
    }
}

/// Record every binding used as a WHOLE value. A bare `Var(n)` is a whole use;
/// `Var(n).field` and `Var(n)[i]` are NOT (the base var is read structurally).
/// Block-bearing forms recurse through [`collect_whole_uses_block`] so a nested
/// single-field self-update (`p.x = v`) stays a structured write, not a whole use.
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
        Expr::If { cond, then_block, else_block } => {
            collect_whole_uses(cond, out);
            collect_whole_uses_block(then_block, out);
            if let Some(b) = else_block {
                collect_whole_uses_block(b, out);
            }
        }
        Expr::While { cond, body } => {
            collect_whole_uses(cond, out);
            collect_whole_uses_block(body, out);
        }
        Expr::For { iter, body, .. } => {
            collect_whole_uses(iter, out);
            collect_whole_uses_block(body, out);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            collect_whole_uses(scrutinee, out);
            collect_whole_uses_block(body, out);
        }
        Expr::Block(b) => collect_whole_uses_block(b, out),
        Expr::Match { scrutinee, arms } => {
            collect_whole_uses(scrutinee, out);
            for a in arms {
                if let Some(g) = &a.guard {
                    collect_whole_uses(g, out);
                }
                collect_whole_uses(&a.body, out);
            }
        }
        // A closure captures (by value) every free variable it references, so ANY
        // mention of a name inside — even `p.field` — makes that name escape whole.
        Expr::Lambda { body, .. } => {
            for s in &body.stmts {
                each_expr_in_stmt(s, &mut |x| mark_all_vars(x, out));
            }
        }
        // A view-safe READ — `list.at(v, i)` / `list.length(v)` — does not use its
        // list argument as a whole value (it indexes/measures it), so a binding
        // used only this way stays a confined-slice candidate (RFC-0028). Likewise
        // `list.slice(src, lo, hi)` reads `src` structurally: a source used ONLY as
        // a slice source is never mutated/aliased whole, which is what lets the
        // copy be elided safely. Records are never list arguments, so this does not
        // affect SROA candidates.
        Expr::Call { name, args }
            if (name == "list.at" || name == "list.length" || is_list_slice(name))
                && matches!(args.first(), Some(Expr::Var(_))) =>
        {
            for a in &args[1..] {
                collect_whole_uses(a, out);
            }
        }
        _ => each_subexpr(e, &mut |s| collect_whole_uses(s, out)),
    }
}

/// Mark every variable an expression mentions as a whole use (used for closure
/// bodies, where any reference is a by-value capture).
fn mark_all_vars(e: &Expr, out: &mut HashSet<String>) {
    if let Expr::Var(n) = e {
        out.insert(n.clone());
    }
    each_subexpr(e, &mut |s| mark_all_vars(s, out));
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
    fn mutable_record_with_field_writes_is_a_candidate() {
        let f = func(
            "type P:\n    x: Int\n    y: Int\nfn d(a: Int) -> Int:\n    var p = P(a, a)\n    p.x = p.x + 1\n    p.y = p.x * 2\n    p.x + p.y\n",
        );
        assert!(sroa_candidates(&f).contains("p"), "a field-written mutable record is SROA-eligible");
    }

    #[test]
    fn mutable_record_field_written_in_loop_is_a_candidate() {
        let f = func(
            "type P:\n    x: Int\n    y: Int\nfn d(n: Int) -> Int:\n    var total = 0\n    var i = 0\n    while i < n:\n        var p = P(i, 0)\n        p.x = p.x + 1\n        total = total + p.x\n        i = i + 1\n    total\n",
        );
        assert!(
            sroa_candidates(&f).contains("p"),
            "a record field-written inside a loop is still SROA-eligible (block-aware scan)"
        );
    }

    #[test]
    fn whole_reassignment_disqualifies() {
        let f = func(
            "type P:\n    x: Int\n    y: Int\nfn d(a: Int, q: P) -> Int:\n    var p = P(a, a)\n    p = q\n    p.x\n",
        );
        assert!(!sroa_candidates(&f).contains("p"), "a whole reassignment escapes");
    }

    #[test]
    fn captured_by_closure_disqualifies() {
        let f = func(
            "type P:\n    x: Int\n    y: Int\nfn d(a: Int) -> Int:\n    let p = P(a, a)\n    let g = fn() -> Int: p.x + p.y\n    use_it(g)\n",
        );
        assert!(
            !sroa_candidates(&f).contains("p"),
            "a record referenced inside a closure is captured whole"
        );
    }

    #[test]
    fn confined_slice_used_via_at_is_a_candidate() {
        let f = func(
            "fn d(xs: List(Int)) -> Int:\n    let w = list.slice(xs, 1, 3)\n    list.at(w, 0) + list.at(w, 1) + list.length(w)\n",
        );
        assert!(
            confined_slice_candidates(&f).contains("w"),
            "a slice used only via at/length is a zero-copy candidate"
        );
    }

    #[test]
    fn slice_passed_whole_is_not_a_candidate() {
        let f = func(
            "fn d(xs: List(Int)) -> Int:\n    let w = list.slice(xs, 1, 3)\n    use_it(w)\n",
        );
        assert!(!confined_slice_candidates(&f).contains("w"), "passing the slice whole escapes");
    }

    #[test]
    fn slice_whose_source_is_mutated_is_not_a_candidate() {
        let f = func(
            "fn d(var xs: List(Int)) -> Int:\n    let w = list.slice(xs, 1, 3)\n    xs = list.push(xs, 9)\n    list.at(w, 0)\n",
        );
        assert!(
            !confined_slice_candidates(&f).contains("w"),
            "mutating the source can reallocate the borrowed buffer"
        );
    }

    #[test]
    fn slice_whose_source_is_used_whole_is_not_a_candidate() {
        // The source is aliased (`let ys = xs`), so an in-place mutation of the
        // alias could rewrite the buffer the view borrows — disqualify it even
        // though `xs` itself is never directly reassigned.
        let f = func(
            "fn d(xs: List(Int)) -> Int:\n    let w = list.slice(xs, 1, 3)\n    let ys = xs\n    list.at(w, 0) + list.length(ys)\n",
        );
        assert!(
            !confined_slice_candidates(&f).contains("w"),
            "a source used as a whole value elsewhere may be aliased and mutated"
        );
    }

    #[test]
    fn two_views_of_an_unmutated_source_are_both_candidates() {
        // The slice binding's own use of the source is exempt, so a source read
        // ONLY as a slice source (here twice) stays eligible for both windows.
        let f = func(
            "fn d(xs: List(Int)) -> Int:\n    let a = list.slice(xs, 0, 2)\n    let b = list.slice(xs, 2, 4)\n    list.at(a, 0) + list.at(b, 0)\n",
        );
        let c = confined_slice_candidates(&f);
        assert!(c.contains("a") && c.contains("b"), "both read-only windows are candidates: {c:?}");
    }

    #[test]
    fn record_list_read_via_at_field_is_a_packed_candidate() {
        let f = func(
            "type P:\n    x: Int\n    y: Int\nfn d() -> Int:\n    let xs = [P(1, 2), P(3, 4)]\n    list.at(xs, 0).x + list.at(xs, 1).y + list.length(xs)\n",
        );
        assert!(
            confined_record_list_candidates(&f).contains("xs"),
            "a record list read only via at(_).field / length is a packed candidate"
        );
    }

    #[test]
    fn record_list_with_element_taken_whole_is_not_a_packed_candidate() {
        // Binding an element whole (`let p = list.at(xs, 0)`) means the element must
        // exist as a record value — the list cannot be packed flat.
        let f = func(
            "type P:\n    x: Int\n    y: Int\nfn d() -> Int:\n    let xs = [P(1, 2), P(3, 4)]\n    let p = list.at(xs, 0)\n    p.x\n",
        );
        assert!(
            !confined_record_list_candidates(&f).contains("xs"),
            "an element taken whole disqualifies packing"
        );
    }

    #[test]
    fn record_list_passed_whole_is_not_a_packed_candidate() {
        let f = func(
            "type P:\n    x: Int\n    y: Int\nfn d() -> Int:\n    let xs = [P(1, 2)]\n    use_it(xs)\n",
        );
        assert!(!confined_record_list_candidates(&f).contains("xs"), "passing the list whole escapes");
    }

    #[test]
    fn reassigned_record_list_is_not_a_packed_candidate() {
        let f = func(
            "type P:\n    x: Int\n    y: Int\nfn d(var xs: List(P)) -> Int:\n    xs = [P(1, 2)]\n    list.at(xs, 0).x\n",
        );
        assert!(
            !confined_record_list_candidates(&f).contains("xs"),
            "a reassigned list is not a fixed packed buffer"
        );
    }

    #[test]
    fn reassigned_confined_list_var_is_a_reuse_candidate() {
        // `var x = [..3..]; while …: x = [..3..]`, x read only via at/length —
        // never whole-used, uniform length → the buffer can be overwritten in place.
        let f = func(
            "fn d(n: Int) -> Int:\n    var x = [0, 0, 0]\n    var i = 0\n    while i < n:\n        x = [i, i + 1, i + 2]\n        i = i + 1\n    list.at(x, 0) + list.length(x)\n",
        );
        assert!(
            confined_inplace_reuse_vars(&f).contains("x"),
            "a confined, never-whole-used var reassigned to same-length literals is a reuse candidate"
        );
    }

    #[test]
    fn reuse_var_used_whole_is_not_a_candidate() {
        let f = func(
            "fn d() -> Int:\n    var x = [0, 0, 0]\n    x = [1, 2, 3]\n    use_it(x)\n    list.length(x)\n",
        );
        assert!(
            !confined_inplace_reuse_vars(&f).contains("x"),
            "passing the var whole means its buffer may be aliased — not reuse-safe"
        );
    }

    #[test]
    fn reuse_var_aliased_by_let_is_not_a_candidate() {
        let f = func(
            "fn d() -> Int:\n    var x = [0, 0, 0]\n    x = [1, 2, 3]\n    let y = x\n    list.at(y, 0)\n",
        );
        assert!(
            !confined_inplace_reuse_vars(&f).contains("x"),
            "binding `let y = x` aliases the buffer — in-place overwrite would corrupt y"
        );
    }

    #[test]
    fn reuse_var_with_nonuniform_length_is_not_a_candidate() {
        let f = func(
            "fn d() -> Int:\n    var x = [0, 0, 0]\n    x = [1, 2]\n    list.length(x)\n",
        );
        assert!(
            !confined_inplace_reuse_vars(&f).contains("x"),
            "a different-length reassignment breaks the fixed-buffer invariant"
        );
    }

    #[test]
    fn interpolating_the_whole_value_escapes() {
        let f = func(
            "type P:\n    x: Int\n    y: Int\nfn d(a: Int) -> String:\n    let p = P(a, a)\n    \"${p}\"\n",
        );
        assert!(!sroa_candidates(&f).contains("p"), "rendering the whole value escapes");
    }
}
