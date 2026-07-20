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
use crate::source_check::{AsyncLoweredModule, GeneratorsLoweredModule};
// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::{HashSet, HashSetExt as _};

pub fn lower(checked: GeneratorsLoweredModule) -> Result<AsyncLoweredModule, String> {
    let view_fns: HashSet<String> = checked
        .module()
        .items
        .iter()
        .flat_map(|item| match item {
            Item::Function(function) => {
                let mut names = Vec::new();
                if function.ret.as_ref().is_some_and(type_is_borrowed_view) {
                    names.push(function.name.clone());
                }
                if function.ret.as_ref().is_some_and(type_is_view_callable) {
                    names.push(format!("@callable:{}", function.name));
                }
                names
            }
            Item::Impl(definition) => definition
                .methods
                .iter()
                .flat_map(|method| {
                    let mut names = Vec::new();
                    if method.ret.as_ref().is_some_and(type_is_borrowed_view) {
                        names.push(format!("@method:{}", method.name));
                    }
                    if method.ret.as_ref().is_some_and(type_is_view_callable) {
                        names.push(format!("@callable-method:{}", method.name));
                    }
                    names
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    lower_with_view_fns(checked, &view_fns)
}

pub(crate) fn lower_with_view_fns(
    mut checked: GeneratorsLoweredModule,
    view_fns: &HashSet<String>,
) -> Result<AsyncLoweredModule, String> {
    if !has_async(checked.module()) {
        return Ok(AsyncLoweredModule::preserve(checked.into_module()));
    }
    let module = checked.module_mut();
    let mut counter: usize = 0;
    let mut items = Vec::with_capacity(module.items.len());
    let mut lifted: Vec<Function> = Vec::new();
    for item in std::mem::take(&mut module.items) {
        match item {
            Item::Function(f) if f.is_async => {
                let is_entry = f.name == "main";
                let (entry, mut segs) = lower_async_fn(f, is_entry, &mut counter, view_fns)?;
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
                            lower_async_fn_with(
                                method,
                                false,
                                &mut counter,
                                Some(self_ty.clone()),
                                view_fns,
                            )?;
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
    Ok(AsyncLoweredModule::preserve(checked.into_module()))
}

/// Validate source-only async rules before lowering removes `async` and
/// rewrites its tail expressions.
pub(crate) fn validate_source(module: &Module) -> Result<(), String> {
    for item in &module.items {
        match item {
            Item::Function(function) if function.is_async => {
                validate_async_source(function, function.name == "main")?;
            }
            Item::Impl(definition) => {
                for method in &definition.methods {
                    if method.is_async {
                        validate_async_source(method, false)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_async_source(function: &Function, is_entry: bool) -> Result<(), String> {
    let declared_ret = function.ret.as_ref();
    if function
        .params
        .iter()
        .filter_map(|param| param.ty.as_ref())
        .any(type_is_borrowed_view)
        || declared_ret.is_some_and(type_is_borrowed_view)
    {
        return Err(format!(
            "async fn `{}` may not expose a borrowed-view parameter or result because its task \
             can outlive the caller's loan — pass/return an owned value",
            function.name,
        ));
    }
    if is_entry
        && declared_ret.is_some_and(|ret| {
            !matches!(ret.unqualified(), Type::Named(name, args) if name == "Nil" && args.is_empty())
                && !matches!(ret.unqualified(), Type::Tuple(types) if types.is_empty())
        })
    {
        let ret = crate::format::type_str(declared_ret.expect("checked above"));
        return Err(format!(
            "async fn `main` returns `{ret}`, but the async executor drives `Task(())` and \
             cannot surface a completed value; handle the value inside `main` and omit the return type"
        ));
    }
    validate_async_statements(&function.body.stmts, &function.name)
}

fn validate_async_statements(statements: &[Stmt], name: &str) -> Result<(), String> {
    for statement in statements {
        match statement {
            Stmt::Yield(_) => {
                return Err(format!("async fn `{name}`: `yield` is not allowed in an async fn"));
            }
            Stmt::Break | Stmt::Continue => {
                return Err(format!(
                    "async fn `{name}`: `break`/`continue` across `await` is not yet supported"
                ));
            }
            _ => {}
        }
    }
    match statements.last() {
        Some(Stmt::Return(Some(expr)) | Stmt::Expr(expr)) => validate_async_tail(expr, name),
        _ => Ok(()),
    }
}

fn validate_async_tail(expr: &Expr, name: &str) -> Result<(), String> {
    match expr {
        Expr::If { then_block, else_block, .. } => {
            if then_block.region.is_some() || else_block.as_ref().is_some_and(|block| block.region.is_some()) {
                return Err(format!(
                    "async fn `{name}`: `region:` in an async tail branch is not yet supported"
                ));
            }
            validate_async_statements(&then_block.stmts, name)?;
            if let Some(block) = else_block {
                validate_async_statements(&block.stmts, name)?;
            }
            Ok(())
        }
        Expr::Match { arms, .. } => {
            for arm in arms {
                validate_async_tail(&arm.body, name)?;
            }
            Ok(())
        }
        Expr::Block(block) => {
            if block.region.is_some() {
                return Err(format!(
                    "async fn `{name}`: `region:` in an async tail expression is not yet supported"
                ));
            }
            validate_async_statements(&block.stmts, name)
        }
        _ => Ok(()),
    }
}

/// Whether the module contains any `async fn` — top level or as an `impl` method.
fn has_async(module: &Module) -> bool {
    module.items.iter().any(|item| match item {
        Item::Function(f) => f.is_async,
        Item::Impl(im) => im.methods.iter().any(|m| m.is_async),
        _ => false,
    })
}

fn type_is_borrowed_view(ty: &Type) -> bool {
    matches!(ty, Type::Qualified(TypeQual::Borrow(_), _))
}

fn type_is_view_callable(ty: &Type) -> bool {
    matches!(ty.unqualified(), Type::Fn(_, ret, _) if type_is_borrowed_view(ret))
}

fn callable_returns_view(value: &Expr, scope: &[Local], view_fns: &HashSet<String>) -> bool {
    match value {
        Expr::Var(name) => view_fns.contains(name)
            || scope
                .iter()
                .rev()
                .find(|local| local.name == *name)
                .is_some_and(|local| local.returns_view),
        Expr::As { ty, .. } => type_is_view_callable(ty),
        Expr::Lambda { ret: Some(ret), .. } => type_is_borrowed_view(ret),
        Expr::Call { name, .. } => view_fns.contains(&format!("@callable:{name}")),
        Expr::MethodCall { method, .. } => {
            view_fns.contains(&format!("@callable-method:{method}"))
        }
        _ => false,
    }
}

fn result_is_borrowed_view(
    value: &Expr,
    scope: &[Local],
    view_fns: &HashSet<String>,
) -> bool {
    match value {
        Expr::Var(name) => scope
            .iter()
            .rev()
            .find(|local| local.name == *name)
            .is_some_and(|local| local.borrowed_view),
        Expr::Call { name, .. } => view_fns.contains(name)
            || scope
                .iter()
                .rev()
                .find(|local| local.name == *name)
                .is_some_and(|local| local.returns_view),
        Expr::MethodCall { method, .. } => view_fns.contains(&format!("@method:{method}")),
        Expr::Apply { func, .. } => callable_returns_view(func, scope, view_fns),
        Expr::If { then_block, else_block, .. } => {
            block_tail_expr(then_block)
                .is_some_and(|tail| result_is_borrowed_view(tail, scope, view_fns))
                || else_block
                    .as_ref()
                    .and_then(block_tail_expr)
                    .is_some_and(|tail| result_is_borrowed_view(tail, scope, view_fns))
        }
        Expr::Match { arms, .. } => arms
            .iter()
            .any(|arm| result_is_borrowed_view(&arm.body, scope, view_fns)),
        Expr::Block(block) => block_tail_expr(block)
            .is_some_and(|tail| result_is_borrowed_view(tail, scope, view_fns)),
        _ => false,
    }
}

fn block_tail_expr(block: &Block) -> Option<&Expr> {
    match block.stmts.last() {
        Some(Stmt::Expr(expr)) => Some(expr),
        _ => None,
    }
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
    borrowed_view: bool,
    /// This local holds a function value whose result is a borrowed view.
    returns_view: bool,
}

/// Where a statement sequence's fall-through goes.
#[derive(Clone)]
enum Tail {
    /// The async fn's result: `task.done(V)` for a value, become-the-task for a
    /// tail `await`, `task.ready_unit()` on falling off the end.
    Return,
    /// A loop body: run the statements for effect and then tail-call `seg` with
    /// the current values of `carried` (the loop's live columns) — one iteration.
    Loop { seg: String, carried: Vec<Local> },
}

/// The header of a recursive-segment loop: a `while` condition, or a `for await`
/// receive over `src` binding `var`.
enum LoopHeader {
    While { cond: Expr },
    Recv { src: Expr, var: String },
}

struct Ctx<'a> {
    fname: String,
    counter: &'a mut usize,
    segments: Vec<Function>,
    view_fns: &'a HashSet<String>,
}

#[derive(Clone, Copy)]
struct Continuation<'a> {
    rest: &'a [Stmt],
    rest_lines: &'a [u32],
    scope: &'a [Local],
    tail: &'a Tail,
    line: u32,
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
    fn go(
        &mut self,
        stmts: &[Stmt],
        lines: &[u32],
        scope: &[Local],
        tail: &Tail,
    ) -> Result<Expr, String> {
        let Some((head, rest)) = stmts.split_first() else {
            return Ok(self.end(tail));
        };
        let line = line_at(lines, 0);
        let rest_lines = remaining_lines(lines);
        let cont = Continuation { rest, rest_lines, scope, tail, line };
        match head {
            Stmt::Let { name, value, mutable, ty } => {
                if let Some(inner) = as_await(value) {
                    reject_await(inner, &self.fname)?;
                    // `let x = E.await` — suspend, then continue with `x` bound.
                    let bind = Local {
                        name: name.clone(),
                        ty: ty.clone(),
                        mutable: *mutable,
                        borrowed_view: ty.as_ref().is_some_and(type_is_borrowed_view),
                        returns_view: ty.as_ref().is_some_and(type_is_view_callable),
                    };
                    self.suspend(inner.clone(), Some(bind), cont)
                } else {
                    reject_await(value, &self.fname)?;
                    let mut scope2 = scope.to_vec();
                    scope2.push(Local {
                        name: name.clone(),
                        ty: ty.clone().or_else(|| derive_type(value)),
                        mutable: *mutable,
                        borrowed_view: ty.as_ref().is_some_and(type_is_borrowed_view)
                            || result_is_borrowed_view(value, scope, self.view_fns),
                        returns_view: ty.as_ref().is_some_and(type_is_view_callable)
                            || callable_returns_view(value, scope, self.view_fns),
                    });
                    let cont2 = Continuation { scope: &scope2, ..cont };
                    Ok(prefix_stmt_at(
                        head.clone(),
                        self.go(cont2.rest, cont2.rest_lines, cont2.scope, cont2.tail)?,
                        line,
                        next_line(rest_lines, line),
                    ))
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
                let mut new_lines = Vec::with_capacity(new_stmts.len());
                new_lines.push(line);
                new_lines.push(line);
                new_lines.extend_from_slice(rest_lines);
                self.go(&new_stmts, &new_lines, scope, tail)
            }
            Stmt::LetPattern { pattern, value } => {
                reject_await(value, &self.fname)?;
                let mut binds = Vec::new();
                pattern_binds(pattern, &mut binds);
                let mut scope2 = scope.to_vec();
                for b in &binds {
                    scope2.push(Local {
                        name: b.clone(),
                        ty: None,
                        mutable: false,
                        borrowed_view: false,
                        returns_view: false,
                    });
                }
                let cont2 = Continuation { scope: &scope2, ..cont };
                Ok(prefix_stmt_at(
                    head.clone(),
                    self.go(cont2.rest, cont2.rest_lines, cont2.scope, cont2.tail)?,
                    line,
                    next_line(rest_lines, line),
                ))
            }
            Stmt::Assign { value, .. } => {
                reject_await(value, &self.fname)?;
                // A plain reassignment of an in-scope `var` (a mutable local or an
                // `own` segment/loop parameter). Kept verbatim; if the var is
                // carried across a later await / loop-back it rides a parameter.
                Ok(prefix_stmt_at(
                    head.clone(),
                    self.go(cont.rest, cont.rest_lines, cont.scope, cont.tail)?,
                    line,
                    next_line(rest_lines, line),
                ))
            }
            Stmt::Return(Some(e)) => {
                // `return e` exits the function early with its value, regardless of
                // any enclosing loop tail.
                self.tail_expr(e, scope, &Tail::Return)
            }
            Stmt::Return(None) => Ok(call("task.ready_unit", vec![])),
            Stmt::Expr(e) => self.expr_stmt(e, cont),
            Stmt::Yield(_) => Err(self.err("`yield` is not allowed in an async fn")),
            Stmt::Break | Stmt::Continue => {
                Err(self.err("`break`/`continue` across `await` is not yet supported"))
            }
        }
    }

    /// The expression a statement sequence produces when it runs off its end.
    fn end(&self, tail: &Tail) -> Expr {
        match tail {
            Tail::Return => call("task.ready_unit", vec![]),
            Tail::Loop { seg, carried } => {
                call(seg, carried.iter().map(|l| Expr::Var(l.name.clone())).collect())
            }
        }
    }

    /// A `Stmt::Expr` — the workhorse: bare awaits, loops, tail values, effects.
    fn expr_stmt(
        &mut self,
        e: &Expr,
        cont: Continuation<'_>,
    ) -> Result<Expr, String> {
        let is_last = cont.rest.is_empty();
        // A loop whose body (or receiver) drives the executor.
        if let Expr::For { var, iter, body } = e {
            if let Some(src) = as_recv_stream(iter) {
                return self.lower_for_await(var, src, body, cont);
            } else if block_contains_await(body) {
                let loop_future = self.lower_for(var, iter, body, cont.scope)?;
                return self.sequence_loop(loop_future, vec![], cont);
            }
        }
        if let Expr::While { cond, body } = e {
            if contains_await(cond) {
                return Err(self.err("`await` in a `while` condition is not yet supported"));
            }
            if block_contains_await(body) {
                return self.lower_while(cond, body, cont);
            }
        }

        if is_last {
            self.tail_expr(e, cont.scope, cont.tail)
        } else if let Some(inner) = as_await(e) {
            // A non-last `await E` runs E for effect and continues.
            reject_await(inner, &self.fname)?;
            self.suspend(inner.clone(), None, cont)
        } else {
            reject_await(e, &self.fname)?;
            Ok(prefix_stmt_at(
                Stmt::Expr(e.clone()),
                self.go(cont.rest, cont.rest_lines, cont.scope, cont.tail)?,
                cont.line,
                next_line(cont.rest_lines, cont.line),
            ))
        }
    }

    /// Sequence a lowered loop `Task` (whose result is the accumulator tuple named
    /// by `accs`, or `Nil` when `accs` is empty) with `rest`: the loop's result is
    /// rebound to the accumulators and the rest continues from there.
    fn sequence_loop(
        &mut self,
        loop_future: Expr,
        accs: Vec<Local>,
        cont: Continuation<'_>,
    ) -> Result<Expr, String> {
        if accs.is_empty() {
            // `Task(Nil)` loop. In tail-return position it IS the result.
            if cont.rest.is_empty() && matches!(cont.tail, Tail::Return) {
                return Ok(loop_future);
            }
            return self.suspend(RawTask(loop_future), None, cont);
        }
        // The loop yields its accumulators; rebind them, then continue.
        let acc_bind = self.fresh_tmp();
        let rebind = rebind_accs(&accs, &acc_bind);
        let mut cont_scope = cont.scope.to_vec();
        for a in &accs {
            // After the loop the accumulator is a fresh (mutable) local.
            cont_scope.push(Local {
                name: a.name.clone(),
                ty: a.ty.clone(),
                mutable: true,
                borrowed_view: a.borrowed_view,
                returns_view: a.returns_view,
            });
        }
        let rebind_count = rebind.len();
        let mut cont_stmts = rebind;
        let cont2 = Continuation { scope: &cont_scope, ..cont };
        let cont_expr = self.go(cont2.rest, cont2.rest_lines, cont2.scope, cont2.tail)?;
        cont_stmts.push(Stmt::Expr(cont_expr));
        let n = cont_stmts.len();
        let mut cont_lines = vec![cont.line; rebind_count];
        cont_lines.push(next_line(cont.rest_lines, cont.line));
        debug_assert_eq!(cont_lines.len(), n);
        let cont_block = Expr::Block(Block {
            stmts: cont_stmts,
            lines: cont_lines,
            region: None,
        });
        let bind = Local {
            name: acc_bind,
            ty: None,
            mutable: false,
            borrowed_view: false,
            returns_view: false,
        };
        self.lift_suspend(RawTask(loop_future), Some(bind), cont_block, cont.scope, cont.line)
    }

    /// The sequence's tail expression under `tail`. For `Tail::Return`: `await E`
    /// -> become E; a tail `if`/`match` -> each branch a task; a value ->
    /// `task.done(value)`. For `Tail::Loop`: run `e` for effect (a tail `await` is
    /// awaited, a value is bound to a throwaway so it is not a discarded result),
    /// then loop back.
    fn tail_expr(&mut self, e: &Expr, scope: &[Local], tail: &Tail) -> Result<Expr, String> {
        if let Some(inner) = as_await(e) {
            reject_await(inner, &self.fname)?;
            return match tail {
                Tail::Return => Ok(inner.clone()),
                Tail::Loop { .. } => {
                    Ok(and_then(inner.clone(), self.fresh_tmp(), self.end(tail)))
                }
            };
        }
        match e {
            Expr::If { cond, then_block, else_block } => {
                if contains_await(cond) {
                    return Err(self.err("`await` in an `if` condition is not yet supported"));
                }
                if then_block.region.is_some() {
                    return Err(self.err("`region:` in an async tail branch is not yet supported"));
                }
                let then_f = self.go(&then_block.stmts, &then_block.lines, scope, tail)?;
                let else_f = match else_block {
                    Some(b) => {
                        if b.region.is_some() {
                            return Err(
                                self.err("`region:` in an async tail branch is not yet supported")
                            );
                        }
                        self.go(&b.stmts, &b.lines, scope, tail)?
                    }
                    None => self.end(tail),
                };
                Ok(Expr::If {
                    cond: cond.clone(),
                    then_block: tail_block_at(then_f, first_line(&then_block.lines)),
                    else_block: Some(tail_block_at(
                        else_f,
                        else_block.as_ref().map_or(first_line(&then_block.lines), |b| {
                            first_line(&b.lines)
                        }),
                    )),
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
                        scope2.push(Local {
                            name: b.clone(),
                            ty: None,
                            mutable: false,
                            borrowed_view: false,
                            returns_view: false,
                        });
                    }
                    new_arms.push(MatchArm {
                        line: a.line,
                        pattern: a.pattern.clone(),
                        guard: a.guard.clone(),
                        body: self.tail_expr(&a.body, &scope2, tail)?,
                    });
                }
                Ok(Expr::Match { scrutinee: scrutinee.clone(), arms: new_arms })
            }
            Expr::Block(b) => {
                if b.region.is_some() {
                    return Err(self.err("`region:` in an async tail expression is not yet supported"));
                }
                self.go(&b.stmts, &b.lines, scope, tail)
            }
            _ => {
                reject_await(e, &self.fname)?;
                match tail {
                    Tail::Return => Ok(call("task.done", vec![e.clone()])),
                    Tail::Loop { .. } => {
                        // Run `e` for its effect (bound so the result is not a
                        // discarded value), then loop back.
                        Ok(prefix_stmt_at(
                            Stmt::Let {
                                name: self.fresh_tmp(),
                                ty: None,
                                mutable: false,
                                value: e.clone(),
                            },
                            self.end(tail),
                            0,
                            0,
                        ))
                    }
                }
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
        cont: Continuation<'_>,
    ) -> Result<Expr, String> {
        let mut cont_scope = cont.scope.to_vec();
        if let Some(b) = &bind {
            cont_scope.push(b.clone());
        }
        let cont2 = Continuation { scope: &cont_scope, ..cont };
        let cont_expr = self.go(cont2.rest, cont2.rest_lines, cont2.scope, cont2.tail)?;
        self.lift_suspend(inner, bind, cont_expr, cont.scope, next_line(cont.rest_lines, cont.line))
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
        line: u32,
    ) -> Result<Expr, String> {
        // Carried = live locals of `cont_expr` that are in `scope` (excluding the
        // resume bind, which is passed separately).
        let bind_name = bind.as_ref().map(|b| b.name.clone());
        let carried = live_locals(&cont_expr, scope, bind_name.as_deref());
        if let Some(view) = carried.iter().find(|local| local.borrowed_view) {
            return Err(self.err(&format!(
                "borrowed view `{}` remains live across `await` — materialize it with \
                 `{}.owned()` before suspension",
                view.name, view.name,
            )));
        }

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
            comptime_only: false,
            name: seg_name.clone(),
            params,
            ret: None,
            body: tail_block_at(cont_expr, line),
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
            body: tail_block_at(call(&seg_name, call_args), line),
            ret: None,
        };
        Ok(call("task.and_then", vec![inner.into_task(), cont_lambda]))
    }

    /// `for x in xs:` whose body awaits — lowered to `task.for_each(xs', fn(x):
    /// <body>)`. The body is transformed (its awaits lifted to segments) and
    /// coerced to `Task(Nil)`. The loop variable stays a lambda parameter so its
    /// element type is inferred. (A `for x in xs:` body cannot fold into an outer
    /// var — that shape is `for await` or `while`.)
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
        scope2.push(Local {
            name: var.to_string(),
            ty: None,
            mutable: false,
            borrowed_view: false,
            returns_view: false,
        });
        let body_future = self.go(&body.stmts, &body.lines, &scope2, &Tail::Return)?;
        let body_nil = and_then(body_future, self.fresh_tmp(), call("task.ready_unit", vec![]));
        let f = Expr::Lambda {
            params: vec![Param {
                name: var.to_string(),
                ty: None,
                convention: Convention::Let,
                default: None,
            }],
            body: tail_block_at(body_nil, first_line(&body.lines)),
            ret: None,
        };
        Ok(call("task.for_each", vec![list_expr, f]))
    }

    /// `for await x in rx:` — a receive-until-closed loop. A body that does NOT
    /// fold into an outer variable keeps the `chan.consume` lowering (its message
    /// variable is a lambda parameter, so its type is inferred and the interleaving
    /// is byte-identical to before). A body that DOES assign an outer, in-scope var
    /// (a fold, RFC-0059 expressiveness) lowers to a recursive segment loop that
    /// threads the accumulator through a parameter and yields it at close.
    fn lower_for_await(
        &mut self,
        var: &str,
        src: &Expr,
        body: &Block,
        cont: Continuation<'_>,
    ) -> Result<Expr, String> {
        reject_await(src, &self.fname)?;
        if !body_folds(body, cont.scope) {
            // Drain / effect body: keep `chan.consume` (interleaving preserved).
            let mut scope2 = cont.scope.to_vec();
            scope2.push(Local {
                name: var.to_string(),
                ty: None,
                mutable: false,
                borrowed_view: false,
                returns_view: false,
            });
            let body_future = self.go(&body.stmts, &body.lines, &scope2, &Tail::Return)?;
            let body_nil =
                and_then(body_future, self.fresh_tmp(), call("task.ready_unit", vec![]));
            let f = Expr::Lambda {
                params: vec![Param {
                    name: var.to_string(),
                    ty: None,
                    convention: Convention::Let,
                    default: None,
                }],
                body: tail_block_at(body_nil, first_line(&body.lines)),
                ret: None,
            };
            let consume = call("chan.consume", vec![src.clone(), f]);
            return self.sequence_loop(consume, vec![], cont);
        }
        // Folding receive loop.
        let want_accs = !(cont.rest.is_empty() && matches!(cont.tail, Tail::Return));
        let (entry, accs) = self.build_loop_seg(
            LoopHeader::Recv { src: src.clone(), var: var.to_string() },
            body,
            cont.scope,
            want_accs,
        )?;
        self.sequence_loop(entry, accs, cont)
    }

    /// `while cond:` whose body awaits — a recursive segment loop that threads its
    /// carried state (counter/accumulator) through parameters. The condition may
    /// not itself `await`.
    fn lower_while(
        &mut self,
        cond: &Expr,
        body: &Block,
        cont: Continuation<'_>,
    ) -> Result<Expr, String> {
        let want_accs = !(cont.rest.is_empty() && matches!(cont.tail, Tail::Return));
        let (entry, accs) = self.build_loop_seg(
            LoopHeader::While { cond: cond.clone() },
            body,
            cont.scope,
            want_accs,
        )?;
        self.sequence_loop(entry, accs, cont)
    }

    /// The core recursive-segment loop builder shared by `while` and folding
    /// `for await`. Returns `(entry, accs)` — a `Task` that runs the loop and
    /// yields the accumulator tuple named by `accs` (or `Nil` when `accs` is
    /// empty). Carried columns (live locals of the header + body) ride the loop
    /// segment's parameters (`own` for mutable ones), so a mutation is an ordinary
    /// reassignment and the loop-back tail-call threads the updated value.
    fn build_loop_seg(
        &mut self,
        header: LoopHeader,
        body: &Block,
        scope: &[Local],
        want_accs: bool,
    ) -> Result<(Expr, Vec<Local>), String> {
        // Live locals of the header + body, in scope order.
        let mut probe = Vec::new();
        let mut seen = HashSet::new();
        let mut order = Vec::new();
        match &header {
            LoopHeader::While { cond } => fv_expr(cond, &HashSet::new(), &mut seen, &mut order),
            LoopHeader::Recv { src, .. } => fv_expr(src, &HashSet::new(), &mut seen, &mut order),
        }
        let mut body_bound = HashSet::new();
        if let LoopHeader::Recv { var, .. } = &header {
            body_bound.insert(var.clone());
        }
        fv_block(body, &body_bound, &mut seen, &mut order);
        for n in order {
            probe.push(n);
        }
        let probe: HashSet<String> = probe.into_iter().collect();
        let carried: Vec<Local> =
            scope.iter().filter(|l| probe.contains(&l.name)).cloned().collect();

        let accs: Vec<Local> = if want_accs {
            carried.iter().filter(|l| l.mutable).cloned().collect()
        } else {
            vec![]
        };

        let seg_name = self.fresh_seg();
        let loop_tail = Tail::Loop { seg: seg_name.clone(), carried: carried.clone() };
        let exit = self.loop_exit(&accs);

        // Loop-segment parameters: the carried columns (`own` for mutable ones).
        let params: Vec<Param> = carried.iter().map(local_to_param).collect();

        let loop_body = match &header {
            LoopHeader::While { cond } => {
                let body_expr = self.go(&body.stmts, &body.lines, &carried, &loop_tail)?;
                Expr::If {
                    cond: Box::new(cond.clone()),
                    then_block: tail_block_at(body_expr, first_line(&body.lines)),
                    else_block: Some(tail_block_at(exit, first_line(&body.lines))),
                }
            }
            LoopHeader::Recv { src, var } => {
                // The received value is a lambda/`match` binding, so its type is
                // inferred from the receiver — then dispatched to a segment where a
                // carried mutation is a real `own`-parameter reassignment.
                let mut body_scope = carried.clone();
                body_scope.push(Local {
                    name: var.clone(),
                    ty: None,
                    mutable: false,
                    borrowed_view: false,
                    returns_view: false,
                });
                let body_expr = self.go(&body.stmts, &body.lines, &body_scope, &loop_tail)?;
                let recv_name = self.fresh_seg();
                let o = self.fresh_tmp();
                let recv_arms = vec![
                    MatchArm {
                        line: 0,
                        pattern: Pattern::Ctor {
                            name: "Some".to_string(),
                            args: vec![Pattern::Var(var.clone())],
                        },
                        guard: None,
                        body: body_expr,
                    },
                    MatchArm {
                        line: 0,
                        pattern: Pattern::Ctor { name: "None".to_string(), args: vec![] },
                        guard: None,
                        body: exit,
                    },
                ];
                let recv_match =
                    Expr::Match { scrutinee: Box::new(Expr::Var(o.clone())), arms: recv_arms };
                // recv-segment: fn recv(carried…, o) = match o { Some(x)->body; None->exit }
                let mut recv_params: Vec<Param> = carried.iter().map(local_to_param).collect();
                recv_params.push(Param {
                    name: o.clone(),
                    ty: None,
                    convention: Convention::Let,
                    default: None,
                });
                self.segments.push(Function {
                    public: false,
                    comptime_only: false,
                    name: recv_name.clone(),
                    params: recv_params,
                    ret: None,
                    body: tail_block_at(recv_match, first_line(&body.lines)),
                    bounds: vec![],
                    is_gen: false,
                    is_async: false,
                });
                // loop-segment: and_then(chan.recv(src), fn(o): recv(carried…, o))
                let mut recv_args: Vec<Expr> =
                    carried.iter().map(|l| Expr::Var(l.name.clone())).collect();
                recv_args.push(Expr::Var(o.clone()));
                let recv_lambda = Expr::Lambda {
                    params: vec![Param {
                        name: o,
                        ty: None,
                        convention: Convention::Let,
                        default: None,
                    }],
                    body: tail_block_at(call(&recv_name, recv_args), first_line(&body.lines)),
                    ret: None,
                };
                call("task.and_then", vec![call("chan.recv", vec![src.clone()]), recv_lambda])
            }
        };

        self.segments.push(Function {
            public: false,
            comptime_only: false,
            name: seg_name.clone(),
            params,
            ret: None,
            body: tail_block_at(loop_body, first_line(&body.lines)),
            bounds: vec![],
            is_gen: false,
            is_async: false,
        });

        let entry = call(&seg_name, carried.iter().map(|l| Expr::Var(l.name.clone())).collect());
        Ok((entry, accs))
    }

    /// The loop's exit expression: `task.done(<accumulator tuple>)`, or
    /// `task.ready_unit()` when there is nothing to carry out.
    fn loop_exit(&self, accs: &[Local]) -> Expr {
        match accs.len() {
            0 => call("task.ready_unit", vec![]),
            1 => call("task.done", vec![Expr::Var(accs[0].name.clone())]),
            _ => {
                let tuple = Expr::Tuple(accs.iter().map(|l| Expr::Var(l.name.clone())).collect());
                call("task.done", vec![tuple])
            }
        }
    }
}

/// One `async fn` -> its entry function (a `Task`-returning ordinary function)
/// plus the lifted segment functions.
fn lower_async_fn(
    f: Function,
    is_entry: bool,
    counter: &mut usize,
    view_fns: &HashSet<String>,
) -> Result<(Function, Vec<Function>), String> {
    lower_async_fn_with(f, is_entry, counter, None, view_fns)
}

/// As [`lower_async_fn`], with an optional receiver type for a method's `self`
/// (so a carried `self` keeps its type when it becomes a segment parameter).
fn lower_async_fn_with(
    f: Function,
    is_entry: bool,
    counter: &mut usize,
    self_ty: Option<Type>,
    view_fns: &HashSet<String>,
) -> Result<(Function, Vec<Function>), String> {
    let declared_ret = f.ret.clone();
    if f.params
        .iter()
        .filter_map(|param| param.ty.as_ref())
        .any(type_is_borrowed_view)
        || declared_ret.as_ref().is_some_and(type_is_borrowed_view)
    {
        return Err(format!(
            "async fn `{}` may not expose a borrowed-view parameter or result because its task \
             can outlive the caller's loan — pass/return an owned value",
            f.name,
        ));
    }
    if is_entry
        && declared_ret
            .as_ref()
            .is_some_and(|ret| {
                !matches!(ret.unqualified(), Type::Named(name, args) if name == "Nil" && args.is_empty())
                && !matches!(ret.unqualified(), Type::Tuple(ts) if ts.is_empty())
            })
    {
        let ret = crate::format::type_str(declared_ret.as_ref().unwrap());
        return Err(format!(
            "async fn `main` returns `{ret}`, but the async executor drives `Task(())` and \
             cannot surface a completed value; handle the value inside `main` and omit the return type"
        ));
    }

    let mut ctx = Ctx {
        fname: f.name.clone(),
        counter,
        segments: Vec::new(),
        view_fns,
    };
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
            borrowed_view: p.ty.as_ref().is_some_and(type_is_borrowed_view),
            returns_view: p.ty.as_ref().is_some_and(type_is_view_callable),
        })
        .collect();
    let body_line = first_line(&f.body.lines);
    let body_future = ctx.go(&f.body.stmts, &f.body.lines, &scope, &Tail::Return)?;
    let lazy_body = call(
        "task.lazy",
        vec![Expr::Lambda {
            params: vec![],
            body: tail_block_at(body_future, body_line),
            ret: None,
        }],
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
        comptime_only: false,
        name: f.name,
        params: f.params,
        // Source `async fn f() -> T` describes the completed value, so callers
        // receive `Task(T)`. `main` is different: its wrapper drives `Task(Nil)`
        // and returns `Nil` directly. An omitted annotation remains inferred.
        ret: if is_entry {
            declared_ret
        } else {
            declared_ret.map(|ret| Type::Named("task.Task".to_string(), vec![ret]))
        },
        body: tail_block_at(entry_body, body_line),
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

/// Whether a loop body assigns to a variable that is already in scope — i.e. it
/// *folds* into an outer accumulator (as opposed to a pure drain / effect body).
/// Such a `for await` needs the recursive-segment loop; a non-folding one keeps
/// the interleaving-preserving `chan.consume` lowering.
fn body_folds(body: &Block, scope: &[Local]) -> bool {
    fn stmt_assigns_outer(s: &Stmt, scope: &[Local]) -> bool {
        match s {
            Stmt::Assign { name, .. } => scope.iter().any(|l| &l.name == name),
            Stmt::Expr(e) | Stmt::Let { value: e, .. } | Stmt::LetPattern { value: e, .. }
            | Stmt::Yield(e) => expr_assigns_outer(e, scope),
            Stmt::Return(v) => v.as_ref().is_some_and(|e| expr_assigns_outer(e, scope)),
            Stmt::Break | Stmt::Continue => false,
        }
    }
    fn block_assigns_outer(b: &Block, scope: &[Local]) -> bool {
        b.stmts.iter().any(|s| stmt_assigns_outer(s, scope))
    }
    fn expr_assigns_outer(e: &Expr, scope: &[Local]) -> bool {
        match e {
            Expr::If { then_block, else_block, .. } => {
                block_assigns_outer(then_block, scope)
                    || else_block.as_ref().is_some_and(|b| block_assigns_outer(b, scope))
            }
            Expr::Match { arms, .. } => arms.iter().any(|a| expr_assigns_outer(&a.body, scope)),
            Expr::Block(b) => block_assigns_outer(b, scope),
            Expr::While { body, .. } | Expr::For { body, .. }
            | Expr::WhileLet { body, .. } => block_assigns_outer(body, scope),
            _ => false,
        }
    }
    body.stmts.iter().any(|s| stmt_assigns_outer(s, scope))
}

/// Statements that rebind a loop's accumulator tuple `acc_bind` back to its named
/// (mutable) columns after the loop yields it.
fn rebind_accs(accs: &[Local], acc_bind: &str) -> Vec<Stmt> {
    match accs.len() {
        0 => vec![],
        1 => vec![Stmt::Let {
            name: accs[0].name.clone(),
            ty: None,
            mutable: true,
            value: Expr::Var(acc_bind.to_string()),
        }],
        _ => vec![Stmt::LetPattern {
            pattern: Pattern::Tuple(accs.iter().map(|l| Pattern::Var(l.name.clone())).collect()),
            value: Expr::Var(acc_bind.to_string()),
        }],
    }
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
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => {
            fv_expr(expr, bound, seen, out)
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            fv_expr(receiver, bound, seen, out);
            for arg in args { fv_expr(arg, bound, seen, out); }
        }
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
        Expr::Call { args, .. } | Expr::Ctor { args, .. }
        | Expr::AnonCtor { args, .. } => {
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
        Expr::RecordUpdate { name: _, base, fields } => {
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
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => {
            contains_await(expr)
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            contains_await(receiver) || args.iter().any(contains_await)
        }
        Expr::Index { base, index } => contains_await(base) || contains_await(index),
        Expr::Binary { lhs, rhs, .. } => contains_await(lhs) || contains_await(rhs),
        Expr::Range { lo, hi, .. } => contains_await(lo) || contains_await(hi),
        Expr::List(xs) | Expr::Tuple(xs) => xs.iter().any(contains_await),
        Expr::Call { args, .. } | Expr::Ctor { args, .. }
        | Expr::AnonCtor { args, .. } => args.iter().any(contains_await),
        Expr::LabeledCall { args, .. } => args.iter().any(|(_, a)| contains_await(a)),
        Expr::MethodCall { receiver, args, .. } => {
            contains_await(receiver) || args.iter().any(contains_await)
        }
        Expr::Apply { func, args } => contains_await(func) || args.iter().any(contains_await),
        Expr::RecordUpdate { name: _, base, fields } => {
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
fn prefix_stmt_at(head: Stmt, k: Expr, head_line: u32, tail_line: u32) -> Expr {
    Expr::Block(Block {
        stmts: vec![head, Stmt::Expr(k)],
        lines: vec![head_line, tail_line],
        region: None,
    })
}

fn call(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Call { name: name.to_string(), args }
}

/// A single-expression block (the body shape for a function/branch whose value is
/// exactly `e`).
fn tail_block(e: Expr) -> Block {
    tail_block_at(e, 0)
}

fn tail_block_at(e: Expr, line: u32) -> Block {
    Block { stmts: vec![Stmt::Expr(e)], lines: vec![line], region: None }
}

fn line_at(lines: &[u32], idx: usize) -> u32 {
    lines.get(idx).copied().unwrap_or(0)
}

fn first_line(lines: &[u32]) -> u32 {
    line_at(lines, 0)
}

fn remaining_lines(lines: &[u32]) -> &[u32] {
    lines.get(1..).unwrap_or(&[])
}

fn next_line(lines: &[u32], fallback: u32) -> u32 {
    first_line(lines).max(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower_module(module: Module) -> Result<Module, String> {
        let checked = crate::source_check::check(module)?;
        let checked = crate::generators::lower(checked)?;
        lower(checked).map(AsyncLoweredModule::into_module)
    }

    fn int_type() -> Type {
        Type::Named("Int".to_string(), Vec::new())
    }

    #[test]
    fn lowering_preserves_explicit_async_return_contracts() {
        let source = "async fn value() -> Int:\n    1\n\nasync fn main(console: Console) -> Nil:\n    return\n";
        let module = crate::parser::parse_module(source).expect("parse async declarations");
        let lowered = lower_module(module).expect("lower async declarations");

        let value = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "value" => Some(function),
                _ => None,
            })
            .expect("lowered value function");
        assert_eq!(
            value.ret,
            Some(Type::Named("task.Task".to_string(), vec![int_type()])),
        );

        let main = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("lowered main function");
        assert_eq!(main.ret, Some(Type::Named("Nil".to_string(), Vec::new())));
    }

    #[test]
    fn lowering_rejects_value_returning_async_main() {
        let source = "async fn main(console: Console) -> Int:\n    1\n";
        let module = crate::parser::parse_module(source).expect("parse async main");
        let error = lower_module(module).expect_err("the executor cannot surface an async root value");

        assert!(error.contains("async fn `main` returns `Int`"), "{error}");
        assert!(error.contains("Task(())"), "{error}");
    }

    #[test]
    fn lowering_preserves_explicit_async_method_return_contract() {
        let source = "type Counter:\n    value: Int\n\nimpl Counter:\n    async fn value(self) -> Int:\n        self.value\n";
        let module = crate::parser::parse_module(source).expect("parse async method");
        let lowered = lower_module(module).expect("lower async method");
        let method = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Impl(definition) => definition
                    .methods
                    .iter()
                    .find(|method| method.name == "value"),
                _ => None,
            })
            .expect("lowered async method");

        assert_eq!(
            method.ret,
            Some(Type::Named("task.Task".to_string(), vec![int_type()])),
        );
    }

    #[test]
    fn lowering_rejects_a_borrowed_view_live_across_await() {
        let source = "mode opt\n\nfn view(text: let('a) String) -> View(String, 'a):\n    text\n\nasync fn bad(console: Console):\n    let text = \"x\"\n    let w = view(text)\n    let _ = task.done(0).await\n    console.print(w)\n";
        let module = crate::parser::parse_module(source).expect("parse borrowed async body");
        let error = lower_module(module).expect_err("a view cannot be carried through a segment");
        assert!(error.contains("borrowed view `w` remains live across `await`"), "{error}");
        assert!(error.contains("w.owned()"), "{error}");
    }

    #[test]
    fn lowering_preserves_a_view_relation_through_a_function_value() {
        let source = "mode opt\n\nfn view(text: let('a) String) -> View(String, 'a):\n    text\n\nasync fn bad(console: Console):\n    let text = \"x\"\n    let make_view = view\n    let w = make_view(text)\n    let _ = task.done(0).await\n    console.print(w)\n";
        let module = crate::parser::parse_module(source).expect("parse indirect borrowed async body");
        let error = lower_module(module).expect_err("an indirect view cannot cross a segment");
        assert!(error.contains("borrowed view `w` remains live across `await`"), "{error}");
    }

    #[test]
    fn lowering_preserves_a_view_relation_through_a_returned_function_value() {
        let source = "mode opt\n\nfn view(text: let('a) String) -> View(String, 'a):\n    text\n\nfn make() -> fn(View(String, 'a)) -> View(String, 'a):\n    view\n\nasync fn bad(console: Console):\n    let text = \"x\"\n    let make_view = make()\n    let w = make_view(text)\n    let _ = task.done(0).await\n    console.print(w)\n";
        let module = crate::parser::parse_module(source).expect("parse returned callable");
        let error = lower_module(module).expect_err("a returned callable's view cannot cross a segment");
        assert!(error.contains("borrowed view `w` remains live across `await`"), "{error}");
    }

    #[test]
    fn lowering_tracks_a_view_returning_method_across_await() {
        let source = "mode opt\n\ntype Holder:\n    text: String\n\nimpl Holder:\n    fn view(self: let('a) Holder) -> View(String, 'a):\n        self.text\n\nasync fn bad(console: Console):\n    let holder = Holder(\"x\")\n    let w = holder.view()\n    let _ = task.done(0).await\n    console.print(w)\n";
        let module = crate::parser::parse_module(source).expect("parse method borrowed async body");
        let error = lower_module(module).expect_err("a method-returned view cannot cross a segment");
        assert!(error.contains("borrowed view `w` remains live across `await`"), "{error}");
    }

    #[test]
    fn lowering_allows_a_view_whose_last_use_precedes_await() {
        let source = "mode opt\n\nfn view(text: let('a) String) -> View(String, 'a):\n    text\n\nasync fn okay(console: Console):\n    let text = \"x\"\n    let w = view(text)\n    console.print(w)\n    task.done(0).await\n";
        let module = crate::parser::parse_module(source).expect("parse borrowed async body");
        lower_module(module).expect("a dead view is not carried across suspension");
    }
}
