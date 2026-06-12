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
pub fn lower(mut module: Module) -> Module {
    if !module.items.iter().any(is_gen_fn) {
        return module;
    }
    let mut items = Vec::with_capacity(module.items.len() + 1);
    for item in module.items {
        match item {
            Item::Function(f) if f.is_gen => {
                let (helper, wrapper) = lower_gen(f);
                items.push(Item::Function(helper));
                items.push(Item::Function(wrapper));
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
    module
}

fn is_gen_fn(item: &Item) -> bool {
    matches!(item, Item::Function(f) if f.is_gen)
}

/// The element type `a` of a `-> Iter(a)` return annotation, if present.
fn iter_elem(ret: &Option<Type>) -> Option<Type> {
    match ret {
        Some(Type::Named(n, args)) if n == "Iter" && args.len() == 1 => Some(args[0].clone()),
        _ => None,
    }
}

fn lower_gen(f: Function) -> (Function, Function) {
    let elem = iter_elem(&f.ret);
    let helper_name = format!("__gen_{}", f.name);

    // Helper params: the original params plus `__target: Int`.
    let mut helper_params = f.params.clone();
    helper_params.push(Param {
        name: TARGET.to_string(),
        ty: Some(Type::Named("Int".to_string(), vec![])),
        convention: Convention::Let,
    });

    // Helper body: `var __i = 0` + the body with yields rewritten + final `None`.
    let mut stmts = vec![Stmt::Let {
        name: COUNTER.to_string(),
        ty: None,
        mutable: true,
        value: Expr::Int(0),
    }];
    stmts.extend(rewrite_block(f.body.clone()).stmts);
    stmts.push(Stmt::Expr(Expr::Ctor { name: "None".to_string(), args: vec![] }));
    let helper = Function {
        public: false,
        name: helper_name.clone(),
        params: helper_params,
        ret: elem.as_ref().map(|a| Type::Named("Option".to_string(), vec![a.clone()])),
        body: Block { stmts, lines: Vec::new(), restrict: None, region: None },
        bounds: f.bounds.clone(),
        is_gen: false,
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
        }],
        body: Block {
            stmts: vec![Stmt::Expr(Expr::Call { name: helper_name, args: forwarded })],
            lines: vec![0],
            restrict: None,
            region: None,
        },
    };
    let wrapper = Function {
        public: f.public,
        name: f.name,
        params: f.params,
        ret: f.ret,
        body: Block {
            stmts: vec![Stmt::Expr(Expr::Call {
                name: "iter.from_gen".to_string(),
                args: vec![thunk],
            })],
            lines: vec![0],
            restrict: None,
            region: None,
        },
        bounds: f.bounds,
        is_gen: false,
    };

    (helper, wrapper)
}

/// Replace each `yield e` with `if __i == __target: return Some(e)` followed by
/// `__i = __i + 1`, recursing through the nested blocks of control-flow forms
/// (but not into lambdas — `yield` belongs to the generator, not a closure).
fn rewrite_block(b: Block) -> Block {
    let mut out = Vec::with_capacity(b.stmts.len());
    for stmt in b.stmts {
        match stmt {
            Stmt::Yield(e) => {
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
                        restrict: None,
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
                out.push(Stmt::Let { name, ty, mutable, value: rewrite_expr(value) })
            }
            Stmt::Assign { name, value } => {
                out.push(Stmt::Assign { name, value: rewrite_expr(value) })
            }
            Stmt::LetTuple { names, value } => {
                out.push(Stmt::LetTuple { names, value: rewrite_expr(value) })
            }
            Stmt::Return(v) => out.push(Stmt::Return(v.map(rewrite_expr))),
            Stmt::Expr(e) => out.push(Stmt::Expr(rewrite_expr(e))),
            other => out.push(other),
        }
    }
    Block { stmts: out, lines: b.lines, restrict: b.restrict, region: b.region }
}

/// Rewrite the nested blocks of an expression's control-flow forms so yields
/// inside `if`/`while`/`for`/`match`/block bodies are transformed too.
fn rewrite_expr(e: Expr) -> Expr {
    match e {
        Expr::If { cond, then_block, else_block } => Expr::If {
            cond,
            then_block: rewrite_block(then_block),
            else_block: else_block.map(rewrite_block),
        },
        Expr::While { cond, body } => Expr::While { cond, body: rewrite_block(body) },
        Expr::For { var, iter, body } => {
            Expr::For { var, iter, body: rewrite_block(body) }
        }
        Expr::Match { scrutinee, arms } => Expr::Match {
            scrutinee,
            arms: arms
                .into_iter()
                .map(|a| MatchArm { pattern: a.pattern, guard: a.guard, body: rewrite_expr(a.body) })
                .collect(),
        },
        Expr::Block(b) => Expr::Block(rewrite_block(b)),
        other => other,
    }
}
