//! Shared compiler contract for values carried across a suspension point.
//!
//! Async and generator lowering must agree on lexical binding identity and
//! deterministic slot order before they can share one owned runtime frame ABI.
//! This module is the single binder-aware free-variable analysis used to derive
//! those slots. It deliberately reports source names in first-use order; each
//! lowering intersects that order with its lexical scope to retain declaration
//! order and attach the checked slot type/ownership metadata.

use crate::ast::{pattern_binds, Block, Expr, Function, Stmt};
use foldhash::{HashSet, HashSetExt as _};

/// Compiler-owned marker on generated segment functions. An `own` parameter on
/// an ordinary function is a consuming operation; on a suspension segment it
/// is an affine frame transfer, so the callee assumes (rather than discharges)
/// any must-consume obligation carried by that slot.
pub const FRAME_FUNCTION_ATTRIBUTE: &str = "__compiler_suspension_frame";

/// Compiler-owned marker on the ordinary entry wrapper of an async function.
/// The wrapper is not itself a frame transfer (and therefore must not receive
/// [`FRAME_FUNCTION_ATTRIBUTE`]), but a synthesized executor needs to know the
/// first state associated with this source callable.
pub const FRAME_ENTRY_ATTRIBUTE: &str = "__compiler_suspension_entry";

/// Prefix for the stable, module-local integer state attached to every async
/// entry and lifted segment. The state is data consumed by the typed carrier
/// catalog; generated function names are deliberately not the runtime ABI.
pub const FRAME_STATE_ATTRIBUTE_PREFIX: &str = "__compiler_suspension_state=";

pub fn frame_state_attribute(state: usize) -> String {
    format!("{FRAME_STATE_ATTRIBUTE_PREFIX}{state}")
}

/// Recover the compiler-owned state identity from a generated callable.
pub fn frame_state(function: &Function) -> Option<usize> {
    function.attributes.iter().find_map(|attribute| {
        attribute
            .strip_prefix(FRAME_STATE_ATTRIBUTE_PREFIX)
            .and_then(|state| state.parse().ok())
    })
}

/// Free lexical bindings referenced by `expression`, in deterministic first-use
/// order. Bindings introduced by nested blocks, patterns, loops, and lambdas are
/// excluded from the result.
pub(crate) fn free_bindings(expression: &Expr) -> Vec<String> {
    free_bindings_with_bound(expression, &HashSet::new())
}

pub(crate) fn free_bindings_with_bound(
    expression: &Expr,
    bound: &HashSet<String>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    visit_expr(expression, bound, &mut seen, &mut order);
    order
}

pub(crate) fn free_bindings_in_block(
    block: &Block,
    bound: &HashSet<String>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    visit_block(block, bound, &mut seen, &mut order);
    order
}

fn visit_expr(
    expression: &Expr,
    bound: &HashSet<String>,
    seen: &mut HashSet<String>,
    output: &mut Vec<String>,
) {
    match expression {
        Expr::Var(name) => {
            if !bound.contains(name) && seen.insert(name.clone()) {
                output.push(name.clone());
            }
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
        Expr::Unary { expr, .. }
        | Expr::Field { base: expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => visit_expr(expr, bound, seen, output),
        Expr::ExistentialCall { receiver, args, .. } => {
            visit_expr(receiver, bound, seen, output);
            visit_all(args, bound, seen, output);
        }
        Expr::Index { base, index } | Expr::Binary { lhs: base, rhs: index, .. } => {
            visit_expr(base, bound, seen, output);
            visit_expr(index, bound, seen, output);
        }
        Expr::Range { lo, hi, .. } => {
            visit_expr(lo, bound, seen, output);
            visit_expr(hi, bound, seen, output);
        }
        Expr::List(items) | Expr::Tuple(items) => visit_all(items, bound, seen, output),
        Expr::Call { name, args } => {
            // Before linker rewriting, application of a local callable is still
            // represented as a named `Call`. Report a bare callee as a possible
            // lexical binding; the lowering's scope intersection discards real
            // top-level function names.
            if !name.contains('.') && !bound.contains(name) && seen.insert(name.clone()) {
                output.push(name.clone());
            }
            visit_all(args, bound, seen, output);
        }
        Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            visit_all(args, bound, seen, output);
        }
        Expr::LabeledCall { name, args } => {
            if !name.contains('.') && !bound.contains(name) && seen.insert(name.clone()) {
                output.push(name.clone());
            }
            for (_, argument) in args {
                visit_expr(argument, bound, seen, output);
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            visit_expr(receiver, bound, seen, output);
            for (_, argument) in args {
                visit_expr(argument, bound, seen, output);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            visit_expr(receiver, bound, seen, output);
            visit_all(args, bound, seen, output);
        }
        Expr::Apply { func, args } => {
            visit_expr(func, bound, seen, output);
            visit_all(args, bound, seen, output);
        }
        Expr::RecordUpdate { base, fields, .. } => {
            visit_expr(base, bound, seen, output);
            for (_, value) in fields {
                visit_expr(value, bound, seen, output);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                visit_expr(value, bound, seen, output);
            }
            if let Some(spread) = spread {
                visit_expr(spread, bound, seen, output);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            visit_expr(cond, bound, seen, output);
            visit_block(then_block, bound, seen, output);
            if let Some(else_block) = else_block {
                visit_block(else_block, bound, seen, output);
            }
        }
        Expr::Match { scrutinee, arms } => {
            visit_expr(scrutinee, bound, seen, output);
            for arm in arms {
                let mut names = Vec::new();
                pattern_binds(&arm.pattern, &mut names);
                let mut inner = bound.clone();
                inner.extend(names);
                if let Some(guard) = &arm.guard {
                    visit_expr(guard, &inner, seen, output);
                }
                visit_expr(&arm.body, &inner, seen, output);
            }
        }
        Expr::Block(block) => visit_block(block, bound, seen, output),
        Expr::While { cond, body } => {
            visit_expr(cond, bound, seen, output);
            visit_block(body, bound, seen, output);
        }
        Expr::For { var, iter, body } => {
            visit_expr(iter, bound, seen, output);
            let mut inner = bound.clone();
            inner.insert(var.clone());
            visit_block(body, &inner, seen, output);
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            visit_expr(scrutinee, bound, seen, output);
            let mut names = Vec::new();
            pattern_binds(pattern, &mut names);
            let mut inner = bound.clone();
            inner.extend(names);
            visit_block(body, &inner, seen, output);
        }
        Expr::Lambda { params, body, .. } => {
            let mut inner = bound.clone();
            inner.extend(params.iter().map(|parameter| parameter.name.clone()));
            visit_block(body, &inner, seen, output);
        }
    }
}

fn visit_all(
    expressions: &[Expr],
    bound: &HashSet<String>,
    seen: &mut HashSet<String>,
    output: &mut Vec<String>,
) {
    for expression in expressions {
        visit_expr(expression, bound, seen, output);
    }
}

fn visit_block(
    block: &Block,
    bound: &HashSet<String>,
    seen: &mut HashSet<String>,
    output: &mut Vec<String>,
) {
    let mut bound = bound.clone();
    for statement in &block.stmts {
        match statement {
            Stmt::Let { name, value, .. } => {
                visit_expr(value, &bound, seen, output);
                bound.insert(name.clone());
            }
            Stmt::LetPattern { pattern, value } => {
                visit_expr(value, &bound, seen, output);
                let mut names = Vec::new();
                pattern_binds(pattern, &mut names);
                bound.extend(names);
            }
            Stmt::Assign { value, .. } => visit_expr(value, &bound, seen, output),
            Stmt::Expr(expression) | Stmt::Yield(expression) => {
                visit_expr(expression, &bound, seen, output);
            }
            Stmt::Return(value) => {
                if let Some(value) = value {
                    visit_expr(value, &bound, seen, output);
                }
            }
            Stmt::Break | Stmt::Continue => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspension_slots_ignore_nested_binders_and_preserve_first_use_order() {
        let module = crate::parser::parse_module(
            "fn sample(a: Int, b: Int, c: Int) -> Int:\n    let body = fn(a: Int): a + c\n    if b > 0:\n        let c = 7\n        body(c)\n    a + body(b)\n",
        )
        .expect("slot-analysis fixture parses");
        let crate::ast::Item::Function(function) = &module.items[0] else {
            panic!("expected function")
        };
        let Stmt::Expr(expression) = function.body.stmts.last().expect("tail expression") else {
            panic!("expected tail expression")
        };
        assert_eq!(free_bindings(expression), ["a", "body", "b"]);
    }
}
