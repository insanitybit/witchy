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
//! type or value: a call through a value (`Apply`) remains positional-only.
//! UFCS method labels are represented as `LabeledMethodCall` and rewritten after
//! the concrete method declaration is resolved.

use crate::ast::*;
// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};

type Params = HashMap<String, Vec<Param>>;

/// Rewrite every labeled/defaulted direct call in the merged module.
pub fn resolve(module: &mut Module) -> Result<(), String> {
    let mut params: Params = HashMap::new();
    let mut types: HashSet<String> = HashSet::new();
    for item in &module.items {
        match item {
            Item::Type(definition) => {
                types.insert(definition.name.clone());
            }
            Item::Function(f) => {
                params.insert(f.name.clone(), f.params.clone());
            }
            Item::Impl(im) => {
                for method in &im.methods {
                    params.insert(format!("{}.{}", im.type_name, method.name.clone()), method.params.clone());
                }
            }
            Item::Trait(t) => {
                for method in &t.methods {
                    params.insert(format!("{}.{}", t.name, method.name.clone()), method.params.clone());
                }
            }
            Item::TypeAlias { .. } | Item::Comptime(_) | Item::Const { .. } => {}
        }
    }
    let mut r = Resolver { params, types, counter: 0, locals: HashMap::new() };
    for item in &mut module.items {
        match item {
            Item::Function(f) => r.block_function_like(&f.params, &mut f.body)?,
            Item::Impl(im) => {
                for meth in &mut im.methods {
                    r.block_function_like(&meth.params, &mut meth.body)?;
                }
            }
            Item::Trait(t) => {
                for ms in &mut t.methods {
                    if let Some(body) = &mut ms.default {
                        r.block_function_like(&ms.params, body)?;
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
    types: HashSet<String>,
    counter: u32,
    locals: HashMap<String, String>,
}

fn nominal_type_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Named(name, _) => Some(name.as_str()),
        _ => None,
    }
}

impl Resolver {
    fn block(&mut self, b: &mut Block) -> Result<(), String> {
        let mut bound: Vec<(String, Option<String>)> = Vec::new();
        for stmt in &mut b.stmts {
            match stmt {
                Stmt::Let { name, value, .. } => {
                    self.expr(value)?;
                    let ty = self.expr_nominal_type(value);
                    self.bind_local(name.clone(), ty, &mut bound);
                }
                Stmt::Assign { value, .. } => {
                    self.expr(value)?;
                }
                Stmt::LetPattern { value, .. }
                | Stmt::Expr(value)
                | Stmt::Yield(value)
                | Stmt::Return(Some(value)) => self.expr(value)?,
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        for (name, old) in bound.drain(..).rev() {
            match old {
                Some(ty) => self.locals.insert(name, ty),
                None => self.locals.remove(&name),
            };
        }
        Ok(())
    }

    fn block_function_like(&mut self, params: &[Param], body: &mut Block) -> Result<(), String> {
        let saved = std::mem::take(&mut self.locals);
        for p in params {
            if let Some(ty) = p.ty.as_ref().and_then(nominal_type_name) {
                self.locals.insert(p.name.clone(), ty.to_string());
            }
        }
        let out = self.block(body);
        self.locals = saved;
        out
    }

    fn bind_local(&mut self, name: String, ty: Option<String>, scope: &mut Vec<(String, Option<String>)>) {
        let old = if let Some(ty) = ty {
            self.locals.insert(name.clone(), ty)
        } else {
            self.locals.remove(&name)
        };
        scope.push((name, old));
    }

    fn expr_nominal_type(&self, e: &Expr) -> Option<String> {
        match e {
            Expr::Var(name) => self.locals.get(name).cloned(),
            Expr::Call { name, .. } => self
                .types
                .get(name)
                .map(std::string::ToString::to_string),
            Expr::List(_) => Some("List".to_string()),
            Expr::Int(_) => Some("Int".to_string()),
            Expr::Float(_) => Some("Float".to_string()),
            Expr::Duration(_) => Some("Duration".to_string()),
            Expr::Str(_) => Some("String".to_string()),
            Expr::Bool(_) => Some("Bool".to_string()),
            Expr::Ctor { name, .. } => Some(name.clone()),
            Expr::Field { base, .. } => self.expr_nominal_type(base),
            Expr::TaggedLit { tag, .. } => self
                .types
                .contains(tag)
                .then_some(tag.to_string()),
            _ => None,
        }
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
                *e = self.rewrite_labeled(name, args, false)?;
            }
            Expr::LabeledMethodCall { receiver, method, args } => {
                self.expr(receiver)?;
                let receiver_type = self.expr_nominal_type(receiver);
                let receiver = std::mem::replace(receiver, Box::new(Expr::Var(String::new())));
                let method = std::mem::take(method);
                let args = std::mem::take(args);
                let callee = self.resolve_method_for_labels(&method, receiver_type.as_deref())?;
                match self.rewrite_labeled(callee, args, true)? {
                    Expr::Call { args, .. } => {
                        *e = Expr::MethodCall {
                            receiver,
                            method,
                            args,
                        };
                    }
                    Expr::Block(mut block) => {
                        let Some(Stmt::Expr(expr)) = block.stmts.last_mut() else {
                            return Err(
                                "internal error: labeled method rewrite produced an empty block"
                                    .to_string(),
                            );
                        };
                        let Expr::Call { args, .. } = std::mem::replace(
                            expr,
                            Expr::Var(String::new()),
                        ) else {
                            return Err(
                                "internal error: labeled method rewrite did not end in a call"
                                    .to_string(),
                            );
                        };
                        *expr = Expr::MethodCall {
                            receiver,
                            method,
                            args,
                        };
                        *e = Expr::Block(block);
                    }
                    other => {
                        return Err(format!(
                            "internal error: rewrite_labeled unexpectedly returned {other:?} for method call"
                        ));
                    }
                }
            }
            Expr::Call { name, args } => {
                for a in args.iter_mut() {
                    self.expr(a)?;
                }
                self.splice_defaults(name, args);
            }
            Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
            | Expr::Var(_) | Expr::TaggedLit { .. } => {}
            Expr::List(xs) | Expr::Tuple(xs) | Expr::Ctor { args: xs, .. }
            | Expr::AnonCtor { args: xs, .. } => {
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
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. }
            | Expr::Field { base: expr, .. } => self.expr(expr)?,
            Expr::ExistentialCall { receiver, args, .. } => {
                self.expr(receiver)?;
                for arg in args { self.expr(arg)?; }
            }
            Expr::Index { base, index } => {
                self.expr(base)?;
                self.expr(index)?;
            }
            Expr::Range { lo, hi, .. } => {
                self.expr(lo)?;
                self.expr(hi)?;
            }
            Expr::RecordUpdate { name: _, base, fields } => {
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

    fn resolve_method_for_labels(&self, method: &str, receiver: Option<&str>) -> Result<String, String> {
        let mut candidates: Vec<String> = self
            .params
            .keys()
            .filter_map(|name| {
                let (_, base) = name.rsplit_once('.')?;
                if base == method {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect();
        if let Some(receiver_type) = receiver {
            // Prefer a method whose owner is exactly the receiver type
            // (`String.substring`) over a module free function that merely takes
            // the receiver as its first parameter (`string.substring(s: String,
            // ...)`). The linker aliases module functions into the method
            // namespace, so both can match a receiver; the nominal owner is the
            // one the user wrote `value.method(...)` against.
            let exact_owner: Vec<String> = candidates
                .iter()
                .filter(|name| {
                    name.rsplit_once('.').is_some_and(|(owner, _)| owner == receiver_type)
                })
                .cloned()
                .collect();
            match exact_owner.as_slice() {
                [only] => return Ok(only.clone()),
                [_, ..] => {
                    let mut sorted = exact_owner;
                    sorted.sort();
                    return Ok(sorted.remove(0));
                }
                [] => {}
            }
            let by_receiver: Vec<String> = candidates
                .iter()
                .filter(|name| {
                    let Some(params) = self.params.get(*name) else {
                        return false;
                    };
                    let Some(first) = params.first().and_then(|p| p.ty.as_ref()) else {
                        return false;
                    };
                    nominal_type_name(first) == Some(receiver_type)
                })
                .cloned()
                .collect();
            match by_receiver.as_slice() {
                [only] => return Ok(only.clone()),
                [] => {}
                _ => return Err(format!(
                    "`{method}` is ambiguous for keyword arguments; resolve it to a single method first"
                )),
            }
        }
        candidates.sort();
        match candidates.as_slice() {
            [only] => Ok(only.clone()),
            [] => Err(format!(
                "labels need the callee's declaration — `{method}` is not a function \
                 (a builtin or a value has no parameter names to label)"
            )),
            _ => Err(format!(
                "`{method}` is ambiguous for keyword arguments; resolve it to a single method first"
            )),
        }
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
        is_method: bool,
    ) -> Result<Expr, String> {
        let Some(stored) = self.params.get(&name) else {
            return Err(format!(
                "labels need the callee's declaration — `{name}` is not a direct function \
                 (a builtin or a value has no parameter names to label)"
            ));
        };
        let mut params = stored.clone();
        if is_method && params.first().is_some_and(|p| p.name == "self") {
            params.remove(0);
        }
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
