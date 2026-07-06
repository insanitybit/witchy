//! (RFC-0056) Resolve keyword arguments and constant default parameters at the
//! link layer, so neither backend ever sees a label or a defaulted call — parity
//! by construction (the divergence surface is empty).
//!
//! This runs on the merged module AFTER call names are qualified
//! (`linker::rewrite_expr`) and UFCS method calls are resolved
//! (`linker::resolve_methods`), so every direct callee is a statically-known
//! function whose declared parameter list this pass can look up.
//!
//! Two rewrites, both a generalization of what `records::build` already does for
//! named-field construction:
//!
//!   * **`Expr::LabeledCall`** — `substring(s, start: 2, end: 7)`. Validate the
//!     labels against the callee's parameters (unknown / duplicate / missing get
//!     their own diagnostic, exactly like a record), then reorder to declared
//!     order. Evaluation order is **source order** (RFC-0056 decision): if the
//!     labels reorder the written arguments, each written argument is bound to a
//!     temp `let __kwN = ...` in the order WRITTEN and the call passes the temps in
//!     declared order — so effects fire left-to-right as the reader sees them, not
//!     in declared order.
//!
//!   * **`Expr::Call` with omitted trailing arguments** — `split(s)` where
//!     `split(s, sep: String = " ")`. Splice each omitted parameter's closed
//!     constant default in. Closed-ness makes the splice hygienic (nothing to
//!     capture) and order-free (a constant has no effects).
//!
//! Labels and defaults are properties of the *declaration*, never of a function
//! type or value: a call through a value (`Apply`) or a method call is
//! positional-only, and the parser never produces a `LabeledCall` for those.

use crate::ast::*;
use std::collections::HashMap;

type Params = HashMap<String, Vec<Param>>;

/// Rewrite every labeled/defaulted direct call in the merged module.
pub fn resolve(module: &mut Module) -> Result<(), String> {
    let params: Params = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Function(f) => Some((f.name.clone(), f.params.clone())),
            _ => None,
        })
        .collect();
    let mut r = Resolver { params, counter: 0 };
    for item in &mut module.items {
        match item {
            Item::Function(f) => r.block(&mut f.body)?,
            Item::Impl(im) => {
                for meth in &mut im.methods {
                    r.block(&mut meth.body)?;
                }
            }
            Item::Trait(t) => {
                for ms in &mut t.methods {
                    if let Some(body) = &mut ms.default {
                        r.block(body)?;
                    }
                }
            }
            Item::Const { value, .. } => r.expr(value)?,
            Item::Type(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    Ok(())
}

struct Resolver {
    params: Params,
    counter: u32,
}

impl Resolver {
    fn block(&mut self, b: &mut Block) -> Result<(), String> {
        for stmt in &mut b.stmts {
            match stmt {
                Stmt::Let { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::Expr(value)
                | Stmt::Yield(value)
                | Stmt::Return(Some(value)) => self.expr(value)?,
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        Ok(())
    }

    fn expr(&mut self, e: &mut Expr) -> Result<(), String> {
        // Post-order: resolve children first so a labeled call nested in an
        // argument is already positional before the parent is rewritten.
        match e {
            Expr::LabeledCall { name, args } => {
                for (_, v) in args.iter_mut() {
                    self.expr(v)?;
                }
                let name = std::mem::take(name);
                let args = std::mem::take(args);
                *e = self.rewrite_labeled(name, args)?;
            }
            Expr::Call { name, args } => {
                for a in args.iter_mut() {
                    self.expr(a)?;
                }
                self.splice_defaults(name, args);
            }
            Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
            | Expr::Var(_) | Expr::TaggedLit { .. } => {}
            Expr::List(xs) | Expr::Tuple(xs) | Expr::Ctor { args: xs, .. } => {
                for x in xs {
                    self.expr(x)?;
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.expr(receiver)?;
                for a in args {
                    self.expr(a)?;
                }
            }
            Expr::Apply { func, args } => {
                self.expr(func)?;
                for a in args {
                    self.expr(a)?;
                }
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::Field { base: expr, .. } => self.expr(expr)?,
            Expr::Index { base, index } => {
                self.expr(base)?;
                self.expr(index)?;
            }
            Expr::Range { lo, hi, .. } => {
                self.expr(lo)?;
                self.expr(hi)?;
            }
            Expr::RecordUpdate { base, fields } => {
                self.expr(base)?;
                for (_, v) in fields {
                    self.expr(v)?;
                }
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields {
                    self.expr(v)?;
                }
                if let Some(s) = spread {
                    self.expr(s)?;
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs)?;
                self.expr(rhs)?;
            }
            Expr::If { cond, then_block, else_block } => {
                self.expr(cond)?;
                self.block(then_block)?;
                if let Some(b) = else_block {
                    self.block(b)?;
                }
            }
            Expr::While { cond, body } => {
                self.expr(cond)?;
                self.block(body)?;
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                self.expr(scrutinee)?;
                self.block(body)?;
            }
            Expr::For { iter, body, .. } => {
                self.expr(iter)?;
                self.block(body)?;
            }
            Expr::Match { scrutinee, arms } => {
                self.expr(scrutinee)?;
                for arm in arms.iter_mut() {
                    if let Some(g) = &mut arm.guard {
                        self.expr(g)?;
                    }
                    self.expr(&mut arm.body)?;
                }
            }
            Expr::Lambda { body, .. } => self.block(body)?,
            Expr::Block(b) => self.block(b)?,
        }
        Ok(())
    }

    /// Splice closed-constant defaults for the omitted trailing parameters of a
    /// plain positional call. A no-op unless the callee is a known function with
    /// fewer arguments than parameters and every missing parameter has a default
    /// (otherwise leave it: the type checker reports the arity mismatch).
    fn splice_defaults(&self, name: &str, args: &mut Vec<Expr>) {
        let Some(params) = self.params.get(name) else {
            return;
        };
        if args.len() >= params.len() {
            return;
        }
        if params[args.len()..].iter().all(|p| p.default.is_some()) {
            for p in &params[args.len()..] {
                // Closed constant: cloning and splicing is hygienic and effect-free.
                args.push(p.default.clone().expect("checked above"));
            }
        }
    }

    /// Validate `args` (positional prefix + labeled suffix) against the callee's
    /// declared parameters and produce a fully-applied positional call, preserving
    /// source evaluation order via temp bindings when the labels reorder.
    fn rewrite_labeled(
        &mut self,
        name: String,
        args: Vec<(Option<String>, Expr)>,
    ) -> Result<Expr, String> {
        let Some(params) = self.params.get(&name).cloned() else {
            return Err(format!(
                "labels need the callee's declaration — `{name}` is not a direct function \
                 (a builtin or a value has no parameter names to label)"
            ));
        };
        let n = params.len();
        let mut filled = vec![false; n];
        // Each written argument paired with its DECLARED index, in source order.
        let mut written: Vec<(usize, Expr)> = Vec::with_capacity(args.len());
        let mut next_pos = 0usize;
        for (label, value) in args {
            let d = match label {
                None => {
                    if next_pos >= n {
                        return Err(format!(
                            "too many arguments to `{name}`: it declares {n} parameter(s)"
                        ));
                    }
                    let d = next_pos;
                    next_pos += 1;
                    d
                }
                Some(lbl) => match params.iter().position(|p| p.name == lbl) {
                    Some(d) => d,
                    None => {
                        return Err(format!("`{name}` has no parameter `{lbl}`"));
                    }
                },
            };
            if filled[d] {
                return Err(format!(
                    "argument `{}` is bound twice in the call to `{name}`",
                    params[d].name
                ));
            }
            filled[d] = true;
            written.push((d, value));
        }
        // Every unfilled parameter must have a default.
        for (d, p) in params.iter().enumerate() {
            if !filled[d] && p.default.is_none() {
                return Err(format!("missing argument `{}` in the call to `{name}`", p.name));
            }
        }
        // Source order is preserved by a plain positional call as long as the
        // written arguments already appear in declared order; otherwise bind each
        // to a temp in written order and pass the temps in declared order.
        let in_declared_order = written.windows(2).all(|w| w[0].0 < w[1].0);
        if in_declared_order {
            let mut by_index: HashMap<usize, Expr> = written.into_iter().collect();
            let call_args = (0..n)
                .map(|d| match by_index.remove(&d) {
                    Some(v) => v,
                    None => params[d].default.clone().expect("checked: unfilled => has default"),
                })
                .collect();
            return Ok(Expr::Call { name, args: call_args });
        }
        // Reorder: temp-bind in source order, then call in declared order.
        let mut stmts: Vec<Stmt> = Vec::with_capacity(written.len() + 1);
        let mut resolved: HashMap<usize, Expr> = HashMap::new();
        for (d, value) in written {
            // An argument to a `var` parameter must be a bare mutable variable
            // (RFC-0043; typeck enforces it), which is passed by reference and has
            // no evaluation effect — so hoisting it into a temp buys no ordering
            // guarantee, and binding it to an immutable `let __kwN` would make the
            // reorder itself ill-typed ("must be a mutable `var`") and leak the
            // synthetic temp name into the diagnostic (BUG-208). Pass it directly;
            // a genuinely non-mutable argument is then reported by typeck against
            // the user's own expression, never a `__kwN`.
            if params[d].convention == Convention::Var {
                resolved.insert(d, value);
                continue;
            }
            let temp = format!("__kw{}", self.counter);
            self.counter += 1;
            stmts.push(Stmt::Let { name: temp.clone(), ty: None, mutable: false, value });
            resolved.insert(d, Expr::Var(temp));
        }
        let call_args = (0..n)
            .map(|d| match resolved.remove(&d) {
                Some(v) => v,
                None => params[d].default.clone().expect("checked: unfilled => has default"),
            })
            .collect();
        stmts.push(Stmt::Expr(Expr::Call { name, args: call_args }));
        let lines = vec![0u32; stmts.len()];
        Ok(Expr::Block(Block { stmts, lines, region: None }))
    }
}
