//! Lower `gen fn` / `yield` into ordinary functions over `std/iter`.
//!
//! A generator
//! ```text
//! gen fn count_up(n: Int) -> Iter(Int):
//!     var i = n
//!     while true:
//!         yield i
//!         i = i + 1
//! ```
//! becomes two plain functions: a helper that, given which yield is wanted,
//! re-runs the body counting yields and returns that one, and a wrapper that
//! turns the helper into a lazy `iter.Iter`:
//! ```text
//! fn __gen_count_up(n: Int, __target: Int) -> Option(Int):
//!     var __i = 0
//!     var i = n
//!     while true:
//!         if __i == __target:
//!             return Some(i)
//!         __i = __i + 1
//!         i = i + 1
//!     None
//! fn count_up(n: Int) -> Iter(Int):
//!     iter.from_gen(fn(__t: Int): __gen_count_up(n, __t))
//! ```
//! Pulling element `k` re-runs the body to the `k`-th yield, so the body may use
//! any control flow (including unbounded loops and mutable state across yields).
//! Generators should be capability-free, since re-running repeats side effects.

use crate::ast::*;

const COUNTER: &str = "__i";
const TARGET: &str = "__target";

/// Rewrite every `gen fn` in the module into a helper + wrapper, and ensure the
/// `iter`/`option` modules it relies on are imported. A no-op for modules
/// without generators.
pub fn lower(mut module: Module) -> Result<Module, String> {
    if !has_generator(&module) {
        return Ok(module);
    }
    let mut items = Vec::with_capacity(module.items.len() + 1);
    for item in module.items {
        match item {
            Item::Function(f) if f.is_gen => {
                let (helper, wrapper) = lower_gen(f, None)?;
                items.push(Item::Function(helper));
                items.push(Item::Function(wrapper));
            }
            // A `gen fn` METHOD in an inherent `impl Type:` block lowers exactly
            // like a top-level one, but its wrapper STAYS a method (so
            // `value.method()` still resolves by receiver type and returns
            // `Iter(a)`); only the helper is hoisted to a top-level function, under
            // a type-qualified name so two types' identically-named generators can't
            // collide. Trait-impl `gen` methods are rejected at parse time, so every
            // impl reaching here is inherent. The enumeration is kept separate from
            // `lower_gen`'s body transform, so a later rewrite of the transform
            // (RFC-0059) leaves this traversal untouched.
            Item::Impl(mut im) if im.methods.iter().any(|m| m.is_gen) => {
                let ctx = MethodCtx {
                    self_ty: impl_self_ty(&im),
                    bounds: im.bounds.clone(),
                    type_name: im.type_name.clone(),
                };
                let mut methods = Vec::with_capacity(im.methods.len());
                for method in std::mem::take(&mut im.methods) {
                    if method.is_gen {
                        let (helper, wrapper) = lower_gen(method, Some(&ctx))?;
                        items.push(Item::Function(helper));
                        methods.push(wrapper);
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
    module.items = items;
    for dep in ["iter", "option"] {
        if !module.imports.iter().any(|m| m == dep) {
            module.imports.push(dep.to_string());
        }
    }
    // `import_lines` is parallel to `imports`; keep them the same length so the
    // formatter's comment placement stays consistent (0 = unknown line).
    while module.import_lines.len() < module.imports.len() {
        module.import_lines.push(0);
    }
    Ok(module)
}

/// Whether the module contains any `gen fn` — top level or as a method in an
/// `impl` block (both are lowered by [`lower`]).
fn has_generator(module: &Module) -> bool {
    module.items.iter().any(|item| match item {
        Item::Function(f) => f.is_gen,
        Item::Impl(im) => im.methods.iter().any(|m| m.is_gen),
        _ => false,
    })
}

/// The impl context a `gen fn` method needs to lower into a top-level helper: the
/// receiver type (to type the helper's `self`), the impl's `where` bounds (so a
/// generic impl's helper monomorphizes per element), and the type name (to
/// disambiguate the helper's name across types).
struct MethodCtx {
    self_ty: Type,
    bounds: Vec<(String, String, Vec<Type>)>,
    type_name: String,
}

/// The type a method's `self` stands for in an `impl`: `List(a)` for a generic
/// target, a tuple for `impl … for (a, b)`, a bare head otherwise. Mirrors
/// `traits::method_fn`'s self-typing so a hoisted generator helper (an ordinary
/// top-level function) type-checks without having to infer its receiver.
fn impl_self_ty(im: &ImplDef) -> Type {
    if im.type_name.starts_with("Tuple") {
        Type::Tuple(im.target_args.clone())
    } else {
        Type::Named(im.type_name.clone(), im.target_args.clone())
    }
}

/// The element type `a` of a `-> Iter(a)` return annotation, if present.
fn iter_elem(ret: &Option<Type>) -> Option<Type> {
    match ret {
        Some(Type::Named(n, args)) if n == "Iter" && args.len() == 1 => Some(args[0].clone()),
        _ => None,
    }
}

/// Lower one `gen fn` into (helper, wrapper). `method` is `Some` when `f` is a
/// method of an inherent `impl`: the helper is then named per-type (so two types'
/// same-named generators don't collide), its `self` receiver is typed to the impl
/// type, and it carries the impl's bounds — while the wrapper is returned to be
/// re-inserted into `impl.methods`, preserving method identity.
fn lower_gen(f: Function, method: Option<&MethodCtx>) -> Result<(Function, Function), String> {
    let elem = iter_elem(&f.ret);
    let helper_name = match method {
        Some(ctx) => format!("__gen_{}_{}", ctx.type_name, f.name),
        None => format!("__gen_{}", f.name),
    };

    // Helper params: the original params plus `__target: Int`.
    let mut helper_params = f.params.clone();
    helper_params.push(Param {
        name: TARGET.to_string(),
        ty: Some(Type::Named("Int".to_string(), vec![])),
        convention: Convention::Let,
        default: None,
    });
    // For an impl method the receiver `self` is unannotated after parsing (the
    // trait/impl pass types it later, but that pass never sees this hoisted
    // helper). Type it here to the implementing type so the helper checks.
    if let Some(ctx) = method {
        if let Some(first) = helper_params.first_mut() {
            if first.ty.is_none() {
                first.ty = Some(ctx.self_ty.clone());
            }
        }
    }

    // Helper body: `var __i = 0` + the body with yields rewritten + final `None`.
    let mut stmts = vec![Stmt::Let {
        name: COUNTER.to_string(),
        ty: None,
        mutable: true,
        value: Expr::Int(0),
    }];
    stmts.extend(rewrite_block(f.body.clone(), &f.name, false)?.stmts);
    stmts.push(Stmt::Expr(Expr::Ctor { name: "None".to_string(), args: vec![] }));
    let helper = Function {
        public: false,
        comptime_only: false,
        name: helper_name.clone(),
        params: helper_params,
        ret: elem.as_ref().map(|a| Type::Named("Option".to_string(), vec![a.clone()])),
        body: Block { stmts, lines: Vec::new(), region: None },
        // A generic impl's helper is monomorphized through its bounds, exactly
        // like the wrapper method (`traits::method_fn` re-applies `impl.bounds`).
        bounds: method.map_or_else(|| f.bounds.clone(), |ctx| ctx.bounds.clone()),
        is_gen: false,
        is_async: false,
    };

    // Wrapper: `f(params) -> Iter(a): iter.from_gen(fn(__t: Int): __gen_f(args, __t))`.
    let forwarded: Vec<Expr> = f
        .params
        .iter()
        .map(|p| Expr::Var(p.name.clone()))
        .chain(std::iter::once(Expr::Var(TARGET.to_string())))
        .collect();
    let thunk = Expr::Lambda {
        params: vec![Param {
            name: TARGET.to_string(),
            ty: Some(Type::Named("Int".to_string(), vec![])),
            convention: Convention::Let,
            default: None,
        }],
        body: Block {
            stmts: vec![Stmt::Expr(Expr::Call { name: helper_name, args: forwarded })],
            lines: vec![0],
            region: None,
        },
        ret: None,
    };
    let wrapper = Function {
        public: f.public,
        comptime_only: false,
        name: f.name,
        params: f.params,
        ret: f.ret,
        body: Block {
            stmts: vec![Stmt::Expr(Expr::Call {
                name: "iter.from_gen".to_string(),
                args: vec![thunk],
            })],
            lines: vec![0],
            region: None,
        },
        bounds: f.bounds,
        is_gen: false,
        is_async: false,
    };

    Ok((helper, wrapper))
}

/// Replace each `yield e` with `if __i == __target: return Some(e)` followed by
/// `__i = __i + 1`, recursing through the nested blocks of control-flow forms
/// (but not into lambdas — `yield` belongs to the generator, not a closure).
///
/// User `return`s are re-expressed in terms of the generator's stream contract,
/// NOT passed untranslated into the synthesized `-> Option(a)` helper:
/// a bare `return` becomes the stream's end (`return None`), and `return <value>`
/// is rejected against the declared `-> Iter(a)` signature (a generator produces
/// its elements with `yield`, not by returning a scalar) — so the internal
/// `Option(a)` protocol never leaks into a user diagnostic. `gen_name` is the
/// generator's name, used only to phrase that rejection.
fn rewrite_block(b: Block, gen_name: &str, in_region: bool) -> Result<Block, String> {
    let in_region = in_region || b.region.is_some();
    let mut out = Vec::with_capacity(b.stmts.len());
    for stmt in b.stmts {
        match stmt {
            Stmt::Yield(e) => {
                if in_region {
                    return Err(
                        "cannot `yield` inside `region:`: the generator frame outlives the region"
                            .to_string(),
                    );
                }
                let check = Expr::If {
                    cond: Box::new(Expr::Binary {
                        op: BinOp::Eq,
                        lhs: Box::new(Expr::Var(COUNTER.to_string())),
                        rhs: Box::new(Expr::Var(TARGET.to_string())),
                    }),
                    then_block: Block {
                        stmts: vec![Stmt::Return(Some(Expr::Ctor {
                            name: "Some".to_string(),
                            args: vec![e],
                        }))],
                        lines: vec![0],
                        region: None,
                    },
                    else_block: None,
                };
                out.push(Stmt::Expr(check));
                out.push(Stmt::Assign {
                    name: COUNTER.to_string(),
                    value: Expr::Binary {
                        op: BinOp::Add,
                        lhs: Box::new(Expr::Var(COUNTER.to_string())),
                        rhs: Box::new(Expr::Int(1)),
                    },
                });
            }
            Stmt::Let { name, ty, mutable, value } => {
                out.push(Stmt::Let {
                    name,
                    ty,
                    mutable,
                    value: rewrite_expr(value, gen_name, in_region)?,
                })
            }
            Stmt::Assign { name, value } => {
                out.push(Stmt::Assign { name, value: rewrite_expr(value, gen_name, in_region)? })
            }
            Stmt::LetPattern { pattern, value } => {
                out.push(Stmt::LetPattern {
                    pattern,
                    value: rewrite_expr(value, gen_name, in_region)?,
                })
            }
            // A bare `return` ends the stream: it becomes the helper's `return None`.
            Stmt::Return(None) => out.push(Stmt::Return(Some(Expr::Ctor {
                name: "None".to_string(),
                args: vec![],
            }))),
            // `return <value>` is not a valid generator statement: a `gen fn`
            // declares `-> Iter(a)` and produces its elements with `yield`, so a
            // returned scalar has nowhere to go. Reject it in the generator's own
            // terms — never let it be typed against the synthesized `Option(a)`.
            Stmt::Return(Some(_)) => {
                return Err(format!(
                    "`return <value>` is not allowed in generator `{gen_name}`: a `gen fn` \
                     declares `-> Iter(a)` and produces its elements with `yield` — use a bare \
                     `return` to end the stream early"
                ));
            }
            Stmt::Expr(e) => out.push(Stmt::Expr(rewrite_expr(e, gen_name, in_region)?)),
            other => out.push(other),
        }
    }
    Ok(Block { stmts: out, lines: b.lines, region: b.region })
}

/// Rewrite the nested blocks of an expression's control-flow forms so yields
/// inside `if`/`while`/`for`/`match`/block bodies are transformed too.
fn rewrite_expr(e: Expr, gen_name: &str, in_region: bool) -> Result<Expr, String> {
    Ok(match e {
        Expr::If { cond, then_block, else_block } => Expr::If {
            cond,
            then_block: rewrite_block(then_block, gen_name, in_region)?,
            else_block: match else_block {
                Some(b) => Some(rewrite_block(b, gen_name, in_region)?),
                None => None,
            },
        },
        Expr::While { cond, body } => {
            Expr::While { cond, body: rewrite_block(body, gen_name, in_region)? }
        }
        Expr::For { var, iter, body } => {
            Expr::For { var, iter, body: rewrite_block(body, gen_name, in_region)? }
        }
        Expr::Match { scrutinee, arms } => {
            let mut new_arms = Vec::with_capacity(arms.len());
            for a in arms {
                new_arms.push(MatchArm {
                    line: a.line,
                    pattern: a.pattern,
                    guard: a.guard,
                    body: rewrite_expr(a.body, gen_name, in_region)?,
                });
            }
            Expr::Match { scrutinee, arms: new_arms }
        }
        Expr::Block(b) => Expr::Block(rewrite_block(b, gen_name, in_region)?),
        other => other,
    })
}
