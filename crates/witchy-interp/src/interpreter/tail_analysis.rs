//! Tail-call analysis: which functions may forward directly to a callee's
//! frame, and which callees appear in tail position. Consumed by the evaluator
//! to decide when a call can reuse the current stack frame.

use std::rc::Rc;

use foldhash::{HashMap as FxHashMap, HashMapExt as _, HashSet as FxHashSet, HashSetExt as _};
use witchy_syntax::ast::{BinOp, Block, Convention, Expr, Stmt};

use super::Function;

pub(super) fn recursive_tail_edges(
    functions: &FxHashMap<String, Rc<Function>>,
) -> FxHashMap<String, FxHashSet<String>> {
    let graph: FxHashMap<_, Vec<_>> = functions
        .values()
        .map(|function| {
            let mut targets = FxHashSet::new();
            collect_tail_callees_block(&function.body, &mut targets);
            targets.retain(|target| {
                functions.get(target).is_some_and(|target| {
                    direct_tail_abis_are_compatible(function, target)
                })
            });
            (function.name.clone(), targets.into_iter().collect())
        })
        .collect();

    let mut recursive: FxHashMap<String, FxHashSet<String>> = FxHashMap::new();
    for (source, targets) in &graph {
        for target in targets {
            let mut pending = vec![target.as_str()];
            let mut seen = FxHashSet::new();
            while let Some(next) = pending.pop() {
                if next == source {
                    recursive.entry(source.clone()).or_default().insert(target.clone());
                    break;
                }
                if seen.insert(next) && let Some(successors) = graph.get(next) {
                    pending.extend(successors.iter().map(String::as_str));
                }
            }
        }
    }
    recursive
}

fn direct_tail_abis_are_compatible(source: &Function, target: &Function) -> bool {
    let source_has_var = source.params.iter().any(|param| param.convention == Convention::Var);
    if !source_has_var {
        return target.params.iter().all(|param| param.convention != Convention::Var);
    }
    source.ret == target.ret
        && source.params.len() == target.params.len()
        && source.params.iter().zip(&target.params).all(|(source, target)| {
            source.convention == target.convention && source.ty == target.ty
        })
}

pub(super) fn direct_tail_envelope_is_forwarded(
    source: &Function,
    target: &Function,
    args: &[Expr],
) -> bool {
    let source_has_var = source.params.iter().any(|param| param.convention == Convention::Var);
    if !source_has_var {
        return target.params.iter().all(|param| param.convention != Convention::Var);
    }
    direct_tail_abis_are_compatible(source, target)
        && source.params.len() == args.len()
        && source.params.iter().zip(&target.params).zip(args).all(
            |((source_param, target_param), arg)| {
                source_param.convention == target_param.convention
                    && (source_param.convention != Convention::Var
                        || matches!(arg, Expr::Var(name) if name == &source_param.name))
            },
        )
}

fn collect_tail_callees_block(block: &Block, out: &mut FxHashSet<String>) {
    for stmt in &block.stmts {
        collect_nested_returns_stmt(stmt, out);
    }
    if let Some(Stmt::Expr(expr) | Stmt::Yield(expr)) = block.stmts.last() {
        collect_tail_callees_expr(expr, out);
    }
}

fn collect_tail_callees_expr(expr: &Expr, out: &mut FxHashSet<String>) {
    match expr {
        Expr::Call { name, .. } => {
            out.insert(name.clone());
        }
        Expr::If { then_block, else_block, .. } => {
            collect_tail_callees_block(then_block, out);
            if let Some(block) = else_block {
                collect_tail_callees_block(block, out);
            }
        }
        Expr::Match { arms, .. } => {
            for arm in arms {
                collect_tail_callees_expr(&arm.body, out);
            }
        }
        Expr::Block(block) => collect_tail_callees_block(block, out),
        Expr::Binary { op: BinOp::Coalesce, rhs, .. } => {
            collect_tail_callees_expr(rhs, out);
        }
        _ => collect_nested_returns_expr(expr, out),
    }
}

fn collect_nested_returns_stmt(stmt: &Stmt, out: &mut FxHashSet<String>) {
    match stmt {
        Stmt::Return(Some(expr)) => collect_tail_callees_expr(expr, out),
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Expr(value)
        | Stmt::Yield(value) => collect_nested_returns_expr(value, out),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
    }
}

fn collect_nested_returns_block(block: &Block, out: &mut FxHashSet<String>) {
    for stmt in &block.stmts {
        collect_nested_returns_stmt(stmt, out);
    }
}

fn collect_nested_returns_expr(expr: &Expr, out: &mut FxHashSet<String>) {
    match expr {
        Expr::List(items) | Expr::Tuple(items) | Expr::Ctor { args: items, .. }
        | Expr::AnonCtor { args: items, .. } => {
            for item in items {
                collect_nested_returns_expr(item, out);
            }
        }
        Expr::Call { args, .. } => {
            for arg in args {
                collect_nested_returns_expr(arg, out);
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, arg) in args {
                collect_nested_returns_expr(arg, out);
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            collect_nested_returns_expr(receiver, out);
            for (_, arg) in args {
                collect_nested_returns_expr(arg, out);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_nested_returns_expr(receiver, out);
            for arg in args {
                collect_nested_returns_expr(arg, out);
            }
        }
        Expr::Apply { func, args } => {
            collect_nested_returns_expr(func, out);
            for arg in args {
                collect_nested_returns_expr(arg, out);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Field { base: expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => collect_nested_returns_expr(expr, out),
        Expr::ExistentialCall { receiver, args, .. } => {
            collect_nested_returns_expr(receiver, out);
            for arg in args {
                collect_nested_returns_expr(arg, out);
            }
        }
        Expr::RecordUpdate { base, fields, .. } => {
            collect_nested_returns_expr(base, out);
            for (_, value) in fields {
                collect_nested_returns_expr(value, out);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                collect_nested_returns_expr(value, out);
            }
            if let Some(spread) = spread {
                collect_nested_returns_expr(spread, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } | Expr::Range { lo: lhs, hi: rhs, .. }
        | Expr::Index { base: lhs, index: rhs } => {
            collect_nested_returns_expr(lhs, out);
            collect_nested_returns_expr(rhs, out);
        }
        Expr::If { cond, then_block, else_block } => {
            collect_nested_returns_expr(cond, out);
            collect_nested_returns_block(then_block, out);
            if let Some(block) = else_block {
                collect_nested_returns_block(block, out);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_nested_returns_expr(scrutinee, out);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_nested_returns_expr(guard, out);
                }
                collect_nested_returns_expr(&arm.body, out);
            }
        }
        Expr::Block(block) => collect_nested_returns_block(block, out),
        Expr::While { cond, body } => {
            collect_nested_returns_expr(cond, out);
            collect_nested_returns_block(body, out);
        }
        Expr::For { iter, body, .. } => {
            collect_nested_returns_expr(iter, out);
            collect_nested_returns_block(body, out);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            collect_nested_returns_expr(scrutinee, out);
            collect_nested_returns_block(body, out);
        }
        Expr::Lambda { .. } | Expr::Int(_) | Expr::Float(_) | Expr::Duration(_)
        | Expr::Str(_) | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
    }
}
