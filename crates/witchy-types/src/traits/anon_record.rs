//! Compiler-synthesized `Reflect` impls for anonymous-record types.
//!
//! Anonymous records are compiler-owned structural declarations. Reflection is
//! language behavior, not user behavior attached to their synthetic names, so
//! generate the impl during trait lowering just like anonymous unions. This
//! keeps RFC-0082 Dynamic construction backend-independent even in compilation
//! paths that do not execute source-level derive blocks.

use foldhash::{HashMap, HashSet};

use witchy_syntax::ast::*;

use super::{is_standard_trait_identity, named_type};

pub(crate) fn synthesize_anon_record_impls(
    items: &[Item],
    trait_method_list: &HashMap<String, Vec<MethodSig>>,
) -> Vec<ImplDef> {
    let Some(reflect_trait) = trait_method_list
        .iter()
        .find(|(name, methods)| {
            is_standard_trait_identity(name, "reflect", "Reflect")
                && methods.iter().any(|method| method.name == "reflect")
        })
        .map(|(name, methods)| {
            (
                name.clone(),
                methods.iter().any(|method| method.name == "__dynamic_field"),
            )
        })
    else {
        return Vec::new();
    };
    if !has_function(items, "reflect.reflect_one") || !has_variant(items, "reflect.MRecord") {
        return Vec::new();
    }
    let existing = items
        .iter()
        .filter_map(|item| match item {
            Item::Impl(definition) => definition
                .trait_name
                .as_ref()
                .map(|trait_name| (trait_name.clone(), definition.type_name.clone())),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut records = items
        .iter()
        .filter_map(|item| match item {
            Item::Type(definition) => witchy_syntax::ast::anon_record_field_names(&definition.name)
                .map(|fields| (definition.name.clone(), fields)),
            _ => None,
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| left.0.cmp(&right.0));
    records
        .into_iter()
        .filter(|(name, _)| !existing.contains(&(reflect_trait.0.clone(), name.clone())))
        .map(|(name, fields)| reflect_impl(name, fields, &reflect_trait.0, reflect_trait.1))
        .collect()
}

fn reflect_impl(
    name: String,
    fields: Vec<String>,
    trait_name: &str,
    has_dynamic_field: bool,
) -> ImplDef {
    let params = (0..fields.len()).map(|index| format!("t{index}")).collect::<Vec<_>>();
    let bindings = (0..fields.len()).map(|index| format!("f{index}")).collect::<Vec<_>>();
    let reflected_fields = fields
        .iter()
        .zip(&bindings)
        .map(|(field, binding)| {
            Expr::Tuple(vec![
                Expr::Str(field.clone()),
                Expr::Call {
                    name: "reflect.reflect_one".to_string(),
                    args: vec![Expr::Var(binding.clone())],
                },
            ])
        })
        .collect();
    let mut methods = vec![Function {
        line: 0,
        public: true,
        comptime_only: false,
        attributes: Vec::new(),
        name: "reflect".to_string(),
        params: vec![Param {
            name: "self".to_string(),
            ty: None,
            convention: Convention::Let,
            default: None,
        }],
        ret: Some(named_type("reflect.Mirror")),
        body: Block {
            stmts: vec![
                Stmt::LetPattern {
                    pattern: Pattern::Ctor {
                        name: name.clone(),
                        args: bindings.iter().cloned().map(Pattern::Var).collect(),
                    },
                    value: Expr::Var("self".to_string()),
                },
                Stmt::Expr(Expr::Ctor {
                    name: "reflect.MRecord".to_string(),
                    args: vec![Expr::Str(String::new()), Expr::List(reflected_fields)],
                }),
            ],
            lines: vec![u32::MAX, u32::MAX],
            region: None,
        },
        bounds: Vec::new(),
        is_gen: false,
        is_async: false,
    }];
    if has_dynamic_field {
        let mut result = Expr::Ctor { name: "None".into(), args: Vec::new() };
        for (field, binding) in fields.iter().zip(&bindings).rev() {
            result = Expr::If {
                cond: Box::new(Expr::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(Expr::Var("__name".into())),
                    rhs: Box::new(Expr::Str(field.clone())),
                }),
                then_block: Block {
                    stmts: vec![Stmt::Expr(Expr::Ctor {
                        name: "Some".into(),
                        args: vec![Expr::Ctor {
                            name: "reflect.DynamicFieldValue".into(),
                            args: vec![
                                Expr::Call {
                                    name: "__dynamic_descriptor_id".into(),
                                    args: vec![Expr::Var("self".into())],
                                },
                                Expr::Call {
                                    name: "__dynamic_descriptor_id".into(),
                                    args: vec![Expr::Var(binding.clone())],
                                },
                                Expr::Var(binding.clone()),
                            ],
                        }],
                    })],
                    lines: vec![u32::MAX],
                    region: None,
                },
                else_block: Some(Block {
                    stmts: vec![Stmt::Expr(result)],
                    lines: vec![u32::MAX],
                    region: None,
                }),
            };
        }
        methods.push(Function {
            line: 0,
            public: true,
            comptime_only: false,
            attributes: Vec::new(),
            name: "__dynamic_field".into(),
            params: vec![
                Param {
                    name: "self".into(),
                    ty: None,
                    convention: Convention::Let,
                    default: None,
                },
                Param {
                    name: "__name".into(),
                    ty: Some(named_type("String")),
                    convention: Convention::Let,
                    default: None,
                },
            ],
            ret: Some(Type::Named(
                "Option".into(),
                vec![named_type("reflect.DynamicFieldValue")],
            )),
            body: Block {
                stmts: vec![
                    Stmt::LetPattern {
                        pattern: Pattern::Ctor {
                            name: name.clone(),
                            args: bindings.into_iter().map(Pattern::Var).collect(),
                        },
                        value: Expr::Var("self".into()),
                    },
                    Stmt::Expr(result),
                ],
                lines: vec![u32::MAX, u32::MAX],
                region: None,
            },
            bounds: Vec::new(),
            is_gen: false,
            is_async: false,
        });
    }
    ImplDef {
        origin: ImplOrigin::CompilerGenerated,
        trait_name: Some(trait_name.to_string()),
        trait_args: Vec::new(),
        type_name: name.clone(),
        target_args: params.iter().map(|param| named_type(param)).collect(),
        bounds: params
            .iter()
            .map(|param| (param.clone(), trait_name.to_string(), Vec::new()))
            .collect(),
        methods,
    }
}

fn has_function(items: &[Item], name: &str) -> bool {
    items.iter().any(|item| {
        matches!(item, Item::Function(function) if function.name == name)
    })
}

fn has_variant(items: &[Item], name: &str) -> bool {
    items.iter().any(|item| match item {
        Item::Type(definition) => definition.variants.iter().any(|variant| variant.name == name),
        _ => false,
    })
}
