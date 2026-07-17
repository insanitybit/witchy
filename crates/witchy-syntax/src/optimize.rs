//! Semantics-preserving optimization over the lowered AST.
//!
//! This runs inside the linker, AFTER const inlining and BEFORE typeck, on the
//! single linked module that both backends consume. Because there is exactly
//! one AST and both the interpreter and the WASM codegen read it, an
//! optimization here can never introduce a backend divergence — parity is
//! preserved by construction.
//!
//! Every rewrite preserves observable behavior:
//!   * integer arithmetic folds with WRAPPING semantics, matching the runtime's
//!     two's-complement overflow on both backends;
//!   * division and modulo are NOT folded — they can trap (`÷0`, `i64::MIN / -1`),
//!     and the trap is observable behavior that must survive to runtime;
//!   * floats fold with IEEE-754 `f64`, identical to what both backends compute;
//!   * only BOTH-literal operands are folded. Identities with a non-literal
//!     operand (`x + 0 → x`) are deliberately omitted here: this pass runs
//!     before typeck, so `x`'s type is unknown, and folding `someString + 0`
//!     away would hide a type error the program should be rejected for.
//!
//! The pass is a post-order walk: children are optimized before their parent, so
//! a nested constant expression like `(1 + 2) + 3` collapses to `6` in one pass.
//!
//! It also performs LOCAL CONSTANT PROPAGATION: an immutable `let x = <literal>`
//! is recorded and later `x` references are replaced by the literal, which feeds
//! the folder — notably `let g = "Hi, "; g + "World"` folds to one string, which
//! eliminates a runtime allocation the WASM backend's mid-end cannot see.
//! Soundness rests on scope discipline: every binder (`let`/`var`/`for`/lambda)
//! removes the names it introduces before recursing, and `match`/`while let`
//! arms (whose patterns bind names this pass does not enumerate) are entered
//! conservatively with no propagation.

use crate::ast::{BinOp, Block, Expr, Item, Module, Stmt, UnOp};
// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashMapExt as _};

/// Immutable `let`-bindings whose value is a literal, in scope at a program
/// point. Used for LOCAL constant propagation: a `Var` reference is replaced by
/// the literal, which in turn unlocks more folding (notably string concatenation
/// of `let`-bound literals, eliminating an allocation the WASM backend's mid-end
/// cannot). Soundness rests on never propagating a name that a narrower scope
/// rebinds — every binder below removes its names before recursing.
type Consts = HashMap<String, Expr>;

/// Optimize every function/method body in the module in place.
pub fn optimize(module: &mut Module) {
    for item in &mut module.items {
        match item {
            Item::Function(f) => opt_block(&mut f.body, &mut Consts::new()),
            Item::Impl(im) => {
                for m in &mut im.methods {
                    opt_block(&mut m.body, &mut Consts::new());
                }
            }
            Item::Trait(t) => {
                for m in &mut t.methods {
                    if let Some(body) = &mut m.default {
                        opt_block(body, &mut Consts::new());
                    }
                }
            }
            _ => {}
        }
    }
}

fn is_literal(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Duration(_)
    )
}

fn opt_block(b: &mut Block, consts: &mut Consts) {
    for s in &mut b.stmts {
        opt_stmt(s, consts);
    }
}

fn opt_stmt(s: &mut Stmt, consts: &mut Consts) {
    match s {
        Stmt::Let { name, mutable, value, .. } => {
            opt_expr(value, consts);
            // An immutable `let x = <literal>` is a propagatable constant; any
            // other binding to `x` (mutable `var`, or a non-literal value)
            // shadows it and must un-track the name.
            if !*mutable && is_literal(value) {
                consts.insert(name.clone(), value.clone());
            } else {
                consts.remove(name);
            }
        }
        Stmt::Assign { name, value } => {
            opt_expr(value, consts);
            consts.remove(name); // reassigned: no longer its old constant
        }
        Stmt::LetPattern { pattern, value } => {
            opt_expr(value, consts);
            let mut names = Vec::new();
            crate::ast::pattern_binds(pattern, &mut names);
            for n in &names {
                consts.remove(n);
            }
        }
        Stmt::Yield(value) | Stmt::Expr(value) => opt_expr(value, consts),
        Stmt::Return(Some(value)) => opt_expr(value, consts),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
    }
}

fn opt_expr(e: &mut Expr, consts: &mut Consts) {
    // Post-order: optimize children first, then substitute/fold this node.
    match e {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
        Expr::List(xs) | Expr::Tuple(xs) => xs.iter_mut().for_each(|x| opt_expr(x, consts)),
        Expr::Call { args, .. } | Expr::Ctor { args, .. }
        | Expr::AnonCtor { args, .. } => {
            args.iter_mut().for_each(|a| opt_expr(a, consts))
        }
        // Normally already lowered to `Call` by `keyword_args::resolve` before
        // folding; recurse defensively so this stays correct if reordered.
        Expr::LabeledCall { args, .. } => {
            args.iter_mut().for_each(|(_, a)| opt_expr(a, consts))
        }
        Expr::MethodCall { receiver, args, .. } => {
            opt_expr(receiver, consts);
            args.iter_mut().for_each(|a| opt_expr(a, consts));
        }
        Expr::Apply { func, args } => {
            opt_expr(func, consts);
            args.iter_mut().for_each(|a| opt_expr(a, consts));
        }
        Expr::Unary { expr, .. } => opt_expr(expr, consts),
        Expr::Field { base, .. } => opt_expr(base, consts),
        Expr::Lambda { params, body, .. } => {
            // Params shadow outer constants inside the body.
            let mut inner = consts.clone();
            for p in params.iter() {
                inner.remove(&p.name);
            }
            opt_block(body, &mut inner);
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            opt_expr(base, consts);
            for (_, v) in fields {
                opt_expr(v, consts);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                opt_expr(v, consts);
            }
            if let Some(s) = spread {
                opt_expr(s, consts);
            }
        }
        Expr::Try(inner) => opt_expr(inner, consts),
        Expr::As { expr, .. } | Expr::ExistentialPack { expr, .. } => {
            opt_expr(expr, consts)
        }
        Expr::Binary { lhs, rhs, .. } => {
            opt_expr(lhs, consts);
            opt_expr(rhs, consts);
        }
        Expr::If { cond, then_block, else_block } => {
            opt_expr(cond, consts);
            // Branches are nested scopes: their own `let`s must not leak out, so
            // each gets a clone (inner shadowing handled by `opt_stmt`).
            opt_block(then_block, &mut consts.clone());
            if let Some(eb) = else_block {
                opt_block(eb, &mut consts.clone());
            }
        }
        Expr::Match { scrutinee, arms } => {
            opt_expr(scrutinee, consts);
            // Arm patterns bind names we don't enumerate here; stay conservative
            // (no propagation into arm guards/bodies) rather than risk a name a
            // pattern rebinds. Folding inside arms still happens (empty map).
            for arm in arms {
                if let Some(g) = &mut arm.guard {
                    opt_expr(g, &mut Consts::new());
                }
                opt_expr(&mut arm.body, &mut Consts::new());
            }
        }
        Expr::Block(b) => opt_block(b, &mut consts.clone()),
        Expr::While { cond, body } => {
            opt_expr(cond, consts);
            opt_block(body, &mut consts.clone());
        }
        Expr::For { var, iter, body } => {
            opt_expr(iter, consts);
            // The loop variable shadows any outer constant of the same name.
            let mut inner = consts.clone();
            inner.remove(var);
            opt_block(body, &mut inner);
        }
        Expr::Range { lo, hi, .. } => {
            opt_expr(lo, consts);
            opt_expr(hi, consts);
        }
        Expr::Index { base, index } => {
            opt_expr(base, consts);
            opt_expr(index, consts);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            opt_expr(scrutinee, consts);
            // Pattern binds names we don't enumerate: conservative (empty map).
            opt_block(body, &mut Consts::new());
        }
    }

    // Replace a tracked constant variable with its literal (then nothing to fold).
    if let Expr::Var(name) = e {
        if let Some(lit) = consts.get(name) {
            *e = lit.clone();
            return;
        }
    }
    if let Some(folded) = fold(e) {
        *e = folded;
    }
}

/// Fold a node whose children are already optimized. Returns the replacement, or
/// `None` to leave the node unchanged. Only literal operands are folded.
fn fold(e: &Expr) -> Option<Expr> {
    match e {
        Expr::Binary { op, lhs, rhs } => fold_binary(*op, lhs, rhs),
        Expr::Unary { op, expr } => fold_unary(*op, expr),
        _ => None,
    }
}

/// `-x`, `!x`, `~x` on a literal operand.
fn fold_unary(op: UnOp, operand: &Expr) -> Option<Expr> {
    use Expr::{Bool, Float, Int};
    Some(match (op, operand) {
        // Wrapping negation matches the runtime (`-i64::MIN == i64::MIN`).
        (UnOp::Neg, Int(a)) => Int(a.wrapping_neg()),
        (UnOp::Neg, Float(a)) => Float(-a),
        (UnOp::Not, Bool(a)) => Bool(!a),
        (UnOp::BitNot, Int(a)) => Int(!a),
        // `move`/`await` are semantic markers, never folded.
        _ => return None,
    })
}

fn fold_binary(op: BinOp, lhs: &Expr, rhs: &Expr) -> Option<Expr> {
    use BinOp::*;
    use Expr::{Bool, Float, Int, Str};
    Some(match (op, lhs, rhs) {
        // Integer arithmetic — WRAPPING (two's-complement, as at runtime).
        (Add, Int(a), Int(b)) => Int(a.wrapping_add(*b)),
        (Sub, Int(a), Int(b)) => Int(a.wrapping_sub(*b)),
        (Mul, Int(a), Int(b)) => Int(a.wrapping_mul(*b)),
        // Div/Mod are intentionally NOT folded: they can trap at runtime.
        // Bitwise ops are total and trap-free. Shifts are NOT folded: the
        // shift-amount masking convention is the runtime's to define.
        (BitAnd, Int(a), Int(b)) => Int(a & b),
        (BitOr, Int(a), Int(b)) => Int(a | b),
        (BitXor, Int(a), Int(b)) => Int(a ^ b),

        // Float arithmetic — IEEE-754 f64, bit-identical to both backends.
        (Add, Float(a), Float(b)) => Float(a + b),
        (Sub, Float(a), Float(b)) => Float(a - b),
        (Mul, Float(a), Float(b)) => Float(a * b),

        // String concatenation: `+` on two string literals. (`Concat` is the
        // post-typeck internal form; handled too for completeness.)
        (Add, Str(a), Str(b)) | (Concat, Str(a), Str(b)) => Str(format!("{a}{b}")),

        // Integer comparisons.
        (Eq, Int(a), Int(b)) => Bool(a == b),
        (NotEq, Int(a), Int(b)) => Bool(a != b),
        (Lt, Int(a), Int(b)) => Bool(a < b),
        (LtEq, Int(a), Int(b)) => Bool(a <= b),
        (Gt, Int(a), Int(b)) => Bool(a > b),
        (GtEq, Int(a), Int(b)) => Bool(a >= b),

        // Float comparisons (IEEE ordering, NaN-correct via Rust's operators).
        (Eq, Float(a), Float(b)) => Bool(a == b),
        (NotEq, Float(a), Float(b)) => Bool(a != b),
        (Lt, Float(a), Float(b)) => Bool(a < b),
        (LtEq, Float(a), Float(b)) => Bool(a <= b),
        (Gt, Float(a), Float(b)) => Bool(a > b),
        (GtEq, Float(a), Float(b)) => Bool(a >= b),

        // Boolean & string equality.
        (Eq, Bool(a), Bool(b)) => Bool(a == b),
        (NotEq, Bool(a), Bool(b)) => Bool(a != b),
        (Eq, Str(a), Str(b)) => Bool(a == b),
        (NotEq, Str(a), Str(b)) => Bool(a != b),

        // Boolean connectives on constants (no short-circuit concern — both
        // operands are literals, so neither is an unevaluated effect).
        (And, Bool(a), Bool(b)) => Bool(*a && *b),
        (Or, Bool(a), Bool(b)) => Bool(*a || *b),

        _ => return None,
    })
}

#[cfg(test)]
#[path = "optimize_tests.rs"]
mod tests;
