//! RFC-0080 structural item-hole substitution.
//!
//! The parser stores one item AST with reserved placeholder nodes. Compile-time
//! evaluation supplies typed hole payloads; this pass replaces only those exact
//! nodes, so the enclosing declaration is never rendered and reparsed.

use crate::ast::{Block, Expr, Function, Item, Param, Pattern, Stmt, Type};
use crate::parser::{
    QUOTE_EXPR_HOLE_PREFIX, QUOTE_PATTERN_HOLE_PREFIX, QUOTE_TYPE_HOLE_PREFIX, parse_module,
};

#[derive(Debug, Clone, PartialEq)]
pub enum ItemSyntaxHole {
    Expr(Expr),
    Type(Type),
    Pattern(Pattern),
}

pub fn parse_expr_payload(source: &str) -> Result<Expr, String> {
    let body = indent(source, 4);
    let module = parse_module(&format!("fn __witchy_syntax_payload():\n{body}\n"))
        .map_err(|error| format!("invalid ExprSyntax payload: {error}"))?;
    let [Item::Function(function)] = module.items.as_slice() else {
        return Err("invalid ExprSyntax payload: expected exactly one expression".into());
    };
    let [Stmt::Expr(expr)] = function.body.stmts.as_slice() else {
        return Err("invalid ExprSyntax payload: expected one expression".into());
    };
    Ok(expr.clone())
}

pub fn parse_type_payload(source: &str) -> Result<Type, String> {
    let module = parse_module(&format!("type __WitchySyntaxPayload = {source}\n"))
        .map_err(|error| format!("invalid TypeSyntax payload: {error}"))?;
    let [Item::TypeAlias { name, ty, .. }] = module.items.as_slice() else {
        return Err("invalid TypeSyntax payload: expected exactly one type".into());
    };
    if name != "__WitchySyntaxPayload" {
        return Err("invalid TypeSyntax payload: parser lost the wrapper alias".into());
    }
    Ok(ty.clone())
}

pub fn parse_pattern_payload(source: &str) -> Result<Pattern, String> {
    let pattern = indent(source, 8);
    let module = parse_module(&format!(
        "fn __witchy_syntax_payload(value):\n    match value:\n{pattern} -> 0\n"
    ))
    .map_err(|error| format!("invalid PatternSyntax payload: {error}"))?;
    let [Item::Function(function)] = module.items.as_slice() else {
        return Err("invalid PatternSyntax payload: expected exactly one pattern".into());
    };
    let [Stmt::Expr(Expr::Match { arms, .. })] = function.body.stmts.as_slice() else {
        return Err("invalid PatternSyntax payload: parser lost the wrapper match".into());
    };
    let [arm] = arms.as_slice() else {
        return Err("invalid PatternSyntax payload: expected one pattern".into());
    };
    Ok(arm.pattern.clone())
}

pub fn instantiate_item(template: &Item, holes: Vec<ItemSyntaxHole>) -> Result<Item, String> {
    let mut exprs = Vec::new();
    let mut types = Vec::new();
    let mut patterns = Vec::new();
    for hole in holes {
        match hole {
            ItemSyntaxHole::Expr(expr) => exprs.push(Some(expr)),
            ItemSyntaxHole::Type(ty) => types.push(Some(ty)),
            ItemSyntaxHole::Pattern(pattern) => patterns.push(Some(pattern)),
        }
    }

    let mut item = template.clone();
    substitute_item(&mut item, &mut exprs, &mut types, &mut patterns)?;
    ensure_consumed("expression", &exprs)?;
    ensure_consumed("type", &types)?;
    ensure_consumed("pattern", &patterns)?;
    Ok(item)
}

fn indent(source: &str, spaces: usize) -> String {
    let prefix = " ".repeat(spaces);
    source
        .lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn ensure_consumed<T>(category: &str, slots: &[Option<T>]) -> Result<(), String> {
    if let Some(index) = slots.iter().position(Option::is_some) {
        Err(format!("compiler-owned item did not contain {category} hole {index}"))
    } else {
        Ok(())
    }
}

fn marker_index(name: &str, prefix: &str) -> Option<usize> {
    name.strip_prefix(prefix)?.parse().ok()
}

fn take_hole<T>(slots: &mut [Option<T>], index: usize, category: &str) -> Result<T, String> {
    slots
        .get_mut(index)
        .and_then(Option::take)
        .ok_or_else(|| format!("compiler-owned item referenced invalid or repeated {category} hole {index}"))
}

fn substitute_item(
    item: &mut Item,
    exprs: &mut [Option<Expr>],
    types: &mut [Option<Type>],
    patterns: &mut [Option<Pattern>],
) -> Result<(), String> {
    match item {
        Item::Function(function) => substitute_function(function, exprs, types, patterns),
        Item::Type(definition) => {
            for variant in &mut definition.variants {
                for ty in &mut variant.fields {
                    substitute_type(ty, types)?;
                }
            }
            Ok(())
        }
        Item::Trait(definition) => {
            for method in &mut definition.methods {
                substitute_params(&mut method.params, exprs, types, patterns)?;
                if let Some(ret) = &mut method.ret {
                    substitute_type(ret, types)?;
                }
                if let Some(default) = &mut method.default {
                    substitute_block(default, exprs, types, patterns)?;
                }
            }
            Ok(())
        }
        Item::Impl(definition) => {
            substitute_types(&mut definition.trait_args, types)?;
            substitute_types(&mut definition.target_args, types)?;
            for (_, _, args) in &mut definition.bounds {
                substitute_types(args, types)?;
            }
            for method in &mut definition.methods {
                substitute_function(method, exprs, types, patterns)?;
            }
            Ok(())
        }
        Item::Const { value, .. } => substitute_expr(value, exprs, types, patterns),
        Item::TypeAlias { ty, .. } => substitute_type(ty, types),
        Item::Comptime(block) => substitute_block(block, exprs, types, patterns),
    }
}

fn substitute_function(
    function: &mut Function,
    exprs: &mut [Option<Expr>],
    types: &mut [Option<Type>],
    patterns: &mut [Option<Pattern>],
) -> Result<(), String> {
    substitute_params(&mut function.params, exprs, types, patterns)?;
    if let Some(ret) = &mut function.ret {
        substitute_type(ret, types)?;
    }
    for (_, _, args) in &mut function.bounds {
        substitute_types(args, types)?;
    }
    substitute_block(&mut function.body, exprs, types, patterns)
}

fn substitute_params(
    params: &mut [Param],
    exprs: &mut [Option<Expr>],
    types: &mut [Option<Type>],
    patterns: &mut [Option<Pattern>],
) -> Result<(), String> {
    for param in params {
        if let Some(ty) = &mut param.ty {
            substitute_type(ty, types)?;
        }
        if let Some(default) = &mut param.default {
            substitute_expr(default, exprs, types, patterns)?;
        }
    }
    Ok(())
}

fn substitute_types(types: &mut [Type], holes: &mut [Option<Type>]) -> Result<(), String> {
    for ty in types {
        substitute_type(ty, holes)?;
    }
    Ok(())
}

fn substitute_type(ty: &mut Type, holes: &mut [Option<Type>]) -> Result<(), String> {
    if let Type::Named(name, args) = ty {
        if args.is_empty()
            && let Some(index) = marker_index(name, QUOTE_TYPE_HOLE_PREFIX)
        {
            *ty = take_hole(holes, index, "type")?;
            return Ok(());
        }
    }
    match ty {
        Type::Named(_, args) | Type::Tuple(args) => substitute_types(args, holes),
        Type::Fn(params, ret, _) => {
            substitute_types(params, holes)?;
            substitute_type(ret, holes)
        }
        Type::Qualified(_, inner) => substitute_type(inner, holes),
    }
}

fn substitute_pattern(pattern: &mut Pattern, holes: &mut [Option<Pattern>]) -> Result<(), String> {
    if let Pattern::Var(name) = pattern
        && let Some(index) = marker_index(name, QUOTE_PATTERN_HOLE_PREFIX)
    {
        *pattern = take_hole(holes, index, "pattern")?;
        return Ok(());
    }
    match pattern {
        Pattern::Ctor { args, .. }
        | Pattern::AnonCtor { args, .. }
        | Pattern::Tuple(args)
        | Pattern::Or(args) => {
            for arg in args {
                substitute_pattern(arg, holes)?;
            }
        }
        Pattern::List { elems, .. } => {
            for elem in elems {
                substitute_pattern(elem, holes)?;
            }
        }
        Pattern::Wildcard
        | Pattern::Var(_)
        | Pattern::Int(_)
        | Pattern::Str(_)
        | Pattern::Bool(_)
        | Pattern::Duration(_)
        | Pattern::IntRange { .. } => {}
    }
    Ok(())
}

fn substitute_block(
    block: &mut Block,
    exprs: &mut [Option<Expr>],
    types: &mut [Option<Type>],
    patterns: &mut [Option<Pattern>],
) -> Result<(), String> {
    if let Some(ty) = block.region.as_mut().and_then(|region| region.ty.as_mut()) {
        substitute_type(ty, types)?;
    }
    for stmt in &mut block.stmts {
        match stmt {
            Stmt::Let { ty, value, .. } => {
                if let Some(ty) = ty {
                    substitute_type(ty, types)?;
                }
                substitute_expr(value, exprs, types, patterns)?;
            }
            Stmt::Assign { value, .. } | Stmt::Yield(value) | Stmt::Expr(value) => {
                substitute_expr(value, exprs, types, patterns)?;
            }
            Stmt::LetPattern { pattern, value } => {
                substitute_pattern(pattern, patterns)?;
                substitute_expr(value, exprs, types, patterns)?;
            }
            Stmt::Return(Some(value)) => substitute_expr(value, exprs, types, patterns)?,
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn substitute_expr(
    expr: &mut Expr,
    holes: &mut [Option<Expr>],
    types: &mut [Option<Type>],
    patterns: &mut [Option<Pattern>],
) -> Result<(), String> {
    if let Expr::Var(name) = expr
        && let Some(index) = marker_index(name, QUOTE_EXPR_HOLE_PREFIX)
    {
        *expr = take_hole(holes, index, "expression")?;
        return Ok(());
    }
    match expr {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
        Expr::List(values)
        | Expr::Tuple(values)
        | Expr::Call { args: values, .. }
        | Expr::Ctor { args: values, .. }
        | Expr::AnonCtor { args: values, .. } => {
            for value in values {
                substitute_expr(value, holes, types, patterns)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, value) in args {
                substitute_expr(value, holes, types, patterns)?;
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            substitute_expr(receiver, holes, types, patterns)?;
            for arg in args {
                substitute_expr(arg, holes, types, patterns)?;
            }
        }
        Expr::Apply { func, args } => {
            substitute_expr(func, holes, types, patterns)?;
            for arg in args {
                substitute_expr(arg, holes, types, patterns)?;
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::Field { base: expr, .. } => {
            substitute_expr(expr, holes, types, patterns)?;
        }
        Expr::As { expr, ty } => {
            substitute_expr(expr, holes, types, patterns)?;
            substitute_type(ty, types)?;
        }
        Expr::Lambda { params, body, ret } => {
            substitute_params(params, holes, types, patterns)?;
            if let Some(ret) = ret {
                substitute_type(ret, types)?;
            }
            substitute_block(body, holes, types, patterns)?;
        }
        Expr::Block(block) => substitute_block(block, holes, types, patterns)?,
        Expr::RecordUpdate { base, fields, .. } => {
            substitute_expr(base, holes, types, patterns)?;
            for (_, value) in fields {
                substitute_expr(value, holes, types, patterns)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                substitute_expr(value, holes, types, patterns)?;
            }
            if let Some(spread) = spread {
                substitute_expr(spread, holes, types, patterns)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            substitute_expr(lhs, holes, types, patterns)?;
            substitute_expr(rhs, holes, types, patterns)?;
        }
        Expr::If { cond, then_block, else_block } => {
            substitute_expr(cond, holes, types, patterns)?;
            substitute_block(then_block, holes, types, patterns)?;
            if let Some(else_block) = else_block {
                substitute_block(else_block, holes, types, patterns)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            substitute_expr(scrutinee, holes, types, patterns)?;
            for arm in arms {
                substitute_pattern(&mut arm.pattern, patterns)?;
                if let Some(guard) = &mut arm.guard {
                    substitute_expr(guard, holes, types, patterns)?;
                }
                substitute_expr(&mut arm.body, holes, types, patterns)?;
            }
        }
        Expr::While { cond, body } => {
            substitute_expr(cond, holes, types, patterns)?;
            substitute_block(body, holes, types, patterns)?;
        }
        Expr::For { iter, body, .. } => {
            substitute_expr(iter, holes, types, patterns)?;
            substitute_block(body, holes, types, patterns)?;
        }
        Expr::Range { lo, hi, .. } => {
            substitute_expr(lo, holes, types, patterns)?;
            substitute_expr(hi, holes, types, patterns)?;
        }
        Expr::Index { base, index } => {
            substitute_expr(base, holes, types, patterns)?;
            substitute_expr(index, holes, types, patterns)?;
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            substitute_pattern(pattern, patterns)?;
            substitute_expr(scrutinee, holes, types, patterns)?;
            substitute_block(body, holes, types, patterns)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_mixed_item_holes_without_reparsing_the_item() {
        let module = parse_module(
            "fn generated(x: __witchy_quote_type_hole_0):\n    match x:\n        __witchy_quote_pattern_hole_0 -> __witchy_quote_expr_hole_0\n",
        )
        .expect("parse template");
        let item = instantiate_item(
            &module.items[0],
            vec![
                ItemSyntaxHole::Type(Type::Named("Int".into(), Vec::new())),
                ItemSyntaxHole::Pattern(Pattern::Int(1)),
                ItemSyntaxHole::Expr(Expr::Int(7)),
            ],
        )
        .expect("substitute holes");
        let source = crate::format::module(&crate::ast::Module {
            modes: Vec::new(),
            imports: Vec::new(),
            from_imports: Vec::new(),
            items: vec![item],
            import_lines: Vec::new(),
            item_lines: vec![0],
            compiler_item_syntax: Vec::new(),
        }, &[]);
        assert!(source.contains("x: Int"), "{source}");
        assert!(source.contains("1 -> 7"), "{source}");
    }

    #[test]
    fn syntax_payloads_reject_category_escape() {
        assert!(parse_expr_payload("1\n2").is_err());
        assert!(parse_type_payload("Int\nfn injected():\n    0").is_err());
        assert!(parse_pattern_payload("_ -> 1\n        _").is_err());
    }
}
