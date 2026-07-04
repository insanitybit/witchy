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

use crate::analysis::Summaries;
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

/// (RFC-0016 never-OOM, in-place reuse rung) A `var` bound to a list literal (any
/// length) reassigned only to list literals, OR a record constructor reassigned only
/// to the same constructor — and which is NEVER used as a whole value (only element
/// reads — `x[i]`, `list.at(x, i)`, `list.length(x)`, `x.field` — and the
/// reassignments themselves). The escape oracle proves such a var's buffer is never
/// aliased, so each reassignment may REUSE the existing buffer instead of allocating
/// a fresh value: a record (fixed arity) overwrites its field slots; a list reuses
/// the buffer when the new length fits its capacity (else reallocates — codegen
/// capacity-checks at runtime). This bounds a build-and-drop loop (`var x = [..]; while …: x = [..]`) that
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
    let mut shape_of: HashMap<String, ReuseShape> = HashMap::new();
    collect_reuse_lets(body, &mut shape_of);
    if shape_of.is_empty() {
        return HashSet::new();
    }
    // Every reassignment must match the binding's shape (a list literal of the same
    // length, or a constructor of the same name → same tag + arity), or the fixed
    // buffer the in-place overwrite relies on would not hold.
    let mut disq = HashSet::new();
    mark_nonuniform_reassigns(body, &shape_of, &mut disq);
    // Never used as a whole value (so the buffer has no other reference). The
    // reuse policy: a reassignment does NOT mark its target (that overwrite is the
    // optimization), only a genuine whole-value use elsewhere does.
    let mut whole = HashSet::new();
    scan_uses_block(body, UseScan::ReuseRhs, CallExempt::ViewReads, &mut whole);
    shape_of
        .into_iter()
        .filter(|(x, _)| !disq.contains(x) && !whole.contains(x))
        .map(|(x, _)| x)
        .collect()
}

/// The fixed buffer shape of a reuse candidate: a list of fixed length, or a record
/// constructor of a fixed name (fixed tag + field arity). A same-shape reassignment
/// can overwrite the existing buffer slot-for-slot.
#[derive(Clone, PartialEq)]
enum ReuseShape {
    List(usize),
    Ctor(String),
}

/// `var x = [..]` / `var x = Ctor(..)` (or `let`) — a binding whose value is a list
/// literal or a record constructor; record `x -> shape`. (A non-literal binding is
/// not a fixed-size buffer.)
fn collect_reuse_lets(b: &Block, out: &mut HashMap<String, ReuseShape>) {
    for s in &b.stmts {
        if let Stmt::Let { name, value, .. } = s {
            match value {
                Expr::List(items) => {
                    out.insert(name.clone(), ReuseShape::List(items.len()));
                }
                Expr::Ctor { name: ctor, .. } => {
                    out.insert(name.clone(), ReuseShape::Ctor(ctor.clone()));
                }
                _ => {}
            }
        }
        each_block_in_stmt(s, &mut |blk| collect_reuse_lets(blk, out));
    }
}

/// Mark a candidate if any reassignment `x = <v>` does NOT match x's recorded shape
/// (a different-length list, a different constructor, or a non-literal — none
/// preserves the fixed buffer the in-place overwrite needs).
fn mark_nonuniform_reassigns(
    b: &Block,
    shape_of: &HashMap<String, ReuseShape>,
    out: &mut HashSet<String>,
) {
    for s in &b.stmts {
        if let Stmt::Assign { name, value } = s {
            if let Some(shape) = shape_of.get(name) {
                let matches = match (shape, value) {
                    // A list var may be reassigned to a list literal of ANY length —
                    // codegen capacity-checks at runtime (reuse the buffer when it
                    // fits, else reallocate). A record var must keep the SAME ctor
                    // (fixed tag + arity) so its fixed buffer is always exactly right.
                    (ReuseShape::List(_), Expr::List(_)) => true,
                    (ReuseShape::Ctor(c), Expr::Ctor { name: c2, .. }) => c == c2,
                    _ => false,
                };
                if !matches {
                    out.insert(name.clone());
                }
            }
        }
        each_block_in_stmt(s, &mut |blk| mark_nonuniform_reassigns(blk, shape_of, out));
    }
}

/// (RFC-0024) How a reassignment `x = <v>` is treated when scanning for whole-value
/// uses — the first policy axis. Consolidates the formerly-duplicated
/// `collect_whole_uses` / `mark_reuse_escapes` / `mark_leaking_uses` walkers.
#[derive(Clone, Copy, PartialEq)]
enum UseScan {
    /// A top-level assignment MARKS its target as a whole use (except a single-field
    /// self-update `p.x = v`, the only SROA-compatible write). Used by the
    /// aggregate / SROA candidate passes.
    Whole,
    /// A reassignment does NOT mark its target (the overwrite is exactly what the
    /// in-place reuse rung / RC-floor want, not an escape) — only a genuine
    /// whole-value use in some expression marks. Otherwise identical to `Whole`.
    ReuseRhs,
}

/// Which call arguments are exempt (read structurally, not escaped whole) — the
/// second policy axis.
#[derive(Clone, Copy)]
enum CallExempt<'a> {
    /// The fixed view-safe reads: only `list.at` / `list.length` / `list.slice` with a
    /// `Var` first argument exempt that argument; every other call escapes its `Var`
    /// args (conservative — the SROA / slice / reuse candidate passes).
    ViewReads,
    /// Summary-aware: a `Var` argument is exempt iff the callee does not alias it out
    /// (a borrow, an `own` move, or a read-only builtin — `Summaries::arg_leaks`),
    /// general over builtins AND user functions (RC-floor confinement).
    NonLeaking(&'a Summaries),
}

/// Record every binding used as a WHOLE value in `b` under the given policies. A bare
/// `Var(n)` is a whole use; `Var(n).field` / `Var(n)[i]` are not; call-argument
/// exemption follows `call`; a closure capture marks everything it mentions. One
/// walker shared by the SROA/slice/reuse passes (`ViewReads`) and RC-floor
/// confinement (`NonLeaking`).
fn scan_uses_block(b: &Block, mode: UseScan, call: CallExempt, out: &mut HashSet<String>) {
    for s in &b.stmts {
        if let Stmt::Assign { name, value } = s {
            match mode {
                UseScan::Whole => {
                    // The only SROA-compatible write is a SINGLE-field self-update
                    // (`p.x = v` desugars to `p = RecordUpdate{ base: p, [(x, v)] }`):
                    // scan only the field value, not the `..p` base. Every other
                    // assignment to `name` marks it.
                    if let Expr::RecordUpdate { base, fields } = value {
                        if fields.len() == 1 && matches!(base.as_ref(), Expr::Var(x) if x == name) {
                            scan_uses_expr(&fields[0].1, mode, call, out);
                            continue;
                        }
                    }
                    out.insert(name.clone());
                    scan_uses_expr(value, mode, call, out);
                }
                // Skip the assign TARGET; scan only the RHS.
                UseScan::ReuseRhs => scan_uses_expr(value, mode, call, out),
            }
            continue;
        }
        each_expr_in_stmt(s, &mut |e| scan_uses_expr(e, mode, call, out));
    }
}

fn scan_uses_expr(e: &Expr, mode: UseScan, call: CallExempt, out: &mut HashSet<String>) {
    match e {
        Expr::Var(n) => {
            out.insert(n.clone());
        }
        Expr::Field { base, .. } => {
            if !matches!(base.as_ref(), Expr::Var(_)) {
                scan_uses_expr(base, mode, call, out);
            }
        }
        Expr::Index { base, index } => {
            if !matches!(base.as_ref(), Expr::Var(_)) {
                scan_uses_expr(base, mode, call, out);
            }
            scan_uses_expr(index, mode, call, out);
        }
        Expr::If { cond, then_block, else_block } => {
            scan_uses_expr(cond, mode, call, out);
            scan_uses_block(then_block, mode, call, out);
            if let Some(b) = else_block {
                scan_uses_block(b, mode, call, out);
            }
        }
        Expr::While { cond, body } => {
            scan_uses_expr(cond, mode, call, out);
            scan_uses_block(body, mode, call, out);
        }
        Expr::For { iter, body, .. } => {
            scan_uses_expr(iter, mode, call, out);
            scan_uses_block(body, mode, call, out);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            scan_uses_expr(scrutinee, mode, call, out);
            scan_uses_block(body, mode, call, out);
        }
        Expr::Block(b) => scan_uses_block(b, mode, call, out),
        Expr::Match { scrutinee, arms } => {
            scan_uses_expr(scrutinee, mode, call, out);
            for a in arms {
                if let Some(g) = &a.guard {
                    scan_uses_expr(g, mode, call, out);
                }
                scan_uses_expr(&a.body, mode, call, out);
            }
        }
        Expr::Lambda { body, .. } => {
            for s in &body.stmts {
                each_expr_in_stmt(s, &mut |x| mark_all_vars(x, out));
            }
        }
        Expr::Call { name, args } => match call {
            // View-safe reads exempt only their `Var` receiver; any other call
            // escapes its `Var` args (the generic walk marks them).
            CallExempt::ViewReads => {
                if (name == "list.at" || name == "list.length" || is_list_slice(name))
                    && matches!(args.first(), Some(Expr::Var(_)))
                {
                    for a in &args[1..] {
                        scan_uses_expr(a, mode, call, out);
                    }
                } else {
                    each_subexpr(e, &mut |s| scan_uses_expr(s, mode, call, out));
                }
            }
            // Any `Var` argument the callee does not alias out is exempt, uniformly.
            CallExempt::NonLeaking(summaries) => {
                for (i, a) in args.iter().enumerate() {
                    match a {
                        Expr::Var(_) if !summaries.arg_leaks(name, i, args.len()) => {}
                        _ => scan_uses_expr(a, mode, call, out),
                    }
                }
            }
        },
        _ => each_subexpr(e, &mut |s| scan_uses_expr(s, mode, call, out)),
    }
}

/// RC-floor reclamation candidates (RFC-0016): `let`/`var`-bound heap variables
/// that are reassigned and never escape as a WHOLE value — so when one is
/// overwritten by a fresh buffer (`x = f(x, …)` for ANY `f`), the old buffer
/// provably has no other live reference and codegen may free it into the
/// size-classed free-list. This is the GENERAL, convention- and escape-oracle-
/// driven confinement: a use is non-escaping when it is a non-leaking call
/// argument (the convention/escape oracle [`Summaries::arg_leaks`] is false — a
/// `let` borrow, an `own` move, or a builtin that only reads it) or an element
/// read (`x[i]` / `x.field`); a bare whole-value use, a leaking call argument, or
/// a closure capture marks the variable as escaping. Being summary-aware, it
/// generalizes across builtins AND user functions with no per-method code.
///
/// Restricted to `let`/`var` locals: a borrowed parameter's buffer belongs to
/// the caller, so its old value must never be freed here.
pub fn confined_reassigned_vars(f: &Function, summaries: &Summaries) -> HashSet<String> {
    confined_reassigned_vars_block(&f.body, summaries)
}

/// As [`confined_reassigned_vars`], over a body block directly.
pub fn confined_reassigned_vars_block(body: &Block, summaries: &Summaries) -> HashSet<String> {
    let mut bound = HashSet::new();
    collect_let_names(body, &mut bound);
    let mut reassigned = HashSet::new();
    collect_assigned_targets(body, &mut reassigned);
    bound.retain(|x| reassigned.contains(x));
    if bound.is_empty() {
        return HashSet::new();
    }
    // (RFC-0016 soundness) A var whose INITIAL binding aliases another value — `var rest = s`
    // (`s` a borrowed param or a still-live local), or a field/index projection of one — does
    // NOT own its first buffer. free-at-overwrite frees `x`'s old buffer at the first `x = f(x)`,
    // which for such a var is the SOURCE's buffer → a use-after-free of the caller's value
    // (e.g. `std/string.last_index_of`: `var rest = s; rest = string.substring(rest, …)`). Later
    // reassignments are fresh allocations, but excluding the whole var is the sound, simple floor
    // (a missed reclamation only leaks). Fresh-allocation inits (`[]`, `dict.new()`, a literal, a
    // call result) are NOT aliases and stay eligible — so `var d = dict.new(); d = dict.remove(d,…)`
    // (the cache-eviction accumulator) is unaffected.
    let mut alias_init = HashSet::new();
    collect_alias_initialized_vars(body, &mut alias_init);
    bound.retain(|x| !alias_init.contains(x));
    if bound.is_empty() {
        return HashSet::new();
    }
    let mut leaked = HashSet::new();
    scan_uses_block(body, UseScan::ReuseRhs, CallExempt::NonLeaking(summaries), &mut leaked);
    bound.into_iter().filter(|x| !leaked.contains(x)).collect()
}

/// `let`/`var` bindings whose value ALIASES another binding — a bare variable read
/// (`var x = y`) or a projection of one (`var x = r.field`, `var x = xs[i]`). Such a
/// binding's first buffer is owned by that source, not by `x`, so `x` must not be
/// free-at-overwrite-reclaimed (see [`confined_reassigned_vars_block`]).
fn collect_alias_initialized_vars(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        if let Stmt::Let { name, value, .. } = s {
            if matches!(value, Expr::Var(_) | Expr::Field { .. } | Expr::Index { .. }) {
                out.insert(name.clone());
            }
        }
        each_block_in_stmt(s, &mut |blk| collect_alias_initialized_vars(blk, out));
    }
}

/// Every `let`/`var` binding name in `body` (including nested blocks).
fn collect_let_names(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        if let Stmt::Let { name, .. } = s {
            out.insert(name.clone());
        }
        each_block_in_stmt(s, &mut |blk| collect_let_names(blk, out));
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

/// (RFC-0027 declared `packed`) Every `let`/`var xs = [Ctor(..), Ctor(..), ..]`
/// binding whose elements are a non-empty list of record constructors — recorded
/// as `name -> the FIRST element's constructor name`. The declared-packed codegen
/// consumer filters these by whether the constructor's type was declared `packed`,
/// to enforce the layout CONTRACT: a confined such list packs flat, and one that
/// is used in a position the flat layout cannot support is a clean compile error
/// (never a silent fall-back to the boxed layout the programmer declared away).
/// Uniformity of the constructor (all elements the same record) is left to the
/// codegen consumer's `packable_record_list`, which also performs the layout.
pub fn record_list_lets_block(body: &Block) -> HashMap<String, String> {
    let mut out = HashMap::new();
    record_list_lets_inner(body, &mut out);
    out
}

fn record_list_lets_inner(b: &Block, out: &mut HashMap<String, String>) {
    for s in &b.stmts {
        if let Stmt::Let { name, value: Expr::List(items), .. } = s {
            if let Some(Expr::Ctor { name: ctor, .. }) = items.first() {
                if items.iter().all(|e| matches!(e, Expr::Ctor { .. })) {
                    out.insert(name.clone(), ctor.clone());
                }
            }
        }
        each_block_in_stmt(s, &mut |blk| record_list_lets_inner(blk, out));
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

/// Record every binding used as a WHOLE value, marking assignment targets (the
/// SROA/aggregate policy). A thin consumer of the unified [`scan_uses_block`].
fn collect_whole_uses_block(b: &Block, out: &mut HashSet<String>) {
    scan_uses_block(b, UseScan::Whole, CallExempt::ViewReads, out);
}

/// Mark every variable an expression mentions as a whole use (used for closure
/// bodies, where any reference is a by-value capture).
fn mark_all_vars(e: &Expr, out: &mut HashSet<String>) {
    if let Expr::Var(n) = e {
        out.insert(n.clone());
    }
    each_subexpr(e, &mut |s| mark_all_vars(s, out));
}

/// Public wrapper over [`each_subexpr`]: apply `f` to each immediate sub-expression
/// of `e` (recursing into nested blocks), for consumers (e.g. codegen) that need to
/// scan an expression tree without re-deriving the AST shape.
pub fn for_each_immediate_subexpr(e: &Expr, f: &mut impl FnMut(&Expr)) {
    each_subexpr(e, f);
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
        | Stmt::LetPattern { value, .. }
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

    /// Run [`confined_reassigned_vars`] on the first function, with summaries built
    /// from the whole module (so user-fn leak facts are available).
    fn rc_floor(src: &str) -> HashSet<String> {
        let m = parser::parse_module(src).expect("parse");
        let summaries = crate::analysis::Summaries::of_module(&m);
        let f = m
            .items
            .iter()
            .find_map(|it| match it {
                witchy_syntax::ast::Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("a function");
        confined_reassigned_vars(f, &summaries)
    }

    #[test]
    fn eviction_accumulator_is_an_rc_floor_candidate() {
        // `d` threads through dict.insert/remove (neither leaks arg0) and is never
        // used as a whole value elsewhere → confined, reclaimable.
        let src = "import dict\nfn main(console: Console):\n    var d = dict.new()\n    var i = 0\n    while i < 10:\n        d = dict.insert(d, i, i)\n        d = dict.remove(d, i)\n        i = i + 1\n    print(console, __render(dict.length(d)))\n";
        assert!(rc_floor(src).contains("d"), "confined eviction accumulator is a candidate");
    }

    #[test]
    fn whole_value_escape_disqualifies_rc_floor() {
        // `d` is passed WHOLE to a user fn that returns it (its summary leaks arg0),
        // so its buffer may be aliased out → not reclaimable.
        let src = "import dict\nfn keep(x: Dict) -> Dict:\n    x\nfn main(console: Console):\n    var d = dict.new()\n    var i = 0\n    while i < 10:\n        d = dict.insert(d, i, i)\n        d = keep(d)\n        i = i + 1\n    print(console, __render(dict.length(d)))\n";
        assert!(!rc_floor(src).contains("d"), "a var aliased out by a call escapes");
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
    fn reassigned_confined_record_var_is_a_reuse_candidate() {
        // `var p = Point(..); while …: p = Point(..)`, p read only via fields —
        // same ctor every time (fixed tag + arity) → overwrite the buffer in place.
        let f = func(
            "type Point:\n    x: Int\n    y: Int\nfn d(n: Int) -> Int:\n    var p = Point(0, 0)\n    var i = 0\n    while i < n:\n        p = Point(i, i * 2)\n        i = i + 1\n    p.x + p.y\n",
        );
        assert!(
            confined_inplace_reuse_vars(&f).contains("p"),
            "a confined record var reassigned to same-ctor values is a reuse candidate"
        );
    }

    #[test]
    fn reuse_record_var_with_different_ctor_is_not_a_candidate() {
        // Different constructors → different tag/arity/size; the fixed buffer can't
        // be overwritten safely. (AST-level: the analysis runs pre-typeck.)
        let f = func(
            "type A:\n    v: Int\ntype B:\n    v: Int\nfn d() -> Int:\n    var o = A(1)\n    o = B(2)\n    o.v\n",
        );
        assert!(
            !confined_inplace_reuse_vars(&f).contains("o"),
            "a different-constructor reassignment breaks the fixed-buffer invariant"
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
    fn reuse_var_with_nonuniform_length_is_a_candidate() {
        // A list var reassigned to a DIFFERENT length is still a candidate — codegen
        // capacity-checks at runtime (reuse when it fits, else reallocate).
        let f = func(
            "fn d() -> Int:\n    var x = [0, 0, 0]\n    x = [1, 2]\n    list.length(x)\n",
        );
        assert!(
            confined_inplace_reuse_vars(&f).contains("x"),
            "a list var reassigned to varying lengths is a capacity-resizing reuse candidate"
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
