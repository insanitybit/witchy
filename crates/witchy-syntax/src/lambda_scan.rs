//! Free-variable / capture analysis over the AST.
//!
//! A lambda's *captures* are the variables it reads from an enclosing scope; its
//! *outer assignments* are the variables it writes that it does not bind
//! internally. Both are pure functions of the syntax tree, so they live here in
//! `witchy-syntax` and are shared by every consumer: the lowering pass uses
//! `scan_lambda`/`captures`/`assigns_outer` to emit closure environments, and the
//! type checker uses `lambda_outer_assigns` to reject by-value writes uniformly
//! (identically to what lowering would detect) rather than backend-specifically.

use crate::ast::*;
use std::collections::HashSet;

/// The reads, internal assignments, and internal bindings gathered while
/// walking a lambda body.
#[derive(Default)]
pub struct LambdaScan {
    reads: HashSet<String>,
    assigns: HashSet<String>,
    bound: HashSet<String>,
}

impl LambdaScan {
    /// Variables read from the enclosing scope (the closure's captures), sorted
    /// for a deterministic capture-slot order.
    pub fn captures(&self) -> Vec<String> {
        let mut free: Vec<String> = self.reads.difference(&self.bound).cloned().collect();
        free.sort();
        free
    }

    /// Variables assigned that are not bound within the lambda — i.e. writes to
    /// an outer binding. By-value capture cannot propagate these back out.
    pub fn assigns_outer(&self) -> Vec<String> {
        let mut a: Vec<String> = self.assigns.difference(&self.bound).cloned().collect();
        a.sort();
        a
    }
}

/// Names a lambda assigns but does not bind internally — i.e. writes to a
/// captured/outer variable. By-value capture cannot propagate these out, so every
/// backend rejects them; the type checker calls this so the rejection is uniform
/// (and identical to what lowering would detect) rather than backend-specific.
pub fn lambda_outer_assigns(params: &[Param], body: &Block) -> Vec<String> {
    scan_lambda(params, body).assigns_outer()
}

/// Scan a lambda for captures and outer assignments. `bound` is seeded with the
/// params and grows with every internal binder (lets, loop vars, match
/// patterns, nested lambda params). The bound set is an over-approximation
/// (binders apply to the whole body), sound for these checks on all but
/// pathological shadowing.
pub fn scan_lambda(params: &[Param], body: &Block) -> LambdaScan {
    let mut s = LambdaScan::default();
    for p in params {
        s.bound.insert(p.name.clone());
    }
    fv_block(body, &mut s);
    s
}

fn fv_block(block: &Block, s: &mut LambdaScan) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                fv_expr(value, s);
                s.bound.insert(name.clone());
            }
            Stmt::Assign { name, value } => {
                s.assigns.insert(name.clone());
                fv_expr(value, s);
            }
            Stmt::LetTuple { names, value } => {
                fv_expr(value, s);
                for n in names {
                    s.bound.insert(n.clone());
                }
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => fv_expr(e, s),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn fv_expr(e: &Expr, s: &mut LambdaScan) {
    match e {
        // A range survives only inside a `for` iterator (its loop is lowered in
        // codegen, not the parser); scan its bounds for free variables. The
        // other sugar nodes are fully lowered before codegen.
        Expr::Range { lo, hi, .. } => {
            fv_expr(lo, s);
            fv_expr(hi, s);
        }
        Expr::Index { .. } | Expr::WhileLet { .. } | Expr::MethodCall { .. } | Expr::Record { .. } => {
            unreachable!("range/index sugar is lowered before codegen (parser::lower_sugar_module)")
        }
        Expr::Var(n) => {
            s.reads.insert(n.clone());
        }
        Expr::Int(_)
        | Expr::Duration(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
        // A `Call` name is a function/builtin (or a closure local, caught at WASM
        // validation), never an outer value capture — only its args matter here.
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                fv_expr(x, s);
            }
        }
        // The callee name matters: it may be a captured function-valued local
        // (which must be pulled into the closure), not only a top-level
        // function. Non-local names are filtered out where captures are built.
        Expr::Call { name, args } => {
            s.reads.insert(name.clone());
            for a in args {
                fv_expr(a, s);
            }
        }
        Expr::Ctor { args, .. } => {
            for a in args {
                fv_expr(a, s);
            }
        }
        Expr::Apply { func, args } => {
            fv_expr(func, s);
            for a in args {
                fv_expr(a, s);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => fv_expr(expr, s),
        Expr::Field { base, .. } => fv_expr(base, s),
        Expr::RecordUpdate { base, fields } => {
            fv_expr(base, s);
            for (_, v) in fields {
                fv_expr(v, s);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            fv_expr(lhs, s);
            fv_expr(rhs, s);
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            fv_expr(cond, s);
            fv_block(then_block, s);
            if let Some(b) = else_block {
                fv_block(b, s);
            }
        }
        Expr::Match { scrutinee, arms } => {
            fv_expr(scrutinee, s);
            for arm in arms {
                let mut pv = Vec::new();
                collect_pattern_vars(&arm.pattern, &mut pv);
                for v in pv {
                    s.bound.insert(v);
                }
                if let Some(g) = &arm.guard {
                    fv_expr(g, s);
                }
                fv_expr(&arm.body, s);
            }
        }
        Expr::Block(b) => fv_block(b, s),
        Expr::While { cond, body } => {
            fv_expr(cond, s);
            fv_block(body, s);
        }
        Expr::For { var, iter, body } => {
            fv_expr(iter, s);
            s.bound.insert(var.clone());
            fv_block(body, s);
        }
        Expr::Lambda { params, body, .. } => {
            for p in params {
                s.bound.insert(p.name.clone());
            }
            fv_block(body, s);
        }
    }
}

/// Collect the variable names a pattern binds (recursively through ctor/tuple/
/// list sub-patterns and a list-rest binding) into any `Extend<String>` sink —
/// a `Vec` to keep binding order, a `HashSet` for set membership.
pub fn collect_pattern_vars<S: Extend<String>>(pat: &Pattern, out: &mut S) {
    match pat {
        Pattern::Var(name) => out.extend([name.clone()]),
        Pattern::Ctor { args, .. } | Pattern::Tuple(args) => {
            for sub in args {
                collect_pattern_vars(sub, out);
            }
        }
        Pattern::List { elems, rest } => {
            for sub in elems {
                collect_pattern_vars(sub, out);
            }
            if let Some(Some(name)) = rest {
                out.extend([name.clone()]);
            }
        }
        _ => {}
    }
}
