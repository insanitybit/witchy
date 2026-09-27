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
                Some(normalized_type_for_typeinfo(t, &aliases).and_then(|ty| {
                    for variant in &ty.variants {
                        for field in &variant.fields {
                            check_reflectable_type(field)?;
                        }
                    }
                    Ok(type_info_expr(&ty))
                }))
            }
            _ => None,
        })
        .collect()
}

/// Resolve a compiler-written type lookup against this stage's declarations.
/// The returned expression is data for generation; callers retain the source
/// module and its loader identity in the checked recipe.
pub fn type_info_expr_for_type(module: &Module, mut requested: Type) -> Result<Expr, String> {
    type_info_definition(module, &mut requested).map(|definition| type_info_expr(&definition))
}

fn type_info_definition(module: &Module, requested: &mut Type) -> Result<TypeDef, String> {
    let aliases = crate::aliases::resolved_map(module)?;
    crate::aliases::resolve_type_aliases(requested, &aliases)?;
    let Type::Named(name, args) = requested else {
        return Err("type_info requires a declared nominal type".into());
    };
    let definition = module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Type(definition) if definition.name == *name => Some(definition),
            _ => None,
        })
        .ok_or_else(|| format!("type_info cannot resolve declared type `{name}`"))?;
    if definition.is_capability {
        return Err(format!("type_info cannot inspect capability `{name}`"));
    }
    let mut definition = normalized_type_for_typeinfo(definition, &aliases)?;
    if definition.params.len() != args.len() {
        return Err(format!(
            "type_info expects {} type arguments for `{name}`",
            definition.params.len()
        ));
    }
    if !args.is_empty() {
        let arguments: HashMap<_, _> = definition.params.iter().cloned().zip(args.iter().cloned()).collect();
        for variant in &mut definition.variants {
            for field in &mut variant.fields {
                substitute_type_parameters(field, &arguments);
            }
        }
        definition.params.clear();
    }
    for variant in &definition.variants {
        for field in &variant.fields {
            check_reflectable_type(field)?;
        }
    }
    Ok(definition)
}

/// Resolve an admitted lookup in the selected module's type namespace, then
/// read the declaration in its defining module. Both modules came from the
/// loader-authenticated source bundle; the caller includes their assignments
/// and the staged source in the executable recipe digest.
pub fn type_info_expr_for_type_in_modules(
    modules: &[(String, Module)], selected: &str,
    user_modules: &std::collections::HashSet<String>, mut requested: Type,
) -> Result<Expr, String> {
    let selected_module = modules.iter().find(|(name, _)| name == selected)
        .ok_or_else(|| format!("type_info has no selected module `{selected}`"))?;
    let aliases = crate::aliases::resolved_map(&selected_module.1)?;
    crate::aliases::resolve_type_aliases(&mut requested, &aliases)?;
    let resolved = crate::type_resolve::resolve_comptime_type_operand(
        modules, selected, user_modules, requested,
    ).map_err(|error| error.to_string())?;
    let Type::Named(name, args) = resolved else {
        return Err("type_info requires a declared nominal type".into());
    };
    let (home, local_name) = name.split_once('.')
        .ok_or_else(|| format!("type_info cannot resolve declared type `{name}`"))?;
    let owner = modules.iter().find(|(module, _)| module == home)
        .ok_or_else(|| format!("type_info has no defining module `{home}`"))?;
    let original = owner.1.items.iter().find_map(|item| match item {
        Item::Type(definition) if definition.name == local_name => Some(definition),
        _ => None,
    }).ok_or_else(|| format!("type_info cannot resolve declared type `{name}`"))?;
    let parameters = crate::ast::effective_nominal_type_def_params(original);
    if parameters.len() != args.len() {
        return Err(format!("type_info expects {} type arguments for `{name}`", parameters.len()));
    }
    // The definition's own written types resolve in its namespace. Concrete
    // arguments were already resolved in the caller's namespace and may name
    // modules the definition never imported.
    let placeholders = parameters.iter().map(|param| Type::Named(param.clone(), Vec::new())).collect();
    let mut definition = type_info_definition(&owner.1,
        &mut Type::Named(local_name.into(), placeholders))?;
    for variant in &mut definition.variants {
        for field in &mut variant.fields {
            *field = crate::type_resolve::resolve_comptime_type_operand(
                modules, home, user_modules, field.clone(),
            ).map_err(|error| error.to_string())?;
        }
    }
    let arguments: HashMap<_, _> = parameters.into_iter().zip(args).collect();
    for variant in &mut definition.variants {
        for field in &mut variant.fields {
            substitute_type_parameters(field, &arguments);
        }
    }
    Ok(type_info_expr(&definition))
}

fn substitute_type_parameters(ty: &mut Type, arguments: &HashMap<String, Type>) {
    match ty {
        Type::Named(name, args) => {
            if args.is_empty() {
                if let Some(argument) = arguments.get(name) {
                    *ty = argument.clone();
                    return;
                }
            }
            for arg in args {
                substitute_type_parameters(arg, arguments);
            }
        }
        Type::Qualified(_, inner) | Type::Slice(inner) => {
            substitute_type_parameters(inner, arguments)
        }
        Type::Tuple(items) => {
            for item in items {
                substitute_type_parameters(item, arguments);
            }
        }
        Type::Fn(params, result, _, _) => {
            for param in params {
                substitute_type_parameters(param, arguments);
            }
            substitute_type_parameters(result, arguments);
        }
        Type::Dyn(_, args) => {
            for arg in args {
                substitute_type_parameters(arg, arguments);
            }
        }
        Type::RecordCompose { base, fields } => {
            substitute_type_parameters(base, arguments);
            for (_, field) in fields {
                substitute_type_parameters(field, arguments);
            }
        }
    }
}

fn check_reflectable_type(ty: &Type) -> Result<(), String> {
    match ty {
        Type::Qualified(TypeQual::Borrow(_) | TypeQual::LegacyBorrow(_), _) =>
            Err("cannot reflect a legacy lifetime-bearing borrow as a rooted view".into()),
        Type::Qualified(TypeQual::BorrowMut(_), _) =>
            Err("cannot reflect a migration-only mutable reference as a rooted view".into()),
        Type::Qualified(_, inner) | Type::Slice(inner) => check_reflectable_type(inner),
        Type::Named(_, args) | Type::Tuple(args) => {
            for arg in args { check_reflectable_type(arg)?; }
            Ok(())
        }
        Type::Fn(params, result, _, _) => {
            for param in params { check_reflectable_type(param)?; }
            check_reflectable_type(result)
        }
        Type::Dyn(..) | Type::RecordCompose { .. } => Ok(()),
    }
}

/// Build compiler-issued type metadata. The unspellable call is restored to a
/// constructor only after the source sealing check has run.
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
    Expr::Call {
        name: "@compiler:type-info".into(),
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
