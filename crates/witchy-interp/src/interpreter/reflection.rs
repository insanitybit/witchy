use std::collections::HashMap;

use witchy_syntax::ast::*;
use witchy_syntax::origin::SyntaxCategory;
use witchy_syntax::parser::parse_module;

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
        ("TypeSyntax", [Value::Str(source)]) => {
            witchy_syntax::syntax_holes::parse_type_payload(source)
                .map_err(|message| RuntimeError { message })
        }
        ("CompilerTypeSyntax", _) => err("CompilerTypeSyntax carried an invalid payload"),
        ("TypeSyntax", _) => err("TypeSyntax carried an invalid source payload"),
        (other, _) => err(format!("meta.stmt_let expected TypeSyntax, got `{other}`")),
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
        ("StmtSyntax", [Value::Str(source)]) => {
            let body = source.replace('\n', "\n    ");
            let module = parse_module(&format!(
                "fn __witchy_meta_stmt_payload():\n    {body}\n"
            ))
            .map_err(|error| RuntimeError {
                message: format!("invalid StmtSyntax payload: {error}"),
            })?;
            let [Item::Function(function)] = module.items.as_slice() else {
                return err("invalid StmtSyntax payload: expected one function wrapper");
            };
            let [stmt] = function.body.stmts.as_slice() else {
                return err("invalid StmtSyntax payload: expected exactly one statement");
            };
            Ok(stmt.clone())
        }
        ("CompilerStmtSyntax", _) => err("CompilerStmtSyntax carried an invalid payload"),
        ("StmtSyntax", _) => err("StmtSyntax carried an invalid source payload"),
        (other, _) => err(format!("meta.block expected StmtSyntax, got `{other}`")),
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
        ("PatternSyntax", [Value::Str(source)]) => {
            witchy_syntax::syntax_holes::parse_pattern_payload(source)
                .map_err(|message| RuntimeError { message })
        }
        ("CompilerPatternSyntax", _) => err("CompilerPatternSyntax carried an invalid payload"),
        ("PatternSyntax", _) => err("PatternSyntax carried an invalid source payload"),
        (other, _) => err(format!("meta.match_arm expected PatternSyntax, got `{other}`")),
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
            Value::Ctor { name, fields }
                if tail(name) == "MatchArmSyntax" && matches!(fields.as_slice(), [Value::Str(_)]) =>
            {
                let Value::Str(source) = &fields[0] else { unreachable!() };
                let source = source.replace('\n', "\n    ");
                let expr = witchy_syntax::syntax_holes::parse_expr_payload(&format!(
                    "match 0:\n    {source}"
                ))
                .map_err(|message| RuntimeError { message })?;
                let Expr::Match { arms, .. } = expr else {
                    return err("meta.expr_match failed to parse a compatibility arm");
                };
                let [arm] = arms.as_slice() else {
                    return err("meta.expr_match expected exactly one compatibility arm");
                };
                Ok(arm.clone())
            }
            _ => err("meta.expr_match expected MatchArmSyntax arms"),
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
            Value::Ctor { name, fields }
                if tail(name) == "ParamSyntax" && matches!(fields.as_slice(), [Value::Str(_)]) =>
            {
                let Value::Str(source) = &fields[0] else { unreachable!() };
                let module = parse_module(&format!(
                    "fn __witchy_meta_param_payload({source}):\n    ()\n"
                ))
                .map_err(|error| RuntimeError {
                    message: format!("invalid ParamSyntax payload: {error}"),
                })?;
                let [Item::Function(function)] = module.items.as_slice() else {
                    return err("invalid ParamSyntax payload: expected one function wrapper");
                };
                let [param] = function.params.as_slice() else {
                    return err("invalid ParamSyntax payload: expected exactly one parameter");
                };
                Ok(param.clone())
            }
            _ => err("meta.function_block expected ParamSyntax values"),
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
        ("BlockSyntax", [Value::Str(source)]) => {
            let body = source.replace('\n', "\n    ");
            let module = parse_module(&format!(
                "fn __witchy_meta_block_payload():\n    {body}\n"
            ))
            .map_err(|error| RuntimeError {
                message: format!("invalid BlockSyntax payload: {error}"),
            })?;
            let [Item::Function(function)] = module.items.as_slice() else {
                return err("invalid BlockSyntax payload: expected one function wrapper");
            };
            Ok(function.body.clone())
        }
        ("CompilerBlockSyntax", _) => err("CompilerBlockSyntax carried an invalid payload"),
        ("BlockSyntax", _) => err("BlockSyntax carried an invalid source payload"),
        (other, _) => err(format!(
            "meta.function_block expected BlockSyntax, got `{other}`"
        )),
    }
}

pub(super) fn compiler_item_holes(
    values: &[Value],
    compiler_expr_syntax: &HashMap<String, Expr>,
    compiler_type_syntax: &HashMap<String, Type>,
    compiler_pattern_syntax: &HashMap<String, Pattern>,
) -> Result<Vec<witchy_syntax::syntax_holes::ItemSyntaxHole>, RuntimeError> {
    use witchy_syntax::syntax_holes::{
        ItemSyntaxHole, parse_expr_payload, parse_pattern_payload, parse_type_payload,
    };

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
                    _ => parse_type_payload(source(syntax, "TypeSyntax")?)
                        .map(ItemSyntaxHole::Type)
                        .map_err(|message| RuntimeError { message }),
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
                    _ => parse_pattern_payload(source(syntax, "PatternSyntax")?)
                        .map(ItemSyntaxHole::Pattern)
                        .map_err(|message| RuntimeError { message }),
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
                "TypeSyntax" => {
                    let [Value::Str(source)] = fields.as_slice() else {
                        return err("TypeSyntax carried an invalid source payload");
                    };
                    witchy_syntax::syntax_holes::parse_type_payload(source)
                        .map_err(|message| RuntimeError { message })
                }
                other => err(format!(
                    "compiler-owned type hole carried `{other}`, expected TypeSyntax"
                )),
            }
        })
        .collect()
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
            Ok(Type::Named(name.to_string(), args))
        }
        ("TTuple", [Value::List(items)]) => {
            let items = items
                .iter()
                .map(compiler_reflected_type)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Type::Tuple(items))
        }
        ("TFn", [Value::List(params), ret, conventions]) => {
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
            Ok(Type::Fn(params, Box::new(ret), conventions))
        }
        ("TQualified", [Value::Str(qualifier), inner]) => {
            let qualifier = match qualifier.as_str() {
                "frozen" => TypeQual::Frozen,
                "unique" => TypeQual::Unique,
                "local unique" => TypeQual::LocalUnique,
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
        (kind, _) if matches!(kind, "TNamed" | "TTuple" | "TFn" | "TQualified") => {
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
                "PatternSyntax" => {
                    let [Value::Str(source)] = fields.as_slice() else {
                        return err("PatternSyntax carried an invalid source payload");
                    };
                    witchy_syntax::syntax_holes::parse_pattern_payload(source)
                        .map_err(|message| RuntimeError { message })
                }
                other => err(format!(
                    "compiler-owned pattern hole carried `{other}`, expected PatternSyntax"
                )),
            }
        })
        .collect()
}

pub(super) fn compiler_ctor_tail(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(_, tail)| tail)
}
