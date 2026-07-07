//! `derive(...)` — desugar each derive to a comptime call of its witchy generator.
//!
//! The per-trait code generation is NOT here anymore: a `derive(X)` becomes a
//! `comptime:` block `emit(meta.derive_<x>(typeInfo_of_T))` (built-ins live in
//! `std/meta`; a user derive routes to a bare `derive_<x>` they define). The
//! comptime expansion (which runs next) calls the generator over the type's
//! structure and appends the impl source it returns — so derives are ordinary,
//! user-extensible witchy code over `meta.TypeInfo`, generated BEFORE type checking.

use crate::ast::*;

/// Whether an `Option` constructor appears anywhere inside `ty` (the type itself
/// or any of its argument positions) — a `derive(Deserialize)` field of shape
/// `Option(T)`, `List(Option(T))`, or `Option(Option(T))` all reach `Some`/`None`
/// in the generated decoder, so all need the option support in scope.
fn mentions_option(ty: &Type) -> bool {
    match ty {
        Type::Named(n, args) => {
            (n == "Option" && args.len() == 1) || args.iter().any(mentions_option)
        }
        Type::Qualified(_, inner) => mentions_option(inner),
        _ => false,
    }
}

fn contains_concrete_float(ty: &Type) -> bool {
    match ty {
        Type::Named(n, args) => n == "Float" || args.iter().any(contains_concrete_float),
        Type::Tuple(slots) => slots.iter().any(contains_concrete_float),
        Type::Fn(params, ret) => {
            params.iter().any(contains_concrete_float) || contains_concrete_float(ret)
        }
        Type::Qualified(_, inner) => contains_concrete_float(inner),
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
        Type::Fn(_, _) => Some("function"),
        Type::Qualified(_, inner) => unsupported_deserialize_shape(inner),
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

/// Expand every `type T derive(...)` into a comptime call of the matching witchy
/// generator. Unsupported shapes for the built-ins are loud errors; an unknown
/// derive routes to a user-provided `derive_<name>` (a comptime error if absent).
pub fn expand(module: &mut Module) -> Result<(), String> {
    let mut generated: Vec<Item> = Vec::new();
    let mut needs_deserialize = false;
    let mut needs_option = false;
    let mut needs_reflect = false;
    let mut needs_show = false;
    for item in &mut module.items {
        let Item::Type(t) = item else { continue };
        // CONSUME the annotation: this pass runs at every pipeline entry
        // (records::lower is called per stage) and must be idempotent.
        let derives = std::mem::take(&mut t.derives);
        // (RFC-0047) Record that this type's PartialEq is the STRUCTURAL derive, so
        // the whole-program `==`-through-PartialEq rule keeps the fast path for it
        // (a container of `T` recurses structurally rather than calling an impl).
        // Set (never cleared): `derives` was just consumed, so a later idempotent
        // re-run sees an empty list — the flag must survive that.
        if derives.iter().any(|d| d == "PartialEq" || d == "Eq") {
            t.partial_eq_derived = true;
        }
        let mut emitted_partial_eq = false;
        for d in &derives {
            match d.as_str() {
                "Show" => {
                    generated.push(derive_via_comptime("meta.derive_show", t));
                    needs_show = true;
                }
                "PartialEq" => {
                    if !emitted_partial_eq {
                        generated.push(derive_via_comptime("meta.derive_partial_eq", t));
                        emitted_partial_eq = true;
                    }
                }
                "Eq" => {
                    if has_float_field(t) {
                        return Err(format!(
                            "type `{}`: derive(Eq) cannot include `Float` fields because Float is not Eq",
                            t.name
                        ));
                    }
                    if !emitted_partial_eq {
                        generated.push(derive_via_comptime("meta.derive_partial_eq", t));
                        emitted_partial_eq = true;
                    }
                    generated.push(derive_via_comptime("meta.derive_eq", t));
                }
                "PartialOrd" => {
                    let is_record = t.variants.len() == 1 && !t.variants[0].field_names.is_empty();
                    if !is_record {
                        return Err(format!(
                            "type `{}`: derive(PartialOrd) supports record types (one constructor with named fields)",
                            t.name
                        ));
                    }
                    generated.push(derive_via_comptime("meta.derive_partial_ord", t));
                }
                "Ord" => {
                    let is_record = t.variants.len() == 1 && !t.variants[0].field_names.is_empty();
                    if !is_record {
                        return Err(format!(
                            "type `{}`: derive(Ord) supports record types (one constructor with named fields)",
                            t.name
                        ));
                    }
                    if has_float_field(t) {
                        return Err(format!(
                            "type `{}`: derive(Ord) cannot include `Float` fields because Float is not Ord",
                            t.name
                        ));
                    }
                    generated.push(derive_via_comptime("meta.derive_ord", t));
                }
                "Reflect" => {
                    generated.push(derive_via_comptime("meta.derive_reflect", t));
                    needs_reflect = true;
                }
                // Decode only: reflection (json.value_of / stringify / Into(Json))
                // serializes ANY value, so there is no `Serialize` derive; `from_json`
                // reconstruction is per-type (reflection is one-directional), so it is
                // the one derive that remains.
                "Deserialize" => {
                    let is_record = t.variants.len() == 1 && !t.variants[0].field_names.is_empty();
                    if !is_record {
                        return Err(format!(
                            "type `{}`: derive(Deserialize) supports record types (one constructor with named fields)",
                            t.name
                        ));
                    }
                    if let Some((field, shape)) = unsupported_deserialize_field(t) {
                        return Err(format!(
                            "type `{}`: derive(Deserialize) does not support {} field `{}`; decode it manually",
                            t.name, shape, field
                        ));
                    }
                    generated.push(derive_via_comptime("meta.derive_deserialize", t));
                    needs_deserialize = true;
                    // `Option` anywhere in a field type — a direct field, a list
                    // element (`List(Option(T))`), or a nested option
                    // (`Option(Option(T))`) — makes the generated decoder reach for
                    // `Some`/`None`, so the import story must account for all of them.
                    if t.variants.first().is_some_and(|v| {
                        v.fields.iter().any(mentions_option)
                    }) {
                        needs_option = true;
                    }
                }
                // A user-defined derive: route to the witchy generator
                // `derive_<name>` (lowercased) — which the program defines or imports
                // and which returns the impl source for the type. Anyone can add a
                // derive this way; the per-trait codegen is no longer Rust-only.
                other => {
                    generated.push(derive_via_comptime(
                        &format!("derive_{}", other.to_lowercase()),
                        t,
                    ));
                }
            }
        }
    }
    // The generated `from_json` names `Json`'s decoders and `Result`/`Ok`/`Err`, and
    // the parser has already qualified calls by the imports it SAW, so the imports
    // must be written by the user, not injected.
    if needs_deserialize && !module.imports.iter().any(|i| i == "json") {
        return Err("derive(Deserialize) needs `import json` in the module".into());
    }
    if needs_deserialize && !module.imports.iter().any(|i| i == "result") {
        return Err("derive(Deserialize) needs `import result` in the module (from_json returns Result)".into());
    }
    if needs_option && !module.imports.iter().any(|i| i == "option") {
        return Err("derive(Deserialize) on a type with an Option field needs `import option`".into());
    }
    if needs_reflect && !module.imports.iter().any(|i| i == "reflect") {
        return Err("derive(Reflect) needs `import reflect` in the module".into());
    }
    // (BUG-299) The generated `impl Show` renders each field/payload through its
    // OWN `Show` — `"Name(" + show(self.f1) + ...`. The scalar `Show` impls
    // (`impl Show for Int`/`String`/`Bool`/`Float`/`Duration`) and the `Show`
    // trait itself live in `std/show`, so the generated impl DEPENDS on that
    // module. Unlike `derive(Deserialize)` (which errors and asks the user to
    // import json/result), `derive(Show)` is a pervasive, low-friction derive
    // whose dependency is an implementation detail of the codegen — so inject the
    // import rather than burden every `derive(Show)` site with an `import show`.
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

/// Desugar one `derive(X)` into a `comptime:` block that calls the witchy
/// generator `generator(typeInfo)` and emits its result — so the per-trait code
/// generation lives in witchy (std/meta), not here. The type's structure is
/// embedded as a `meta.TypeInfo` literal; comptime auto-imports `meta`.
fn derive_via_comptime(generator: &str, t: &TypeDef) -> Item {
    let emit = Expr::Call {
        name: "emit".into(),
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
