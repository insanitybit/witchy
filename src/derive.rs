//! `derive(...)` — desugar each derive to a comptime call of its witchy generator.
//!
//! The per-trait code generation is NOT here anymore: a `derive(X)` becomes a
//! `comptime:` block `emit(meta.derive_<x>(typeInfo_of_T))` (built-ins live in
//! `std/meta`; a user derive routes to a bare `derive_<x>` they define). The
//! comptime expansion (which runs next) calls the generator over the type's
//! structure and appends the impl source it returns — so derives are ordinary,
//! user-extensible witchy code over `meta.TypeInfo`, generated BEFORE type checking.

use crate::ast::*;

/// Expand every `type T derive(...)` into a comptime call of the matching witchy
/// generator. Unsupported shapes for the built-ins are loud errors; an unknown
/// derive routes to a user-provided `derive_<name>` (a comptime error if absent).
pub fn expand(module: &mut Module) -> Result<(), String> {
    let mut generated: Vec<Item> = Vec::new();
    let mut needs_deserialize = false;
    let mut needs_option = false;
    let mut needs_reflect = false;
    for item in &mut module.items {
        let Item::Type(t) = item else { continue };
        // CONSUME the annotation: this pass runs at every pipeline entry
        // (records::lower is called per stage) and must be idempotent.
        let derives = std::mem::take(&mut t.derives);
        for d in &derives {
            match d.as_str() {
                "Show" => generated.push(derive_via_comptime("meta.derive_show", t)),
                "PartialEq" => generated.push(derive_via_comptime("meta.derive_partial_eq", t)),
                "Eq" => generated.push(derive_via_comptime("meta.derive_eq", t)),
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
                    generated.push(derive_via_comptime("meta.derive_deserialize", t));
                    needs_deserialize = true;
                    if t.variants.first().is_some_and(|v| {
                        v.fields
                            .iter()
                            .any(|ty| matches!(ty, Type::Named(n, a) if n == "Option" && a.len() == 1))
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
            args: vec![crate::comptime::type_info_expr(t)],
        }],
    };
    Item::Comptime(Block {
        stmts: vec![Stmt::Expr(emit)],
        lines: vec![0],
        restrict: None,
        region: None,
    })
}
