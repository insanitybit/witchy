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

/// Build the `meta.TypeInfo(...)` constructor expression describing `t`.
pub fn type_info_expr(t: &TypeDef) -> Expr {
    let s = |v: &str| Expr::Str(v.to_string());
    let str_list = |xs: &[String]| Expr::List(xs.iter().map(|x| Expr::Str(x.clone())).collect());
    let is_record = t.variants.len() == 1 && !t.variants[0].field_names.is_empty();
    let kind = if is_record {
        "record"
    } else if t.variants.is_empty() {
        "unit"
    } else {
        "sum"
    };
    let fields = if is_record {
        let v = &t.variants[0];
        v.field_names
            .iter()
            .zip(&v.fields)
            .map(|(name, ty)| Expr::Ctor {
                name: "FieldInfo".into(),
                args: vec![s(name), s(&type_to_string(ty))],
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
                name: "VariantInfo".into(),
                args: vec![
                    s(&v.name),
                    Expr::List(v.fields.iter().map(|ty| s(&type_to_string(ty))).collect()),
                ],
            })
            .collect()
    };
    Expr::Ctor {
        name: "TypeInfo".into(),
        args: vec![
            s(&t.name),
            s(kind),
            str_list(&t.params),
            Expr::List(fields),
            Expr::List(variants),
        ],
    }
}

/// Render a declared type to the string form `meta.TypeInfo` exposes — `Int`,
/// `List(String)`, `Option(Point)`, `(Int, String)`, `fn(Int) -> Bool`.
fn type_to_string(t: &Type) -> String {
    match t {
        Type::Qualified(q, inner) => format!("{} {}", q.as_str(), type_to_string(inner)),
        Type::Named(n, args) if args.is_empty() => n.clone(),
        Type::Named(n, args) => {
            let inner: Vec<String> = args.iter().map(type_to_string).collect();
            format!("{n}({})", inner.join(", "))
        }
        Type::Tuple(ts) => {
            let inner: Vec<String> = ts.iter().map(type_to_string).collect();
            format!("({})", inner.join(", "))
        }
        Type::Fn(ps, r) => {
            let inner: Vec<String> = ps.iter().map(type_to_string).collect();
            format!("fn({}) -> {}", inner.join(", "), type_to_string(r))
        }
    }
}
