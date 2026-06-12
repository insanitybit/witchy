//! `derive(Show, Eq, Ord)` — compiler-generated trait impls.
//!
//! Comptime enters witchy as ADDITIVE item generation (docs/language-
//! evolution.md Phase 4): the expansion appends `impl` items to the module
//! BEFORE type checking and footprint analysis run, so every existing
//! invariant applies to the expanded program. Nothing is rewritten or
//! removed; the generated impls are exactly what a user would write.

use crate::ast::*;

/// Expand every `type T derive(...)` into the corresponding impl items,
/// appended to the module. Unknown derive names and unsupported shapes are
/// loud errors.
pub fn expand(module: &mut Module) -> Result<(), String> {
    let mut generated: Vec<Item> = Vec::new();
    let mut needs_json = false;
    for item in &mut module.items {
        let Item::Type(t) = item else { continue };
        // CONSUME the annotation: this pass runs at every pipeline entry
        // (records::lower is called per stage) and must be idempotent.
        let derives = std::mem::take(&mut t.derives);
        for d in &derives {
            match d.as_str() {
                "Show" => generated.push(impl_show(t)),
                "Eq" => generated.push(impl_eq(t)),
                "Ord" => generated.push(impl_ord(t)?),
                "Json" => {
                    generated.push(impl_json(t)?);
                    needs_json = true;
                }
                other => {
                    return Err(format!(
                        "type `{}`: unknown derive `{other}` (supported: Show, Eq, Ord, Json)",
                        t.name
                    ))
                }
            }
        }
    }
    // The generated `to_json` names `Json` and its constructors, and any use
    // of the result goes through `json.encode` — both need the import, and
    // the parser has already qualified calls by the imports it SAW, so it
    // must be written, not injected.
    if needs_json && !module.imports.iter().any(|i| i == "json") {
        return Err("derive(Json) needs `import json` in the module".into());
    }
    let n = generated.len();
    module.items.extend(generated);
    // Keep the comment/line bookkeeping parallel (appended items have no
    // source lines).
    for _ in 0..n {
        if !module.item_lines.is_empty() {
            module.item_lines.push(u32::MAX);
        }
    }
    Ok(())
}

fn method(name: &str, params: Vec<Param>, ret: Type, body_expr: Expr) -> Function {
    Function {
        public: true,
        name: name.to_string(),
        params,
        ret: Some(ret),
        body: Block {
            stmts: vec![Stmt::Expr(body_expr)],
            lines: vec![0],
            restrict: None,
            region: None,
        },
        bounds: Vec::new(),
        is_gen: false,
    }
}

fn self_param() -> Param {
    Param {
        name: "self".into(),
        ty: None,
        convention: Convention::Let,
    }
}

fn other_param(ty: &str) -> Param {
    Param {
        name: "other".into(),
        ty: Some(Type::Named(ty.into(), Vec::new())),
        convention: Convention::Let,
    }
}

/// `impl Show for T: fn show(self) -> String: __render(self)` — the
/// structural rendering as the derived default.
fn impl_show(t: &TypeDef) -> Item {
    Item::Impl(ImplDef {
        trait_args: Vec::new(),
        trait_name: Some("Show".into()),
        type_name: t.name.clone(),
        handlers: Vec::new(),
        methods: vec![method(
            "show",
            vec![self_param()],
            Type::Named("String".into(), Vec::new()),
            Expr::Call {
                name: "__render".into(),
                args: vec![Expr::Var("self".into())],
            },
        )],
    })
}

/// `impl Eq for T: fn eq(self, other: T) -> Bool: self == other` —
/// structural equality (deep, both backends).
fn impl_eq(t: &TypeDef) -> Item {
    Item::Impl(ImplDef {
        trait_args: Vec::new(),
        trait_name: Some("Eq".into()),
        type_name: t.name.clone(),
        handlers: Vec::new(),
        methods: vec![method(
            "eq",
            vec![self_param(), other_param(&t.name)],
            Type::Named("Bool".into(), Vec::new()),
            Expr::Binary {
                op: BinOp::Eq,
                lhs: Box::new(Expr::Var("self".into())),
                rhs: Box::new(Expr::Var("other".into())),
            },
        )],
    })
}

/// `impl Ord for T` — lexicographic field comparison for RECORD types (the
/// std Ord trait's primitive is `compare(self, other) -> Int`).
fn impl_ord(t: &TypeDef) -> Result<Item, String> {
    let [variant] = t.variants.as_slice() else {
        return Err(format!(
            "type `{}`: derive(Ord) supports record types (one constructor with named fields)",
            t.name
        ));
    };
    if variant.field_names.is_empty() {
        return Err(format!(
            "type `{}`: derive(Ord) supports record types (one constructor with named fields)",
            t.name
        ));
    }
    // if self.f != other.f: ordering of f else ... 0
    // built backwards: innermost tail = 0.
    let mut body = Expr::Int(0);
    for (name, ty) in variant.field_names.iter().zip(&variant.fields).rev() {
        let field = |of: &str| Expr::Field {
            base: Box::new(Expr::Var(of.into())),
            field: name.clone(),
        };
        let _ = ty;
        // compare: self.f < other.f -> -1; self.f > other.f -> 1; else next
        body = Expr::If {
            cond: Box::new(Expr::Binary {
                op: BinOp::Lt,
                lhs: Box::new(field("self")),
                rhs: Box::new(field("other")),
            }),
            then_block: Block {
                stmts: vec![Stmt::Expr(Expr::Int(-1))],
                lines: vec![0],
                restrict: None,
                region: None,
            },
            else_block: Some(Block {
                stmts: vec![Stmt::Expr(Expr::If {
                    cond: Box::new(Expr::Binary {
                        op: BinOp::Gt,
                        lhs: Box::new(field("self")),
                        rhs: Box::new(field("other")),
                    }),
                    then_block: Block {
                        stmts: vec![Stmt::Expr(Expr::Int(1))],
                        lines: vec![0],
                        restrict: None,
                        region: None,
                    },
                    else_block: Some(Block {
                        stmts: vec![Stmt::Expr(body)],
                        lines: vec![0],
                        restrict: None,
                        region: None,
                    }),
                })],
                lines: vec![0],
                restrict: None,
                region: None,
            }),
        };
    }
    Ok(Item::Impl(ImplDef {
        trait_args: Vec::new(),
        trait_name: Some("Ord".into()),
        type_name: t.name.clone(),
        handlers: Vec::new(),
        methods: vec![method(
            "compare",
            vec![self_param(), other_param(&t.name)],
            Type::Named("Int".into(), Vec::new()),
            body,
        )],
    }))
}

/// `impl T: fn to_json(self) -> Json` — encode a RECORD as a `Json` object,
/// field by declared field. Scalars map to their `Json` constructors; a
/// `List` maps element-wise; an `Option` is the payload or `JsonNull`; any
/// other named type is encoded by ITS `to_json` (so nested records compose
/// when they derive `Json` too). Anything else is a loud error.
fn impl_json(t: &TypeDef) -> Result<Item, String> {
    let [variant] = t.variants.as_slice() else {
        return Err(format!(
            "type `{}`: derive(Json) supports record types (one constructor with named fields)",
            t.name
        ));
    };
    if variant.field_names.is_empty() {
        return Err(format!(
            "type `{}`: derive(Json) supports record types (one constructor with named fields)",
            t.name
        ));
    }
    let mut pairs = Vec::new();
    for (name, ty) in variant.field_names.iter().zip(&variant.fields) {
        let field = Expr::Field {
            base: Box::new(Expr::Var("self".into())),
            field: name.clone(),
        };
        let value = json_value(&t.name, name, ty, field)?;
        pairs.push(Expr::Tuple(vec![Expr::Str(name.clone()), value]));
    }
    let body = Expr::Ctor {
        name: "JsonObject".into(),
        args: vec![Expr::List(pairs)],
    };
    Ok(Item::Impl(ImplDef {
        trait_args: Vec::new(),
        trait_name: None,
        type_name: t.name.clone(),
        handlers: Vec::new(),
        methods: vec![method(
            "to_json",
            vec![self_param()],
            Type::Named("Json".into(), Vec::new()),
            body,
        )],
    }))
}

/// The `Json`-building expression for one value of declared type `ty`.
fn json_value(tyname: &str, fname: &str, ty: &Type, value: Expr) -> Result<Expr, String> {
    let ctor = |name: &str, value: Expr| Expr::Ctor {
        name: name.into(),
        args: vec![value],
    };
    match ty {
        Type::Named(n, args) if args.is_empty() => match n.as_str() {
            "Int" => Ok(ctor("JsonInt", value)),
            "Float" => Ok(ctor("JsonFloat", value)),
            "Bool" => Ok(ctor("JsonBool", value)),
            "String" => Ok(ctor("JsonString", value)),
            other if other.chars().next().is_some_and(|c| c.is_uppercase()) => {
                Ok(Expr::MethodCall {
                    receiver: Box::new(value),
                    method: "to_json".into(),
                    args: Vec::new(),
                })
            }
            other => Err(format!(
                "type `{tyname}`: derive(Json) cannot encode field `{fname}: {other}` \
                 (a type variable has no JSON shape)"
            )),
        },
        Type::Named(n, args) if n == "List" && args.len() == 1 => {
            let elem = json_value(tyname, fname, &args[0], Expr::Var("x".into()))?;
            Ok(ctor(
                "JsonArray",
                Expr::Call {
                    name: "list.map".into(),
                    args: vec![
                        value,
                        Expr::Lambda {
                            params: vec![Param {
                                name: "x".into(),
                                ty: Some(args[0].clone()),
                                convention: Convention::default(),
                            }],
                            body: Block {
                                stmts: vec![Stmt::Expr(elem)],
                                lines: vec![0],
                                restrict: None,
                                region: None,
                            },
                        },
                    ],
                },
            ))
        }
        Type::Named(n, args) if n == "Option" && args.len() == 1 => {
            let payload = json_value(tyname, fname, &args[0], Expr::Var("x".into()))?;
            Ok(Expr::Match {
                scrutinee: Box::new(value),
                arms: vec![
                    MatchArm {
                        pattern: Pattern::Ctor {
                            name: "Some".into(),
                            args: vec![Pattern::Var("x".into())],
                        },
                        guard: None,
                        body: payload,
                    },
                    MatchArm {
                        pattern: Pattern::Ctor {
                            name: "None".into(),
                            args: Vec::new(),
                        },
                        guard: None,
                        body: Expr::Ctor {
                            name: "JsonNull".into(),
                            args: Vec::new(),
                        },
                    },
                ],
            })
        }
        other => Err(format!(
            "type `{tyname}`: derive(Json) cannot encode field `{fname}: {other:?}` \
             (supported: Int, Float, Bool, String, List, Option, and record types \
              that derive Json themselves)"
        )),
    }
}
