use crate::ast::{Expr, Item, Module, Stmt, Type};

#[derive(Debug, Clone)]
pub struct AsyncLocalTypeHint {
    pub module: String,
    pub item: usize,
    pub method: Option<usize>,
    pub statement: usize,
    pub expression: u32,
    pub name: String,
    pub initializer: Expr,
    pub ty: Type,
}

pub(crate) fn apply(module_name: &str, module: &mut Module, hints: &[AsyncLocalTypeHint]) {
    for hint in hints.iter().filter(|hint| hint.module == module_name) {
        if hints.iter().filter(|other| other.module == hint.module && other.item == hint.item
            && other.method == hint.method && other.statement == hint.statement).count() != 1 { continue; }
        let function = match (module.items.get_mut(hint.item), hint.method) {
            (Some(Item::Function(function)), None) => Some(function),
            (Some(Item::Impl(implementation)), Some(method)) => implementation.methods.get_mut(method),
            _ => None,
        };
        let Some(function) = function.filter(|function| function.is_async) else { continue };
        let Some(Stmt::Let { value, .. }) = function.body.stmts.get(hint.statement) else { continue };
        if expression_ordinal(&function.body, value) != Some(hint.expression) { continue; }
        let Some(Stmt::Let { name, ty, mutable: true, value }) = function.body.stmts.get_mut(hint.statement) else { continue };
        if ty.is_none() && name == &hint.name && value == &hint.initializer {
            *ty = Some(hint.ty.clone());
        }
    }
}


fn expression_ordinal(body: &crate::ast::Block, target: &Expr) -> Option<u32> {
    let mut next = 0u32;
    let mut ignored = std::collections::HashSet::new();
    let mut found = None;
    crate::ast::visit::visit_block(body, &mut |expression| {
        if ignored.contains(&(expression as *const Expr as usize)) { return Ok::<_, ()>(()); }
        if std::ptr::eq(expression, target) { found = Some(next); }
        next += 1;
        if let Expr::Lambda { body, .. } = expression {
            crate::ast::visit::visit_block(body, &mut |nested| {
                ignored.insert(nested as *const Expr as usize);
                Ok::<_, ()>(())
            })?;
        }
        Ok::<_, ()>(())
    }).expect("infallible source expression walk");
    found
}
