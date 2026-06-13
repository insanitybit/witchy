//! Lower `async fn`/`await` into ordinary functions over `std/future`, by a
//! CPS-over-closures transform run BEFORE typeck (like `crate::generators`), so
//! typeck / codegen / the interpreter never see `async` or `await`.
//!
//! An async function
//! ```text
//! async fn pipe(seed: Int) -> Int:
//!     let a = await step(seed)
//!     print_it(a)
//!     a + 1
//! ```
//! becomes a plain function returning a `Future`, where each `await` is the seam
//! at which the rest of the body is captured as a continuation closure:
//! ```text
//! fn pipe(seed: Int) -> Future(Int):
//!     future.lazy(fn():
//!         future.and_then(step(seed), fn(a):
//!             {
//!                 print_it(a)
//!                 future.ready(a + 1)
//!             }))
//! ```
//! `await E` lowers to `future.and_then(E, fn(x): <rest>)`; a statement with no
//! `await` is kept verbatim (so ordinary `let`/`var`/effect semantics are
//! untouched) and the continuation rides as the block's tail. The whole body is
//! wrapped in `future.lazy` so calling an async fn does NO work until the future
//! is driven.
//!
//! Because the body's live locals become the captured values of continuation
//! closures — and captures are owned values, never internal references — there is
//! nothing self-referential and so (unlike Rust) no `Pin`.
//!
//! Scope of this pass (the rest is rejected with a clear error, to be lifted as
//! the transform grows): `await` may appear only as the entire right-hand side of
//! a `let`, as a bare statement, or in tail position (including the branches of a
//! tail `if`/`match`). `await` inside a `while`/`for` loop, inside a condition or
//! scrutinee, or nested within a larger expression is not yet supported. Carrying
//! a mutable `var` across an `await` is likewise unsupported — and is caught for
//! free by the existing rule that a closure may not assign to a captured variable.

use crate::ast::*;

pub fn lower(mut module: Module) -> Result<Module, String> {
    if !module.items.iter().any(is_async_fn) {
        return Ok(module);
    }
    let mut items = Vec::with_capacity(module.items.len());
    for item in module.items {
        match item {
            Item::Function(f) if f.is_async => items.push(Item::Function(lower_async_fn(f)?)),
            other => items.push(other),
        }
    }
    module.items = items;
    if !module.imports.iter().any(|m| m == "chan") {
        module.imports.push("chan".to_string());
    }
    while module.import_lines.len() < module.imports.len() {
        module.import_lines.push(0);
    }
    Ok(module)
}

fn is_async_fn(item: &Item) -> bool {
    matches!(item, Item::Function(f) if f.is_async)
}

fn lower_async_fn(f: Function) -> Result<Function, String> {
    // The whole body, deferred so the function is lazy.
    let mut ctx = Ctx { counter: 0, fname: f.name.clone() };
    let body_future = ctx.cps_stmts(&f.body.stmts)?;
    let lazy_body = call(
        "chan.lazy",
        vec![Expr::Lambda { params: vec![], body: tail_block(body_future) }],
    );

    if f.name == "main" {
        // The runtime calls `main` directly and cannot drive a task, so an async
        // `main` IS the executor's entry point: run its body (a single task) to
        // completion on the cooperative scheduler.
        let driven = call("chan.run", vec![Expr::List(vec![lazy_body])]);
        return Ok(Function {
            public: f.public,
            name: f.name,
            params: f.params,
            ret: None,
            body: tail_block(driven),
            bounds: f.bounds,
            is_gen: false,
            is_async: false,
        });
    }

    // Leave the return type to inference: the body already determines it
    // (`Task(Int, Nil)` when the fn `send`s/`recv`s `Int`, `Task(<phantom>, T)`
    // when it touches no channel). Declaring `Task(<msg>, T)` would FAIL the
    // soundness check whenever the body pins the message type to a concrete one.
    Ok(Function {
        public: f.public,
        name: f.name,
        params: f.params,
        ret: None,
        body: tail_block(lazy_body),
        bounds: f.bounds,
        is_gen: false,
        is_async: false,
    })
}

struct Ctx {
    counter: usize,
    fname: String,
}

impl Ctx {
    fn fresh(&mut self) -> String {
        let n = self.counter;
        self.counter += 1;
        format!("__await{n}")
    }

    fn err(&self, msg: &str) -> String {
        format!("async fn `{}`: {msg}", self.fname)
    }

    /// Transform a statement sequence into a `Future`-valued expression.
    fn cps_stmts(&mut self, stmts: &[Stmt]) -> Result<Expr, String> {
        let Some((head, rest)) = stmts.split_first() else {
            return Ok(call("chan.ready_unit", vec![]));
        };
        let is_last = rest.is_empty();
        match head {
            Stmt::Let { name, value, .. } => {
                if let Some(inner) = as_await(value) {
                    reject_await(inner, &self.fname)?;
                    let k = self.cps_stmts(rest)?;
                    Ok(and_then(inner.clone(), name.clone(), k))
                } else {
                    reject_await(value, &self.fname)?;
                    Ok(prefix_stmt(head.clone(), self.cps_stmts(rest)?))
                }
            }
            Stmt::Expr(e) => {
                if is_last {
                    // A tail `await E` yields E's value (`cps_value` returns the
                    // future itself), NOT the discard path below.
                    self.cps_value(e)
                } else if let Some(inner) = as_await(e) {
                    // A non-last `await E` runs E for effect and continues.
                    reject_await(inner, &self.fname)?;
                    let bind = self.fresh();
                    let k = self.cps_stmts(rest)?;
                    Ok(and_then(inner.clone(), bind, k))
                } else {
                    reject_await(e, &self.fname)?;
                    Ok(prefix_stmt(head.clone(), self.cps_stmts(rest)?))
                }
            }
            Stmt::Return(Some(e)) => {
                if let Some(inner) = as_await(e) {
                    reject_await(inner, &self.fname)?;
                    Ok(inner.clone())
                } else {
                    self.cps_value(e)
                }
            }
            Stmt::Return(None) => Ok(call("chan.ready_unit", vec![])),
            Stmt::Assign { value, .. } | Stmt::LetTuple { value, .. } => {
                reject_await(value, &self.fname)?;
                if is_last {
                    Ok(prefix_stmt(head.clone(), call("chan.ready_unit", vec![])))
                } else {
                    Ok(prefix_stmt(head.clone(), self.cps_stmts(rest)?))
                }
            }
            Stmt::Yield(_) => Err(self.err("`yield` is not allowed in an async fn")),
            Stmt::Break | Stmt::Continue => {
                Err(self.err("`break`/`continue` across `await` is not yet supported"))
            }
        }
    }

    /// Transform an expression in VALUE position (the function's result) into a
    /// `Future`-valued expression: `await E` -> `E`; a tail `if`/`match` ->
    /// branches each made into a future; a plain value -> `future.ready(value)`.
    fn cps_value(&mut self, e: &Expr) -> Result<Expr, String> {
        if let Some(inner) = as_await(e) {
            reject_await(inner, &self.fname)?;
            return Ok(inner.clone());
        }
        match e {
            Expr::If { cond, then_block, else_block } => {
                if contains_await(cond) {
                    return Err(self.err("`await` in an `if` condition is not yet supported"));
                }
                let then_f = self.cps_stmts(&then_block.stmts)?;
                let else_f = match else_block {
                    Some(b) => self.cps_stmts(&b.stmts)?,
                    None => call("chan.ready_unit", vec![]),
                };
                Ok(Expr::If {
                    cond: cond.clone(),
                    then_block: tail_block(then_f),
                    else_block: Some(tail_block(else_f)),
                })
            }
            Expr::Match { scrutinee, arms } => {
                if contains_await(scrutinee) {
                    return Err(self.err("`await` in a `match` scrutinee is not yet supported"));
                }
                let mut new_arms = Vec::with_capacity(arms.len());
                for a in arms {
                    new_arms.push(MatchArm {
                        pattern: a.pattern.clone(),
                        guard: a.guard.clone(),
                        body: self.cps_value(&a.body)?,
                    });
                }
                Ok(Expr::Match { scrutinee: scrutinee.clone(), arms: new_arms })
            }
            Expr::Block(b) => self.cps_stmts(&b.stmts),
            _ => {
                reject_await(e, &self.fname)?;
                Ok(call("chan.done", vec![e.clone()]))
            }
        }
    }
}

/// `await E` -> `Some(&E)`, else None.
fn as_await(e: &Expr) -> Option<&Expr> {
    match e {
        Expr::Unary { op: UnOp::Await, expr } => Some(expr),
        _ => None,
    }
}

/// Reject an expression that still contains an `await` somewhere inside — the
/// transform only handles `await` in the supported positions above.
fn reject_await(e: &Expr, fname: &str) -> Result<(), String> {
    if contains_await(e) {
        Err(format!(
            "async fn `{fname}`: `await` must be the whole right-hand side of a \
             `let`, a bare statement, or in tail position — not nested inside a \
             larger expression (this restriction will be lifted later)"
        ))
    } else {
        Ok(())
    }
}

/// Whether an `await` appears anywhere in `e`. Exhaustive (no `_` arm) so adding
/// an `Expr` variant later forces this to be revisited — a surviving `await`
/// would be miscompiled (Phase-1 typeck treats it as identity), so completeness
/// matters. Descends into lambdas too: an `await` in a sync lambda is unsupported
/// and must be flagged, not skipped.
fn contains_await(e: &Expr) -> bool {
    match e {
        Expr::Unary { op: UnOp::Await, .. } => true,
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::Spawn { .. } => false,
        Expr::Unary { expr, .. } | Expr::Field { base: expr, .. } | Expr::Try(expr)
        | Expr::As { expr, .. } => contains_await(expr),
        Expr::Index { base, index } => contains_await(base) || contains_await(index),
        Expr::Binary { lhs, rhs, .. } => contains_await(lhs) || contains_await(rhs),
        Expr::Range { lo, hi, .. } => contains_await(lo) || contains_await(hi),
        Expr::List(xs) | Expr::Tuple(xs) => xs.iter().any(contains_await),
        Expr::Call { args, .. } | Expr::Ctor { args, .. } => args.iter().any(contains_await),
        Expr::MethodCall { receiver, args, .. } => {
            contains_await(receiver) || args.iter().any(contains_await)
        }
        Expr::Apply { func, args } => contains_await(func) || args.iter().any(contains_await),
        Expr::RecordUpdate { base, fields } => {
            contains_await(base) || fields.iter().any(|(_, v)| contains_await(v))
        }
        Expr::Record { fields, spread, .. } => {
            fields.iter().any(|(_, v)| contains_await(v))
                || spread.as_ref().is_some_and(|s| contains_await(s))
        }
        Expr::If { cond, then_block, else_block } => {
            contains_await(cond)
                || block_contains_await(then_block)
                || else_block.as_ref().is_some_and(block_contains_await)
        }
        Expr::Match { scrutinee, arms } => {
            contains_await(scrutinee)
                || arms.iter().any(|a| {
                    contains_await(&a.body) || a.guard.as_ref().is_some_and(contains_await)
                })
        }
        Expr::Block(b) => block_contains_await(b),
        Expr::While { cond, body } => contains_await(cond) || block_contains_await(body),
        Expr::For { iter, body, .. } => contains_await(iter) || block_contains_await(body),
        Expr::WhileLet { scrutinee, body, .. } => {
            contains_await(scrutinee) || block_contains_await(body)
        }
        Expr::Lambda { body, .. } => block_contains_await(body),
    }
}

fn block_contains_await(b: &Block) -> bool {
    b.stmts.iter().any(stmt_contains_await)
}

fn stmt_contains_await(s: &Stmt) -> bool {
    match s {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetTuple { value, .. }
        | Stmt::Expr(value)
        | Stmt::Yield(value) => contains_await(value),
        Stmt::Return(v) => v.as_ref().is_some_and(contains_await),
        Stmt::Break | Stmt::Continue => false,
    }
}

/// `future.and_then(inner, fn(bind): k)`.
fn and_then(inner: Expr, bind: String, k: Expr) -> Expr {
    let lambda = Expr::Lambda {
        params: vec![Param { name: bind, ty: None, convention: Convention::Let }],
        body: tail_block(k),
    };
    call("chan.and_then", vec![inner, lambda])
}

/// A block whose value is `head` (a normal statement) followed by the
/// continuation future `k` as the tail expression.
fn prefix_stmt(head: Stmt, k: Expr) -> Expr {
    Expr::Block(Block {
        stmts: vec![head, Stmt::Expr(k)],
        lines: vec![0, 0],
        restrict: None,
        region: None,
    })
}

fn call(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Call { name: name.to_string(), args }
}

/// A single-expression block (the body shape for a function/branch whose value is
/// exactly `e`).
fn tail_block(e: Expr) -> Block {
    Block { stmts: vec![Stmt::Expr(e)], lines: vec![0], restrict: None, region: None }
}
