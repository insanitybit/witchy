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

pub(crate) fn normalized_type_for_typeinfo(t: &TypeDef, aliases: &HashMap<String, crate::aliases::Alias>) -> TypeDef {
    let mut out = t.clone();
    for variant in &mut out.variants {
        for field in &mut variant.fields {
            crate::aliases::resolve_type_aliases(field, aliases);
        }
    }
    if out.params.is_empty() {
        let mut params = Vec::new();
        for variant in &out.variants {
            for field in &variant.fields {
                collect_type_vars(field, &mut params);
            }
        }
        out.params = params;
    }
    out
}

/// Build normalized `meta.TypeInfo` expressions for every type in `module`.
///
/// This is the public compile-time fact model: aliases are expanded in the
/// reflected field types and omitted generic parameters are inferred from the
/// fields, matching the type checker's later view without mutating the source
/// module or consuming alias declarations.
pub fn module_type_info_exprs(module: &Module) -> Vec<Expr> {
    let aliases = crate::aliases::resolved_map(module);
    module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) => {
                let normalized = normalized_type_for_typeinfo(t, &aliases);
                Some(type_info_expr(&normalized))
            }
            _ => None,
        })
        .collect()
}

/// Build the `meta.TypeInfo(...)` constructor expression describing `t`.
pub fn type_info_expr(t: &TypeDef) -> Expr {
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
        Type::Qualified(q, inner) => Expr::Ctor {
            name: "meta.TQualified".into(),
            args: vec![s(q.as_str()), type_expr(inner)],
        },
        Type::Named(n, args) => Expr::Ctor {
            name: "meta.TNamed".into(),
            args: vec![s(n), Expr::List(args.iter().map(type_expr).collect())],
        },
        Type::Tuple(ts) => Expr::Ctor {
            name: "meta.TTuple".into(),
            args: vec![Expr::List(ts.iter().map(type_expr).collect())],
        },
        Type::Fn(ps, r) => Expr::Ctor {
            name: "meta.TFn".into(),
            args: vec![
                Expr::List(ps.iter().map(type_expr).collect()),
                type_expr(r),
            ],
        },
    }
}
