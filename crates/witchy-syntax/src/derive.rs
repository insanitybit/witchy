//! `derive(...)` — desugar each derive to a comptime call of its witchy generator.
//!
//! The per-trait code generation is NOT here anymore: a `derive(X)` becomes a
//! `comptime:` block `emit_item(meta.derive_<x>(typeInfo_of_T))`
//! (built-ins live in `std/meta`; a user derive routes to a bare `derive_<x>`
//! they define and is source-wrapped for compatibility). Built-ins now return
//! `meta.ItemSyntax`; user derives may keep returning source text during the
//! migration.

use crate::ast::*;
// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashSet};

#[derive(Clone, Copy)]
enum UserDeriveOutput {
    SourceString,
    ItemSyntax,
    ItemSyntaxList,
}

fn contains_concrete_float(ty: &Type) -> bool {
    match ty {
        Type::Named(n, args) => n == "Float" || args.iter().any(contains_concrete_float),
        Type::Tuple(slots) => slots.iter().any(contains_concrete_float),
        Type::Fn(params, ret, _) => {
            params.iter().any(contains_concrete_float) || contains_concrete_float(ret)
        }
        Type::Qualified(_, inner) => contains_concrete_float(inner),
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
        Type::Fn(_, _, _) => Some("function"),
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
        "Show" | "PartialEq" | "Eq" | "PartialOrd" | "Ord" | "Reflect" | "Deserialize"
    )
}

fn is_item_syntax_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, args) if args.is_empty() && (name == "ItemSyntax" || name == "meta.ItemSyntax"))
}

fn is_item_syntax_list_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, args) if name == "List" && matches!(args.as_slice(), [inner] if is_item_syntax_type(inner)))
}

fn user_derive_outputs(module: &Module) -> HashMap<String, UserDeriveOutput> {
    let mut outputs = HashMap::default();
    for item in &module.items {
        let Item::Function(f) = item else { continue };
        if !f.comptime_only || !f.name.starts_with("derive_") {
            continue;
        }
        let Some(ret) = &f.ret else { continue };
        let output = if is_item_syntax_type(ret) {
            Some(UserDeriveOutput::ItemSyntax)
        } else if is_item_syntax_list_type(ret) {
            Some(UserDeriveOutput::ItemSyntaxList)
        } else {
            None
        };
        if let Some(output) = output {
            outputs.insert(f.name.clone(), output);
        }
    }
    outputs
}

/// Expand every `type T derive(...)` into a comptime call of the matching witchy
/// generator. Unsupported shapes for the built-ins are loud errors; an unknown
/// derive routes to a user-provided `derive_<name>` (a comptime error if absent).
pub fn expand(module: &mut Module) -> Result<(), String> {
    let mut generated: Vec<Item> = Vec::new();
    let mut needs_deserialize = false;
    let mut needs_reflect = false;
    let mut needs_show = false;
    let aliases = crate::aliases::resolved_map(module);
    let user_derive_outputs = user_derive_outputs(module);
    let explicit_partial_eq_targets: HashSet<String> = module
        .items
        .iter()
        .filter_map(|item| {
            let Item::Impl(im) = item else { return None };
            (im.trait_name.as_deref() == Some("PartialEq")).then(|| im.type_name.clone())
        })
        .collect();
    for item in &mut module.items {
        let Item::Type(t) = item else { continue };
        // CONSUME the annotation: this pass runs at every pipeline entry
        // (records::lower is called per stage) and must be idempotent.
        let derives = std::mem::take(&mut t.derives);
        if t.variants.is_empty() {
            if let Some(d) = derives.iter().find(|d| builtin_derive_on_fieldless_type(d)) {
                return Err(format!(
                    "type `{}`: derive({d}) does not support fieldless types because they have no constructors",
                    t.name
                ));
            }
        }
        let derive_type = crate::reflect::normalized_type_for_typeinfo(t, &aliases);
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
        let mut emitted_partial_eq = false;
        for d in &derives {
            match d.as_str() {
                "Show" => {
                    generated.push(derive_item_via_comptime("meta.derive_show", &derive_type));
                    needs_show = true;
                }
                "PartialEq" => {
                    if !emitted_partial_eq {
                        generated.push(derive_item_via_comptime("meta.derive_partial_eq", &derive_type));
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
                        generated.push(derive_item_via_comptime("meta.derive_partial_eq", &derive_type));
                        emitted_partial_eq = true;
                    }
                    generated.push(derive_item_via_comptime("meta.derive_eq", &derive_type));
                }
                "PartialOrd" => {
                    let is_record =
                        derive_type.variants.len() == 1 && !derive_type.variants[0].field_names.is_empty();
                    if !is_record {
                        return Err(format!(
                            "type `{}`: derive(PartialOrd) supports record types (one constructor with named fields)",
                            t.name
                        ));
                    }
                    generated.push(derive_item_via_comptime("meta.derive_partial_ord", &derive_type));
                }
                "Ord" => {
                    let is_record =
                        derive_type.variants.len() == 1 && !derive_type.variants[0].field_names.is_empty();
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
                    generated.push(derive_item_via_comptime("meta.derive_ord", &derive_type));
                }
                "Reflect" => {
                    generated.push(derive_item_via_comptime("meta.derive_reflect", &derive_type));
                    needs_reflect = true;
                }
                // Decode only: reflection (json.value_of / stringify / Into(Json))
                // serializes ANY value, so there is no `Serialize` derive; `from_json`
                // reconstruction is per-type (reflection is one-directional), so it is
                // the one derive that remains.
                "Deserialize" => {
                    let is_record =
                        derive_type.variants.len() == 1 && !derive_type.variants[0].field_names.is_empty();
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
                    generated.push(derive_item_via_comptime("meta.derive_deserialize", &derive_type));
                    needs_deserialize = true;
                }
                // A user-defined derive: route to the witchy generator
                // `derive_<name>` (lowercased) — which the program defines or imports
                // and which returns the impl source for the type. Anyone can add a
                // derive this way; the per-trait codegen is no longer Rust-only.
                other => {
                    let generator = format!("derive_{}", other.to_lowercase());
                    generated.push(match user_derive_outputs
                        .get(&generator)
                        .copied()
                        .unwrap_or(UserDeriveOutput::SourceString)
                    {
                        UserDeriveOutput::SourceString => {
                            derive_source_via_comptime(&generator, &derive_type)
                        }
                        UserDeriveOutput::ItemSyntax => {
                            derive_item_via_comptime(&generator, &derive_type)
                        }
                        UserDeriveOutput::ItemSyntaxList => {
                            derive_items_via_comptime(&generator, &derive_type)
                        }
                    });
                }
            }
        }
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

/// Desugar a user-defined derive through the legacy source-string contract,
/// then wrap that source as `meta.ItemSyntax` at the append boundary.
fn derive_source_via_comptime(generator: &str, t: &TypeDef) -> Item {
    let emit = Expr::Call {
        name: "emit_item".into(),
        args: vec![Expr::Call {
            name: "item".into(),
            args: vec![Expr::Call {
                name: generator.into(),
                args: vec![crate::reflect::type_info_expr(t)],
            }],
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
