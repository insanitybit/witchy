use witchy_syntax::ast::Stmt;
use witchy_syntax::async_lower::type_hints::AsyncLocalTypeHint;
use crate::typeck::{self, TypedModule};

pub(crate) fn collect(typed: &TypedModule) -> Result<Vec<AsyncLocalTypeHint>, String> {
    let inventory = crate::async_control_flow::SourceAsyncControlFlowInventory::from_typed_projection(typed)?;
    let mut hints = Vec::new();
    for flow in inventory.callables {
        let function = crate::typed_async_lowering::typed_source_function(typed, &flow.callable)
            .ok_or("local type hint lost its original source callable")?;
        for (statement, local) in function.body.stmts.iter().enumerate() {
            let Stmt::Let { name, ty: None, mutable: true, value } = local else { continue };
            let Some(ty) = typed.table().type_of(value).and_then(typeck::ty_to_ast) else { continue };
            let Some(expression) = flow.expression_spans.iter().find_map(|span| {
                crate::async_control_flow::source_expression(function, span.root)
                    .filter(|expression| std::ptr::eq(*expression, value)).map(|_| span.root.0)
            }) else { continue };
            hints.push(AsyncLocalTypeHint {
                module: flow.callable.module.clone(), item: flow.callable.surface.item as usize,
                method: flow.callable.surface.method.map(|method| method as usize),
                statement, expression, name: name.clone(), initializer: value.clone(), ty,
            });
        }
    }
    Ok(hints)
}
