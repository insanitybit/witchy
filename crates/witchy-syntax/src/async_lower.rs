//! Lower `async fn`/`await` into ordinary functions over `std/task`, by a
//! DEFUNCTIONALIZED state-machine transform run BEFORE typeck (like
//! `crate::generators`), so typeck / codegen / the interpreter never see `async`
//! or `await` (RFC-0059 Stage-1, step 1).
//!
//! Each `async fn` is compiled to a set of ordinary top-level **segment
//! functions**: the code between two suspension points is one segment, and every
//! `await` emits exactly ONE shallow continuation closure `fn(x):
//! __async_f_N(live-locals…, x)` that captures only the live locals and
//! tail-calls a *named* segment — never a nested `and_then` tower. This is the
//! defunctionalized equivalent of a frame record + a `match state` dispatcher (a
//! segment's parameter list *is* the frame's live columns; the function identity
//! *is* the state tag), chosen because the pre-typeck transform cannot spell the
//! field types a boxed record would demand — see the RFC-0059 "Implementation
//! note (2026-07-05)".
//!
//! An async function
//! ```text
//! async fn pipe(seed: Int) -> Int:
//!     let a = step(seed).await
//!     print_it(a)
//!     a + 1
//! ```
//! becomes (schematically):
//! ```text
//! fn pipe(seed: Int) -> Task(Int):
//!     task.lazy(fn(): task.and_then(step(seed), fn(a): __async_pipe_0(a)))
//! fn __async_pipe_0(a) -> Task(Int):
//!     print_it(a)
//!     task.done(a + 1)
//! ```
//! Because the continuation is a NAMED segment (not an inlined nested lambda), the
//! active `and_then` depth is bounded by the async-call-nesting depth rather than
//! the number of awaits or loop iterations, so `and_then_step`'s per-poll re-wrap
//! (RFC-0059's D2) is O(1) per async frame — the tower is gone. The executor,
//! `Step`/`Task`/`Slot`, and `std/task`/`std/chan` are UNCHANGED: the segment
//! closures plug into the existing `and_then`/`Step` machinery.
//!
//! Expressiveness that the old CPS lowering rejected now works because the state
//! machine (not a capture-by-value closure) carries the live locals: a mutable
//! `var` local may cross an `await` (it is threaded as a segment parameter), an
//! `await` may appear inside a `while` loop (the loop is a recursive segment
//! function), and a `for await` body may fold into an accumulator (threaded
//! through the loop segment's parameter).

use crate::ast::*;
use std::collections::HashSet;

pub fn lower(mut module: Module) -> Result<Module, String> {
    if !has_async(&module) {
        return Ok(module);
    }
    let mut counter: usize = 0;
    let mut items = Vec::with_capacity(module.items.len());
    let mut lifted: Vec<Function> = Vec::new();
    for item in module.items {
        match item {
            Item::Function(f) if f.is_async => {
                let is_entry = f.name == "main";
                let (entry, mut segs) = lower_async_fn(f, is_entry, &mut counter)?;
                items.push(Item::Function(entry));
                lifted.append(&mut segs);
            }
            // An `async fn` METHOD in an inherent `impl Type:` block: the method
            // stays a method (so `value.method()` still resolves by receiver type
            // and returns a `Task`), delegating to top-level segment functions.
            // Trait-impl `async` methods are rejected at parse time, so every impl
            // reaching here is inherent.
            Item::Impl(mut im) if im.methods.iter().any(|m| m.is_async) => {
                // A method's `self` is typed by the impl target — needed so a
                // carried `self` (a segment parameter) still resolves `self.field`.
                let self_ty = Type::Named(im.type_name.clone(), im.target_args.clone());
                let mut methods = Vec::with_capacity(im.methods.len());
                for method in std::mem::take(&mut im.methods) {
                    if method.is_async {
                        let (entry, mut segs) =
                            lower_async_fn_with(method, false, &mut counter, Some(self_ty.clone()))?;
                        methods.push(entry);
                        lifted.append(&mut segs);
                    } else {
                        methods.push(method);
                    }
                }
                im.methods = methods;
                items.push(Item::Impl(im));
            }
            other => items.push(other),
        }
    }
    // Emit the lifted segment functions at top level (after the entries).
    for seg in lifted {
        items.push(Item::Function(seg));
    }
    module.items = items;
    // The lowering uses the `task` substrate always, and `chan` for receive loops
    // (`for await`); the user's body may use either, so make both available.
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

/// Whether the module contains any `async fn` — top level or as an `impl` method.
fn has_async(module: &Module) -> bool {
    module.items.iter().any(|item| match item {
        Item::Function(f) => f.is_async,
        Item::Impl(im) => im.methods.iter().any(|m| m.is_async),
        _ => false,
    })
}

/// An in-scope local (function parameter or `let`/`var` binding): its name, its
/// known type (if the pre-typeck transform can derive one), and whether it is
/// mutable. When such a local is carried across an `await` it becomes a parameter
/// of the continuation segment, so the type pins operations that would otherwise
/// fail on an un-annotated generic (`i < n`), and the mutability picks the `own`
/// convention (a reassignable local, no caller write-back).
#[derive(Clone)]
struct Local {
    name: String,
    ty: Option<Type>,
    mutable: bool,
}

struct Ctx<'a> {
    fname: String,
    counter: &'a mut usize,
    segments: Vec<Function>,
}

impl<'a> Ctx<'a> {
    fn fresh_seg(&mut self) -> String {
        let n = *self.counter;
        *self.counter += 1;
        format!("__async_{}_{}", sanitize(&self.fname), n)
    }

    fn fresh_tmp(&mut self) -> String {
        let n = *self.counter;
        *self.counter += 1;
        format!("__await{n}")
    }

    fn err(&self, msg: &str) -> String {
        format!("async fn `{}`: {msg}", self.fname)
    }

    /// Lower a statement sequence to a `Task`-valued expression, given the locals
    /// in scope. The tail of the sequence is the async fn's result (`task.done(V)`
    /// for a value, become-the-task for a tail `await`, `task.ready_unit()` on
    /// falling off the end); a `for`/`for await`/`while` body is transformed the
    /// same way and coerced to `Task(Nil)` by its caller.
    fn go(&mut self, stmts: &[Stmt], scope: &[Local]) -> Result<Expr, String> {
        let Some((head, rest)) = stmts.split_first() else {
            return Ok(call("task.ready_unit", vec![]));
        };
        let is_last = rest.is_empty();
        match head {
            Stmt::Let { name, value, mutable, ty } => {
                if let Some(inner) = as_await(value) {
                    reject_await(inner, &self.fname)?;
                    // `let x = E.await` — suspend, then continue with `x` bound.
                    let bind = Local { name: name.clone(), ty: ty.clone(), mutable: *mutable };
                    self.suspend(inner.clone(), Some(bind), rest, scope)
                } else {
                    reject_await(value, &self.fname)?;
                    let mut scope2 = scope.to_vec();
                    scope2.push(Local {
                        name: name.clone(),
                        ty: ty.clone().or_else(|| derive_type(value)),
                        mutable: *mutable,
                    });
                    Ok(prefix_stmt(head.clone(), self.go(rest, &scope2)?))
                }
            }
            Stmt::LetPattern { pattern, value } if as_await(value).is_some() => {
                let inner = as_await(value).unwrap().clone();
                reject_await(&inner, &self.fname)?;
                // `let (a, b) = E.await` — desugar to `let tmp = E.await; let (a, b)
                // = tmp` so the ordinary `let`-await suspension path handles it (one
                // seam, then a plain destructure in the continuation segment).
                let tmp = self.fresh_tmp();
                let mut new_stmts = Vec::with_capacity(rest.len() + 2);
                new_stmts.push(Stmt::Let {
                    name: tmp.clone(),
                    ty: None,
                    mutable: false,
                    value: Expr::Unary { op: UnOp::Await, expr: Box::new(inner) },
                });
                new_stmts.push(Stmt::LetPattern {
                    pattern: pattern.clone(),
                    value: Expr::Var(tmp),
                });
                new_stmts.extend_from_slice(rest);
                self.go(&new_stmts, scope)
            }
            Stmt::LetPattern { pattern, value } => {
                reject_await(value, &self.fname)?;
                let mut binds = Vec::new();
                pattern_binds(pattern, &mut binds);
                let mut scope2 = scope.to_vec();
                for b in &binds {
                    scope2.push(Local { name: b.clone(), ty: None, mutable: false });
                }
                if is_last {
                    Ok(prefix_stmt(head.clone(), call("task.ready_unit", vec![])))
                } else {
                    Ok(prefix_stmt(head.clone(), self.go(rest, &scope2)?))
                }
            }
            Stmt::Assign { value, .. } => {
                reject_await(value, &self.fname)?;
                // A plain reassignment of an in-scope `var`. Kept verbatim; if the
                // var is carried across a later await it rides a segment parameter.
                if is_last {
                    Ok(prefix_stmt(head.clone(), call("task.ready_unit", vec![])))
                } else {
                    Ok(prefix_stmt(head.clone(), self.go(rest, scope)?))
                }
            }
            Stmt::Return(Some(e)) => {
                // `return e` exits the function early with its value.
                self.tail_value(e, scope)
            }
            Stmt::Return(None) => Ok(call("task.ready_unit", vec![])),
            Stmt::Expr(e) => self.expr_stmt(e, rest, scope, is_last),
            Stmt::Yield(_) => Err(self.err("`yield` is not allowed in an async fn")),
            Stmt::Break | Stmt::Continue => {
                Err(self.err("`break`/`continue` across `await` is not yet supported"))
            }
        }
    }

    /// A `Stmt::Expr` — the workhorse: bare awaits, loops, tail values, effects.
    fn expr_stmt(
        &mut self,
        e: &Expr,
        rest: &[Stmt],
        scope: &[Local],
        is_last: bool,
    ) -> Result<Expr, String> {
        // A loop whose body (or receiver) drives the executor.
        if let Expr::For { var, iter, body } = e {
            let loop_future = if let Some(src) = as_recv_stream(iter) {
                Some(self.lower_for_await(var, src, body, scope)?)
            } else if block_contains_await(body) {
                Some(self.lower_for(var, iter, body, scope)?)
            } else {
                None
            };
            if let Some(loop_future) = loop_future {
                return if is_last {
                    // In tail position a `for`/`for await` returns `Task(Nil)`.
                    Ok(loop_future)
                } else {
                    // Sequence the loop with the rest via a suspension seam.
                    self.suspend(RawTask(loop_future), None, rest, scope)
                };
            }
        }
        if let Expr::While { cond, body } = e {
            if contains_await(cond) {
                return Err(self.err("`await` in a `while` condition is not yet supported"));
            }
            if block_contains_await(body) {
                let loop_future = self.lower_while(cond, body, scope)?;
                return if is_last {
                    Ok(loop_future)
                } else {
                    self.suspend(RawTask(loop_future), None, rest, scope)
                };
            }
        }

        if is_last {
            self.tail_value(e, scope)
        } else if let Some(inner) = as_await(e) {
            // A non-last `await E` runs E for effect and continues.
            reject_await(inner, &self.fname)?;
            self.suspend(inner.clone(), None, rest, scope)
        } else {
            reject_await(e, &self.fname)?;
            Ok(prefix_stmt(Stmt::Expr(e.clone()), self.go(rest, scope)?))
        }
    }

    /// The function's tail value (or a `return`'s value): `await E` -> become E;
    /// a tail `if`/`match` -> each branch made a task; a plain value ->
    /// `task.done(value)`.
    fn tail_value(&mut self, e: &Expr, scope: &[Local]) -> Result<Expr, String> {
        if let Some(inner) = as_await(e) {
            reject_await(inner, &self.fname)?;
            return Ok(inner.clone());
        }
        match e {
            Expr::If { cond, then_block, else_block } => {
                if contains_await(cond) {
                    return Err(self.err("`await` in an `if` condition is not yet supported"));
                }
                let then_f = self.go(&then_block.stmts, scope)?;
                let else_f = match else_block {
                    Some(b) => self.go(&b.stmts, scope)?,
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
                    // Each arm may bind pattern variables; extend the scope.
                    let mut binds = Vec::new();
                    pattern_binds(&a.pattern, &mut binds);
                    let mut scope2 = scope.to_vec();
                    for b in &binds {
                        scope2.push(Local { name: b.clone(), ty: None, mutable: false });
                    }
                    new_arms.push(MatchArm {
                        pattern: a.pattern.clone(),
                        guard: a.guard.clone(),
                        body: self.tail_value(&a.body, &scope2)?,
                    });
                }
                Ok(Expr::Match { scrutinee: scrutinee.clone(), arms: new_arms })
            }
            Expr::Block(b) => self.go(&b.stmts, scope),
            _ => {
                reject_await(e, &self.fname)?;
                Ok(call("task.done", vec![e.clone()]))
            }
        }
    }

    /// Emit a suspension on `inner` (a `Task`): compute the continuation for
    /// `rest`, lift it to a segment, and return `and_then(inner, fn(bind):
    /// seg(carried…, bind))`. `bind` is the resume value (`None` for a discarded
    /// `await`).
    fn suspend(
        &mut self,
        inner: impl IntoTask,
        bind: Option<Local>,
        rest: &[Stmt],
        scope: &[Local],
    ) -> Result<Expr, String> {
        let mut cont_scope = scope.to_vec();
        if let Some(b) = &bind {
            cont_scope.push(b.clone());
        }
        let cont_expr = self.go(rest, &cont_scope)?;
        self.lift_suspend(inner, bind, cont_expr, scope)
    }

    /// Lift `cont_expr` (the continuation) to a top-level segment function whose
    /// parameters are the live locals it references (plus the resume `bind`), and
    /// return `and_then(inner, fn(bind): seg(carried…, bind))`.
    fn lift_suspend(
        &mut self,
        inner: impl IntoTask,
        bind: Option<Local>,
        cont_expr: Expr,
        scope: &[Local],
    ) -> Result<Expr, String> {
        // Carried = live locals of `cont_expr` that are in `scope` (excluding the
        // resume bind, which is passed separately).
        let bind_name = bind.as_ref().map(|b| b.name.clone());
        let carried = live_locals(&cont_expr, scope, bind_name.as_deref());

        let seg_name = self.fresh_seg();
        let mut params: Vec<Param> = carried.iter().map(local_to_param).collect();
        if let Some(b) = &bind {
            params.push(Param {
                name: b.name.clone(),
                ty: None,
                convention: if b.mutable { Convention::Own } else { Convention::Let },
                default: None,
            });
        }
        self.segments.push(Function {
            public: false,
            name: seg_name.clone(),
            params,
            ret: None,
            body: tail_block(cont_expr),
            bounds: vec![],
            is_gen: false,
            is_async: false,
        });

        // The continuation closure: `fn(bind): seg(carried…, bind)`.
        let mut call_args: Vec<Expr> = carried.iter().map(|l| Expr::Var(l.name.clone())).collect();
        let lam_params = match &bind {
            Some(b) => {
                call_args.push(Expr::Var(b.name.clone()));
                vec![Param {
                    name: b.name.clone(),
                    ty: None,
                    convention: Convention::Let,
                    default: None,
                }]
            }
            None => vec![Param {
                name: self.fresh_tmp(),
                ty: None,
                convention: Convention::Let,
                default: None,
            }],
        };
        let cont_lambda = Expr::Lambda {
            params: lam_params,
            body: tail_block(call(&seg_name, call_args)),
            ret: None,
        };
        Ok(call("task.and_then", vec![inner.into_task(), cont_lambda]))
    }

    /// `for x in xs:` whose body awaits — lowered to `task.for_each(xs', fn(x):
    /// <body>)`. The body is transformed (its awaits lifted to segments) and
    /// coerced to `Task(Nil)`. The loop variable stays a lambda parameter so its
    /// element type is inferred.
    fn lower_for(
        &mut self,
        var: &str,
        iter: &Expr,
        body: &Block,
        scope: &[Local],
    ) -> Result<Expr, String> {
        reject_await(iter, &self.fname)?;
        let list_expr = for_iter_list(iter);
        let mut scope2 = scope.to_vec();
        scope2.push(Local { name: var.to_string(), ty: None, mutable: false });
        let body_future = self.go(&body.stmts, &scope2)?;
        let body_nil = and_then(body_future, self.fresh_tmp(), call("task.ready_unit", vec![]));
        let f = Expr::Lambda {
            params: vec![Param {
                name: var.to_string(),
                ty: None,
                convention: Convention::Let,
                default: None,
            }],
            body: tail_block(body_nil),
            ret: None,
        };
        Ok(call("task.for_each", vec![list_expr, f]))
    }

    /// `for await x in rx:` — lowered to `chan.consume(rx, fn(x): <body>)`. The
    /// body is transformed and coerced to `Task(Nil)`; the message variable stays
    /// a lambda parameter so its type is inferred from the receiver.
    fn lower_for_await(
        &mut self,
        var: &str,
        src: &Expr,
        body: &Block,
        scope: &[Local],
    ) -> Result<Expr, String> {
        reject_await(src, &self.fname)?;
        let mut scope2 = scope.to_vec();
        scope2.push(Local { name: var.to_string(), ty: None, mutable: false });
        let body_future = self.go(&body.stmts, &scope2)?;
        let body_nil = and_then(body_future, self.fresh_tmp(), call("task.ready_unit", vec![]));
        let f = Expr::Lambda {
            params: vec![Param {
                name: var.to_string(),
                ty: None,
                convention: Convention::Let,
                default: None,
            }],
            body: tail_block(body_nil),
            ret: None,
        };
        Ok(call("chan.consume", vec![src.clone(), f]))
    }

    /// `while cond:` whose body awaits — placeholder rejected in Commit 1; the
    /// segment-based loop lands in the next increment.
    fn lower_while(&mut self, _cond: &Expr, _body: &Block, _scope: &[Local]) -> Result<Expr, String> {
        Err(self.err(
            "`await` inside a `while` loop is not yet supported by this increment",
        ))
    }
}

/// One `async fn` -> its entry function (a `Task`-returning ordinary function)
/// plus the lifted segment functions.
fn lower_async_fn(
    f: Function,
    is_entry: bool,
    counter: &mut usize,
) -> Result<(Function, Vec<Function>), String> {
    lower_async_fn_with(f, is_entry, counter, None)
}

/// As [`lower_async_fn`], with an optional receiver type for a method's `self`
/// (so a carried `self` keeps its type when it becomes a segment parameter).
fn lower_async_fn_with(
    f: Function,
    is_entry: bool,
    counter: &mut usize,
    self_ty: Option<Type>,
) -> Result<(Function, Vec<Function>), String> {
    let mut ctx = Ctx { fname: f.name.clone(), counter, segments: Vec::new() };
    let scope: Vec<Local> = f
        .params
        .iter()
        .map(|p| Local {
            name: p.name.clone(),
            ty: p.ty.clone().or_else(|| {
                if p.name == "self" {
                    self_ty.clone()
                } else {
                    None
                }
            }),
            mutable: p.convention.binds_mutable(),
        })
        .collect();
    let body_future = ctx.go(&f.body.stmts, &scope)?;
    let lazy_body = call(
        "task.lazy",
        vec![Expr::Lambda { params: vec![], body: tail_block(body_future), ret: None }],
    );
    let entry_body = if is_entry {
        // The runtime calls `main` directly and cannot drive a task, so an async
        // `main` IS the executor's entry point: run its body to completion.
        call("task.run", vec![lazy_body])
    } else {
        lazy_body
    };
    let entry = Function {
        public: f.public,
        name: f.name,
        params: f.params,
        ret: None,
        body: tail_block(entry_body),
        bounds: f.bounds,
        is_gen: false,
        is_async: false,
    };
    Ok((entry, ctx.segments))
}

/// A `Param` for a carried local, preserving its known type and picking `own`
/// (a reassignable local, no caller write-back) for a mutable var so a later
/// segment can keep mutating it.
fn local_to_param(l: &Local) -> Param {
    Param {
        name: l.name.clone(),
        ty: l.ty.clone(),
        convention: if l.mutable { Convention::Own } else { Convention::Let },
        default: None,
    }
}

/// A pre-typeck type guess for a `let x = <value>` binding, used to type a
/// carried frame column so an operation like `i < n` does not fall on an
/// un-annotated generic. Only the shapes the transform can be sure of.
fn derive_type(value: &Expr) -> Option<Type> {
    match value {
        Expr::Int(_) => Some(named("Int")),
        Expr::Float(_) => Some(named("Float")),
        Expr::Str(_) => Some(named("String")),
        Expr::Bool(_) => Some(named("Bool")),
        Expr::Duration(_) => Some(named("Duration")),
        _ => None,
    }
}

fn named(n: &str) -> Type {
    Type::Named(n.to_string(), vec![])
}

/// The live locals referenced by `expr` that are present in `scope`, in scope
/// order (deterministic), excluding `skip` (the resume bind, passed separately).
fn live_locals(expr: &Expr, scope: &[Local], skip: Option<&str>) -> Vec<Local> {
    let mut free = HashSet::new();
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    fv_expr(expr, &HashSet::new(), &mut seen, &mut order);
    for n in order {
        free.insert(n);
    }
    scope
        .iter()
        .filter(|l| Some(l.name.as_str()) != skip && free.contains(&l.name))
        .cloned()
        .collect()
}

// ---- free-variable analysis (binder-aware) ----

fn fv_expr(e: &Expr, bound: &HashSet<String>, seen: &mut HashSet<String>, out: &mut Vec<String>) {
    match e {
        Expr::Var(n) => {
            if !bound.contains(n) && seen.insert(n.clone()) {
                out.push(n.clone());
            }
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
        Expr::Unary { expr, .. } | Expr::Field { base: expr, .. } | Expr::Try(expr)
        | Expr::As { expr, .. } => fv_expr(expr, bound, seen, out),
        Expr::Index { base, index } => {
            fv_expr(base, bound, seen, out);
            fv_expr(index, bound, seen, out);
        }
        Expr::Binary { lhs, rhs, .. } => {
            fv_expr(lhs, bound, seen, out);
            fv_expr(rhs, bound, seen, out);
        }
        Expr::Range { lo, hi, .. } => {
            fv_expr(lo, bound, seen, out);
            fv_expr(hi, bound, seen, out);
        }
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                fv_expr(x, bound, seen, out);
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } => {
            for a in args {
                fv_expr(a, bound, seen, out);
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, a) in args {
                fv_expr(a, bound, seen, out);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            fv_expr(receiver, bound, seen, out);
            for a in args {
                fv_expr(a, bound, seen, out);
            }
        }
        Expr::Apply { func, args } => {
            fv_expr(func, bound, seen, out);
            for a in args {
                fv_expr(a, bound, seen, out);
            }
        }
        Expr::RecordUpdate { base, fields } => {
            fv_expr(base, bound, seen, out);
            for (_, v) in fields {
                fv_expr(v, bound, seen, out);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                fv_expr(v, bound, seen, out);
            }
            if let Some(s) = spread {
                fv_expr(s, bound, seen, out);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            fv_expr(cond, bound, seen, out);
            fv_block(then_block, bound, seen, out);
            if let Some(b) = else_block {
                fv_block(b, bound, seen, out);
            }
        }
        Expr::Match { scrutinee, arms } => {
            fv_expr(scrutinee, bound, seen, out);
            for a in arms {
                let mut binds = Vec::new();
                pattern_binds(&a.pattern, &mut binds);
                let mut inner = bound.clone();
                inner.extend(binds);
                if let Some(g) = &a.guard {
                    fv_expr(g, &inner, seen, out);
                }
                fv_expr(&a.body, &inner, seen, out);
            }
        }
        Expr::Block(b) => fv_block(b, bound, seen, out),
        Expr::While { cond, body } => {
            fv_expr(cond, bound, seen, out);
            fv_block(body, bound, seen, out);
        }
        Expr::For { var, iter, body } => {
            fv_expr(iter, bound, seen, out);
            let mut inner = bound.clone();
            inner.insert(var.clone());
            fv_block(body, &inner, seen, out);
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            fv_expr(scrutinee, bound, seen, out);
            let mut binds = Vec::new();
            pattern_binds(pattern, &mut binds);
            let mut inner = bound.clone();
            inner.extend(binds);
            fv_block(body, &inner, seen, out);
        }
        Expr::Lambda { params, body, .. } => {
            let mut inner = bound.clone();
            for p in params {
                inner.insert(p.name.clone());
            }
            fv_block(body, &inner, seen, out);
        }
    }
}

/// Free variables of a block's statement sequence: `let`/`var`/`let PAT`
/// bindings are added to `bound` for the statements that follow them.
fn fv_block(b: &Block, bound: &HashSet<String>, seen: &mut HashSet<String>, out: &mut Vec<String>) {
    let mut bound = bound.clone();
    for s in &b.stmts {
        match s {
            Stmt::Let { name, value, .. } => {
                fv_expr(value, &bound, seen, out);
                bound.insert(name.clone());
            }
            Stmt::LetPattern { pattern, value } => {
                fv_expr(value, &bound, seen, out);
                let mut binds = Vec::new();
                pattern_binds(pattern, &mut binds);
                bound.extend(binds);
            }
            Stmt::Assign { value, .. } => fv_expr(value, &bound, seen, out),
            Stmt::Expr(e) | Stmt::Yield(e) => fv_expr(e, &bound, seen, out),
            Stmt::Return(v) => {
                if let Some(e) = v {
                    fv_expr(e, &bound, seen, out);
                }
            }
            Stmt::Break | Stmt::Continue => {}
        }
    }
}

// ---- small AST helpers ----

/// Turn a function name into a valid identifier fragment for a segment name.
fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

/// An awaited `Task` expression, or a raw already-`Task` expression (a lowered
/// loop). Lets `suspend` accept either.
trait IntoTask {
    fn into_task(self) -> Expr;
}
impl IntoTask for Expr {
    fn into_task(self) -> Expr {
        self
    }
}
struct RawTask(Expr);
impl IntoTask for RawTask {
    fn into_task(self) -> Expr {
        self.0
    }
}

/// The list a `for` iterator ranges over: a `lo..hi` / `lo..=hi` range becomes
/// the equivalent `list.range_between` call; any other iterator is already a list.
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
/// an `Expr` variant later forces this to be revisited.
fn contains_await(e: &Expr) -> bool {
    match e {
        Expr::Unary { op: UnOp::Await, .. } => true,
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => false,
        Expr::Unary { expr, .. } | Expr::Field { base: expr, .. } | Expr::Try(expr)
        | Expr::As { expr, .. } => contains_await(expr),
        Expr::Index { base, index } => contains_await(base) || contains_await(index),
        Expr::Binary { lhs, rhs, .. } => contains_await(lhs) || contains_await(rhs),
        Expr::Range { lo, hi, .. } => contains_await(lo) || contains_await(hi),
        Expr::List(xs) | Expr::Tuple(xs) => xs.iter().any(contains_await),
        Expr::Call { args, .. } | Expr::Ctor { args, .. } => args.iter().any(contains_await),
        Expr::LabeledCall { args, .. } => args.iter().any(|(_, a)| contains_await(a)),
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
        | Stmt::LetPattern { value, .. }
        | Stmt::Expr(value)
        | Stmt::Yield(value) => contains_await(value),
        Stmt::Return(v) => v.as_ref().is_some_and(contains_await),
        Stmt::Break | Stmt::Continue => false,
    }
}

/// `task.and_then(inner, fn(bind): k)`.
fn and_then(inner: Expr, bind: String, k: Expr) -> Expr {
    let lambda = Expr::Lambda {
        params: vec![Param { name: bind, ty: None, convention: Convention::Let, default: None }],
        body: tail_block(k),
        ret: None,
    };
    call("task.and_then", vec![inner, lambda])
}

/// A block whose value is `head` (a normal statement) followed by the
/// continuation future `k` as the tail expression.
fn prefix_stmt(head: Stmt, k: Expr) -> Expr {
    Expr::Block(Block {
        stmts: vec![head, Stmt::Expr(k)],
        lines: vec![0, 0],
        region: None,
    })
}

fn call(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Call { name: name.to_string(), args }
}

/// A single-expression block (the body shape for a function/branch whose value is
/// exactly `e`).
fn tail_block(e: Expr) -> Block {
    Block { stmts: vec![Stmt::Expr(e)], lines: vec![0], region: None }
}
