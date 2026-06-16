//! Lower `async fn`/`await` into ordinary functions over `std/task`, by a
//! CPS-over-closures transform run BEFORE typeck (like `crate::generators`), so
//! typeck / codegen / the interpreter never see `async` or `await`.
//!
//! An async function
//! ```text
//! async fn pipe(seed: Int) -> Int:
//!     let a = step(seed).await
//!     print_it(a)
//!     a + 1
//! ```
//! becomes a plain function returning a `Task`, where each `await` is the seam
//! at which the rest of the body is captured as a continuation closure:
//! ```text
//! fn pipe(seed: Int) -> Task(m, Int):
//!     task.lazy(fn():
//!         task.and_then(step(seed), fn(a):
//!             {
//!                 print_it(a)
//!                 task.done(a + 1)
//!             }))
//! ```
//! `await E` lowers to `task.and_then(E, fn(x): <rest>)`; a statement with no
//! `await` is kept verbatim (so ordinary `let`/`var`/effect semantics are
//! untouched) and the continuation rides as the block's tail. The whole body is
//! wrapped in `task.lazy` so calling an async fn does NO work until the task
//! is driven.
//!
//! Because the body's live locals become the captured values of continuation
//! closures — and captures are owned values, never internal references — there is
//! nothing self-referential and so (unlike Rust) no `Pin`.
//!
//! Scope of this pass (the rest is rejected with a clear error, to be lifted as
//! the transform grows): `await` may appear as the entire right-hand side of a
//! `let`, as a bare statement, in tail position (including the branches of a tail
//! `if`/`match`), or inside the body of a `for x in xs:` loop — which lowers to a
//! sequential `task.for_each` over the elements. `await` inside a `while` loop,
//! inside a condition or scrutinee, or nested within a larger expression is not
//! yet supported. Carrying a mutable `var` across an `await` is likewise
//! unsupported — and is caught for free by the existing rule that a closure may
//! not assign to a captured variable (which also bounds the `for` body to
//! loop-local state).

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
    // The lowering uses the `task` substrate (lazy/and_then/done/run) always, and
    // `chan` for receive loops (`for await`); the user's body may use either, so
    // make both available. Unused imports are harmless declarations.
    for needed in ["task", "chan"] {
        if !module.imports.iter().any(|m| m == needed) {
            module.imports.push(needed.to_string());
        }
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
        "task.lazy",
        vec![Expr::Lambda { params: vec![], body: tail_block(body_future) }],
    );

    if f.name == "main" {
        // The runtime calls `main` directly and cannot drive a task, so an async
        // `main` IS the executor's entry point: run its body (a single task, which
        // may itself `spawn` more) to completion on the cooperative scheduler.
        let driven = call("task.run", vec![lazy_body]);
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

    /// Transform a statement sequence into a `Task`-valued expression.
    fn cps_stmts(&mut self, stmts: &[Stmt]) -> Result<Expr, String> {
        let Some((head, rest)) = stmts.split_first() else {
            return Ok(call("task.ready_unit", vec![]));
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
                // A `for x in xs:` whose body awaits becomes a sequential
                // `task.for_each` over the elements — each iteration's body is a
                // task, run to completion before the next. A range iterator
                // becomes its list; assigning an outer `var` in the body is
                // rejected downstream (a closure can't mutate a captured var), so
                // only loop-local state crosses an `await` here.
                if let Expr::For { var, iter, body } = e {
                    // `for await x in rx:` (marked `chan.__recv_stream(rx)` by the
                    // parser) — a receive loop, lowered whether or not the body
                    // awaits; a plain `for` with an awaiting body becomes for_each.
                    let loop_future = if let Some(src) = as_recv_stream(iter) {
                        Some(self.cps_consume(var, src, body)?)
                    } else if block_contains_await(body) {
                        Some(self.cps_for(var, iter, body)?)
                    } else {
                        None
                    };
                    if let Some(loop_future) = loop_future {
                        return if is_last {
                            Ok(loop_future)
                        } else {
                            let bind = self.fresh();
                            let k = self.cps_stmts(rest)?;
                            Ok(and_then(loop_future, bind, k))
                        };
                    }
                }
                // A `while` loop needs mutable state carried across iterations to
                // make progress, which can't cross an `await` (captures are by
                // value) — so point at the supported forms instead of the generic
                // "nested await" message.
                if let Expr::While { cond, body } = e {
                    if contains_await(cond) || block_contains_await(body) {
                        return Err(self.err(
                            "`await` inside a `while` loop is not yet supported — \
                             iterate a list with `for x in xs:` (which supports \
                             `await`), or loop by recursing with an async fn",
                        ));
                    }
                }
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
            Stmt::Return(None) => Ok(call("task.ready_unit", vec![])),
            // `let (a, b) = await E` — await the value, then destructure it. The
            // common shape for `let (tx, rx) = chan.channel(..).await`.
            Stmt::LetTuple { names, value } if as_await(value).is_some() => {
                let inner = as_await(value).unwrap();
                reject_await(inner, &self.fname)?;
                let tmp = self.fresh();
                let k = self.cps_stmts(rest)?;
                let destructure =
                    Stmt::LetTuple { names: names.clone(), value: Expr::Var(tmp.clone()) };
                let body = Expr::Block(Block {
                    stmts: vec![destructure, Stmt::Expr(k)],
                    lines: vec![0, 0],
                    restrict: None,
                    region: None,
                });
                Ok(and_then(inner.clone(), tmp, body))
            }
            Stmt::Assign { value, .. } | Stmt::LetTuple { value, .. } => {
                reject_await(value, &self.fname)?;
                if is_last {
                    Ok(prefix_stmt(head.clone(), call("task.ready_unit", vec![])))
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

    /// Lower `for await x in rx:` into `chan.consume(rx, fn(x): <body>)` — a
    /// receive loop that runs the (CPS-transformed) body for each message until
    /// the channel closes.
    fn cps_consume(&mut self, var: &str, src: &Expr, body: &Block) -> Result<Expr, String> {
        reject_await(src, &self.fname)?;
        let body_future = self.cps_stmts(&body.stmts)?;
        let discard = self.fresh();
        let body_nil = and_then(body_future, discard, call("task.ready_unit", vec![]));
        let f = Expr::Lambda {
            params: vec![Param { name: var.to_string(), ty: None, convention: Convention::Let }],
            body: tail_block(body_nil),
        };
        Ok(call("chan.consume", vec![src.clone(), f]))
    }

    /// Lower a `for x in xs:` whose body awaits into a `chan.for_each(xs', fn(x):
    /// <body>)` task. The body is CPS-transformed and coerced to `Task(m, Nil)`
    /// (the loop discards each iteration's value); a range iterator is turned into
    /// its list.
    fn cps_for(&mut self, var: &str, iter: &Expr, body: &Block) -> Result<Expr, String> {
        reject_await(iter, &self.fname)?;
        let list_expr = for_iter_list(iter);
        let body_future = self.cps_stmts(&body.stmts)?;
        let discard = self.fresh();
        let body_nil = and_then(body_future, discard, call("task.ready_unit", vec![]));
        let f = Expr::Lambda {
            params: vec![Param { name: var.to_string(), ty: None, convention: Convention::Let }],
            body: tail_block(body_nil),
        };
        Ok(call("task.for_each", vec![list_expr, f]))
    }

    /// Transform an expression in VALUE position (the function's result) into a
    /// `Task`-valued expression: `await E` -> `E`; a tail `if`/`match` ->
    /// branches each made into a task; a plain value -> `task.done(value)`.
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
                    None => call("task.ready_unit", vec![]),
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
                Ok(call("task.done", vec![e.clone()]))
            }
        }
    }
}

/// The list a `for` iterator ranges over: a `lo..hi` / `lo..=hi` range becomes
/// the equivalent `list.range_between` call (the loop runs over a real list, the
/// shape `for_each` consumes); any other iterator is already a list expression.
fn for_iter_list(iter: &Expr) -> Expr {
    match iter {
        Expr::Range { lo, hi, inclusive } => {
            let hi_expr = if *inclusive {
                Expr::Binary { op: BinOp::Add, lhs: hi.clone(), rhs: Box::new(Expr::Int(1)) }
            } else {
                (**hi).clone()
            };
            call("list.range_between", vec![(**lo).clone(), hi_expr])
        }
        other => other.clone(),
    }
}

/// The receiver of a `for await x in rx:` loop — the parser marks it as
/// `chan.__recv_stream(rx)`; this unwraps back to `rx`.
fn as_recv_stream(e: &Expr) -> Option<&Expr> {
    match e {
        Expr::Call { name, args } if name == "chan.__recv_stream" && args.len() == 1 => {
            Some(&args[0])
        }
        _ => None,
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
        | Expr::Var(_) => false,
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

/// `task.and_then(inner, fn(bind): k)`.
fn and_then(inner: Expr, bind: String, k: Expr) -> Expr {
    let lambda = Expr::Lambda {
        params: vec![Param { name: bind, ty: None, convention: Convention::Let }],
        body: tail_block(k),
    };
    call("task.and_then", vec![inner, lambda])
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
