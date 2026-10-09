//! The owning adapter for a named function value whose parameters borrow.
//!
//! A `let` parameter leaves the argument with the caller, who drops it after
//! the call; a default parameter hands the callee ownership. The two have
//! different cleanup, so `fn(let String) -> String` is not a
//! `fn(String) -> String`. The checker nonetheless accepts a named function
//! such as `string.trim` in an owned slot (`lines.map(string.trim)`) and
//! records the use; this rewrite then replaces it with `fn(p): string.trim(p)`.
//! The lambda owns `p`, lends it to the direct call, and drops it, so every
//! later stage sees an ordinary closure and an ordinary call.

use witchy_syntax::ast::visit::{visit_block_mut, visit_expr_mut};
use std::collections::HashSet;

use witchy_syntax::ast::{Block, CallableQualifiers, Convention, Expr, Item, Module, Param, Stmt, Type};

use crate::typeck::{Ty, TypeTable, ty_to_ast};

/// Give every recorded adapter use its owning lambda. Returns true if any node
/// was rewritten (so the caller re-annotates, since new nodes change the
/// address-keyed `TypeTable`).
pub fn rewrite_let_callable_adapters(module: &mut Module, table: &TypeTable) -> bool {
    if !table.has_let_callable_adapters() {
        return false;
    }
    // A use inside a generic body has no resolved type to read the target's
    // purity from, so take it from the declarations.
    let pure: HashSet<String> = module.items.iter()
        .filter_map(|item| match item {
            Item::Function(function) if function.pure => Some(function.name.clone()),
            _ => None,
        })
        .collect();
    let mut changed = false;
    let mut visitor = |expression: &mut Expr| -> Result<(), ()> {
        if let Some(conventions) = table.let_callable_adapter(expression) {
            changed |= adapt(expression, conventions, table, &pure);
        }
        Ok(())
    };
    for item in &mut module.items {
        let _ = match item {
            Item::Function(function) => visit_block_mut(&mut function.body, &mut visitor),
            Item::Impl(definition) => definition.methods.iter_mut()
                .try_for_each(|method| visit_block_mut(&mut method.body, &mut visitor)),
            Item::Trait(definition) => definition.methods.iter_mut()
                .filter_map(|method| method.default.as_mut())
                .try_for_each(|body| visit_block_mut(body, &mut visitor)),
            Item::Const { value, .. } => visit_expr_mut(value, &mut visitor),
            Item::Comptime(block) => visit_block_mut(block, &mut visitor),
            Item::Type(_) | Item::TypeAlias { .. } => Ok(()),
        };
    }
    changed
}

/// The linker's eta lambda for a module-qualified function already forwards
/// to it, so its parameters take the slot's conventions. A bare function name
/// becomes the same eta shape, `fn(__eta0, ...): target(__eta0, ...)`, keeping
/// the target's purity so a `pure` target still fills a `pure` slot. Its
/// parameters carry their checked types when the use has one; inside a
/// generic body they are left to the recheck to infer from the call.
fn adapt(expression: &mut Expr, conventions: &[Convention], table: &TypeTable, pure: &HashSet<String>) -> bool {
    if let Expr::Lambda { params, .. } = expression {
        for (param, convention) in params.iter_mut().zip(conventions) {
            param.convention = *convention;
        }
        return true;
    }
    let types: Vec<Option<Type>> = match table.type_of(expression) {
        Some(Ty::Fn(params, ..)) if params.len() == conventions.len() => params.iter().map(ty_to_ast).collect(),
        _ => vec![None; conventions.len()],
    };
    let Expr::Var(target) = expression else { return false };
    let qualifiers = CallableQualifiers::new(pure.contains(target), false);
    let names = (0..conventions.len()).map(|index| format!("__eta{index}")).collect::<Vec<_>>();
    *expression = Expr::Lambda {
        params: names.iter().zip(types).zip(conventions).map(|((name, ty), convention)| Param {
            name: name.clone(),
            ty,
            convention: *convention,
            default: None,
        }).collect(),
        body: Block {
            stmts: vec![Stmt::Expr(Expr::Call {
                name: target.clone(),
                args: names.into_iter().map(Expr::Var).collect(),
            })],
            lines: vec![0],
            region: None,
        },
        ret: None,
        qualifiers,
    };
    true
}
