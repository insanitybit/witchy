use std::collections::HashMap;

use witchy_syntax::ast::*;
use witchy_syntax::origin::SyntaxCategory;

use super::{err, ComptimeHoleOrigin, ComptimeSyntaxOrigin, RuntimeError, Value};

pub(super) fn compiler_expr_leaf(kind: &str, payload: &str) -> Result<Expr, RuntimeError> {
    if kind == "name" && payload.chars().next().is_some_and(char::is_uppercase) {
        Ok(Expr::Ctor { name: payload.to_string(), args: Vec::new() })
    } else if kind == "name" {
        Ok(Expr::Var(payload.to_string()))
    } else if kind == "int" {
        Ok(Expr::Int(payload.parse().map_err(|_| RuntimeError {
            message: "meta.expr_int carried an invalid Int payload".into(),
        })?))
    } else if kind == "bool" {
        Ok(Expr::Bool(payload.parse().map_err(|_| RuntimeError {
            message: "meta.expr_bool carried an invalid Bool payload".into(),
        })?))
    } else if kind == "string" {
        Ok(Expr::Str(payload.to_string()))
    } else {
        err(format!("unknown compiler-owned expression leaf `{kind}`"))
    }
}

pub(super) fn compiler_pattern_leaf(
    kind: &str,
    first: &str,
    second: &str,
    inclusive: bool,
) -> Result<Pattern, RuntimeError> {
    let parse_int = |payload: &str, operation: &str| {
        payload.parse::<i64>().map_err(|_| RuntimeError {
            message: format!("{operation} carried an invalid Int payload"),
        })
    };
    if kind == "var" {
        Ok(Pattern::Var(first.to_string()))
    } else if kind == "call-site-var" {
        err("meta.pattern_var: meta.call_site is reference-only")
    } else if kind == "wildcard" {
        Ok(Pattern::Wildcard)
    } else if kind == "int" {
        Ok(Pattern::Int(parse_int(first, "meta.pattern_int")?))
    } else if kind == "bool" {
        Ok(Pattern::Bool(first.parse().map_err(|_| RuntimeError {
            message: "meta.pattern_bool carried an invalid Bool payload".into(),
        })?))
    } else if kind == "string" {
        Ok(Pattern::Str(first.to_string()))
    } else if kind == "duration" {
        Ok(Pattern::Duration(parse_int(first, "meta.pattern_duration_ms")?))
    } else if kind == "range" {
        Ok(Pattern::IntRange {
            lo: parse_int(first, "meta.pattern_range")?,
            hi: parse_int(second, "meta.pattern_range")?,
            inclusive,
        })
    } else {
        err(format!("unknown compiler-owned pattern leaf `{kind}`"))
    }
}

pub(super) fn compiler_stmt_leaf(kind: &str) -> Result<Stmt, RuntimeError> {
    if kind == "return" {
        Ok(Stmt::Return(None))
    } else {
        err(format!("unknown compiler-owned statement leaf `{kind}`"))
    }
}

/// Decode one `meta.ExprSyntax` value for compiler-owned structural builders.
/// A compatibility payload is parsed in isolation; an owned payload transfers
/// its AST directly so definition-site and call-site markers cannot be erased
/// by an intermediate source projection.
pub(super) fn compiler_expr_syntax_value(
    value: &Value,
    compiler_expr_syntax: &HashMap<String, Expr>,
) -> Result<Expr, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::Ctor { name, fields } = value else {
        return err("meta.expr_call expected ExprSyntax values");
    };
    match (tail(name), fields.as_slice()) {
        ("CompilerExprSyntax", [Value::Str(handle), Value::Str(_source)]) => compiler_expr_syntax
            .get(handle.as_str())
            .cloned()
            .ok_or_else(|| RuntimeError {
                message: "CompilerExprSyntax carried an invalid syntax handle".into(),
            }),
        ("ExprSyntax", [Value::Str(source)]) => {
            witchy_syntax::syntax_holes::parse_expr_payload(source).map_err(|message| RuntimeError { message })
        }
        ("CompilerExprSyntax", _) => err("CompilerExprSyntax carried an invalid payload"),
        ("ExprSyntax", _) => err("ExprSyntax carried an invalid source payload"),
        (other, _) => err(format!("meta.expr_call expected ExprSyntax, got `{other}`")),
    }
}

/// Decode only an interpreter-issued expression handle. Structural tagged
/// literals deliberately exclude the legacy source-backed `ExprSyntax` shape:
/// their interpolation payloads must remain owned AST nodes end to end.
pub(super) fn compiler_owned_expr_syntax_value(
    value: &Value,
    compiler_expr_syntax: &HashMap<String, Expr>,
    operation: &str,
) -> Result<Expr, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::Ctor { name, fields } = value else {
        return err(format!("{operation} expects compiler-owned ExprSyntax holes"));
    };
    match (tail(name), fields.as_slice()) {
        ("CompilerExprSyntax", [Value::Str(handle), Value::Str(_source)]) => compiler_expr_syntax
            .get(handle.as_str())
            .cloned()
            .ok_or_else(|| RuntimeError {
                message: "CompilerExprSyntax carried an invalid syntax handle".into(),
            }),
        ("CompilerExprSyntax", _) => err("CompilerExprSyntax carried an invalid payload"),
        _ => err(format!("{operation} expects compiler-owned ExprSyntax holes")),
    }
}

pub(super) fn compiler_type_syntax_value(
    value: &Value,
    compiler_type_syntax: &HashMap<String, Type>,
) -> Result<Type, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::Ctor { name, fields } = value else {
        return err("meta.stmt_let expected TypeSyntax");
    };
    match (tail(name), fields.as_slice()) {
        ("CompilerTypeSyntax", [Value::Str(handle), Value::Str(_source)]) => {
            compiler_type_syntax.get(handle.as_str()).cloned().ok_or_else(|| RuntimeError {
                message: "CompilerTypeSyntax carried an invalid syntax handle".into(),
            })
        }
        ("CompilerTypeSyntax", _) => err("CompilerTypeSyntax carried an invalid payload"),
        _ => err("meta.stmt_let expected compiler-owned TypeSyntax"),
    }
}

pub(super) fn compiler_optional_type_syntax_value(
    value: &Value,
    compiler_type_syntax: &HashMap<String, Type>,
) -> Result<Option<Type>, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    match value {
        Value::Ctor { name, fields } if tail(name) == "Some" && fields.len() == 1 => {
            Ok(Some(compiler_type_syntax_value(
                &fields[0],
                compiler_type_syntax,
            )?))
        }
        Value::Ctor { name, fields } if tail(name) == "None" && fields.is_empty() => Ok(None),
        _ => err("meta.stmt_let expected Option(TypeSyntax) annotation"),
    }
}

pub(super) fn compiler_function_conventions(
    value: &Value,
    parameter_count: usize,
    operation: &str,
) -> Result<Vec<Convention>, RuntimeError> {
    let Value::List(conventions) = value else {
        return err(format!("{operation} expected List(String) conventions"));
    };
    if conventions.is_empty() {
        return Ok(vec![Convention::Let; parameter_count]);
    }
    if conventions.len() != parameter_count {
        return err(format!(
            "{operation} expected {parameter_count} conventions, got {}",
            conventions.len()
        ));
    }
    conventions
        .iter()
        .map(|convention| match convention {
            Value::Str(name) if name.is_empty() || name.as_str() == "let" => {
                Ok(Convention::Let)
            }
            Value::Str(name) if name.as_str() == "borrow" => Ok(Convention::Borrow),
            Value::Str(name) if name.as_str() == "var" => Ok(Convention::Var),
            Value::Str(name) if name.as_str() == "own" => Ok(Convention::Own),
            Value::Str(name) => err(format!(
                "{operation} unknown parameter convention `{name}`"
            )),
            _ => err(format!("{operation} expected String conventions")),
        })
        .collect()
}

pub(super) fn compiler_stmt_syntax_value(
    value: &Value,
    compiler_stmt_syntax: &HashMap<String, Stmt>,
) -> Result<Stmt, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::Ctor { name, fields } = value else {
        return err("meta.block expected StmtSyntax values");
    };
    match (tail(name), fields.as_slice()) {
        ("CompilerStmtSyntax", [Value::Str(handle), Value::Str(_source)]) => {
            compiler_stmt_syntax.get(handle.as_str()).cloned().ok_or_else(|| RuntimeError {
                message: "CompilerStmtSyntax carried an invalid syntax handle".into(),
            })
        }
        ("CompilerStmtSyntax", _) => err("CompilerStmtSyntax carried an invalid payload"),
        _ => err("meta.block expected compiler-owned StmtSyntax values"),
    }
}

pub(super) fn compiler_optional_expr_syntax_value(
    value: &Value,
    compiler_expr_syntax: &HashMap<String, Expr>,
) -> Result<Option<Expr>, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    match value {
        Value::Ctor { name, fields } if tail(name) == "Some" && fields.len() == 1 => {
            Ok(Some(compiler_expr_syntax_value(
                &fields[0],
                compiler_expr_syntax,
            )?))
        }
        Value::Ctor { name, fields } if tail(name) == "None" && fields.is_empty() => Ok(None),
        _ => err("meta.block expected Option(ExprSyntax) tail"),
    }
}

pub(super) fn compiler_ident_name(value: &Value, operation: &str) -> Result<String, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    match value {
        Value::Ctor { name, fields }
            if matches!(tail(name), "Ident" | "CallSiteIdent") && matches!(fields.as_slice(), [Value::Str(_)]) =>
        {
            let Value::Str(name) = &fields[0] else { unreachable!() };
            Ok(name.to_string())
        }
        _ => err(format!("{operation} expected an Ident field name")),
    }
}

pub(super) fn compiler_binding_ident_name(value: &Value, operation: &str) -> Result<String, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    match value {
        Value::Ctor { name, fields }
            if tail(name) == "Ident" && matches!(fields.as_slice(), [Value::Str(_)]) =>
        {
            let Value::Str(name) = &fields[0] else { unreachable!() };
            Ok(name.to_string())
        }
        Value::Ctor { name, .. } if tail(name) == "CallSiteIdent" => err(format!(
            "{operation} requires a binding identifier; meta.call_site is reference-only"
        )),
        _ => err(format!("{operation} expected an Ident binding name")),
    }
}

pub(super) fn compiler_definition_ident_name(
    value: &Value,
    operation: &str,
) -> Result<String, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    match value {
        Value::Ctor { name, fields }
            if tail(name) == "Ident" && matches!(fields.as_slice(), [Value::Str(_)]) =>
        {
            let Value::Str(name) = &fields[0] else { unreachable!() };
            Ok(name.to_string())
        }
        Value::Ctor { name, .. } if tail(name) == "CallSiteIdent" => err(format!(
            "{operation} requires a definition-site identifier; meta.call_site is reference-only"
        )),
        _ => err(format!("{operation} expected an Ident name")),
    }
}

pub(super) fn compiler_pattern_syntax_value(
    value: &Value,
    compiler_pattern_syntax: &HashMap<String, Pattern>,
) -> Result<Pattern, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::Ctor { name, fields } = value else {
        return err("meta.match_arm expected PatternSyntax");
    };
    match (tail(name), fields.as_slice()) {
        ("CompilerPatternSyntax", [Value::Str(handle), Value::Str(_source)]) => {
            compiler_pattern_syntax.get(handle.as_str()).cloned().ok_or_else(|| RuntimeError {
                message: "CompilerPatternSyntax carried an invalid syntax handle".into(),
            })
        }
        ("CompilerPatternSyntax", _) => err("CompilerPatternSyntax carried an invalid payload"),
        _ => err("meta.match_arm expected compiler-owned PatternSyntax"),
    }
}

pub(super) fn compiler_match_arms(
    value: &Value,
    compiler_match_arm_syntax: &HashMap<String, MatchArm>,
) -> Result<Vec<MatchArm>, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::List(arms) = value else {
        return err("meta.expr_match expected List(MatchArmSyntax) arms");
    };
    arms.iter()
        .map(|arm| match arm {
            Value::Ctor { name, fields }
                if tail(name) == "CompilerMatchArmSyntax"
                    && matches!(fields.as_slice(), [Value::Str(_), Value::Str(_)]) =>
            {
                let Value::Str(handle) = &fields[0] else { unreachable!() };
                compiler_match_arm_syntax.get(handle.as_str()).cloned().ok_or_else(|| {
                    RuntimeError {
                        message: "CompilerMatchArmSyntax carried an invalid syntax handle".into(),
                    }
                })
            }
            _ => err("meta.expr_match expected compiler-owned MatchArmSyntax arms"),
        })
        .collect()
}

pub(super) fn compiler_params(
    value: &Value,
    compiler_param_syntax: &HashMap<String, Param>,
) -> Result<Vec<Param>, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::List(params) = value else {
        return err("meta.function_block expected List(ParamSyntax)");
    };
    params
        .iter()
        .map(|param| match param {
            Value::Ctor { name, fields }
                if tail(name) == "CompilerParamSyntax"
                    && matches!(fields.as_slice(), [Value::Str(_), Value::Str(_)]) =>
            {
                let Value::Str(handle) = &fields[0] else { unreachable!() };
                compiler_param_syntax.get(handle.as_str()).cloned().ok_or_else(|| {
                    RuntimeError {
                        message: "CompilerParamSyntax carried an invalid syntax handle".into(),
                    }
                })
            }
            _ => err("meta.function_block expected compiler-owned ParamSyntax values"),
        })
        .collect()
}

pub(super) fn compiler_block_syntax_value(
    value: &Value,
    compiler_block_syntax: &HashMap<String, Block>,
) -> Result<Block, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::Ctor { name, fields } = value else {
        return err("meta.function_block expected a BlockSyntax body");
    };
    match (tail(name), fields.as_slice()) {
        ("CompilerBlockSyntax", [Value::Str(handle), Value::Str(_source)]) => {
            compiler_block_syntax.get(handle.as_str()).cloned().ok_or_else(|| RuntimeError {
                message: "CompilerBlockSyntax carried an invalid syntax handle".into(),
            })
        }
        ("CompilerBlockSyntax", _) => err("CompilerBlockSyntax carried an invalid payload"),
        _ => err("meta.function_block expected compiler-owned BlockSyntax values"),
    }
}

pub(super) fn compiler_item_holes(
    values: &[Value],
    compiler_expr_syntax: &HashMap<String, Expr>,
    compiler_type_syntax: &HashMap<String, Type>,
    compiler_pattern_syntax: &HashMap<String, Pattern>,
) -> Result<Vec<witchy_syntax::syntax_holes::ItemSyntaxHole>, RuntimeError> {
    use witchy_syntax::syntax_holes::{ItemSyntaxHole, parse_expr_payload};

    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    fn source<'a>(value: &'a Value, expected: &str) -> Result<&'a str, RuntimeError> {
        let Value::Ctor { name, fields } = value else {
            return err(format!("{expected} hole carried a non-syntax value"));
        };
        if tail(name) != expected {
            return err(format!("{expected} hole carried `{}`", tail(name)));
        }
        let [Value::Str(source)] = fields.as_slice() else {
            return err(format!("{expected} carried an invalid source payload"));
        };
        Ok(source)
    }

    values
        .iter()
        .map(|value| {
            let Value::Ctor { name, fields } = value else {
                return err("compiler-owned item hole was not a meta.SyntaxHole");
            };
            let [syntax] = fields.as_slice() else {
                return err("compiler-owned item hole carried an invalid payload");
            };
            match tail(name) {
                "ExprHole" => match syntax {
                    Value::Ctor { name, fields } if tail(name) == "CompilerExprSyntax" => {
                        let [Value::Str(handle), Value::Str(_source)] = fields.as_slice() else {
                            return err("CompilerExprSyntax carried an invalid payload");
                        };
                        compiler_expr_syntax
                            .get(handle.as_str())
                            .cloned()
                            .map(ItemSyntaxHole::Expr)
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned expression referenced an invalid syntax handle"
                                    .into(),
                            })
                    }
                    _ => parse_expr_payload(source(syntax, "ExprSyntax")?)
                        .map(ItemSyntaxHole::Expr)
                        .map_err(|message| RuntimeError { message }),
                },
                "TypeHole" => match syntax {
                    Value::Ctor { name, fields } if tail(name) == "CompilerTypeSyntax" => {
                        let [Value::Str(handle), Value::Str(_source)] = fields.as_slice() else {
                            return err("CompilerTypeSyntax carried an invalid payload");
                        };
                        compiler_type_syntax
                            .get(handle.as_str())
                            .cloned()
                            .map(ItemSyntaxHole::Type)
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned type referenced an invalid syntax handle"
                                    .into(),
                            })
                    }
                    _ => err("compiler-owned item hole expected compiler-owned TypeSyntax"),
                },
                "PatternHole" => match syntax {
                    Value::Ctor { name, fields } if tail(name) == "CompilerPatternSyntax" => {
                        let [Value::Str(handle), Value::Str(_source)] = fields.as_slice() else {
                            return err("CompilerPatternSyntax carried an invalid payload");
                        };
                        compiler_pattern_syntax
                            .get(handle.as_str())
                            .cloned()
                            .map(ItemSyntaxHole::Pattern)
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned pattern referenced an invalid syntax handle"
                                    .into(),
                            })
                    }
                    _ => err("compiler-owned item hole expected compiler-owned PatternSyntax"),
                },
                other => err(format!("compiler-owned item hole had unknown category `{other}`")),
            }
        })
        .collect()
}

pub(super) fn compiler_item_hole_origins(
    values: &[Value],
    expr_origins: &HashMap<String, ComptimeSyntaxOrigin>,
    type_origins: &HashMap<String, ComptimeSyntaxOrigin>,
    pattern_origins: &HashMap<String, ComptimeSyntaxOrigin>,
    invocation_line: u32,
) -> Vec<ComptimeHoleOrigin> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    values
        .iter()
        .flat_map(|value| {
            let Value::Ctor { name, fields } = value else {
                return Vec::new();
            };
            let [syntax] = fields.as_slice() else {
                return Vec::new();
            };
            let (category, compiler_ctor, origins) = match tail(name) {
                "ExprHole" => (SyntaxCategory::Expr, "CompilerExprSyntax", expr_origins),
                "TypeHole" => (SyntaxCategory::Type, "CompilerTypeSyntax", type_origins),
                "PatternHole" => (
                    SyntaxCategory::Pattern,
                    "CompilerPatternSyntax",
                    pattern_origins,
                ),
                _ => return Vec::new(),
            };
            syntax_hole_origin(category, syntax, compiler_ctor, origins, invocation_line)
        })
        .collect()
}

pub(super) fn compiler_direct_hole_origins(
    values: &[Value],
    category: SyntaxCategory,
    compiler_ctor: &str,
    origins: &HashMap<String, ComptimeSyntaxOrigin>,
    invocation_line: u32,
) -> Vec<ComptimeHoleOrigin> {
    values
        .iter()
        .flat_map(|syntax| {
            syntax_hole_origin(category, syntax, compiler_ctor, origins, invocation_line)
        })
        .collect()
}

fn syntax_hole_origin(
    category: SyntaxCategory,
    syntax: &Value,
    compiler_ctor: &str,
    origins: &HashMap<String, ComptimeSyntaxOrigin>,
    invocation_line: u32,
) -> Vec<ComptimeHoleOrigin> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let syntax_origin = match syntax {
        Value::Ctor { name, fields } if tail(name) == compiler_ctor => {
            let [Value::Str(handle), ..] = fields.as_slice() else {
                return Vec::new();
            };
            origins.get(handle.as_str())
        }
        _ => None,
    };
    let mut ancestry = vec![ComptimeHoleOrigin {
        category,
        definition_line: syntax_origin.map_or(0, |origin| origin.definition_line),
        invocation_line,
    }];
    if let Some(origin) = syntax_origin {
        ancestry.extend(origin.hole_ancestry.iter().cloned());
    }
    ancestry
}

pub(super) fn compiler_type_holes(
    values: &[Value],
    compiler_type_syntax: &HashMap<String, Type>,
) -> Result<Vec<Type>, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    values
        .iter()
        .map(|value| {
            let Value::Ctor { name, fields } = value else {
                return err("compiler-owned type hole was not meta.TypeSyntax");
            };
            match tail(name) {
                "CompilerTypeSyntax" => {
                    let [Value::Str(handle), Value::Str(_source)] = fields.as_slice() else {
                        return err("CompilerTypeSyntax carried an invalid payload");
                    };
                    compiler_type_syntax
                        .get(handle.as_str())
                        .cloned()
                        .ok_or_else(|| RuntimeError {
                            message: "compiler-owned type referenced an invalid syntax handle"
                                .into(),
                        })
                }
                _ => err("compiler-owned type hole expected compiler-owned TypeSyntax"),
            }
        })
        .collect()
}

/// Typed builders must reject the retired two-argument view spelling. Ordinary
/// rooted views use `&T` or `&[T]`, without a written lifetime.
pub(super) fn validate_compiler_named_type(name: &str, args: &[Type]) -> Result<(), RuntimeError> {
    if name == "View" && args.len() == 2
        && let Type::Named(lifetime, nested) = &args[1]
        && nested.is_empty() && lifetime.starts_with('\'')
    {
        return err("`View(T, 'a)` is retired; use `&T` (or `&[T]` for a slice view)");
    }
    Ok(())
}

pub(super) fn compiler_reflected_type(value: &Value) -> Result<Type, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::Ctor { name, fields } = value else {
        return err("meta.type_expr expected TypeExpr");
    };
    match (tail(name), fields.as_slice()) {
        ("TNamed", [Value::Str(name), Value::List(args)]) => {
            let args = args
                .iter()
                .map(compiler_reflected_type)
                .collect::<Result<Vec<_>, _>>()?;
            validate_compiler_named_type(name, &args)?;
            Ok(Type::Named(name.to_string(), args))
        }
        ("TTuple", [Value::List(items)]) => {
            let items = items
                .iter()
                .map(compiler_reflected_type)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Type::Tuple(items))
        }
        ("TFn", [Value::List(params), ret, conventions, Value::Bool(pure), Value::Bool(once)]) => {
            let params = params
                .iter()
                .map(compiler_reflected_type)
                .collect::<Result<Vec<_>, _>>()?;
            let conventions = compiler_function_conventions(
                conventions,
                params.len(),
                "meta.type_expr function",
            )?;
            let ret = compiler_reflected_type(ret)?;
            Ok(Type::Fn(
                params,
                Box::new(ret),
                conventions,
                CallableQualifiers::new(*pure, *once),
            ))
        }
        ("TQualified", [Value::Str(qualifier), inner]) => {
            let qualifier = match qualifier.as_str() {
                "frozen" => TypeQual::Frozen,
                "unique" => TypeQual::Unique,
                other => {
                    return err(format!(
                        "meta.type_expr unknown qualifier `{other}`"
                    ));
                }
            };
            Ok(Type::Qualified(
                qualifier,
                Box::new(compiler_reflected_type(inner)?),
            ))
        }
        ("TView", [inner]) => Ok(Type::Qualified(
            TypeQual::View,
            Box::new(compiler_reflected_type(inner)?),
        )),
        (kind, _)
            if matches!(
                kind,
                "TNamed" | "TTuple" | "TFn" | "TQualified" | "TView"
            ) => {
            err(format!("meta.type_expr `{kind}` carried an invalid payload"))
        }
        (kind, _) => err(format!("meta.type_expr expected TypeExpr, got `{kind}`")),
    }
}

pub(super) fn compiler_pattern_holes(
    values: &[Value],
    compiler_pattern_syntax: &HashMap<String, Pattern>,
) -> Result<Vec<Pattern>, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    values
        .iter()
        .map(|value| {
            let Value::Ctor { name, fields } = value else {
                return err("compiler-owned pattern hole was not meta.PatternSyntax");
            };
            match tail(name) {
                "CompilerPatternSyntax" => {
                    let [Value::Str(handle), Value::Str(_source)] = fields.as_slice() else {
                        return err("CompilerPatternSyntax carried an invalid payload");
                    };
                    compiler_pattern_syntax
                        .get(handle.as_str())
                        .cloned()
                        .ok_or_else(|| RuntimeError {
                            message: "compiler-owned pattern referenced an invalid syntax handle"
                                .into(),
                        })
                }
                _ => err("compiler-owned pattern hole expected compiler-owned PatternSyntax"),
            }
        })
        .collect()
}

pub(super) fn compiler_ctor_tail(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(_, tail)| tail)
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use crate::interpreter::Interpreter;

    use super::*;

    #[test]
    fn retired_local_unique_reflection_is_rejected_recursively() {
        let retired = Value::Ctor {
            name: "meta.TQualified".into(),
            fields: Rc::new(vec![
                Value::Str(Rc::new("local unique".into())),
                Value::Ctor {
                    name: "meta.TNamed".into(),
                    fields: Rc::new(vec![
                        Value::Str(Rc::new("String".into())),
                        Value::List(Rc::new(Vec::new())),
                    ]),
                },
            ]),
        };
        let nested = Value::Ctor {
            name: "meta.TTuple".into(),
            fields: Rc::new(vec![Value::List(Rc::new(vec![retired.clone()]))]),
        };
        for reflected in [retired, nested] {
            let error = compiler_reflected_type(&reflected).expect_err("retired qualifier");
            assert!(error.to_string().contains("unknown qualifier `local unique`"), "{error}");
        }
    }

    #[test]
    fn retired_local_unique_typed_builder_is_rejected_before_handle_decoding() {
        let module = witchy_syntax::parser::parse_module("").expect("empty module");
        let mut interpreter = Interpreter::new(module);
        interpreter.fresh_ident_scope = Some("retired-qualifier-test".into());
        let error = interpreter.call_builtin(
            "__meta_type_qualified",
            &[Value::Str(Rc::new("local unique".into())), Value::Unit],
        ).expect_err("retired qualifier cannot construct compiler-owned TypeSyntax");
        assert!(error.to_string().contains("unknown qualifier `local unique`"), "{error}");
    }

    #[test]
    fn reflected_view_type_roundtrips_without_lifetime_or_mutability() {
        let reflected = Value::Ctor {
            name: "meta.TView".into(),
            fields: Rc::new(vec![
                Value::Ctor {
                    name: "meta.TNamed".into(),
                    fields: Rc::new(vec![
                        Value::Str(Rc::new("String".into())),
                        Value::List(Rc::new(Vec::new())),
                    ]),
                },
            ]),
        };
        assert_eq!(
            compiler_reflected_type(&reflected).expect("rooted view converts to type syntax"),
            Type::Qualified(
                TypeQual::View,
                Box::new(Type::Named("String".into(), Vec::new())),
            )
        );
    }

    #[test]
    fn nested_aggregate_views_roundtrip_as_views() {
        let named = |name: &str| Value::Ctor {
            name: "meta.TNamed".into(),
            fields: Rc::new(vec![Value::Str(Rc::new(name.into())), Value::List(Rc::new(Vec::new()))]),
        };
        let reflected = Value::Ctor {
            name: "meta.TView".into(),
            fields: Rc::new(vec![Value::Ctor {
                name: "meta.TTuple".into(),
                fields: Rc::new(vec![Value::List(Rc::new(vec![
                    named("Header"),
                    Value::Ctor {
                        name: "meta.TView".into(),
                        fields: Rc::new(vec![Value::Ctor {
                            name: "meta.TNamed".into(),
                            fields: Rc::new(vec![
                                Value::Str(Rc::new("[Int]".into())),
                                Value::List(Rc::new(vec![named("Int")])),
                            ]),
                        }]),
                    },
                ]))]),
            }]),
        };
        assert_eq!(
            compiler_reflected_type(&reflected).expect("nested rooted views convert to type syntax"),
            Type::Qualified(
                TypeQual::View,
                Box::new(Type::Tuple(vec![
                    Type::Named("Header".into(), Vec::new()),
                    Type::Qualified(
                        TypeQual::View,
                        Box::new(Type::Named("[Int]".into(), vec![Type::Named("Int".into(), Vec::new())])),
                    ),
                ])),
            )
        );
    }

    #[test]
    fn legacy_reference_metadata_is_rejected_instead_of_reentering_the_type_ast() {
        let named = Value::Ctor {
            name: "meta.TNamed".into(),
            fields: Rc::new(vec![Value::Str(Rc::new("String".into())), Value::List(Rc::new(Vec::new()))]),
        };
        for reflected in [
            Value::Ctor {
                name: "meta.TBorrowed".into(),
                fields: Rc::new(vec![named.clone(), Value::Str(Rc::new("owner".into()))]),
            },
            Value::Ctor {
                name: "meta.TReference".into(),
                fields: Rc::new(vec![
                    Value::Str(Rc::new("mut".into())),
                    named.clone(),
                    Value::Str(Rc::new("owner".into())),
                ]),
            },
            Value::Ctor {
                name: "meta.TReference".into(),
                fields: Rc::new(vec![
                    Value::Str(Rc::new("shared".into())),
                    named,
                    Value::Str(Rc::new("owner".into())),
                ]),
            },
        ] {
            let error = compiler_reflected_type(&reflected).expect_err("legacy reference metadata must reject");
            assert!(error.to_string().contains("expected TypeExpr"), "{error}");
        }
    }

    #[test]
    fn structural_tagged_holes_reject_source_backed_expr_syntax() {
        let source_backed = Value::Ctor {
            name: "meta.ExprSyntax".into(),
            fields: Rc::new(vec![Value::str("not parsed")]),
        };
        let error = compiler_owned_expr_syntax_value(
            &source_backed,
            &HashMap::new(),
            "meta.expr_tagged",
        )
        .expect_err("structural tagged holes must not parse source payloads");
        assert!(
            error.message.contains("compiler-owned ExprSyntax holes"),
            "{}",
            error.message,
        );
    }

    #[test]
    fn function_parameters_accept_only_compiler_owned_ast_handles() {
        let parameter = Param {
            name: "value".into(),
            ty: Some(Type::Named("Int".into(), Vec::new())),
            convention: Convention::Let,
            default: None,
        };
        let handles = HashMap::from([("parameter-1".into(), parameter.clone())]);
        let owned = Value::List(Rc::new(vec![Value::Ctor {
            name: "meta.CompilerParamSyntax".into(),
            fields: Rc::new(vec![Value::str("parameter-1"), Value::str("value: Int")]),
        }]));
        assert_eq!(compiler_params(&owned, &handles).expect("authenticated parameter"), vec![parameter]);

        let source_backed = Value::List(Rc::new(vec![Value::Ctor {
            name: "meta.ParamSyntax".into(),
            fields: Rc::new(vec![Value::str("value: Int")]),
        }]));
        let error = compiler_params(&source_backed, &handles)
            .expect_err("source-backed parameters cannot be reparsed");
        assert!(error.message.contains("compiler-owned ParamSyntax"), "{error}");
    }

    #[test]
    fn match_arms_reject_source_backed_syntax() {
        let source_backed = Value::List(Rc::new(vec![Value::Ctor {
            name: "meta.MatchArmSyntax".into(),
            fields: Rc::new(vec![Value::str("1 -> 42")]),
        }]));
        let error = compiler_match_arms(&source_backed, &HashMap::new())
            .expect_err("source-backed arms cannot be reparsed");
        assert!(error.message.contains("compiler-owned MatchArmSyntax"), "{error}");
    }

    #[test]
    fn statements_and_blocks_reject_source_backed_syntax() {
        let statement = Value::Ctor {
            name: "meta.StmtSyntax".into(),
            fields: Rc::new(vec![Value::str("let x = 42")]),
        };
        let error = compiler_stmt_syntax_value(&statement, &HashMap::new())
            .expect_err("source-backed statements cannot be reparsed");
        assert!(error.message.contains("compiler-owned StmtSyntax"), "{error}");

        let block = Value::Ctor {
            name: "meta.BlockSyntax".into(),
            fields: Rc::new(vec![Value::str("42")]),
        };
        let error = compiler_block_syntax_value(&block, &HashMap::new())
            .expect_err("source-backed blocks cannot be reparsed");
        assert!(error.message.contains("compiler-owned BlockSyntax"), "{error}");
    }

    #[test]
    fn type_builders_and_item_holes_reject_source_backed_syntax() {
        let type_source = Value::Ctor {
            name: "meta.TypeSyntax".into(),
            fields: Rc::new(vec![Value::str("Int")]),
        };
        let error = compiler_type_syntax_value(&type_source, &HashMap::new())
            .expect_err("source-backed type cannot enter a statement");
        assert!(error.message.contains("compiler-owned TypeSyntax"), "{error}");
        let error = compiler_type_holes(&[type_source.clone()], &HashMap::new())
            .expect_err("source-backed type cannot enter a type builder");
        assert!(error.message.contains("compiler-owned TypeSyntax"), "{error}");
        let hole = Value::Ctor {
            name: "meta.TypeHole".into(),
            fields: Rc::new(vec![type_source]),
        };
        let error = compiler_item_holes(&[hole], &HashMap::new(), &HashMap::new(), &HashMap::new())
            .expect_err("source-backed type cannot enter an item quote");
        assert!(error.message.contains("compiler-owned TypeSyntax"), "{error}");
    }

    #[test]
    fn pattern_builders_and_item_holes_reject_source_backed_syntax() {
        let pattern_source = Value::Ctor {
            name: "meta.PatternSyntax".into(),
            fields: Rc::new(vec![Value::str("Some(42)")]),
        };
        let error = compiler_pattern_syntax_value(&pattern_source, &HashMap::new())
            .expect_err("source-backed pattern cannot enter a match arm");
        assert!(error.message.contains("compiler-owned PatternSyntax"), "{error}");
        let error = compiler_pattern_holes(&[pattern_source.clone()], &HashMap::new())
            .expect_err("source-backed pattern cannot enter a builder");
        assert!(error.message.contains("compiler-owned PatternSyntax"), "{error}");
        let hole = Value::Ctor {
            name: "meta.PatternHole".into(),
            fields: Rc::new(vec![pattern_source]),
        };
        let error = compiler_item_holes(&[hole], &HashMap::new(), &HashMap::new(), &HashMap::new())
            .expect_err("source-backed pattern cannot enter an item quote");
        assert!(error.message.contains("compiler-owned PatternSyntax"), "{error}");
    }
}
