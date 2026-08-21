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
//!     task.lazy(fn(): task.and_then(step(seed), fn(own a): __async_pipe_0(a)))
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

/// Nominal identities whose values carry a declared lifetime relation. Async
/// lowering runs before typeck, so this deliberately records only declarations
/// available in the current module. An imported shell remains detectable from
/// an explicit lifetime argument in an annotation; an unannotated cross-module
/// constructor result is left for the later whole-program loan gate.
#[derive(Default)]
struct BorrowedShellCatalog {
    types: HashSet<String>,
    constructors: HashSet<String>,
}

impl BorrowedShellCatalog {
    fn from_module(module: &Module) -> Self {
        let mut catalog = Self::default();
        for item in &module.items {
            let Item::Type(definition) = item else {
                continue;
            };
            if !definition
                .params
                .iter()
                .any(|parameter| is_lifetime_param(parameter))
            {
                continue;
            }
            catalog.types.insert(definition.name.clone());
            catalog.constructors.extend(
                definition
                    .variants
                    .iter()
                    .map(|variant| variant.name.clone()),
            );
        }
        catalog
    }

    fn type_is_borrowed(&self, ty: &Type) -> bool {
        match ty {
            Type::Qualified(
                TypeQual::Borrow(_) | TypeQual::LegacyBorrow(_) | TypeQual::BorrowMut(_),
                _,
            ) => true,
            Type::Qualified(_, inner) => self.type_is_borrowed(inner),
            Type::Slice(inner) => self.type_is_borrowed(inner),
            Type::Named(name, arguments) => {
                if is_lifetime_param(name) {
                    return false;
                }
                self.types.contains(name)
                    || arguments.iter().any(|argument| {
                        matches!(argument, Type::Named(name, args)
                            if args.is_empty() && is_lifetime_param(name))
                            || self.type_is_borrowed(argument)
                    })
            }
            Type::Tuple(items) | Type::Dyn(_, items) => {
                items.iter().any(|item| self.type_is_borrowed(item))
            }
            Type::RecordCompose { base, fields } => {
                self.type_is_borrowed(base)
                    || fields.iter().any(|(_, field)| self.type_is_borrowed(field))
            }
            // A function value owns its separately quantified relation. The
            // callable-return path below tracks what invoking it produces.
            Type::Fn(..) => false,
        }
    }
}

pub fn lower(checked: GeneratorsLoweredModule) -> Result<AsyncLoweredModule, String> {
    lower_with_item_mapping(checked).map(|(module, _)| module)
}

pub(crate) fn lower_with_item_mapping(
    checked: GeneratorsLoweredModule,
) -> Result<(AsyncLoweredModule, Vec<Vec<usize>>), String> {
    let borrowed_shells = BorrowedShellCatalog::from_module(checked.module());
    let view_fns: HashSet<String> = checked
        .module()
        .items
        .iter()
        .flat_map(|item| match item {
            Item::Function(function) => {
                let mut names = Vec::new();
                if function
                    .ret
                    .as_ref()
                    .is_some_and(|ty| borrowed_shells.type_is_borrowed(ty))
                {
                    names.push(function.name.clone());
                }
                if function
                    .ret
                    .as_ref()
                    .is_some_and(|ty| type_is_view_callable(ty, &borrowed_shells))
                {
                    names.push(format!("@callable:{}", function.name));
                }
                names
            }
            Item::Impl(definition) => definition
                .methods
                .iter()
                .flat_map(|method| {
                    let mut names = Vec::new();
                    if method
                        .ret
                        .as_ref()
                        .is_some_and(|ty| borrowed_shells.type_is_borrowed(ty))
                    {
                        names.push(format!("@method:{}", method.name));
                    }
                    if method
                        .ret
                        .as_ref()
                        .is_some_and(|ty| type_is_view_callable(ty, &borrowed_shells))
                    {
                        names.push(format!("@callable-method:{}", method.name));
                    }
                    names
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    lower_with_view_fns_and_item_mapping(checked, &view_fns)
}

pub(crate) fn lower_with_view_fns_and_item_mapping(
    mut checked: GeneratorsLoweredModule,
    view_fns: &HashSet<String>,
) -> Result<(AsyncLoweredModule, Vec<Vec<usize>>), String> {
    if !has_async(checked.module()) {
        let item_count = checked.module().items.len();
        return Ok((
            AsyncLoweredModule::preserve(checked.into_module()),
            (0..item_count).map(|index| vec![index]).collect(),
        ));
    }
    let borrowed_shells = BorrowedShellCatalog::from_module(checked.module());
    let mut known_view_fns = view_fns.clone();
    for item in &checked.module().items {
        match item {
            Item::Function(function) => {
                if function
                    .ret
                    .as_ref()
                    .is_some_and(|ty| borrowed_shells.type_is_borrowed(ty))
                {
                    known_view_fns.insert(function.name.clone());
                }
                if function
                    .ret
                    .as_ref()
                    .is_some_and(|ty| type_is_view_callable(ty, &borrowed_shells))
                {
                    known_view_fns.insert(format!("@callable:{}", function.name));
                }
            }
            Item::Impl(definition) => {
                for method in &definition.methods {
                    if method
                        .ret
                        .as_ref()
                        .is_some_and(|ty| borrowed_shells.type_is_borrowed(ty))
                    {
                        known_view_fns.insert(format!("@method:{}", method.name));
                    }
                    if method
                        .ret
                        .as_ref()
                        .is_some_and(|ty| type_is_view_callable(ty, &borrowed_shells))
                    {
                        known_view_fns.insert(format!("@callable-method:{}", method.name));
                    }
                }
            }
            _ => {}
        }
    }
    let module = checked.module_mut();
    let source_item_count = module.items.len();
    let mut mapping = vec![Vec::new(); source_item_count];
    let mut counter: usize = 0;
    let mut state_counter: usize = 0;
    let mut items = Vec::with_capacity(module.items.len());
    let mut lifted: Vec<(usize, Function)> = Vec::new();
    for (source_index, item) in std::mem::take(&mut module.items).into_iter().enumerate() {
        mapping[source_index].push(items.len());
        match item {
            Item::Function(f) if f.is_async => {
                let is_entry = f.name == "main";
                let (entry, mut segs) = lower_async_fn(
                    f,
                    is_entry,
                    &mut counter,
                    &mut state_counter,
                    &known_view_fns,
                    &borrowed_shells,
                )?;
                items.push(Item::Function(entry));
                lifted.extend(segs.drain(..).map(|segment| (source_index, segment)));
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
                        let (entry, mut segs) = lower_async_fn_with(
                            method,
                            false,
                            &mut counter,
                            &mut state_counter,
                            Some(self_ty.clone()),
                            &known_view_fns,
                            &borrowed_shells,
                        )?;
                        methods.push(entry);
                        lifted.extend(segs.drain(..).map(|segment| (source_index, segment)));
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
    for (source_index, seg) in lifted {
        mapping[source_index].push(items.len());
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
    Ok((AsyncLoweredModule::preserve(checked.into_module()), mapping))
}

/// Validate source-only async rules before lowering removes `async` and
/// rewrites its tail expressions.
pub(crate) fn validate_source(
    module: &Module,
) -> Result<(), crate::source_check::SourceValidationError> {
    let borrowed_shells = BorrowedShellCatalog::from_module(module);
    for (item_index, item) in module.items.iter().enumerate() {
        match item {
            Item::Function(function) if function.is_async => {
                validate_async_source(function, function.name == "main", None, &borrowed_shells)
                    .map_err(|message| {
                        crate::source_check::SourceValidationError::new(item_index, message)
                    })?;
            }
            Item::Impl(definition) => {
                let self_ty =
                    Type::Named(definition.type_name.clone(), definition.target_args.clone());
                for method in &definition.methods {
                    if method.is_async {
                        validate_async_source(method, false, Some(&self_ty), &borrowed_shells)
                            .map_err(|message| {
                                crate::source_check::SourceValidationError::new(item_index, message)
                            })?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_async_source(
    function: &Function,
    is_entry: bool,
    self_ty: Option<&Type>,
    borrowed_shells: &BorrowedShellCatalog,
) -> Result<(), String> {
    let declared_ret = function.ret.as_ref();
    if function
        .params
        .iter()
        .filter_map(|param| resolved_async_parameter_type(param, self_ty))
        .any(|ty| borrowed_shells.type_is_borrowed(ty))
        || declared_ret.is_some_and(|ty| borrowed_shells.type_is_borrowed(ty))
    {
        return Err(format!(
            "async fn `{}` may not expose a borrowed view or lifetime-bearing shell as a \
             parameter or result because its task \
             can outlive the caller's loan — pass/return an owned value",
            function.name,
        ));
    }
    if is_entry && declared_ret.is_some_and(|ret| {
        !matches!(ret.unqualified(), Type::Named(name, args) if name == "Nil" && args.is_empty())
            && !matches!(ret.unqualified(), Type::Tuple(types) if types.is_empty())
    }) {
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
                return Err(format!(
                    "async fn `{name}`: `yield` is not allowed in an async fn"
                ));
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
        Expr::If {
            then_block,
            else_block,
            ..
        } => {
            if then_block.region.is_some()
                || else_block
                    .as_ref()
                    .is_some_and(|block| block.region.is_some())
            {
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

fn type_is_view_callable(ty: &Type, borrowed_shells: &BorrowedShellCatalog) -> bool {
    matches!(ty.unqualified(), Type::Fn(_, ret, _, _)
        if borrowed_shells.type_is_borrowed(ret))
}

fn callable_returns_view(
    value: &Expr,
    scope: &[Local],
    view_fns: &HashSet<String>,
    borrowed_shells: &BorrowedShellCatalog,
) -> bool {
    match value {
        Expr::Var(name) => {
            view_fns.contains(name)
                || scope
                    .iter()
                    .rev()
                    .find(|local| local.name == *name)
                    .is_some_and(|local| local.returns_view)
        }
        Expr::As { ty, .. } => type_is_view_callable(ty, borrowed_shells),
        Expr::Lambda { ret: Some(ret), .. } => borrowed_shells.type_is_borrowed(ret),
        Expr::Call { name, .. } => view_fns.contains(&format!("@callable:{name}")),
        Expr::MethodCall { method, .. } => view_fns.contains(&format!("@callable-method:{method}")),
        _ => false,
    }
}

fn result_is_borrowed_view(
    value: &Expr,
    scope: &[Local],
    view_fns: &HashSet<String>,
    borrowed_shells: &BorrowedShellCatalog,
) -> bool {
    match value {
        Expr::Var(name) => scope
            .iter()
            .rev()
            .find(|local| local.name == *name)
            .is_some_and(|local| local.borrowed_view),
        Expr::Call { name, .. } => {
            view_fns.contains(name)
                || scope
                    .iter()
                    .rev()
                    .find(|local| local.name == *name)
                    .is_some_and(|local| local.returns_view)
        }
        Expr::MethodCall { method, .. } => view_fns.contains(&format!("@method:{method}")),
        Expr::Apply { func, .. } => callable_returns_view(func, scope, view_fns, borrowed_shells),
        Expr::As { ty, .. } => borrowed_shells.type_is_borrowed(ty),
        Expr::Ctor { name, args } => {
            borrowed_shells.constructors.contains(name)
                || args
                    .iter()
                    .any(|arg| result_is_borrowed_view(arg, scope, view_fns, borrowed_shells))
        }
        Expr::Record {
            name,
            fields,
            spread,
        } => {
            borrowed_shells.types.contains(name)
                || fields.iter().any(|(_, value)| {
                    result_is_borrowed_view(value, scope, view_fns, borrowed_shells)
                })
                || spread.as_ref().is_some_and(|value| {
                    result_is_borrowed_view(value, scope, view_fns, borrowed_shells)
                })
        }
        Expr::Tuple(items) | Expr::List(items) => items
            .iter()
            .any(|item| result_is_borrowed_view(item, scope, view_fns, borrowed_shells)),
        Expr::RecordUpdate { base, fields, .. } => {
            result_is_borrowed_view(base, scope, view_fns, borrowed_shells)
                || fields.iter().any(|(_, value)| {
                    result_is_borrowed_view(value, scope, view_fns, borrowed_shells)
                })
        }
        Expr::If {
            then_block,
            else_block,
            ..
        } => {
            block_tail_expr(then_block)
                .is_some_and(|tail| result_is_borrowed_view(tail, scope, view_fns, borrowed_shells))
                || else_block
                    .as_ref()
                    .and_then(block_tail_expr)
                    .is_some_and(|tail| {
                        result_is_borrowed_view(tail, scope, view_fns, borrowed_shells)
                    })
        }
        Expr::Match { arms, .. } => arms
            .iter()
            .any(|arm| result_is_borrowed_view(&arm.body, scope, view_fns, borrowed_shells)),
        Expr::Block(block) => block_tail_expr(block)
            .is_some_and(|tail| result_is_borrowed_view(tail, scope, view_fns, borrowed_shells)),
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
    /// A direct view or an aggregate/nominal shell carrying a lifetime relation.
    borrowed_view: bool,
    /// This local holds a function value whose result carries a borrowed relation.
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
    state_counter: &'a mut usize,
    segments: Vec<Function>,
    view_fns: &'a HashSet<String>,
    borrowed_shells: &'a BorrowedShellCatalog,
    attributes: Vec<String>,
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

    fn frame_attributes(&mut self) -> Vec<String> {
        let state = *self.state_counter;
        *self.state_counter += 1;
        let mut attributes = self.attributes.clone();
        attributes.push(crate::suspension::frame_state_attribute(state));
        attributes
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
        let cont = Continuation {
            rest,
            rest_lines,
            scope,
            tail,
            line,
        };
        match head {
            Stmt::Let {
                name,
                value,
                mutable,
                ty,
            } => {
                if let Some(inner) = as_await(value) {
                    reject_await(inner, &self.fname)?;
                    // `let x = E.await` — suspend, then continue with `x` bound.
                    let bind = Local {
                        name: name.clone(),
                        ty: ty.clone(),
                        mutable: *mutable,
                        borrowed_view: ty
                            .as_ref()
                            .is_some_and(|ty| self.borrowed_shells.type_is_borrowed(ty)),
                        returns_view: ty
                            .as_ref()
                            .is_some_and(|ty| type_is_view_callable(ty, self.borrowed_shells)),
                    };
                    self.suspend(inner.clone(), Some(bind), cont)
                } else {
                    reject_await(value, &self.fname)?;
                    let mut scope2 = scope.to_vec();
                    scope2.push(Local {
                        name: name.clone(),
                        ty: ty.clone().or_else(|| derive_type(value)),
                        mutable: *mutable,
                        borrowed_view: ty
                            .as_ref()
                            .is_some_and(|ty| self.borrowed_shells.type_is_borrowed(ty))
                            || result_is_borrowed_view(
                                value,
                                scope,
                                self.view_fns,
                                self.borrowed_shells,
                            ),
                        returns_view: ty
                            .as_ref()
                            .is_some_and(|ty| type_is_view_callable(ty, self.borrowed_shells))
                            || callable_returns_view(
                                value,
                                scope,
                                self.view_fns,
                                self.borrowed_shells,
                            ),
                    });
                    let cont2 = Continuation {
                        scope: &scope2,
                        ..cont
                    };
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
                    value: Expr::Unary {
                        op: UnOp::Await,
                        expr: Box::new(inner),
                    },
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
                // Before typeck there is no field table for a pattern. If its
                // source carries a loan relation, conservatively keep that
                // relation on every bound value; the later typed loan gate can
                // become more precise without this lowering accepting a shell.
                let binds_borrowed =
                    result_is_borrowed_view(value, scope, self.view_fns, self.borrowed_shells);
                let mut binds = Vec::new();
                pattern_binds(pattern, &mut binds);
                let mut scope2 = scope.to_vec();
                for b in &binds {
                    scope2.push(Local {
                        name: b.clone(),
                        ty: None,
                        mutable: false,
                        borrowed_view: binds_borrowed,
                        returns_view: false,
                    });
                }
                let cont2 = Continuation {
                    scope: &scope2,
                    ..cont
                };
                Ok(prefix_stmt_at(
                    head.clone(),
                    self.go(cont2.rest, cont2.rest_lines, cont2.scope, cont2.tail)?,
                    line,
                    next_line(rest_lines, line),
                ))
            }
            Stmt::Assign { name, value } => {
                reject_await(value, &self.fname)?;
                // A plain reassignment of an in-scope `var` (a mutable local or an
                // `own` segment/loop parameter). Kept verbatim; if the var is
                // carried across a later await / loop-back it rides a parameter.
                let mut scope2 = scope.to_vec();
                if let Some(local) = scope2.iter_mut().rev().find(|local| local.name == *name) {
                    local.borrowed_view =
                        result_is_borrowed_view(value, scope, self.view_fns, self.borrowed_shells);
                    local.returns_view =
                        callable_returns_view(value, scope, self.view_fns, self.borrowed_shells);
                }
                let cont2 = Continuation {
                    scope: &scope2,
                    ..cont
                };
                Ok(prefix_stmt_at(
                    head.clone(),
                    self.go(cont2.rest, cont2.rest_lines, cont2.scope, cont2.tail)?,
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
            Tail::Loop { seg, carried } => call(
                seg,
                carried.iter().map(|l| Expr::Var(l.name.clone())).collect(),
            ),
        }
    }

    /// A `Stmt::Expr` — the workhorse: bare awaits, loops, tail values, effects.
    fn expr_stmt(&mut self, e: &Expr, cont: Continuation<'_>) -> Result<Expr, String> {
        let is_last = cont.rest.is_empty();
        // A loop whose body (or receiver) drives the executor.
        if let Expr::For { var, iter, body } = e {
            if let Some(src) = as_recv_stream(iter) {
                return self.lower_for_await(var, src, body, cont);
            } else if block_contains_await(body) {
                if let Expr::Range { lo, hi, inclusive } = iter.as_ref() {
                    return self.lower_range_for(var, lo, hi, *inclusive, body, cont);
                }
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
        let cont2 = Continuation {
            scope: &cont_scope,
            ..cont
        };
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
        self.lift_suspend(
            RawTask(loop_future),
            Some(bind),
            cont_block,
            cont.scope,
            cont.line,
        )
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
                Tail::Loop { .. } => Ok(and_then(inner.clone(), self.fresh_tmp(), self.end(tail))),
            };
        }
        match e {
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
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
                        else_block
                            .as_ref()
                            .map_or(first_line(&then_block.lines), |b| first_line(&b.lines)),
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
                Ok(Expr::Match {
                    scrutinee: scrutinee.clone(),
                    arms: new_arms,
                })
            }
            Expr::Block(b) => {
                if b.region.is_some() {
                    return Err(
                        self.err("`region:` in an async tail expression is not yet supported")
                    );
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
    /// `rest`, lift it to a segment, and return `and_then(inner, fn(own bind):
    /// seg(carried…, bind))`. `bind` is the resume value (`None` for a discarded
    /// `await`).
    fn suspend(
        &mut self,
        inner: impl IntoTask,
        bind: Option<Local>,
        cont: Continuation<'_>,
    ) -> Result<Expr, String> {
        // Even a source-level discarded await needs a frame input. If its
        // produced type is must-consume, the generated segment's affine slot
        // must reject dropping it instead of losing the obligation in the
        // shallow continuation closure.
        let bind = bind.or_else(|| {
            Some(Local {
                name: self.fresh_tmp(),
                ty: None,
                mutable: false,
                borrowed_view: false,
                returns_view: false,
            })
        });
        let mut cont_scope = cont.scope.to_vec();
        if let Some(b) = &bind {
            cont_scope.push(b.clone());
        }
        let cont2 = Continuation {
            scope: &cont_scope,
            ..cont
        };
        let cont_expr = self.go(cont2.rest, cont2.rest_lines, cont2.scope, cont2.tail)?;
        self.lift_suspend(
            inner,
            bind,
            cont_expr,
            cont.scope,
            next_line(cont.rest_lines, cont.line),
        )
    }

    /// Lift `cont_expr` (the continuation) to a top-level segment function whose
    /// parameters are the live locals it references (plus the resume `bind`), and
    /// return `and_then(inner, fn(own bind): seg(carried…, bind))`.
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
                "borrowed value `{}` remains live across `await` — borrowed views and \
                 lifetime-bearing shells cannot cross suspension; materialize a direct view \
                 with `.owned()` before building the shell, or keep the value's last use \
                 before suspension",
                view.name,
            )));
        }

        let seg_name = self.fresh_seg();
        let mut params: Vec<Param> = carried.iter().map(local_to_param).collect();
        if let Some(b) = &bind {
            params.push(Param {
                name: b.name.clone(),
                ty: None,
                convention: Convention::Own,
                default: None,
            });
        }
        let attributes = self.frame_attributes();
        self.segments.push(Function {
            line,
            public: false,
            pure: false,
            comptime_only: false,
            attributes,
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
                    convention: Convention::Own,
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
            qualifiers: CallableQualifiers::ORDINARY,
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
        let body_nil = and_then(
            body_future,
            self.fresh_tmp(),
            call("task.ready_unit", vec![]),
        );
        let f = Expr::Lambda {
            params: vec![Param {
                name: var.to_string(),
                ty: None,
                convention: Convention::Let,
                default: None,
            }],
            body: tail_block_at(body_nil, first_line(&body.lines)),
            ret: None,
            qualifiers: CallableQualifiers::ORDINARY,
        };
        Ok(call("task.for_each", vec![list_expr, f]))
    }

    /// An awaited integer range is itself suspension state, not a temporary
    /// `List(Int)`. Evaluate both bounds once, carry one scalar cursor through a
    /// recursive segment, and bind the source loop variable for each iteration.
    /// This is the range analogue of [`Self::lower_while`]: no range materialization,
    /// element boxes, iterator tail copies, or loop-body continuation closure.
    fn lower_range_for(
        &mut self,
        var: &str,
        lo: &Expr,
        hi: &Expr,
        inclusive: bool,
        body: &Block,
        cont: Continuation<'_>,
    ) -> Result<Expr, String> {
        reject_await(lo, &self.fname)?;
        reject_await(hi, &self.fname)?;
        let cursor = self.fresh_tmp();
        let end = self.fresh_tmp();
        let line = cont.line;
        let body_line = first_line(&body.lines).max(line);

        let mut loop_stmts = Vec::with_capacity(body.stmts.len() + 2);
        let mut loop_lines = Vec::with_capacity(body.lines.len() + 2);
        loop_stmts.push(Stmt::Let {
            name: var.to_string(),
            ty: Some(named("Int")),
            mutable: false,
            value: Expr::Var(cursor.clone()),
        });
        loop_lines.push(body_line);
        loop_stmts.extend(body.stmts.iter().cloned());
        loop_lines.extend(body.lines.iter().copied());
        loop_stmts.push(Stmt::Assign {
            name: cursor.clone(),
            value: Expr::Binary {
                op: BinOp::Add,
                lhs: Box::new(Expr::Var(cursor.clone())),
                rhs: Box::new(Expr::Int(1)),
            },
        });
        loop_lines.push(body.lines.last().copied().unwrap_or(body_line));

        let end_value = if inclusive {
            Expr::Binary {
                op: BinOp::Add,
                lhs: Box::new(hi.clone()),
                rhs: Box::new(Expr::Int(1)),
            }
        } else {
            hi.clone()
        };
        let condition = Expr::Binary {
            op: BinOp::Lt,
            lhs: Box::new(Expr::Var(cursor.clone())),
            rhs: Box::new(Expr::Var(end.clone())),
        };
        let loop_expr = Expr::While {
            cond: Box::new(condition),
            body: Block {
                stmts: loop_stmts,
                lines: loop_lines,
                region: None,
            },
        };

        let mut statements = Vec::with_capacity(cont.rest.len() + 3);
        let mut lines = Vec::with_capacity(cont.rest_lines.len() + 3);
        statements.push(Stmt::Let {
            name: cursor,
            ty: Some(named("Int")),
            mutable: true,
            value: lo.clone(),
        });
        lines.push(line);
        statements.push(Stmt::Let {
            name: end,
            ty: Some(named("Int")),
            mutable: false,
            value: end_value,
        });
        lines.push(line);
        statements.push(Stmt::Expr(loop_expr));
        lines.push(line);
        statements.extend(cont.rest.iter().cloned());
        lines.extend(cont.rest_lines.iter().copied());

        self.go(&statements, &lines, cont.scope, cont.tail)
    }

    /// `for await x in rx:` — a receive-until-closed recursive segment loop.
    /// Drains and folds use the same representation; a fold additionally threads
    /// each live mutable accumulator through the segment parameters. Keeping the
    /// non-folding case here avoids falling back to `chan.consume`'s first-class
    /// body closure after the compiler has already assigned suspension states.
    fn lower_for_await(
        &mut self,
        var: &str,
        src: &Expr,
        body: &Block,
        cont: Continuation<'_>,
    ) -> Result<Expr, String> {
        reject_await(src, &self.fname)?;
        let carries_return = block_contains_return(body);
        if carries_return {
            // An early return yields the async function's result, not the loop's
            // accumulator tuple. Run the remaining source continuation on the
            // normal exit so every branch of the loop segment has that result.
            let exit = self.go(cont.rest, cont.rest_lines, cont.scope, cont.tail)?;
            let (entry, _) = self.build_loop_seg(
                LoopHeader::Recv {
                    src: src.clone(),
                    var: var.to_string(),
                },
                body,
                cont.scope,
                false,
                Some(exit),
            )?;
            return Ok(entry);
        }
        // Folding receive loop.
        let want_accs = !(cont.rest.is_empty() && matches!(cont.tail, Tail::Return));
        let (entry, accs) = self.build_loop_seg(
            LoopHeader::Recv {
                src: src.clone(),
                var: var.to_string(),
            },
            body,
            cont.scope,
            want_accs,
            None,
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
        if block_contains_return(body) {
            // See `lower_for_await`: normal exit and early return must share the
            // enclosing async function's result type.
            let exit = self.go(cont.rest, cont.rest_lines, cont.scope, cont.tail)?;
            let (entry, _) = self.build_loop_seg(
                LoopHeader::While { cond: cond.clone() },
                body,
                cont.scope,
                false,
                Some(exit),
            )?;
            return Ok(entry);
        }
        let want_accs = !(cont.rest.is_empty() && matches!(cont.tail, Tail::Return));
        let (entry, accs) = self.build_loop_seg(
            LoopHeader::While { cond: cond.clone() },
            body,
            cont.scope,
            want_accs,
            None,
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
        continuation: Option<Expr>,
    ) -> Result<(Expr, Vec<Local>), String> {
        // Live locals of the header + body, in scope order.
        let mut probe = match &header {
            LoopHeader::While { cond } => crate::suspension::free_bindings(cond),
            LoopHeader::Recv { src, .. } => crate::suspension::free_bindings(src),
        };
        let mut body_bound = HashSet::new();
        if let LoopHeader::Recv { var, .. } = &header {
            body_bound.insert(var.clone());
        }
        probe.extend(crate::suspension::free_bindings_in_block(body, &body_bound));
        if let Some(continuation) = &continuation {
            probe.extend(crate::suspension::free_bindings(continuation));
        }
        let probe: HashSet<String> = probe.into_iter().collect();
        let carried: Vec<Local> = scope
            .iter()
            .filter(|l| probe.contains(&l.name))
            .cloned()
            .collect();

        let accs: Vec<Local> = if want_accs {
            carried.iter().filter(|l| l.mutable).cloned().collect()
        } else {
            vec![]
        };

        let seg_name = self.fresh_seg();
        let loop_tail = Tail::Loop {
            seg: seg_name.clone(),
            carried: carried.clone(),
        };
        let exit = continuation.unwrap_or_else(|| self.loop_exit(&accs));

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
                        pattern: Pattern::Ctor {
                            name: "None".to_string(),
                            args: vec![],
                        },
                        guard: None,
                        body: exit,
                    },
                ];
                let recv_match = Expr::Match {
                    scrutinee: Box::new(Expr::Var(o.clone())),
                    arms: recv_arms,
                };
                // recv-segment: fn recv(carried…, o) = match o { Some(x)->body; None->exit }
                let mut recv_params: Vec<Param> = carried.iter().map(local_to_param).collect();
                recv_params.push(Param {
                    name: o.clone(),
                    ty: None,
                    convention: Convention::Own,
                    default: None,
                });
                let attributes = self.frame_attributes();
                self.segments.push(Function {
                    line: first_line(&body.lines),
                    public: false,
                    pure: false,
                    comptime_only: false,
                    attributes,
                    name: recv_name.clone(),
                    params: recv_params,
                    ret: None,
                    body: tail_block_at(recv_match, first_line(&body.lines)),
                    bounds: vec![],
                    is_gen: false,
                    is_async: false,
                });
                // loop-segment: and_then(chan.recv(src), fn(own o): recv(carried…, o))
                let mut recv_args: Vec<Expr> =
                    carried.iter().map(|l| Expr::Var(l.name.clone())).collect();
                recv_args.push(Expr::Var(o.clone()));
                let recv_lambda = Expr::Lambda {
                    params: vec![Param {
                        name: o,
                        ty: None,
                        convention: Convention::Own,
                        default: None,
                    }],
                    body: tail_block_at(call(&recv_name, recv_args), first_line(&body.lines)),
                    ret: None,
                    qualifiers: CallableQualifiers::ORDINARY,
                };
                call(
                    "task.and_then",
                    vec![call("chan.recv", vec![src.clone()]), recv_lambda],
                )
            }
        };

        let attributes = self.frame_attributes();
        self.segments.push(Function {
            line: first_line(&body.lines),
            public: false,
            pure: false,
            comptime_only: false,
            attributes,
            name: seg_name.clone(),
            params,
            ret: None,
            body: tail_block_at(loop_body, first_line(&body.lines)),
            bounds: vec![],
            is_gen: false,
            is_async: false,
        });

        let entry = call(
            &seg_name,
            carried.iter().map(|l| Expr::Var(l.name.clone())).collect(),
        );
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
    state_counter: &mut usize,
    view_fns: &HashSet<String>,
    borrowed_shells: &BorrowedShellCatalog,
) -> Result<(Function, Vec<Function>), String> {
    lower_async_fn_with(
        f,
        is_entry,
        counter,
        state_counter,
        None,
        view_fns,
        borrowed_shells,
    )
}

fn resolved_async_parameter_type<'a>(
    parameter: &'a Param,
    self_ty: Option<&'a Type>,
) -> Option<&'a Type> {
    parameter.ty.as_ref().or_else(|| {
        if parameter.name == "self" {
            self_ty
        } else {
            None
        }
    })
}

/// As [`lower_async_fn`], with an optional receiver type for a method's `self`
/// (so a carried `self` keeps its type when it becomes a segment parameter).
fn lower_async_fn_with(
    f: Function,
    is_entry: bool,
    counter: &mut usize,
    state_counter: &mut usize,
    self_ty: Option<Type>,
    view_fns: &HashSet<String>,
    borrowed_shells: &BorrowedShellCatalog,
) -> Result<(Function, Vec<Function>), String> {
    let declared_ret = f.ret.clone();
    if f.params
        .iter()
        .filter_map(|param| resolved_async_parameter_type(param, self_ty.as_ref()))
        .any(|ty| borrowed_shells.type_is_borrowed(ty))
        || declared_ret
            .as_ref()
            .is_some_and(|ty| borrowed_shells.type_is_borrowed(ty))
    {
        return Err(format!(
            "async fn `{}` may not expose a borrowed view or lifetime-bearing shell as a \
             parameter or result because its task \
             can outlive the caller's loan — pass/return an owned value",
            f.name,
        ));
    }
    if is_entry && declared_ret.as_ref().is_some_and(|ret| {
        !matches!(ret.unqualified(), Type::Named(name, args) if name == "Nil" && args.is_empty())
            && !matches!(ret.unqualified(), Type::Tuple(ts) if ts.is_empty())
    }) {
        let ret = crate::format::type_str(declared_ret.as_ref().unwrap());
        return Err(format!(
            "async fn `main` returns `{ret}`, but the async executor drives `Task(())` and \
             cannot surface a completed value; handle the value inside `main` and omit the return type"
        ));
    }

    let entry_state = *state_counter;
    *state_counter += 1;
    let mut segment_attributes = f.attributes.clone();
    segment_attributes.push(crate::suspension::FRAME_FUNCTION_ATTRIBUTE.to_string());
    let source_callable = self_ty
        .as_ref()
        .and_then(|ty| match ty.unqualified() {
            Type::Named(name, _) => Some(format!("{name}.{}", f.name)),
            _ => None,
        })
        .unwrap_or_else(|| f.name.clone());
    segment_attributes.push(crate::suspension::source_callable_attribute(
        &source_callable,
    ));
    let mut ctx = Ctx {
        fname: f.name.clone(),
        counter,
        state_counter,
        segments: Vec::new(),
        view_fns,
        borrowed_shells,
        attributes: segment_attributes,
    };
    let scope: Vec<Local> = f
        .params
        .iter()
        .map(|p| {
            let ty = resolved_async_parameter_type(p, self_ty.as_ref()).cloned();
            Local {
                name: p.name.clone(),
                borrowed_view: ty
                    .as_ref()
                    .is_some_and(|ty| borrowed_shells.type_is_borrowed(ty)),
                returns_view: ty
                    .as_ref()
                    .is_some_and(|ty| type_is_view_callable(ty, borrowed_shells)),
                ty,
                mutable: p.convention.binds_mutable(),
            }
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
            qualifiers: CallableQualifiers::ORDINARY,
        }],
    );
    let entry_body = if is_entry {
        // The runtime calls `main` directly and cannot drive a task, so an async
        // `main` IS the executor's entry point: run its body to completion.
        call("task.run", vec![lazy_body])
    } else {
        lazy_body
    };
    let mut entry_attributes = f.attributes;
    entry_attributes.push(crate::suspension::FRAME_ENTRY_ATTRIBUTE.to_string());
    entry_attributes.push(crate::suspension::frame_state_attribute(entry_state));
    let entry = Function {
        line: f.line,
        public: f.public,
        pure: false,
        comptime_only: false,
        attributes: entry_attributes,
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
        convention: Convention::Own,
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
    let free = crate::suspension::free_bindings(expr)
        .into_iter()
        .collect::<HashSet<_>>();
    let mut shadowed = HashSet::new();
    let mut live = scope
        .iter()
        .rev()
        .filter(|l| Some(l.name.as_str()) != skip && free.contains(&l.name))
        .filter(|l| shadowed.insert(l.name.clone()))
        .cloned()
        .collect::<Vec<_>>();
    live.reverse();
    live
}

// ---- small AST helpers ----

/// Turn a function name into a valid identifier fragment for a segment name.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
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
                Expr::Binary {
                    op: BinOp::Add,
                    lhs: hi.clone(),
                    rhs: Box::new(Expr::Int(1)),
                }
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
        Expr::Unary {
            op: UnOp::Await,
            expr,
        } => Some(expr),
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
        Expr::Unary {
            op: UnOp::Await, ..
        } => true,
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => false,
        Expr::Unary { expr, .. }
        | Expr::Field { base: expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => contains_await(expr),
        Expr::ExistentialCall { receiver, args, .. } => {
            contains_await(receiver) || args.iter().any(contains_await)
        }
        Expr::Index { base, index } => contains_await(base) || contains_await(index),
        Expr::Binary { lhs, rhs, .. } => contains_await(lhs) || contains_await(rhs),
        Expr::Range { lo, hi, .. } => contains_await(lo) || contains_await(hi),
        Expr::List(xs) | Expr::Tuple(xs) => xs.iter().any(contains_await),
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            args.iter().any(contains_await)
        }
        Expr::LabeledCall { args, .. } => args.iter().any(|(_, a)| contains_await(a)),
        Expr::LabeledMethodCall { receiver, args, .. } => {
            contains_await(receiver) || args.iter().any(|(_, a)| contains_await(a))
        }
        Expr::MethodCall { receiver, args, .. } => {
            contains_await(receiver) || args.iter().any(contains_await)
        }
        Expr::Apply { func, args } => contains_await(func) || args.iter().any(contains_await),
        Expr::RecordUpdate {
            name: _,
            base,
            fields,
        } => contains_await(base) || fields.iter().any(|(_, v)| contains_await(v)),
        Expr::Record { fields, spread, .. } => {
            fields.iter().any(|(_, v)| contains_await(v))
                || spread.as_ref().is_some_and(|s| contains_await(s))
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
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
        Expr::WhileLet {
            scrutinee, body, ..
        } => contains_await(scrutinee) || block_contains_await(body),
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

fn block_contains_return(block: &Block) -> bool {
    block.stmts.iter().any(stmt_contains_return)
}

fn stmt_contains_return(statement: &Stmt) -> bool {
    match statement {
        Stmt::Return(_) => true,
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Expr(value)
        | Stmt::Yield(value) => expr_contains_return(value),
        Stmt::Break | Stmt::Continue => false,
    }
}

fn expr_contains_return(expression: &Expr) -> bool {
    match expression {
        Expr::If {
            then_block,
            else_block,
            ..
        } => {
            block_contains_return(then_block)
                || else_block.as_ref().is_some_and(block_contains_return)
        }
        Expr::Match { arms, .. } => arms.iter().any(|arm| expr_contains_return(&arm.body)),
        Expr::Block(block)
        | Expr::While { body: block, .. }
        | Expr::For { body: block, .. }
        | Expr::WhileLet { body: block, .. } => block_contains_return(block),
        // A return in a nested callable belongs to that callable, not the async
        // function whose loop is being lowered.
        Expr::Lambda { .. } => false,
        _ => false,
    }
}

/// `task.and_then(inner, fn(own bind): k)`.
fn and_then(inner: Expr, bind: String, k: Expr) -> Expr {
    let lambda = Expr::Lambda {
        params: vec![Param {
            name: bind,
            ty: None,
            convention: Convention::Own,
            default: None,
        }],
        body: tail_block(k),
        ret: None,
        qualifiers: CallableQualifiers::ORDINARY,
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
    Expr::Call {
        name: name.to_string(),
        args,
    }
}

/// A single-expression block (the body shape for a function/branch whose value is
/// exactly `e`).
fn tail_block(e: Expr) -> Block {
    tail_block_at(e, 0)
}

fn tail_block_at(e: Expr, line: u32) -> Block {
    Block {
        stmts: vec![Stmt::Expr(e)],
        lines: vec![line],
        region: None,
    }
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
        let checked = crate::source_check::check(module).map_err(|error| error.to_string())?;
        let checked = crate::generators::lower(checked)?;
        lower(checked).map(AsyncLoweredModule::into_module)
    }

    fn int_type() -> Type {
        Type::Named("Int".to_string(), Vec::new())
    }

    #[test]
    fn suspension_slots_keep_only_the_innermost_shadowed_binding() {
        let local = |name: &str, mutable| Local {
            name: name.to_string(),
            ty: Some(int_type()),
            mutable,
            borrowed_view: false,
            returns_view: false,
        };
        let expression = Expr::Binary {
            op: BinOp::Add,
            lhs: Box::new(Expr::Var("sum".into())),
            rhs: Box::new(Expr::Var("n".into())),
        };
        let slots = live_locals(
            &expression,
            &[local("sum", false), local("n", false), local("sum", true)],
            None,
        );

        assert_eq!(
            slots
                .iter()
                .map(|slot| (slot.name.as_str(), slot.mutable))
                .collect::<Vec<_>>(),
            [("n", false), ("sum", true)],
        );
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
    fn lowering_maps_lifted_segments_back_to_their_source_item() {
        let source = "fn before() -> Int:\n    0\n\nasync fn value() -> Int:\n    let x = task.done(1).await\n    x\n\nfn after() -> Int:\n    2\n";
        let module = crate::parser::parse_module(source).expect("parse async mapping fixture");
        let checked = crate::source_check::check(module).expect("source check");
        let checked = crate::generators::lower(checked).expect("generator lowering");
        let (lowered, mapping) =
            lower_with_item_mapping(checked).expect("async lowering with item mapping");

        assert_eq!(mapping[0], vec![0]);
        assert_eq!(mapping[2], vec![2]);
        assert_eq!(mapping[1][0], 1);
        assert!(
            mapping[1].len() > 1,
            "an await must lift at least one segment"
        );
        let lowered_item_count = lowered.into_module().items.len();
        assert!(mapping[1][1..]
            .iter()
            .all(|index| *index >= 3 && *index < lowered_item_count));
    }

    #[test]
    fn lowering_assigns_dense_stable_carrier_states_to_entries_and_segments() {
        let source = "async fn first() -> Int:\n    let x = task.done(1).await\n    x\n\nasync fn second() -> Int:\n    let y = task.done(2).await\n    y\n";
        let module = crate::parser::parse_module(source).expect("parse carrier-state fixture");
        let lowered = lower_module(module).expect("lower carrier-state fixture");
        let mut states = lowered
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(function) => {
                    crate::suspension::frame_state(function).map(|state| (state, function))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        states.sort_by_key(|(state, _)| *state);

        assert_eq!(
            states.iter().map(|(state, _)| *state).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        assert!(states[0]
            .1
            .attributes
            .iter()
            .any(|attribute| attribute == crate::suspension::FRAME_ENTRY_ATTRIBUTE));
        assert!(states[1]
            .1
            .attributes
            .iter()
            .any(|attribute| attribute == crate::suspension::FRAME_FUNCTION_ATTRIBUTE));
        assert!(states[2]
            .1
            .attributes
            .iter()
            .any(|attribute| attribute == crate::suspension::FRAME_ENTRY_ATTRIBUTE));
        assert!(states[3]
            .1
            .attributes
            .iter()
            .any(|attribute| attribute == crate::suspension::FRAME_FUNCTION_ATTRIBUTE));
    }

    #[test]
    fn lowering_carries_a_local_callable_across_await() {
        let source = "async fn run() -> Int:\n    let stepper = fn(x: Int): x + 1\n    let _ = task.done(0).await\n    stepper(41)\n";
        let module = crate::parser::parse_module(source).expect("parse callable-frame fixture");
        let lowered = lower_module(module).expect("lower callable-frame fixture");
        let segment = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name.starts_with("__async_run_") => {
                    Some(function)
                }
                _ => None,
            })
            .expect("await produces a continuation segment");
        assert!(
            segment
                .params
                .iter()
                .any(|parameter| parameter.name == "stepper"),
            "the frame must carry the local callable used after suspension: {:?}",
            segment.params,
        );
    }

    #[test]
    fn lowering_rejects_value_returning_async_main() {
        let source = "async fn main(console: Console) -> Int:\n    1\n";
        let module = crate::parser::parse_module(source).expect("parse async main");
        let error =
            lower_module(module).expect_err("the executor cannot surface an async root value");

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
        assert!(
            error.contains("borrowed value `w` remains live across `await`"),
            "{error}"
        );
        assert!(
            error.contains("materialize a direct view with `.owned()`"),
            "{error}"
        );
    }

    #[test]
    fn lowering_preserves_a_view_relation_through_a_function_value() {
        let source = "mode opt\n\nfn view(text: let('a) String) -> View(String, 'a):\n    text\n\nasync fn bad(console: Console):\n    let text = \"x\"\n    let make_view = view\n    let w = make_view(text)\n    let _ = task.done(0).await\n    console.print(w)\n";
        let module =
            crate::parser::parse_module(source).expect("parse indirect borrowed async body");
        let error = lower_module(module).expect_err("an indirect view cannot cross a segment");
        assert!(
            error.contains("borrowed value `w` remains live across `await`"),
            "{error}"
        );
    }

    #[test]
    fn lowering_preserves_a_view_relation_through_a_returned_function_value() {
        let source = "mode opt\n\nfn view(text: let('a) String) -> View(String, 'a):\n    text\n\nfn make() -> fn(View(String, 'a)) -> View(String, 'a):\n    view\n\nasync fn bad(console: Console):\n    let text = \"x\"\n    let make_view = make()\n    let w = make_view(text)\n    let _ = task.done(0).await\n    console.print(w)\n";
        let module = crate::parser::parse_module(source).expect("parse returned callable");
        let error =
            lower_module(module).expect_err("a returned callable's view cannot cross a segment");
        assert!(
            error.contains("borrowed value `w` remains live across `await`"),
            "{error}"
        );
    }

    #[test]
    fn lowering_tracks_a_view_returning_method_across_await() {
        let source = "mode opt\n\ntype Holder:\n    text: String\n\nimpl Holder:\n    fn view(self: let('a) Holder) -> View(String, 'a):\n        self.text\n\nasync fn bad(console: Console):\n    let holder = Holder(\"x\")\n    let w = holder.view()\n    let _ = task.done(0).await\n    console.print(w)\n";
        let module = crate::parser::parse_module(source).expect("parse method borrowed async body");
        let error =
            lower_module(module).expect_err("a method-returned view cannot cross a segment");
        assert!(
            error.contains("borrowed value `w` remains live across `await`"),
            "{error}"
        );
    }

    #[test]
    fn lowering_rejects_lifetime_bearing_nominal_async_signatures() {
        let source = "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nasync fn bad(input: (Holder('a), Int)):\n    task.done(0).await\n";
        let module = crate::parser::parse_module(source).expect("parse borrowed nominal signature");
        let error = lower_module(module)
            .expect_err("a nested lifetime-bearing shell cannot enter an async task");

        assert!(error.contains("lifetime-bearing shell"), "{error}");
        assert!(error.contains("parameter or result"), "{error}");
    }

    #[test]
    fn lowering_rejects_exclusive_reference_async_signatures() {
        let source =
            "mode opt\n\nasync fn bad(input: &'a mut String) -> Nil:\n    task.done(0).await\n";
        let module = crate::parser::parse_module(source).expect("parse exclusive async signature");
        let error = lower_module(module)
            .expect_err("an exclusive reference cannot cross an async boundary");

        assert!(error.contains("async fn `bad`"), "{error}");
        assert!(error.contains("parameter or result"), "{error}");
    }

    #[test]
    fn source_validation_rejects_a_lifetime_bearing_async_method_receiver() {
        let source = "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nimpl Holder('a):\n    async fn bad(self):\n        task.done(0).await\n";
        let module = crate::parser::parse_module(source)
            .expect("parse lifetime-bearing async method receiver");
        let error = crate::source_check::check(module)
            .expect_err("source validation must reject an implicit borrowed-shell receiver")
            .to_string();

        assert!(error.contains("async fn `bad`"), "{error}");
        assert!(error.contains("lifetime-bearing shell"), "{error}");
        assert!(error.contains("parameter or result"), "{error}");
    }

    #[test]
    fn lowering_rejects_an_unannotated_borrowed_nominal_live_across_await() {
        let source = "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nasync fn bad():\n    let text = \"x\"\n    let holder = Holder(text)\n    let _ = task.done(0).await\n    let keep = holder\n";
        let module = crate::parser::parse_module(source).expect("parse borrowed nominal local");
        let error = lower_module(module)
            .expect_err("a lifetime-bearing constructor result cannot cross suspension");

        assert!(
            error.contains("borrowed value `holder` remains live across `await`"),
            "{error}"
        );
        assert!(error.contains("before building the shell"), "{error}");
    }

    #[test]
    fn lowering_rejects_a_nested_borrowed_nominal_live_across_await() {
        let source = "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nasync fn bad():\n    let text = \"x\"\n    let holder = Holder(text)\n    let nested = (0, holder)\n    let _ = task.done(0).await\n    let keep = nested\n";
        let module = crate::parser::parse_module(source).expect("parse nested borrowed nominal");
        let error = lower_module(module)
            .expect_err("an aggregate containing a borrowed shell cannot cross suspension");

        assert!(
            error.contains("borrowed value `nested` remains live across `await`"),
            "{error}"
        );
    }

    #[test]
    fn lowering_allows_an_ordinary_generic_owner_across_await() {
        let source = "type Box(a):\n    Box(a)\n\nasync fn okay():\n    let boxed = Box(\"x\")\n    let _ = task.done(0).await\n    let keep = boxed\n";
        let module = crate::parser::parse_module(source).expect("parse ordinary generic local");
        lower_module(module).expect("an ordinary generic owner has no borrowed lifetime relation");
    }

    #[test]
    fn lowering_allows_a_view_whose_last_use_precedes_await() {
        let source = "mode opt\n\nfn view(text: let('a) String) -> View(String, 'a):\n    text\n\nasync fn okay(console: Console):\n    let text = \"x\"\n    let w = view(text)\n    console.print(w)\n    task.done(0).await\n";
        let module = crate::parser::parse_module(source).expect("parse borrowed async body");
        lower_module(module).expect("a dead view is not carried across suspension");
    }

    #[test]
    fn awaited_integer_range_is_a_scalar_recursive_segment() {
        let source = "async fn run(n: Int):\n    for i in 0..n:\n        task.done(i).await\n";
        let module = crate::parser::parse_module(source).expect("parse awaited range");
        let lowered = lower_module(module).expect("lower awaited range");
        let rendered = crate::format::module(&lowered, &[]);

        assert!(!rendered.contains("list.range_between"), "{rendered}");
        assert!(!rendered.contains("task.for_each"), "{rendered}");
        assert!(
            lowered
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Function(function) => crate::suspension::frame_state(function),
                    _ => None,
                })
                .count()
                >= 3,
            "entry, range loop, and await continuation must all be named frame states: {rendered}",
        );
    }

    #[test]
    fn nonfolding_receive_loop_is_a_named_segment_not_consume() {
        let source = "from chan import Receiver\n\nasync fn drain(rx: Receiver(Int)):\n    for await value in rx:\n        task.done(value).await\n";
        let module = crate::parser::parse_module(source).expect("parse receive drain");
        let lowered = lower_module(module).expect("lower receive drain");
        let rendered = crate::format::module(&lowered, &[]);

        assert!(!rendered.contains("chan.consume"), "{rendered}");
        assert!(rendered.contains("chan.recv"), "{rendered}");
    }

    #[test]
    fn lowering_preserves_target_availability_on_async_segments() {
        let source = "@browser\nasync fn browser_value() -> Int:\n    let value = task.done(1).await\n    value\n";
        let module = crate::parser::parse_module(source).expect("parse targeted async function");
        let lowered = lower_module(module).expect("lower targeted async function");
        let functions: Vec<&Function> = lowered
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(function)
                    if function.name == "browser_value"
                        || function.name.starts_with("__async_browser_value_") =>
                {
                    Some(function)
                }
                _ => None,
            })
            .collect();
        assert!(
            functions.len() >= 2,
            "an await must produce a lifted segment"
        );
        assert!(functions.iter().all(|function| function
            .attributes
            .iter()
            .any(|attribute| attribute == "browser")));
        assert!(functions.iter().all(|function| {
            function.name == "browser_value"
                || function
                    .attributes
                    .iter()
                    .any(|attribute| attribute == crate::suspension::FRAME_FUNCTION_ATTRIBUTE)
        }));
    }
}
