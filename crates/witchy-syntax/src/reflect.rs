//! Compile-time reflection: build the `meta.TypeInfo(...)` descriptor for a
//! declared type, as AST.
//!
//! This is the Rust side of `derive(Reflect)` / the `typeInfo` comptime
//! primitive — given a `TypeDef`, it constructs the `meta.TypeInfo` literal that
//! witchy code (`std/meta`) consumes. Both the comptime evaluator (which injects
//! a module's type infos so a block can read its own structure as data) and
//! `derive` (which embeds a type's structure in the generator call it desugars
//! to) build it, so it lives in this shared leaf rather than in either stage.

use crate::ast::*;
// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::HashMap;

pub(crate) fn normalized_type_for_typeinfo(
    t: &TypeDef,
    aliases: &HashMap<String, crate::aliases::Alias>,
) -> Result<TypeDef, String> {
    // Parameter identity belongs to the declaration spelling. Alias expansion
    // may reorder variables inside a field and must not reorder the nominal
    // type's positional generic arguments.
    let parameters = crate::ast::effective_nominal_type_def_params(t);
    let mut out = t.clone();
    for variant in &mut out.variants {
        for field in &mut variant.fields {
            crate::aliases::resolve_type_aliases(field, aliases)?;
        }
    }
    out.params = parameters;
    Ok(out)
}

/// Build normalized `meta.TypeInfo` expressions for every type in `module`.
///
/// This is the public compile-time fact model: aliases are expanded in the
/// reflected field types and omitted generic parameters are inferred from the
/// fields, matching the type checker's later view without mutating the source
/// module or consuming alias declarations.
pub fn module_type_info_exprs(module: &Module) -> Result<Vec<Expr>, String> {
    let aliases = crate::aliases::resolved_map(module)?;
    module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) => {
                Some(normalized_type_for_typeinfo(t, &aliases).map(|ty| type_info_expr(&ty)))
            }
            _ => None,
        })
        .collect()
}

/// Build the `meta.TypeInfo(...)` constructor expression describing `t`.
pub(crate) fn type_info_expr(t: &TypeDef) -> Expr {
    let s = |v: &str| Expr::Str(v.to_string());
    let str_list = |xs: &[String]| Expr::List(xs.iter().map(|x| Expr::Str(x.clone())).collect());
    let is_record = t.variants.len() == 1 && !t.variants[0].field_names.is_empty();
    let kind = if is_record {
        "meta.TypeRecord"
    } else if t.variants.is_empty() {
        "meta.TypeUninhabited"
    } else {
        "meta.TypeSum"
    };
    let fields = if is_record {
        let v = &t.variants[0];
        v.field_names
            .iter()
            .zip(&v.fields)
            .map(|(name, ty)| Expr::Ctor {
                name: "meta.FieldInfo".into(),
                args: vec![s(name), type_expr(ty)],
            })
            .collect()
    } else {
        Vec::new()
    };
    let variants = if is_record {
        Vec::new()
    } else {
        t.variants
            .iter()
            .map(|v| Expr::Ctor {
                name: "meta.VariantInfo".into(),
                args: vec![
                    s(&v.name),
                    Expr::List(v.fields.iter().map(type_expr).collect()),
                ],
            })
            .collect()
    };
    Expr::Ctor {
        name: "meta.TypeInfo".into(),
        args: vec![
            s(&t.name),
            Expr::Ctor {
                name: kind.into(),
                args: Vec::new(),
            },
            str_list(&t.params),
            Expr::List(fields),
            Expr::List(variants),
        ],
    }
}

/// Build the structured `meta.TypeExpr` for a declared type. Rendering is kept
/// at the explicit generated-source boundary in `meta.type_source`.
fn type_expr(t: &Type) -> Expr {
    let s = |v: &str| Expr::Str(v.to_string());
    match t {
        Type::Qualified(TypeQual::View, inner) => Expr::Ctor {
            name: "meta.TView".into(),
            args: vec![type_expr(inner)],
        },
        Type::Qualified(TypeQual::Borrow(_) | TypeQual::LegacyBorrow(_), _) => {
            panic!("cannot reflect a legacy lifetime-bearing borrow as a rooted view")
        }
        Type::Qualified(TypeQual::BorrowMut(_), _) => {
            panic!("cannot reflect a migration-only mutable reference as a rooted view")
        }
        Type::Qualified(q, inner) => Expr::Ctor {
            name: "meta.TQualified".into(),
            args: vec![s(q.as_str()), type_expr(inner)],
        },
        Type::Named(n, args) => Expr::Ctor {
            name: "meta.TNamed".into(),
            args: vec![s(n), Expr::List(args.iter().map(type_expr).collect())],
        },
        // (RFC-0081) Placeholder: reflect the canonical rendering as an opaque
        // named head with NO argument list, until the witness/runtime slice adds
        // a structured `meta.TDyn` (std/meta.witchy belongs to another lane).
        // Unobservable from runnable programs — dyn-carrying modules fail the
        // typeck feature gate before either backend lowers them.
        Type::Dyn(..) => Expr::Ctor {
            name: "meta.TNamed".into(),
            args: vec![s(&crate::format::type_str(t)), Expr::List(Vec::new())],
        },
        Type::RecordCompose { .. } => Expr::Ctor {
            name: "meta.TNamed".into(),
            args: vec![s(&crate::format::type_str(t)), Expr::List(Vec::new())],
        },
        Type::Slice(elem) => Expr::Ctor {
            name: "meta.TNamed".into(),
            args: vec![
                s(&crate::format::type_str(t)),
                Expr::List(vec![type_expr(elem)]),
            ],
        },
        Type::Tuple(ts) => Expr::Ctor {
            name: "meta.TTuple".into(),
            args: vec![Expr::List(ts.iter().map(type_expr).collect())],
        },
        Type::Fn(ps, r, conventions, qualifiers) => Expr::Ctor {
            name: "meta.TFn".into(),
            args: vec![
                Expr::List(ps.iter().map(type_expr).collect()),
                type_expr(r),
                Expr::List(
                    conventions
                        .iter()
                        .map(|convention| {
                            s(match convention {
                                Convention::Let => "value",
                                Convention::Borrow => "borrow",
                                Convention::Var => "var",
                                Convention::Own => "own",
                            })
                        })
                        .collect(),
                ),
                Expr::Bool(qualifiers.pure),
                Expr::Bool(qualifiers.once),
            ],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed_type(source: &str, name: &str) -> TypeDef {
        crate::parser::parse_module(source)
            .expect("parse type")
            .items
            .into_iter()
            .find_map(|item| match item {
                Item::Type(definition) if definition.name == name => Some(definition),
                _ => None,
            })
            .expect("type declaration")
    }

    #[test]
    fn effective_params_preserve_explicit_then_inferred_order() {
        let definition = parsed_type(
            "type Mixed(a):\n    first: b\n    second: a\n    third: c\n    repeated: b\n",
            "Mixed",
        );
        assert_eq!(
            crate::ast::effective_type_def_params(&definition),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn normalized_typeinfo_includes_mixed_explicit_and_inferred_params() {
        let definition = parsed_type("type Mixed(a):\n    first: a\n    second: b\n", "Mixed");
        let normalized = normalized_type_for_typeinfo(&definition, &HashMap::default())
            .expect("normalize reflected type");
        assert_eq!(normalized.params, ["a", "b"]);
    }

    #[test]
    #[should_panic(expected = "lifetime parameters are retired")]
    fn ordinary_parser_rejects_lifetime_bearing_reflection_input() {
        let definition = parsed_type(
            "mode opt\n\ntype Pair(a, 'left, 'right):\n    first: &'left a\n    second: &'right a\n    metadata: b\n",
            "Pair",
        );
        let _ = normalized_type_for_typeinfo(&definition, &HashMap::default());
    }

    #[test]
    fn rfc0167_reflection_preserves_lifetime_free_rooted_view_access() {
        let definition = parsed_type(
            "type Access(a):\n    shared: &a\n    items: &[a]\n",
            "Access",
        );
        let normalized = normalized_type_for_typeinfo(&definition, &HashMap::default())
            .expect("normalize canonical rooted-view fields");
        assert_eq!(normalized.params, ["a"]);
        let Expr::Ctor { args, .. } = type_info_expr(&normalized) else { panic!("TypeInfo") };
        let Expr::List(fields) = &args[3] else { panic!("fields") };
        for field in fields {
            let Expr::Ctor { args: field_args, .. } = field else { panic!("field") };
            let Expr::Ctor { name, args: view, .. } = &field_args[1] else { panic!("view") };
            assert_eq!(name, "meta.TView");
            assert_eq!(view.len(), 1);
        }
    }

    #[test]
    #[should_panic(expected = "cannot reflect a migration-only mutable reference")]
    fn reflected_mutable_reference_cannot_be_emitted_as_a_rooted_view() {
        let reference = Type::Qualified(
            TypeQual::BorrowMut("owner".into()),
            Box::new(Type::Named("String".into(), Vec::new())),
        );
        let _ = type_expr(&reference);
    }

    #[test]
    fn reflected_function_type_preserves_all_callable_contract_bits() {
        for qualifiers in [
            CallableQualifiers::ORDINARY,
            CallableQualifiers::new(true, false),
            CallableQualifiers::new(false, true),
            CallableQualifiers::new(true, true),
        ] {
            let reflected = type_expr(&Type::Fn(
                vec![Type::Named("Int".into(), Vec::new())],
                Box::new(Type::Named("String".into(), Vec::new())),
                vec![Convention::Own],
                qualifiers,
            ));
            let Expr::Ctor { name, args } = reflected else {
                panic!("function type must reflect as a structured constructor")
            };
            assert_eq!(name, "meta.TFn");
            assert_eq!(args[3], Expr::Bool(qualifiers.pure));
            assert_eq!(args[4], Expr::Bool(qualifiers.once));
        }
    }

    #[test]
    fn normalized_typeinfo_preserves_parameter_order_across_alias_expansion() {
        let module = crate::parser::parse_module(
            "type Flip(x, y) = (y, x)\n\ntype Mixed(a):\n    payload: Flip(b, c)\n",
        )
        .expect("parse aliased mixed parameters");
        let aliases = crate::aliases::resolved_map(&module).expect("resolve aliases");
        let definition = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Type(definition) if definition.name == "Mixed" => Some(definition),
                _ => None,
            })
            .expect("Mixed declaration");
        let normalized = normalized_type_for_typeinfo(definition, &aliases)
            .expect("normalize aliased reflected type");
        assert_eq!(normalized.params, ["a", "b", "c"]);
    }

    #[test]
    fn rfc0167_reflection_uses_a_lifetime_free_rooted_view_descriptor() {
        let view = Type::Qualified(
            TypeQual::View,
            Box::new(Type::Slice(Box::new(Type::Named("Int".into(), Vec::new())))),
        );
        assert_eq!(type_expr(&view), Expr::Ctor {
            name: "meta.TView".into(),
            args: vec![Expr::Ctor {
                name: "meta.TNamed".into(),
                args: vec![Expr::Str("[Int]".into()), Expr::List(vec![Expr::Ctor {
                    name: "meta.TNamed".into(),
                    args: vec![Expr::Str("Int".into()), Expr::List(Vec::new())],
                }])],
            }],
        });
        assert_eq!(crate::format::type_str(&view), "&[Int]");
    }

    #[test]
    fn rfc0167_reflection_keeps_nested_aggregate_views_structural() {
        let nested = Type::Qualified(
            TypeQual::View,
            Box::new(Type::Tuple(vec![
                Type::Named("Header".into(), Vec::new()),
                Type::Qualified(
                    TypeQual::View,
                    Box::new(Type::Slice(Box::new(Type::Named("Int".into(), Vec::new())))),
                ),
            ])),
        );
        let Expr::Ctor { name, args } = type_expr(&nested) else { panic!("outer view") };
        assert_eq!(name, "meta.TView");
        let Expr::Ctor { name, args } = &args[0] else { panic!("tuple") };
        assert_eq!(name, "meta.TTuple");
        let Expr::List(items) = &args[0] else { panic!("tuple items") };
        let Expr::Ctor { name, args } = &items[1] else { panic!("nested view") };
        assert_eq!(name, "meta.TView");
        assert_eq!(args.len(), 1);
    }

    #[test]
    #[should_panic(expected = "cannot reflect a legacy lifetime-bearing borrow")]
    fn legacy_borrow_ast_cannot_be_rewritten_into_rooted_view_metadata() {
        let legacy = Type::Qualified(
            TypeQual::Borrow("owner".into()),
            Box::new(Type::Named("String".into(), Vec::new())),
        );
        let _ = type_expr(&legacy);
    }
}
