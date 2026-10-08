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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_bind_actual_source_coordinate_initializer_and_declaration() {
        let original = crate::parser::parse_module("async fn f():\n    let callback = fn(): 9\n    var xs = list.repeat(0, 4)\n    ()\n").unwrap();
        let Item::Function(function) = &original.items[0] else { panic!() };
        let Stmt::Let { value, .. } = &function.body.stmts[1] else { panic!() };
        let hint = AsyncLocalTypeHint { module: "main".into(), item: 0, method: None, statement: 1,
            expression: 1, name: "xs".into(), initializer: value.clone(),
            ty: Type::Named("List".into(), vec![Type::Named("Int".into(), vec![])]) };
        let annotated = |hints: &[AsyncLocalTypeHint]| {
            let mut module = original.clone(); apply("main", &mut module, hints);
            let Item::Function(function) = &module.items[0] else { panic!() };
            let Stmt::Let { ty, .. } = &function.body.stmts[1] else { panic!() };
            ty.clone()
        };
        assert_eq!(annotated(&[hint.clone()]), Some(hint.ty.clone()));
        let mut stale = hint.clone(); stale.expression += 1;
        assert_eq!(annotated(&[stale]), None);
        let mut changed = hint.clone(); changed.initializer = Expr::List(vec![]);
        assert_eq!(annotated(&[changed]), None);
        let mut shadow = hint.clone(); shadow.name = "callback".into();
        assert_eq!(annotated(&[shadow]), None);
        assert_eq!(annotated(&[hint.clone(), hint]), None);
    }
}
