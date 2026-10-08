use std::collections::BTreeSet;
use witchy_syntax::ast::{BinOp, Block, Expr, Function, Pattern, Stmt, Type, UnOp};
use crate::scalar_region_recipe::ActorRecipe;
use crate::typeck::TypedModule;

pub(crate) fn scalar(expression: &Expr, names: &BTreeSet<String>) -> bool {
    match expression {
        Expr::Binary { op: BinOp::Mod, lhs, rhs } => {
            matches!(rhs.as_ref(), Expr::Int(value) if *value > 0) && scalar(lhs, names)
        }
        Expr::Binary { op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Eq | BinOp::NotEq
            | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq | BinOp::And | BinOp::Or
            | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor, lhs, rhs } => scalar(lhs, names) && scalar(rhs, names),
        Expr::Unary { op: UnOp::Neg | UnOp::Not, expr } => scalar(expr, names),
        Expr::Int(_) | Expr::Bool(_) => true,
        Expr::Var(name) => names.contains(name),
        _ => false,
    }
}

pub(crate) fn scalar_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, args) if args.is_empty() && matches!(name.as_str(), "Int" | "Bool"))
}

pub(crate) fn source_block(body: &Block, start: usize, end: usize) -> Block {
    Block { stmts: body.stmts[start..end].to_vec(), lines: body.lines[start..end].to_vec(), region: None }
}

pub(crate) fn producer(function: &Function, typed: &TypedModule) -> Option<ActorRecipe> {
    let original = typed.module().items.iter().find_map(|item| match item {
        witchy_syntax::ast::Item::Function(original) if original.name == function.name && original == function => Some(original), _ => None,
    })?;
    let function = original;
    let mut projected = function.clone();
    let loops = function.body.stmts.iter().enumerate().filter_map(|(index, statement)| match statement {
        Stmt::Expr(Expr::While { body, .. }) => Some((index, body)), _ => None,
    }).collect::<Vec<_>>();
    let [(loop_index, body)] = loops.as_slice() else { return None };
    if body.lines.len() != body.stmts.len() { return None; }
    let sends = body.stmts.iter().enumerate().filter_map(|(index, statement)| match statement {
        Stmt::Expr(Expr::Unary { op: UnOp::Await, expr }) => Some((index, expr.as_ref())), _ => None,
    }).collect::<Vec<_>>();
    let [(send, Expr::Call { name, args })] = sends.as_slice() else { return None };
    let [_, payload] = args.as_slice() else { return None };
    if name != "chan.send" { return None; }
    let mut names = function.params.iter().filter(|parameter| parameter.ty.as_ref().is_some_and(scalar_type))
        .map(|parameter| parameter.name.clone()).collect::<BTreeSet<_>>();
    for statement in &function.body.stmts[..*loop_index] {
        match statement {
            Stmt::Let { name, .. } | Stmt::LetPattern { pattern: Pattern::Var(name), .. } => { names.insert(name.clone()); }
            _ => return None,
        }
    }
    let mut projected_statements = Vec::new();
    let mut projected_lines = Vec::new();
    for (index, statement) in body.stmts.iter().enumerate() {
        if index < *send {
            let local = match statement {
                Stmt::Let { name, value, mutable: false, .. }
                | Stmt::LetPattern { pattern: Pattern::Var(name), value } => Some((name, value)),
                _ => None,
            };
            if let Some((name, value)) = local {
                if !scalar(value, &names) || !names.insert(name.clone())
                    || !typed.table().type_of(value).and_then(crate::typeck::ty_to_ast).is_some_and(|ty| scalar_type(&ty)) {
                    return None;
                }
                continue;
            }
        }
        let mut statement = statement.clone();
        if index == *send {
            let Stmt::Expr(Expr::Unary { expr, .. }) = &mut statement else { return None };
            let Expr::Call { args, .. } = expr.as_mut() else { return None };
            args[1] = Expr::Int(0);
        }
        projected_statements.push(statement);
        projected_lines.push(body.lines[index]);
    }
    if !scalar(payload, &names) { return None; }
    let Stmt::Expr(Expr::While { body: projected_body, .. }) = &mut projected.body.stmts[*loop_index] else { return None };
    projected_body.stmts = projected_statements;
    projected_body.lines = projected_lines;
    let mut recipe = ActorRecipe::derive(&projected, true)?;
    recipe.loop_prefix = source_block(body, 0, *send);
    recipe.payload = Some(payload.clone());
    recipe.payload_line = body.lines[*send];
    Some(recipe)
}
