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
            Expr::Ctor { name: kind.into(), args: Vec::new() },
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
        // RFC-0112 nominal reflection must retain the compile-time owner relation
        // even though the runtime value still has the viewed inner shape. Keeping
        // it structured lets generators distinguish `View(T, 'left)` from
        // `View(T, 'right)` without treating either lifetime as a runtime type.
        Type::Qualified(TypeQual::Borrow(lifetime), inner) => Expr::Ctor {
            name: "meta.TBorrowed".into(),
            args: vec![type_expr(inner), s(lifetime)],
        },
        Type::Qualified(TypeQual::BorrowMut(lifetime), inner) => Expr::Ctor {
            name: "meta.TReference".into(),
            args: vec![Expr::Str("mut".into()), type_expr(inner), Expr::Str(lifetime.clone())],
        },
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
        Type::Tuple(ts) => Expr::Ctor {
            name: "meta.TTuple".into(),
            args: vec![Expr::List(ts.iter().map(type_expr).collect())],
        },
        Type::Fn(ps, r, conventions) => Expr::Ctor {
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
        let definition = parsed_type(
            "type Mixed(a):\n    first: a\n    second: b\n",
            "Mixed",
        );
        let normalized = normalized_type_for_typeinfo(&definition, &HashMap::default())
            .expect("normalize reflected type");
        assert_eq!(normalized.params, ["a", "b"]);
    }

    #[test]
    fn normalized_typeinfo_preserves_nominal_lifetime_kinds_and_order() {
        let definition = parsed_type(
            "mode opt\n\ntype Pair(a, 'left, 'right):\n    first: View(a, 'left)\n    second: View(a, 'right)\n    metadata: b\n",
            "Pair",
        );
        let normalized = normalized_type_for_typeinfo(&definition, &HashMap::default())
            .expect("normalize lifetime-bearing reflected type");
        assert_eq!(normalized.params, ["a", "'left", "'right", "b"]);

        let info = type_info_expr(&normalized);
        let Expr::Ctor { args, .. } = info else { panic!("expected TypeInfo constructor") };
        let Expr::List(parameters) = &args[2] else { panic!("expected parameter list") };
        assert_eq!(
            parameters,
            &vec![
                Expr::Str("a".into()),
                Expr::Str("'left".into()),
                Expr::Str("'right".into()),
                Expr::Str("b".into()),
            ]
        );
        let Expr::List(fields) = &args[3] else { panic!("expected reflected fields") };
        let Expr::Ctor { args: first_field, .. } = &fields[0] else {
            panic!("expected first reflected field")
        };
        let Expr::Ctor { name, args: borrow, .. } = &first_field[1] else {
            panic!("expected structured borrowed field relation")
        };
        assert_eq!(name, "meta.TBorrowed");
        assert_eq!(borrow[1], Expr::Str("left".into()));
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
}
