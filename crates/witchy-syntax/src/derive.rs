//! `derive(...)` — desugar each derive to a comptime call of its witchy generator.
//!
//! Record equality is a deterministic compiler-owned AST transform. Other
//! `derive(X)` forms become `comptime:` calls to `meta.derive_<x>` or a user
//! `derive_<x>` generator. Generators return typed `meta.ItemSyntax` values;
//! source text is not an emission channel.

use crate::ast::*;
// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashSet};

/// These record derives need no user compile-time execution. Their checked AST
/// can be built directly from the authenticated declaration in a source session.
pub fn has_only_direct_record_derives(module: &Module) -> bool {
    let mut found = false;
    for item in &module.items {
        let Item::Type(definition) = item else { continue };
        if definition.derives.is_empty() { continue; }
        found = true;
        if definition.variants.len() != 1 || definition.variants[0].field_names.is_empty()
            || definition.derives.iter().any(|name| !matches!(name.as_str(),
                "Eq" | "PartialEq" | "Show" | "PartialOrd" | "Ord")) {
            return false;
        }
    }
    found
}

/// Expand the authenticated toolchain Glamour declarations before static-site
/// linking. The caller binds this rewrite to the raw source and toolchain owner.
pub fn expand_toolchain_reflect(module: &mut Module) -> Result<(), String> {
    expand_with_record_ordering(module, true, true)
}

fn reflect_call(ty: &Type, value: Expr) -> Expr {
    let helper = match ty {
        Type::Named(name, _) if name == "List" => "reflect_list",
        Type::Named(name, _) if name == "Option" => "reflect_option",
        _ => "reflect_one",
    };
    Expr::Call { name: format!("reflect.{helper}"), args: vec![value] }
}

fn record_dynamic_field(fields: &[(String, Type)], index: usize, line: u32) -> Expr {
    let Some((name, _)) = fields.get(index) else {
        return Expr::Ctor { name: "None".into(), args: Vec::new() };
    };
    let owner = Expr::Var("self".into());
    let field = Expr::Field { base: Box::new(owner.clone()), field: name.clone() };
    let descriptor = |value: Expr| Expr::Call {
        name: "__dynamic_descriptor_id".into(), args: vec![value],
    };
    let value = Expr::Call { name: "reflect.DynamicFieldValue".into(),
        args: vec![descriptor(owner), descriptor(field.clone()), field] };
    Expr::Match { scrutinee: Box::new(Expr::Binary { op: BinOp::Eq,
        lhs: Box::new(Expr::Var("__name".into())), rhs: Box::new(Expr::Str(name.clone())) }),
        arms: vec![ordering_arm(Pattern::Bool(true), Expr::Ctor {
            name: "Some".into(), args: vec![value],
        }, line), ordering_arm(Pattern::Wildcard,
            record_dynamic_field(fields, index + 1, line), line)] }
}

fn reflect_impl(t: &TypeDef, line: u32) -> Item {
    let params = t.params.iter().filter(|name| !name.starts_with('\''))
        .cloned().collect::<Vec<_>>();
    let target_args = params.iter().map(|name| Type::Named(name.clone(), Vec::new()))
        .collect::<Vec<_>>();
    let bounds = params.iter().map(|name|
        (name.clone(), "Reflect".to_string(), Vec::new())).collect::<Vec<_>>();
    let target = Type::Named(t.name.clone(), target_args.clone());
    let record = t.variants.len() == 1 && !t.variants[0].field_names.is_empty();
    let body = if record {
        let variant = &t.variants[0];
        let pairs = variant.field_names.iter().zip(&variant.fields).map(|(name, ty)| {
            Expr::Tuple(vec![Expr::Str(name.clone()), reflect_call(ty, Expr::Field {
                base: Box::new(Expr::Var("self".into())), field: name.clone(),
            })])
        }).collect();
        Expr::Call { name: "reflect.MRecord".into(),
            args: vec![Expr::Str(t.name.clone()), Expr::List(pairs)] }
    } else {
        let arms = t.variants.iter().map(|variant| {
            let vars = (0..variant.fields.len()).map(|i| format!("__reflect_p{i}"))
                .collect::<Vec<_>>();
            let values = variant.fields.iter().zip(&vars).map(|(ty, name)|
                reflect_call(ty, Expr::Var(name.clone()))).collect();
            ordering_arm(Pattern::Ctor { name: variant.name.clone(),
                args: vars.into_iter().map(Pattern::Var).collect() },
                Expr::Call { name: "reflect.MVariant".into(), args: vec![
                    Expr::Str(t.name.clone()), Expr::Str(variant.name.clone()), Expr::List(values),
                ] }, line)
        }).collect();
        Expr::Match { scrutinee: Box::new(Expr::Var("self".into())), arms }
    };
    let method = |name: &str, params: Vec<Param>, ret: Type, body: Expr| Function {
        line, public: false, pure: false, comptime_only: false, attributes: Vec::new(),
        name: name.into(), type_params: Vec::new(), params, ret: Some(ret),
        body: Block { stmts: vec![Stmt::Expr(body)], lines: vec![line], region: None },
        bounds: bounds.clone(), is_gen: false, is_async: false,
    };
    let self_param = || Param { name: "self".into(), ty: Some(target.clone()),
        convention: Convention::Let, default: None };
    let mut methods = vec![method("reflect", vec![self_param()],
        Type::Named("reflect.Mirror".into(), Vec::new()), body)];
    if record {
        let fields = t.variants[0].field_names.iter().cloned()
            .zip(t.variants[0].fields.iter().cloned()).collect::<Vec<_>>();
        methods.push(method("__dynamic_field", vec![self_param(), Param {
            name: "__name".into(), ty: Some(Type::Named("String".into(), Vec::new())),
            convention: Convention::Let, default: None,
        }], Type::Named("Option".into(), vec![Type::Named("reflect.DynamicFieldValue".into(), Vec::new())]),
            record_dynamic_field(&fields, 0, line)));
    }
    Item::Impl(ImplDef { origin: ImplOrigin::CompilerGenerated,
        trait_name: Some("Reflect".into()), trait_args: Vec::new(),
        type_name: t.name.clone(), target_args, bounds, methods })
}

fn ordering_arm(pattern: Pattern, body: Expr, line: u32) -> MatchArm {
    MatchArm { line, pattern, guard: None, body }
}

fn ordering_ctor(name: &str) -> Pattern {
    Pattern::Ctor { name: name.into(), args: Vec::new() }
}

fn ordering_value(name: &str, partial: bool) -> Expr {
    let value = Expr::Ctor { name: name.into(), args: Vec::new() };
    if partial { Expr::Ctor { name: "Some".into(), args: vec![value] } } else { value }
}

fn record_ordering_body(fields: &[String], index: usize, partial: bool, line: u32) -> Expr {
    let Some(field) = fields.get(index) else { return ordering_value("Equal", partial) };
    let field_of = |owner: &str| Expr::Field {
        base: Box::new(Expr::Var(owner.into())), field: field.clone(),
    };
    let compared = Expr::MethodCall {
        receiver: Box::new(field_of("self")),
        method: if partial { "partial_compare" } else { "compare" }.into(),
        args: vec![field_of("other")],
    };
    let pattern = |name: &str| if partial {
        Pattern::Ctor { name: "Some".into(), args: vec![ordering_ctor(name)] }
    } else { ordering_ctor(name) };
    let mut arms = Vec::new();
    if partial {
        arms.push(ordering_arm(ordering_ctor("None"),
            Expr::Ctor { name: "None".into(), args: Vec::new() }, line));
    }
    for name in ["Less", "Greater"] {
        arms.push(ordering_arm(pattern(name), ordering_value(name, partial), line));
    }
    arms.push(ordering_arm(pattern("Equal"),
        record_ordering_body(fields, index + 1, partial, line), line));
    Expr::Match { scrutinee: Box::new(compared), arms }
}

fn record_ordering_impl(t: &TypeDef, partial: bool, line: u32) -> Item {
    let trait_name = if partial { "PartialOrd" } else { "Ord" };
    let method_name = if partial { "partial_compare" } else { "compare" };
    let params = t.params.iter().filter(|name| !name.starts_with('\''))
        .cloned().collect::<Vec<_>>();
    let target_args = params.iter().map(|name| Type::Named(name.clone(), Vec::new()))
        .collect::<Vec<_>>();
    let bounds = params.iter().map(|name|
        (name.clone(), trait_name.to_string(), Vec::new())).collect::<Vec<_>>();
    let target = Type::Named(t.name.clone(), target_args.clone());
    let ordering = Type::Named("Ordering".into(), Vec::new());
    Item::Impl(ImplDef {
        origin: ImplOrigin::CompilerGenerated, trait_name: Some(trait_name.into()),
        trait_args: Vec::new(), type_name: t.name.clone(), target_args,
        bounds: bounds.clone(), methods: vec![Function {
            line, public: false, pure: false, comptime_only: false,
            attributes: Vec::new(), name: method_name.into(), type_params: Vec::new(),
            params: ["self", "other"].into_iter().map(|name| Param {
                name: name.into(), ty: Some(target.clone()),
                convention: Convention::Let, default: None,
            }).collect(),
            ret: Some(if partial { Type::Named("Option".into(), vec![ordering]) } else { ordering }),
            body: Block { stmts: vec![Stmt::Expr(record_ordering_body(
                &t.variants[0].field_names, 0, partial, line,
            ))], lines: vec![line], region: None },
            bounds, is_gen: false, is_async: false,
        }],
    })
}

fn record_show_impl(t: &TypeDef, line: u32) -> Item {
    let params = t.params.iter().filter(|name| !name.starts_with('\''))
        .cloned().collect::<Vec<_>>();
    let target_args = params.iter().map(|name| Type::Named(name.clone(), Vec::new()))
        .collect::<Vec<_>>();
    let bounds = params.iter().map(|name|
        (name.clone(), "Show".to_string(), Vec::new())).collect::<Vec<_>>();
    let target = Type::Named(t.name.clone(), target_args.clone());
    let mut body = Expr::Str(format!("{}(", t.name));
    for (index, name) in t.variants[0].field_names.iter().enumerate() {
        if index > 0 {
            body = Expr::Binary { op: BinOp::Add, lhs: Box::new(body),
                rhs: Box::new(Expr::Str(", ".into())) };
        }
        body = Expr::Binary { op: BinOp::Add, lhs: Box::new(body),
            rhs: Box::new(Expr::Call { name: "show".into(), args: vec![Expr::Field {
                base: Box::new(Expr::Var("self".into())), field: name.clone(),
            }] }) };
    }
    body = Expr::Binary { op: BinOp::Add, lhs: Box::new(body),
        rhs: Box::new(Expr::Str(")".into())) };
    Item::Impl(ImplDef {
        origin: ImplOrigin::CompilerGenerated, trait_name: Some("Show".into()),
        trait_args: Vec::new(), type_name: t.name.clone(), target_args,
        bounds: bounds.clone(), methods: vec![Function {
            line, public: false, pure: false, comptime_only: false,
            attributes: Vec::new(), name: "show".into(), type_params: Vec::new(),
            params: vec![Param { name: "self".into(), ty: Some(target),
                convention: Convention::Let, default: None }],
            ret: Some(Type::Named("String".into(), Vec::new())),
            body: Block { stmts: vec![Stmt::Expr(body)], lines: vec![line], region: None },
            bounds, is_gen: false, is_async: false,
        }],
    })
}

/// A local user generator already has a typed result contract. Imported or
/// unknown generators remain on the compatibility path until their source and
/// ownership can be selected by an admitted session.
pub fn has_only_typed_user_derives(module: &Module) -> bool {
    let outputs = user_derive_outputs(module);
    let mut found = false;
    for item in &module.items {
        let Item::Type(definition) = item else { continue };
        for name in &definition.derives {
            found = true;
            if matches!(name.as_str(), "Show" | "PartialEq" | "Eq" | "PartialOrd"
                | "Ord" | "Reflect" | "Deserialize" | "PublicState") {
                return false;
            }
            let generator = format!("derive_{}", name.to_lowercase());
            if !matches!(outputs.get(&generator),
                Some(UserDeriveOutput::ItemSyntax | UserDeriveOutput::ItemSyntaxList)) {
                return false;
            }
        }
    }
    found
}

fn record_equality_impl(t: &TypeDef, trait_name: &str, line: u32) -> Item {
    let params = t.params.iter().filter(|name| !name.starts_with('\''))
        .cloned().collect::<Vec<_>>();
    let target_args = params.iter().map(|name| Type::Named(name.clone(), Vec::new()))
        .collect::<Vec<_>>();
    let bounds = params.iter().map(|name|
        (name.clone(), trait_name.to_string(), Vec::new())).collect::<Vec<_>>();
    let methods = if trait_name == "Eq" { Vec::new() } else {
        let target = Type::Named(t.name.clone(), target_args.clone());
        let comparisons = t.variants[0].field_names.iter().map(|name| Expr::Binary {
            op: BinOp::Eq,
            lhs: Box::new(Expr::Field {
                base: Box::new(Expr::Var("self".into())), field: name.clone(),
            }),
            rhs: Box::new(Expr::Field {
                base: Box::new(Expr::Var("other".into())), field: name.clone(),
            }),
        });
        let body = comparisons.reduce(|lhs, rhs| Expr::Binary {
            op: BinOp::And, lhs: Box::new(lhs), rhs: Box::new(rhs),
        }).unwrap_or(Expr::Bool(true));
        vec![Function {
            line, public: false, pure: false, comptime_only: false,
            attributes: Vec::new(), name: "eq".into(), type_params: Vec::new(),
            params: ["self", "other"].into_iter().map(|name| Param {
                name: name.into(), ty: Some(target.clone()),
                convention: Convention::Let, default: None,
            }).collect(),
            ret: Some(Type::Named("Bool".into(), Vec::new())),
            body: Block { stmts: vec![Stmt::Expr(body)], lines: vec![line], region: None },
            bounds: bounds.clone(), is_gen: false, is_async: false,
        }]
    };
    Item::Impl(ImplDef {
        origin: ImplOrigin::CompilerGenerated, trait_name: Some(trait_name.into()),
        trait_args: Vec::new(), type_name: t.name.clone(), target_args,
        bounds, methods,
    })
}

#[derive(Clone, Copy)]
enum UserDeriveOutput {
    ItemSyntax,
    ItemSyntaxList,
    SourceText,
    NonComptime,
}

fn contains_concrete_float(ty: &Type) -> bool {
    match ty {
        Type::Named(n, args) => n == "Float" || args.iter().any(contains_concrete_float),
        Type::Tuple(slots) => slots.iter().any(contains_concrete_float),
        Type::RecordCompose { base, fields } => {
            contains_concrete_float(base)
                || fields
                    .iter()
                    .any(|(_, field)| contains_concrete_float(field))
        }
        Type::Fn(params, ret, _, _) => {
            params.iter().any(contains_concrete_float) || contains_concrete_float(ret)
        }
        Type::Qualified(_, inner) => contains_concrete_float(inner),
        Type::Slice(elem) => contains_concrete_float(elem),
        Type::Dyn(_, args) => args.iter().any(contains_concrete_float),
    }
}

fn has_float_field(t: &TypeDef) -> bool {
    t.variants
        .iter()
        .any(|v| v.fields.iter().any(contains_concrete_float))
}

fn unsupported_deserialize_shape(ty: &Type) -> Option<&'static str> {
    match ty {
        Type::Named(_, args) => args.iter().find_map(unsupported_deserialize_shape),
        Type::Tuple(_) => Some("tuple"),
        Type::Slice(_) => Some("slice"),
        Type::RecordCompose { .. } => Some("unresolved structural record composition"),
        Type::Fn(..) => Some("function"),
        Type::Qualified(_, inner) => unsupported_deserialize_shape(inner),
        Type::Dyn(_, _) => Some("existential `dyn` trait"),
    }
}

fn unsupported_deserialize_field(t: &TypeDef) -> Option<(&str, &'static str)> {
    let variant = t.variants.first()?;
    variant
        .fields
        .iter()
        .zip(variant.field_names.iter())
        .find_map(|(field_type, field_name)| {
            unsupported_deserialize_shape(field_type).map(|shape| (field_name.as_str(), shape))
        })
}

fn builtin_derive_on_fieldless_type(d: &str) -> bool {
    matches!(
        d,
        "Show"
            | "PartialEq"
            | "Eq"
            | "PartialOrd"
            | "Ord"
            | "Reflect"
            | "Deserialize"
            | "PublicState"
    )
}

fn reject_borrowed_nominal_runtime_derives(
    definition: &TypeDef,
    derives: &[String],
) -> Result<(), String> {
    if !definition
        .params
        .iter()
        .any(|parameter| is_lifetime_param(parameter))
    {
        return Ok(());
    }
    if derives.iter().any(|derive| derive == "Reflect") {
        return Err(format!(
            "type `{}`: derive(Reflect) would expose a lifetime-bearing borrowed shell as a \
             runtime value. Compile-time TypeInfo reflection remains available through \
             `module_types`, but borrowed values cannot be reflected or serialized in \
             RFC-0112 stage 1",
            definition.name,
        ));
    }
    if derives.iter().any(|derive| derive == "Deserialize") {
        return Err(format!(
            "type `{}`: derive(Deserialize) cannot reconstruct a lifetime-bearing borrowed \
             shell without an authenticated owner relation. Decode into an owned companion \
             type instead",
            definition.name,
        ));
    }
    Ok(())
}

fn is_item_syntax_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, args) if args.is_empty() && (name == "ItemSyntax" || name == "meta.ItemSyntax"))
}

fn is_item_syntax_list_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, args) if name == "List" && matches!(args.as_slice(), [inner] if is_item_syntax_type(inner)))
}

fn is_source_text_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, args)
        if (name == "String" && args.is_empty())
            || (name == "List" && matches!(args.as_slice(), [Type::Named(inner, fields)]
                if inner == "String" && fields.is_empty())))
}

fn user_derive_outputs(module: &Module) -> HashMap<String, UserDeriveOutput> {
    let mut outputs = HashMap::default();
    for item in &module.items {
        let Item::Function(f) = item else { continue };
        if !f.name.starts_with("derive_") {
            continue;
        }
        let output = match (f.comptime_only, f.ret.as_ref()) {
            (false, _) => UserDeriveOutput::NonComptime,
            (true, Some(ret)) if is_item_syntax_type(ret) => UserDeriveOutput::ItemSyntax,
            (true, Some(ret)) if is_item_syntax_list_type(ret) => UserDeriveOutput::ItemSyntaxList,
            (true, Some(ret)) if is_source_text_type(ret) => UserDeriveOutput::SourceText,
            // Aliases are expanded after derive desugaring. Leave all other
            // return types to the typed `emit_item` boundary so a valid alias
            // for ItemSyntax keeps working.
            _ => continue,
        };
        outputs.insert(f.name.clone(), output);
    }
    outputs
}

/// Expand `type T derive(...)` from its parsed declaration. Record equality is
/// direct AST; other forms call the matching Witchy generator. Unsupported
/// built-in shapes reject, and unknown derives require `derive_<name>`.
pub fn expand(module: &mut Module) -> Result<(), String> {
    expand_with_record_ordering(module, false, false)
}

/// Linking has the reflected declaration's module context and can build
/// `Reflect` implementations directly. Standalone type checks keep the
/// compatibility expansion until their imports are linked.
pub fn expand_linked(module: &mut Module) -> Result<(), String> {
    expand_with_record_ordering(module, false, true)
}

/// The authenticated source session supplies the linked `cmp` declaration
/// needed to check compiler-owned ordering impls immediately.
pub fn expand_admitted(module: &mut Module) -> Result<(), String> {
    expand_with_record_ordering(module, true, false)
}

fn expand_with_record_ordering(module: &mut Module, direct_ordering: bool, direct_reflect: bool) -> Result<(), String> {
    let mut generated: Vec<Item> = Vec::new();
    let mut generated_lines = Vec::new();
    let mut needs_deserialize = false;
    let mut needs_reflect = false;
    let mut needs_show = false;
    let mut needs_public_state = false;
    let aliases = crate::aliases::resolved_map(module)?;
    let user_derive_outputs = user_derive_outputs(module);
    let explicit_partial_eq_targets: HashSet<String> = module
        .items
        .iter()
        .filter_map(|item| {
            let Item::Impl(im) = item else { return None };
            (im.trait_name.as_deref() == Some("PartialEq")).then(|| im.type_name.clone())
        })
        .collect();
    let source_lines = module.item_lines.clone();
    for (item_index, item) in module.items.iter_mut().enumerate() {
        let Item::Type(t) = item else { continue };
        // CONSUME the annotation: this pass runs at every pipeline entry
        // (records::lower is called per stage) and must be idempotent.
        let derives = std::mem::take(&mut t.derives);
        reject_borrowed_nominal_runtime_derives(t, &derives)?;
        if t.variants.is_empty() {
            if let Some(d) = derives.iter().find(|d| builtin_derive_on_fieldless_type(d)) {
                return Err(format!(
                    "type `{}`: derive({d}) does not support fieldless types because they have no constructors",
                    t.name
                ));
            }
        }
        let derive_type = crate::reflect::normalized_type_for_typeinfo(t, &aliases)?;
        let record_equality = derive_type.variants.len() == 1
            && !derive_type.variants[0].field_names.is_empty();
        let has_explicit_partial_eq = explicit_partial_eq_targets.contains(&t.name);
        // (RFC-0047) Record that this type's PartialEq is the STRUCTURAL derive,
        // so whole-program container equality can keep its structural fast path.
        // `derive(Eq)` only implies structural PartialEq when no hand-written
        // `impl PartialEq for T` exists; in that case the Eq derive below emits
        // the missing structural PartialEq. If the user wrote PartialEq, Eq is
        // just the marker and nested equality must keep calling the custom impl.
        // Set (never cleared): `derives` was just consumed, so a later idempotent
        // re-run sees an empty list — the flag must survive that.
        if derives.iter().any(|d| d == "PartialEq")
            || (derives.iter().any(|d| d == "Eq") && !has_explicit_partial_eq)
        {
            t.partial_eq_derived = true;
        }
        if derives.iter().any(|d| d == "PublicState") {
            t.public_state_derived = true;
        }
        let mut emitted_partial_eq = false;
        for d in &derives {
            match d.as_str() {
                "Show" => {
                    generated.push(if record_equality {
                        record_show_impl(&derive_type, source_lines.get(item_index).copied().unwrap_or(0))
                    } else {
                        derive_item_via_comptime("meta.derive_show", &derive_type)
                    });
                    needs_show = true;
                }
                "PartialEq" => {
                    if !emitted_partial_eq {
                        generated.push(if record_equality {
                            record_equality_impl(&derive_type, "PartialEq", source_lines.get(item_index).copied().unwrap_or(0))
                        } else {
                            derive_item_via_comptime("meta.derive_partial_eq", &derive_type)
                        });
                        emitted_partial_eq = true;
                    }
                }
                "Eq" => {
                    if has_float_field(&derive_type) {
                        return Err(format!(
                            "type `{}`: derive(Eq) cannot include `Float` fields because Float is not Eq",
                            t.name
                        ));
                    }
                    if !has_explicit_partial_eq && !emitted_partial_eq {
                        generated.push(if record_equality {
                            record_equality_impl(&derive_type, "PartialEq", source_lines.get(item_index).copied().unwrap_or(0))
                        } else {
                            derive_item_via_comptime("meta.derive_partial_eq", &derive_type)
                        });
                        emitted_partial_eq = true;
                    }
                    generated.push(if record_equality {
                        record_equality_impl(&derive_type, "Eq", source_lines.get(item_index).copied().unwrap_or(0))
                    } else {
                        derive_item_via_comptime("meta.derive_eq", &derive_type)
                    });
                }
                "PartialOrd" => {
                    let is_record = derive_type.variants.len() == 1
                        && !derive_type.variants[0].field_names.is_empty();
                    if !is_record {
                        return Err(format!(
                            "type `{}`: derive(PartialOrd) supports record types (one constructor with named fields)",
                            t.name
                        ));
                    }
                    generated.push(if direct_ordering {
                        record_ordering_impl(&derive_type, true,
                            source_lines.get(item_index).copied().unwrap_or(0))
                    } else {
                        derive_item_via_comptime("meta.derive_partial_ord", &derive_type)
                    });
                }
                "Ord" => {
                    let is_record = derive_type.variants.len() == 1
                        && !derive_type.variants[0].field_names.is_empty();
                    if !is_record {
                        return Err(format!(
                            "type `{}`: derive(Ord) supports record types (one constructor with named fields)",
                            t.name
                        ));
                    }
                    if has_float_field(&derive_type) {
                        return Err(format!(
                            "type `{}`: derive(Ord) cannot include `Float` fields because Float is not Ord",
                            t.name
                        ));
                    }
                    generated.push(if direct_ordering {
                        record_ordering_impl(&derive_type, false,
                            source_lines.get(item_index).copied().unwrap_or(0))
                    } else {
                        derive_item_via_comptime("meta.derive_ord", &derive_type)
                    });
                }
                "Reflect" => {
                    generated.push(if direct_reflect {
                        reflect_impl(&derive_type, source_lines.get(item_index).copied().unwrap_or(0))
                    } else {
                        derive_item_via_comptime("meta.derive_reflect", &derive_type)
                    });
                    needs_reflect = true;
                }
                // Decode only: reflection (json.value_of / stringify / Into(Json))
                // serializes ANY value, so there is no `Serialize` derive; `from_json`
                // reconstruction is per-type (reflection is one-directional), so it is
                // the one derive that remains.
                "Deserialize" => {
                    let is_record = derive_type.variants.len() == 1
                        && !derive_type.variants[0].field_names.is_empty();
                    if !is_record {
                        return Err(format!(
                            "type `{}`: derive(Deserialize) supports record types (one constructor with named fields)",
                            t.name
                        ));
                    }
                    if let Some((field, shape)) = unsupported_deserialize_field(&derive_type) {
                        return Err(format!(
                            "type `{}`: derive(Deserialize) does not support {} field `{}`; decode it manually",
                            t.name, shape, field
                        ));
                    }
                    generated.push(derive_item_via_comptime(
                        "meta.derive_deserialize",
                        &derive_type,
                    ));
                    needs_deserialize = true;
                }
                "PublicState" => {
                    generated.push(derive_item_via_comptime(
                        "meta.derive_public_state",
                        &derive_type,
                    ));
                    needs_public_state = true;
                }
                // A user-defined derive: route to the witchy generator
                // `derive_<name>` (lowercased) — which the program defines or imports
                // and which returns compiler-owned item syntax. An imported
                // generator is checked by the typed `emit_item` boundary.
                other => {
                    let generator = format!("derive_{}", other.to_lowercase());
                    let item = match user_derive_outputs.get(&generator).copied() {
                        Some(UserDeriveOutput::SourceText | UserDeriveOutput::NonComptime) => return Err(format!(
                            "type `{}`: derive({other}) generator `{generator}` must be a comptime function returning ItemSyntax or List(ItemSyntax)",
                            t.name,
                        )),
                        Some(UserDeriveOutput::ItemSyntaxList) => {
                            derive_items_via_comptime(&generator, &derive_type)
                        }
                        Some(UserDeriveOutput::ItemSyntax) | None => {
                            derive_item_via_comptime(&generator, &derive_type)
                        }
                    };
                    generated.push(item);
                }
            }
        }
        generated_lines.resize(
            generated.len(),
            source_lines.get(item_index).copied().unwrap_or(u32::MAX),
        );
    }
    // The generated `from_json` names `Json`'s decoders, so `json` must be
    // imported explicitly. `Result`, `Ok`/`Err`, `Option`, and `Some`/`None` are
    // prelude names; generated deserialize code follows the same visibility rule
    // as handwritten source and does not require redundant result/option imports.
    if needs_deserialize && !module.imports.iter().any(|i| i == "json") {
        return Err("derive(Deserialize) needs `import json` in the module".into());
    }
    if needs_reflect && !module.imports.iter().any(|i| i == "reflect") {
        return Err("derive(Reflect) needs `import reflect` in the module".into());
    }
    // (BUG-299) The generated `impl Show` renders each field/payload through its
    // OWN `Show` — `"Name(" + show(self.f1) + ...`. The scalar `Show` impls
    // (`impl Show for Int`/`String`/`Bool`/`Float`/`Duration`) and the `Show`
    // trait itself live in `std/show`, so the generated impl DEPENDS on that
    // module. `derive(Deserialize)` still asks for explicit `import json`,
    // because json is not prelude. `derive(Show)` is a pervasive, low-friction
    // derive whose dependency is an implementation detail of the codegen — so
    // inject the import rather than burden every `derive(Show)` site with an
    // `import show`.
    // Runs before the linker's import-pull loop (records-lowering happens first),
    // so the pulled module is linked; a program that already imports show is
    // unaffected (idempotent — the annotation is consumed, imports are a set).
    if needs_show && !module.imports.iter().any(|i| i == "show") {
        module.imports.push("show".to_string());
    }
    if needs_public_state && !module.imports.iter().any(|i| i == "public_state") {
        module.imports.push("public_state".to_string());
    }
    module.items.extend(generated);
    // A derive block is synthetic, but its invocation site is the source type
    // carrying the annotation. Comptime provenance reads this parallel line
    // table when it records the generated implementation.
    if !module.item_lines.is_empty() {
        module.item_lines.extend(generated_lines);
    }
    Ok(())
}

/// Desugar a built-in derive into a `comptime:` block that calls the witchy
/// generator `generator(typeInfo)` and emits the returned `meta.ItemSyntax`.
fn derive_item_via_comptime(generator: &str, t: &TypeDef) -> Item {
    let emit = Expr::Call {
        name: "emit_item".into(),
        args: vec![Expr::Call {
            name: generator.into(),
            args: vec![crate::reflect::type_info_expr(t)],
        }],
    };
    Item::Comptime(Block {
        stmts: vec![Stmt::Expr(emit)],
        lines: vec![0],
        region: None,
    })
}

/// Desugar a typed user-defined derive returning `List(ItemSyntax)`.
fn derive_items_via_comptime(generator: &str, t: &TypeDef) -> Item {
    let item_name = "generated_item".to_string();
    let emit = Expr::For {
        var: item_name.clone(),
        iter: Box::new(Expr::Call {
            name: generator.into(),
            args: vec![crate::reflect::type_info_expr(t)],
        }),
        body: Block {
            stmts: vec![Stmt::Expr(Expr::Call {
                name: "emit_item".into(),
                args: vec![Expr::Var(item_name)],
            })],
            lines: vec![0],
            region: None,
        },
    };
    Item::Comptime(Block {
        stmts: vec![Stmt::Expr(emit)],
        lines: vec![0],
        region: None,
    })
}
