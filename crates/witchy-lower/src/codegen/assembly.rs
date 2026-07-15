//! Module assembly: the compile entry points and the wiring that turns lowered
//! per-function WIR into a finished module — reachability roots, item
//! registration, the static prelude/helper-registry selection, and the final
//! `WirModule` -> wasm encode. `compile_module_binary`/`assemble_wir_module`/
//! `compile_build_module` are the public entry points (re-exported by the parent).

use super::*;

/// The names of every JS-callable string export in declaration order (`__export_*`
/// wrappers are emitted for these and they are extra reachability roots).
fn string_export_functions(module: &Module) -> Vec<String> {
    let grantable = grantable_cap_names(module);
    module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Function(f) if is_string_export(f, &grantable) => Some(f.name.clone()),
            _ => None,
        })
        .collect()
}

/// (RFC-0040) The bare grantable capability type names declared in the module.
fn grantable_cap_names(module: &Module) -> HashSet<&str> {
    module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => Some(t.name.as_str()),
            _ => None,
        })
        .collect()
}

fn is_compiler_syntax_type_name(name: &str) -> bool {
    matches!(
        name,
        "meta.ItemSyntax" | "meta.TypeSyntax" | "meta.ExprSyntax" | "meta.PatternSyntax"
            | "meta.SyntaxHole" | "meta.StmtSyntax" | "meta.BlockSyntax"
            | "meta.MatchArmSyntax" | "meta.ParamSyntax" | "meta.Ident" | "ItemSyntax"
            | "TypeSyntax" | "ExprSyntax" | "PatternSyntax" | "SyntaxHole"
            | "StmtSyntax" | "BlockSyntax" | "MatchArmSyntax" | "ParamSyntax" | "Ident"
    )
}

fn ast_type_mentions_compiler_syntax(ty: &Type) -> bool {
    match ty {
        Type::Named(name, args) => {
            is_compiler_syntax_type_name(name)
                || args.iter().any(ast_type_mentions_compiler_syntax)
        }
        Type::Tuple(items) => items.iter().any(ast_type_mentions_compiler_syntax),
        Type::Fn(params, ret, _) => {
            params.iter().any(ast_type_mentions_compiler_syntax)
                || ast_type_mentions_compiler_syntax(ret)
        }
        Type::Qualified(_, inner) => ast_type_mentions_compiler_syntax(inner),
    }
}

fn function_signature_mentions_compiler_syntax(f: &Function) -> bool {
    f.params
        .iter()
        .filter_map(|p| p.ty.as_ref())
        .any(ast_type_mentions_compiler_syntax)
        || f.ret.as_ref().is_some_and(ast_type_mentions_compiler_syntax)
}

fn strip_compiler_syntax_items_for_runtime(mut module: Module) -> Module {
    module.items.retain(|item| match item {
        Item::Type(t) => !is_compiler_syntax_type_name(&t.name),
        Item::Function(f) => !function_signature_mentions_compiler_syntax(f),
        _ => true,
    });
    module
}

/// (RFC-0040) If `f` is a cap-gated string export (`export_*(cap, String)`), the
/// leading grantable capability's `(type name, field count)`.
fn export_cap_of<'a>(f: &'a Function, module: &'a Module) -> Option<(&'a str, usize)> {
    let grantable = grantable_cap_names(module);
    let cap = match f.params.as_slice() {
        [cap, _s] => crate::codegen::export_cap_name(cap).filter(|n| grantable.contains(n))?,
        _ => return None,
    };
    let nfields = module.items.iter().find_map(|it| match it {
        Item::Type(t) if t.name == cap => t.variants.first().map(|v| v.fields.len()),
        _ => None,
    })?;
    Some((cap, nfields))
}

/// The functions reachable from `main` (+ string-export roots), plus `extra_roots`
/// — additional reachability roots for functions a reached AST body does not name
/// directly. (RFC-0047) A container `==` over a CUSTOM-`PartialEq` element type
/// calls that type's `PartialEq__T__eq` from a codegen-synthesized eq helper, so the
/// call is invisible to the AST walk; seeding those impls as roots keeps them (and
/// their transitive callees) emitted, so the honored-at-every-depth guarantee holds
/// for the compiled backend too.
fn reachable_functions_with(module: &Module, extra_roots: &[String]) -> HashSet<String> {
    let mut bodies: HashMap<&str, &Block> = HashMap::new();
    for item in &module.items {
        if let Item::Function(f) = item {
            bodies.insert(f.name.as_str(), &f.body);
        }
    }
    let mut reachable: HashSet<String> = HashSet::new();
    let mut work: Vec<String> = Vec::new();
    if bodies.contains_key("main") {
        reachable.insert("main".to_string());
        work.push("main".to_string());
    }
    // String exports (`pub fn f(String) -> String`) are additional roots: the host
    // calls them directly through their `__export_*` wrapper, so they must be
    // compiled and kept even when `main` never reaches them.
    for name in string_export_functions(module) {
        if reachable.insert(name.clone()) {
            work.push(name);
        }
    }
    for name in extra_roots {
        if bodies.contains_key(name.as_str()) && reachable.insert(name.clone()) {
            work.push(name.clone());
        }
    }
    while let Some(name) = work.pop() {
        if let Some(body) = bodies.get(name.as_str()) {
            let mut refs = HashSet::new();
            collect_fn_refs_block(body, &mut refs);
            for r in refs {
                if bodies.contains_key(r.as_str()) && reachable.insert(r.clone()) {
                    work.push(r);
                }
            }
        }
    }
    reachable
}

fn eq_impl_types(module: &Module) -> HashSet<String> {
    module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Impl(im) if im.trait_name.as_deref() == Some("Eq") => Some(im.type_name.clone()),
            _ => None,
        })
        .collect()
}

fn transparent_externref_brand_entries(module: &Module) -> Vec<(String, String, Type)> {
    let candidates: Vec<(String, String, Type)> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(t) if t.is_capability && t.variants.len() == 1 => {
                let variant = t.variants.first()?;
                if variant.name == t.name && variant.field_names.is_empty() && variant.fields.len() == 1 {
                    Some((t.name.clone(), variant.name.clone(), variant.fields[0].clone()))
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    let mut transparent: HashSet<String> = HashSet::new();
    let mut out: Vec<(String, String, Type)> = Vec::new();
    let mut changed = true;
    while changed {
        changed = false;
        for (brand, ctor, field) in &candidates {
            if transparent.contains(brand) {
                continue;
            }
            let is_ref = match field.unqualified() {
                Type::Named(n, _) if is_builtin_externref_type(n) => true,
                Type::Named(n, args) if args.is_empty() => transparent.contains(n),
                _ => false,
            };
            if is_ref {
                transparent.insert(brand.clone());
                out.push((brand.clone(), ctor.clone(), field.clone()));
                changed = true;
            }
        }
    }
    out
}

/// Register every item's compile-time metadata (parameter conventions,
/// return kinds/types, record fields, generic shape hints, ...) on `cg`.
fn register_module_items(cg: &mut Codegen, module: &Module) {
    // `Option`/`Result` are language-level (`?`, `Some`/`Ok` literals, the
    // interpreter evaluates them natively): their constructors exist for
    // patterns whether or not std/option / std/result are linked. Tags match
    // the std declarations (Some=0/None=1, Ok=0/Err=1); if the modules ARE
    // linked, the Item::Type pass below re-registers identical values.
    for (ty, variants) in [
        ("Option", [("Some", 1usize), ("None", 0)]),
        ("Result", [("Ok", 1), ("Err", 1)]),
    ] {
        cg.adt_variant_names
            .insert(ty.to_string(), variants.iter().map(|(n, _)| n.to_string()).collect());
        for (tag, (name, nfields)) in variants.iter().enumerate() {
            cg.ctor_type_name.insert(name.to_string(), ty.to_string());
            cg.ctors.insert(name.to_string(), (tag as u32, *nfields));
        }
    }
    for (brand, ctor, field) in transparent_externref_brand_entries(module) {
        cg.transparent_externref_brands.insert(brand);
        cg.transparent_externref_ctors.insert(ctor, field);
    }
    // (RFC-0005 stage 4 / BUG-566) The GC-record classification lives in ONE
    // home — typeck — and codegen consumes it, so the boundary checks and the
    // struct registration can never disagree on which records are GC-lowered.
    let gc_records = witchy_types::typeck::gc_cap_record_entries(module);
    // Collect parameter conventions up front so call sites can resolve `var`
    // write-back even for forward references.
    for item in &module.items {
        match item {
            Item::Function(f) => {
                cg.fn_conventions.insert(
                    f.name.clone(),
                    f.params.iter().map(|p| p.convention).collect(),
                );
                cg.fn_params.insert(f.name.clone(), f.params.clone());
                let ret = f.ret.as_ref().map(|t| cg.kind_for_type(t)).unwrap_or(Kind::I32);
                cg.fn_ret.insert(f.name.clone(), ret);
                if let Some(t) = &f.ret {
                    cg.fn_ret_valtype.insert(f.name.clone(), ty_to_valtype(t));
                    cg.fn_ret_ty.insert(f.name.clone(), t.clone());
                    if type_is_unique_capacity(t) {
                        cg.fn_unique_ret.insert(f.name.clone());
                    }
                }
                // A function returning a closure (`-> fn(...) -> RET`): record the
                // closure's return kind so a `let f = make(...)` then `f(x)` call
                // recovers the result at the right width.
                if let Some(Type::Fn(_, cret, _)) = &f.ret {
                    cg.fn_ret_closure_kind.insert(f.name.clone(), cg.kind_for_type(cret));
                }
                // A function returning a tuple: record its slot value types so a
                // `let (a, b) = f(...)` destructures each at the right width.
                if let Some(Type::Tuple(slots)) = &f.ret {
                    cg.fn_ret_tuple_slots
                        .insert(f.name.clone(), slots.iter().map(ty_to_valtype).collect());
                    // Per slot, the element type if the slot is `List(<scalar>)`
                    // (e.g. unzip's `(List(Int), List(Int))`), so a destructure
                    // binds each list var's element type.
                    let elems: Vec<Option<ValType>> = slots
                        .iter()
                        .map(|t| match t {
                            Type::Named(n, a) if n == "List" => a.first().and_then(|e| {
                                match ty_to_valtype(e) {
                                    ValType::Other => None,
                                    vt => Some(vt),
                                }
                            }),
                            _ => None,
                        })
                        .collect();
                    if elems.iter().any(|e| e.is_some()) {
                        cg.fn_ret_tuple_slot_list_elem.insert(f.name.clone(), elems);
                    }
                }
            }
            Item::Type(t) if !is_compiler_syntax_type_name(&t.name) => {
                if t.packed {
                    cg.packed_types.insert(t.name.clone());
                }
                cg.adt_variants
                    .insert(t.name.clone(), t.variants.iter().map(|v| v.fields.clone()).collect());
                cg.adt_variant_names
                    .insert(t.name.clone(), t.variants.iter().map(|v| v.name.clone()).collect());
                for (tag, variant) in t.variants.iter().enumerate() {
                    cg.ctor_type_name.insert(variant.name.clone(), t.name.clone());
                    cg.ctors
                        .insert(variant.name.clone(), (tag as u32, variant.fields.len()));
                    if !variant.field_names.is_empty() {
                        let fields = variant
                            .field_names
                            .iter()
                            .zip(&variant.fields)
                            .map(|(name, ty)| {
                                let ty_name = match ty {
                                    Type::Named(n, _) => Some(n.clone()),
                                    _ => None,
                                };
                                (name.clone(), ty_name)
                            })
                            .collect();
                        cg.record_fields.insert(t.name.clone(), fields);
                        cg.record_field_types.insert(t.name.clone(), variant.fields.clone());
                        // Declared type parameters, in order (`Pair(a, b)` -> [a, b]),
                        // so a generic record's `RecInst` maps use-site type arguments
                        // to the correct field type variable even when fields are
                        // declared out of parameter order (BUG-319).
                        cg.record_generics.insert(t.name.clone(), t.params.clone());
                    }
                }
            }
            Item::Type(_)
            | Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    for (name, ctor) in gc_records {
        if cg.gc_record_ids.contains_key(&name) {
            continue;
        }
        let id = cg.gc_structs.len() as u32;
        cg.gc_record_ids.insert(name.clone(), id);
        cg.gc_record_ctors.insert(ctor, name);
        cg.gc_structs.push(witchy_wir::wir::WirStructDef { fields: Vec::new() });
    }
    let gc_names: Vec<String> = cg.gc_record_ids.keys().cloned().collect();
    for name in gc_names {
        let Some(id) = cg.gc_record_ids.get(&name).copied() else {
            continue;
        };
        let fields = cg
            .record_field_types
            .get(&name)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|ty| Codegen::wir_kind(cg.kind_for_type(ty)))
            .collect();
        if let Some(slot) = cg.gc_structs.get_mut(id as usize) {
            slot.fields = fields;
        }
    }
    // Function return kinds may have been recorded before the GC-record registry
    // existed. Refresh them now so forward references to cap-carrying records use
    // the `(ref null $s)` ABI at call sites.
    for item in &module.items {
        if let Item::Function(f) = item {
            let ret = f.ret.as_ref().map(|t| cg.kind_for_type(t)).unwrap_or(Kind::I32);
            cg.fn_ret.insert(f.name.clone(), ret);
            if let Some(Type::Fn(_, cret, _)) = &f.ret {
                cg.fn_ret_closure_kind.insert(f.name.clone(), cg.kind_for_type(cret));
            }
        }
    }
    // (RFC-0047) The whole-program set of types with a CUSTOM (non-derived)
    // `PartialEq`. Detected post-lowering (like the interpreter): a declared type
    // whose `PartialEq__T__eq` function exists but which did NOT derive PartialEq.
    // A compound `==` over such an element type calls that impl rather than
    // recursing structurally; everything else keeps the structural fast path.
    {
        let has_eq_fn = |name: &str| {
            let mangled = format!("PartialEq__{name}__eq");
            module
                .items
                .iter()
                .any(|it| matches!(it, Item::Function(f) if f.name == mangled))
        };
        for item in &module.items {
            if let Item::Type(t) = item {
                if is_compiler_syntax_type_name(&t.name) {
                    continue;
                }
                if !t.partial_eq_derived && has_eq_fn(&t.name) {
                    cg.custom_eq_types.insert(t.name.clone());
                }
            }
        }
    }
    // Now that all record types are known, record which constructor fields are
    // records, so binding `Circle(p)` in a pattern lets `p.field` resolve.
    for item in &module.items {
        if let Item::Type(t) = item {
            if is_compiler_syntax_type_name(&t.name) {
                continue;
            }
            for variant in &t.variants {
                let field_recs: Vec<Option<String>> = variant
                    .fields
                    .iter()
                    .map(|ty| match ty {
                        Type::Named(n, _) if cg.record_fields.contains_key(n) => Some(n.clone()),
                        _ => None,
                    })
                    .collect();
                if field_recs.iter().any(|r| r.is_some()) {
                    cg.ctor_field_records.insert(variant.name.clone(), field_recs);
                }
            }
        }
    }
    // Now that record types are known, note which functions return a record, so
    // `let q = f(...)` resolves `q.field`; and which return a Result/Option whose
    // success payload is a record, so `let q = f(...)?` resolves it too.
    for item in &module.items {
        if let Item::Function(f) = item {
            if let Some(Type::Named(n, args)) = &f.ret {
                if cg.record_fields.contains_key(n) {
                    cg.fn_ret_records.insert(f.name.clone(), n.clone());
                } else if n == "List" {
                    // `List(Account)`: `for x in f(...)` binds x to that record.
                    if let Some(Type::Named(elem, _)) = args.first() {
                        if cg.record_fields.contains_key(elem) {
                            cg.fn_ret_list_elem.insert(f.name.clone(), elem.clone());
                        }
                    }
                    // `List(String)` etc.: record the scalar element value type so
                    // `list.at(f(...), i)` is typed (e.g. a String element compares by
                    // content). Skips `Other` (generic / non-scalar elements).
                    if let Some(elem) = args.first() {
                        let evt = ty_to_valtype(elem);
                        if evt != ValType::Other {
                            cg.fn_ret_list_elem_valtype.insert(f.name.clone(), evt);
                        }
                        // `List((T, U))` (e.g. zip): record the element tuple's
                        // slot types so a destructure of `list.at(f(...), i)` is typed.
                        if let Type::Tuple(slots) = elem {
                            cg.fn_ret_list_elem_tuple_slots
                                .insert(f.name.clone(), slots.iter().map(ty_to_valtype).collect());
                        }
                    }
                } else if let Some(payload) = args.first() {
                    // e.g. `Result(Account, _)` / `Option(Account)`: `?` yields it.
                    if let Type::Named(rec, _) = payload {
                        if cg.record_fields.contains_key(rec) {
                            cg.fn_ret_result_record.insert(f.name.clone(), rec.clone());
                        }
                    }
                    // A scalar success payload (e.g. `Option(Int)` from parse_int,
                    // or a user `R(Int, _)`): record it so a `match`/`?` recovers
                    // the Some/Ok value at the right width instead of truncating a
                    // big Int to the generic i32. The success payload is the first
                    // type argument (true for Option/Result and result-like sum
                    // types); only ever consulted at a Some/Ok/`?` site, so a
                    // non-result type's first arg is harmless.
                    let pvt = ty_to_valtype(payload);
                    if pvt != ValType::Other {
                        cg.fn_ret_result_valtype.insert(f.name.clone(), pvt);
                    }
                }
            }
            // Generic shapes over a `List(a)` argument: `-> Option(a)/Result(a,_)`
            // (find/head/min_by) and `-> List(a)` (filter/take/reverse/sort_by).
            // Record which argument carries `a` so a call's payload / element
            // record type resolves from that argument, without full inference.
            if let Some(tv) = payload_type_var(&f.ret) {
                if let Some(k) = list_param_of_var(&f.params, &tv) {
                    cg.fn_ret_option_of_list_arg.insert(f.name.clone(), k);
                }
            }
            if let Some(tv) = list_elem_type_var(&f.ret) {
                if let Some(k) = list_param_of_var(&f.params, &tv) {
                    cg.fn_ret_list_of_list_arg.insert(f.name.clone(), k);
                } else if let Some(k) = fn_param_returning_var(&f.params, &tv) {
                    // `map`: result element type is the mapper's return type.
                    cg.fn_ret_list_of_fn_arg.insert(f.name.clone(), k);
                }
            }
        }
    }
}

/// Compile a module straight to a wasm **binary** via WIR + `wir_encode::encode`.
/// Returns `Ok(Some(bytes))` only when the whole module assembles to WIR (see
/// `assemble_wir_module`); otherwise `Ok(None)`, which the caller treats as a
/// hard "cannot compile" error (there is no WAT fallback). The `wir_opt`
/// slot-elimination pass runs before encoding, and the assembled binary is
/// wasm-validated — an assembly slip returns `Ok(None)` rather than shipping a
/// malformed module.
pub fn compile_module_binary(
    module: &Module,
) -> Result<Option<Vec<u8>>, CodegenError> {
    let Some((mut wir_module, gc_structs)) = assemble_wir_module_with_structs(module)? else {
        return Ok(None);
    };
    witchy_wir::wir_opt::lower_direct_tail_calls(&mut wir_module);
    witchy_wir::wir_opt::optimize(&mut wir_module);
    // Robustness net: if any reached `Call` names a func that didn't make it into
    // the module — an unregistered guest helper like `$string_from_code`, which
    // `assemble`'s prelude/wir-helper resolution doesn't account for — bail with
    // `Ok(None)` rather than panic in the encoder's func-index lookup.
    {
        let mut defined: HashSet<String> = HashSet::new();
        for imp in &wir_module.imports {
            defined.insert(imp.name.clone());
        }
        for f in &wir_module.funcs {
            defined.insert(f.name.clone());
        }
        let mut called: HashSet<String> = HashSet::new();
        for f in &wir_module.funcs {
            collect_called_funcs(&f.body, &mut called);
        }
        if !called.iter().all(|c| defined.contains(c)) {
            if std::env::var_os("WIRDIAG").is_some() {
                let missing: Vec<&String> = called.iter().filter(|c| !defined.contains(*c)).collect();
                eprintln!("WIRBAIL called-undefined-func: {missing:?}");
            }
            return Ok(None);
        }
    }
    let bytes = witchy_wir::wir_encode::encode(&wir_module, &gc_structs);
    // Validate before committing; a malformed assembly returns `Ok(None)`.
    if let Err(e) = wasmparser::validate(&bytes) {
        if std::env::var_os("WIRDIAG").is_some() {
            eprintln!("WIRBAIL validate-failed: {e}");
        }
        return Ok(None);
    }
    Ok(Some(bytes))
}

/// Assemble the complete pre-optimization `WirModule` for a program — the static
/// prelude raw-body helpers + the lowered user functions + the `run` export +
/// imports/globals/data/table — or `Ok(None)` when any reachable function does
/// not fully lower to WIR or the program needs something outside the static
/// prelude. Split out from `compile_module_binary` so tests can compare the
/// optimized vs. unoptimized encoding (the slot-elimination differential).
pub fn assemble_wir_module(
    module: &Module,
) -> Result<Option<witchy_wir::wir::WirModule>, CodegenError> {
    Ok(assemble_wir_module_with_structs(module)?.map(|(module, _)| module))
}

fn assemble_wir_module_with_structs(
    module: &Module,
) -> Result<Option<(witchy_wir::wir::WirModule, Vec<witchy_wir::wir::WirStructDef>)>, CodegenError> {
    use witchy_wir::wir::{
        DataSegment, GlobalInit, Kind as WK, WirExpr, WirFunc, WirGlobal, WirImport, WirModule,
        WirNode, WirTable,
    };
    use witchy_wir::wir_prelude::WasmTy;
    // Front-end, identical to `compile_module_with`.
    let runtime_module = strip_compiler_syntax_items_for_runtime(module.clone());
    let recs = witchy_syntax::records::lower(runtime_module).map_err(|message| CodegenError { message })?;
    let eq_types = eq_impl_types(&recs);
    let mut lowered = witchy_types::traits::lower_for_wasm(recs);
    witchy_syntax::parser::lower_sugar_module(&mut lowered);
    alpha_rename_module(&mut lowered);
    let mut typed = witchy_types::typeck::annotate(lowered);
    // `e ? "msg"` desugar (`__try_ctx`) is type-directed: an `Option` operand lowers
    // via `option.ok_or`, a `Result` via `result.map_err`. Rewrite it here — after
    // annotation (so the operand's type is known) and before the string-`+` flip +
    // lowering (so the synthesized `map_err` lambda's `+` flips to `Concat` and its
    // nodes get typed). Re-annotate so the freshly minted calls/lambda are in the
    // type table.
    typed = typed.rewrite_and_reannotate_if(|table, module| {
        rewrite_try_ctx_module(module, table)
    });
    typed.rewrite_preserving_nodes(|table, module| flip_string_add_module(module, table));
    let module = typed.module();
    let loan_facts = witchy_types::loans::facts(module)
        .map_err(|error| CodegenError { message: error.to_string() })?;
    let mut cg = Codegen::new(typed.table(), loan_facts);
    cg.collect_wir = true;
    register_module_items(&mut cg, module);
    cg.eq_types = eq_types;
    cg.summaries = analysis::Summaries::of_module(module);

    // (RFC-0047) A custom-`PartialEq` type's `PartialEq__T__eq` may be called only
    // from a codegen-synthesized container eq helper (invisible to the AST walk), so
    // seed those impls as reachability roots — otherwise a `[CI] == [CI]` helper
    // calls an un-emitted function and the whole module bails to `Ok(None)`.
    let custom_eq_roots: Vec<String> = cg
        .custom_eq_types
        .iter()
        .map(|t| format!("PartialEq__{t}__eq"))
        .collect();
    let reachable = reachable_functions_with(module, &custom_eq_roots);
    // The exact `$name` functions this module emits — the discriminator
    // `lower_expr`'s call arm uses to tell a user call from an intrinsic/native.
    cg.emitted_funcs = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Function(f)
                if reachable.contains(&f.name) && !witchy_types::typeck::intrinsic(&f.name) =>
            {
                Some(f.name.clone())
            }
            _ => None,
        })
        .collect();
    let mut has_main = false;
    let mut main_params = 0usize;
    let mut main_param_is_args: Vec<bool> = Vec::new();
    let mut main_param_is_dir: Vec<bool> = Vec::new();
    let mut main_param_is_file: Vec<bool> = Vec::new();
    let mut main_param_is_net: Vec<bool> = Vec::new();
    let mut main_param_is_secret: Vec<bool> = Vec::new();
    // RFC-0038: `Some((type_name, nfields))` for a grantable-capability `main` param
    // (its record is minted at the root); `None` otherwise.
    let mut main_param_user_cap: Vec<Option<(String, usize)>> = Vec::new();
    // Grantable capability name -> field count, to detect + size a grantable param.
    let grantable_caps: HashMap<&str, usize> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => {
                Some((t.name.as_str(), t.variants.first().map(|v| v.fields.len()).unwrap_or(0)))
            }
            _ => None,
        })
        .collect();
    let mut main_returns_int = false;
    let mut main_returns_float = false;
    let mut user_order: Vec<String> = Vec::new();
    // The JS-callable string exports (`pub fn f(String) -> String`); each gets an
    // `__export_f` wrapper and is an extra reachability root (above).
    let string_exports = string_export_functions(module);
    // (RFC-0040) Cap-gated exports (`export_*(cap, String)`): (export name, cap type,
    // field count). Their `__export_*` wrapper mints the grantable cap host-side, so
    // register the record allocator arity now (while `cg` is mutable).
    let export_cap_info: Vec<(String, String, usize)> = string_exports
        .iter()
        .filter_map(|name| {
            let f = module.items.iter().find_map(|it| match it {
                Item::Function(fu) if &fu.name == name => Some(fu),
                _ => None,
            })?;
            export_cap_of(f, module).map(|(c, n)| (name.clone(), c.to_string(), n))
        })
        .collect();
    for (_, _, nfields) in &export_cap_info {
        cg.mk_arities.insert(*nfields);
    }
    for item in &module.items {
        if let Item::Function(f) = item {
            if f.name == "main" {
                has_main = true;
                main_params = f.params.len();
                main_returns_int = matches!(&f.ret, Some(Type::Named(n, _)) if n == "Int");
                main_returns_float = matches!(&f.ret, Some(Type::Named(n, _)) if n == "Float");
                for p in &f.params {
                    let is_args = matches!(&p.ty, Some(t) if witchy_types::typeck::is_args_type(t));
                    if is_args {
                        cg.uses_args = true;
                    }
                    main_param_is_args.push(is_args);
                    main_param_is_dir
                        .push(matches!(&p.ty, Some(Type::Named(n, _)) if n == "Dir"));
                    main_param_is_file
                        .push(matches!(&p.ty, Some(Type::Named(n, _)) if n == "File"));
                    main_param_is_net
                        .push(matches!(&p.ty, Some(Type::Named(n, _)) if n == "Net"));
                    main_param_is_secret
                        .push(matches!(&p.ty, Some(Type::Named(n, _)) if n == "Secret"));
                    let uc = match &p.ty {
                        Some(Type::Named(n, _)) => {
                            grantable_caps.get(n.as_str()).map(|nf| (n.clone(), *nf))
                        }
                        _ => None,
                    };
                    if let Some((_, nfields)) = &uc {
                        cg.mk_arities.insert(*nfields); // the record allocator for the sealed cap
                    }
                    main_param_user_cap.push(uc);
                }
            }
            if reachable.contains(&f.name) && !witchy_types::typeck::intrinsic(&f.name) {
                // Compiled for its side effects: stashes a `WirFunc` in
                // `cg.wir_funcs` iff the whole body lowered, and sets the
                // `uses_*` import-gating flags.
                cg.compile_function(f)?;
                user_order.push(f.name.clone());
            }
        }
    }
    // A module needs an entry: either a `main` (the `run` export) or at least one
    // string export (a `__export_*` host entry). A library with neither has nothing
    // to instantiate against.
    if !has_main && string_exports.is_empty() {
        if std::env::var_os("WIRDIAG").is_some() { eprintln!("WIRBAIL no-main"); }
        return Ok(None);
    }

    // Every reachable function must have fully lowered to WIR.
    if !user_order.iter().all(|n| cg.wir_funcs.contains_key(n)) {
        // Migration aid: `WIRDIAG=1` names the function(s) that didn't lower, so the
        // remaining WAT-fallback surface can be bisected. Inert otherwise.
        if std::env::var_os("WIRDIAG").is_some() {
            let missing: Vec<&String> =
                user_order.iter().filter(|n| !cg.wir_funcs.contains_key(*n)).collect();
            eprintln!("WIRBAIL user-fn-incomplete: {missing:?}");
        }
        return Ok(None);
    }
    // Bail if the program needs program-specific helpers (not in the prelude) or
    // closure types beyond the reserved band. An Int/Float `main` is fine now —
    // the prelude declares `print_int`/`print_float` and the `run` wrapper prints
    // the result.
    // Structural `==` / generated render are fine when every legacy eq/ts helper
    // has a WIR twin; a shape the WIR generator couldn't build leaves its key
    // without a twin → bail to WAT.
    let eq_all_wir = cg.eq_helpers.keys().all(|k| cg.eq_wir_helpers.contains_key(k));
    let ts_all_wir = cg.ts_helpers.keys().all(|k| cg.ts_wir_helpers.contains_key(k));
    // Lambdas/closures are fine now: each lifted body is in `lambda_wir_funcs` and
    // the closure types are synthesized by the encoder from the `CallIndirect`
    // nodes. A lambda the WIR couldn't lower already bailed its enclosing function
    // at the lower stage (so the user_order check below catches it).
    if !eq_all_wir || !ts_all_wir || !cg.rcopy_helpers.is_empty() {
        if std::env::var_os("WIRDIAG").is_some() {
            eprintln!("WIRBAIL eq_ts_rcopy: eq={eq_all_wir} ts={ts_all_wir} rcopy={}", cg.rcopy_helpers.len());
        }
        return Ok(None);
    }
    let prelude = witchy_wir::wir_prelude::prelude();

    let wasmty_kind = |t: WasmTy| -> WK {
        match t {
            WasmTy::I32 => WK::I32,
            WasmTy::I64 => WK::I64,
            WasmTy::F64 | WasmTy::F32 => WK::F64,
            // (RFC-0005) A migrated capability import (mint_dir, dir_*,
            // mint_file, file_*, mint_net, net_*, mint_secret, crypto/secretstore)
            // takes/returns an unforgeable `externref`.
            WasmTy::ExternRef => WK::ExternRef,
        }
    };

    // --- Capability-minimal WIR-helper path (#35) -------------------------------
    // If every prelude helper the program reaches has a WIR-native form (the
    // `wir_helper` registry), build a PRUNED module that declares only those
    // helpers and imports only their authority — instead of splicing the full
    // "all features on" raw-body prelude (which would over-import and break the
    // capability model). Falls through to the raw-body path otherwise.
    {
        let helper_names: HashSet<&str> =
            prelude.funcs.iter().map(|f| f.name.as_str()).collect();
        let mut called = HashSet::new();
        let mut user_host_imports = HashSet::new();
        for name in &user_order {
            if let Some(wf) = cg.wir_funcs.get(name) {
                collect_called_funcs(&wf.body, &mut called);
                collect_called_host_imports(&wf.body, &mut user_host_imports);
            }
        }
        let custom_key_eq = cg.dict_key_eq_wir_helper();
        // The generated structural-eq / render helpers (included below) call
        // prelude helpers themselves — a Str field eq via `$str_eq`, a renderer via
        // `$concat`/`$int_to_string`. Pull those (and nested eq_*/ts_* calls) into
        // the reached set so the resolution loop declares them.
        for f in cg.eq_wir_helpers.values() {
            collect_called_funcs(&f.body, &mut called);
        }
        if let Some(f) = &custom_key_eq {
            collect_called_funcs(&f.body, &mut called);
        }
        for f in cg.ts_wir_helpers.values() {
            collect_called_funcs(&f.body, &mut called);
        }
        // Generated rcopy helpers call `$ensure`, `$rcopy_str`, and each other.
        // Only when a region actually reclaimed (so the `$rcopy_*` globals are
        // declared); a helper generated for a region that then fell back to a plain
        // block is an orphan and must not enter the module.
        if cg.uses_region {
            for f in cg.rcopy_wir_helpers.values() {
                collect_called_funcs(&f.body, &mut called);
            }
        }
        // Lifted lambda bodies call `$mkN`/`$ensure`/prelude helpers and each
        // other; pull their reached helpers into the resolution set.
        for f in &cg.lambda_wir_funcs {
            collect_called_funcs(&f.body, &mut called);
        }
        // (RFC-0045) `__witchy_abort` is authority-free and always linked (like the
        // checked-heap `heap_register`/`heap_frontier`), so a direct `fail(msg)` in
        // user code calling it does NOT disqualify the capability-minimal pruned
        // path. Pull it out of the direct-host set before the gate, but remember to
        // declare the import below.
        let user_calls_abort = user_host_imports.remove("__witchy_abort");
        // A direct host call in user code (e.g. `now`, `dir.subdir`, `recv_*`)
        // needs authority the capability-minimal helper registry can't account
        // for — give up on such programs (`Ok(None)`). (Host access that goes
        // THROUGH a migrated helper is fine; its imports come from import_deps.)
        let no_direct_host =
            !called.iter().any(|n| n.starts_with("host:")) && user_host_imports.is_empty();
        if cg.uses_args {
            called.insert("build_args".to_string());
        }
        // RFC-0038: the `run` wrapper mints each grantable-cap param via
        // `mk{N}(build_user_cap_field(k, 0..N))`; those synthesized calls are in no
        // user body, so pull the helpers into the reached set explicitly.
        for (_, nfields) in main_param_user_cap.iter().flatten() {
            called.insert("build_user_cap_field".to_string());
            called.insert(format!("mk{nfields}"));
        }
        // (RFC-0040) cap-gated exports mint their grantable cap in the __export wrapper.
        for (_, _, nfields) in &export_cap_info {
            called.insert("build_user_cap_field".to_string());
            called.insert(format!("mk{nfields}"));
        }
        // The `__galloc` allocator the string-export wrappers expose delegates to
        // `$bump_alloc` (RFC-0051 I2 — the single ensure-prefixed allocator), so pull
        // it into the reached set (it brings `ensure` + the `$heap` global via its
        // registry deps). Harmless if a string-export body already reaches it.
        if !string_exports.is_empty() {
            called.insert("bump_alloc".to_string());
        }
        // Resolve every reached helper through the registry (transitively).
        let mut resolved: std::collections::BTreeMap<String, witchy_wir::wir_helpers::WirHelperSpec> =
            std::collections::BTreeMap::new();
        let mut all_registered = true;
        // A called name is a prelude helper to pull in if the static prelude
        // declares it OR the WIR registry resolves it — the latter covers helpers
        // migrated to WIR that have no static-prelude body (e.g. crypto_sha512).
        let mut queue: Vec<String> = called
            .iter()
            .filter(|n| helper_names.contains(n.as_str()) || witchy_wir::wir_helpers::wir_helper(n).is_some())
            .cloned()
            .collect();
        while let Some(h) = queue.pop() {
            if resolved.contains_key(&h) {
                continue;
            }
            match witchy_wir::wir_helpers::wir_helper(&h) {
                Some(spec) => {
                    for d in spec.helper_deps {
                        queue.push((*d).to_string());
                    }
                    resolved.insert(h, spec);
                }
                None => {
                    all_registered = false;
                    if std::env::var_os("WIRDIAG").is_some() { eprintln!("WIRBAIL unregistered-helper: {h}"); }
                    break;
                }
            }
        }
        if std::env::var_os("WIRDIAG").is_some() && !(no_direct_host && all_registered) {
            let hosts: Vec<&String> = called.iter().filter(|n| n.starts_with("host:")).collect();
            eprintln!("WIRBAIL prune-fail: no_direct_host={no_direct_host} all_registered={all_registered} user_host={user_host_imports:?} hosts={hosts:?}");
        }
        if no_direct_host && all_registered {
            // Host-backed helpers receive the caller's packed source site as a
            // final i64 argument. They publish it only immediately before the
            // host edge, so successful nested calls cannot stale the location.
            for (name, spec) in &mut resolved {
                prepare_diagnostic_helper(name, &mut spec.func);
            }
            let mut import_names: std::collections::BTreeSet<&str> =
                std::collections::BTreeSet::new();
            let mut uses_heap = false;
            let mut uses_table = false;
            for spec in resolved.values() {
                for i in spec.import_deps {
                    import_names.insert(i);
                }
                uses_heap |= spec.uses_heap;
                uses_table |= spec.uses_table;
            }
            // A watermarked loop in user code reads/writes `$heap` even when no
            // reached helper allocates, so the global must still be declared.
            uses_heap |= cg.uses_wm;
            // An Int/Float-returning `main` prints its result in the `run`
            // wrapper, so the corresponding host import must be declared.
            if main_returns_int {
                import_names.insert("print_int");
            } else if main_returns_float {
                import_names.insert("print_float");
            }
            if main_param_is_file.iter().any(|is_file| *is_file) {
                import_names.insert("mint_file");
            }
            if main_param_is_dir.iter().any(|is_dir| *is_dir) {
                import_names.insert("mint_dir");
            }
            if main_param_is_net.iter().any(|is_net| *is_net) {
                import_names.insert("mint_net");
            }
            if main_param_is_secret.iter().any(|is_secret| *is_secret) {
                import_names.insert("mint_secret");
            }
            // (RFC-0045) A user `fail(msg)` calls `__witchy_abort` directly (its
            // import_deps aren't consulted because it's not a registry helper), so
            // declare the import when user code reaches it.
            if user_calls_abort {
                import_names.insert("__witchy_abort");
            }
            let pruned_imports: Vec<WirImport> = import_names
                .iter()
                .map(|iname| {
                    let pi = prelude
                        .imports
                        .iter()
                        .find(|p| p.name.as_str() == *iname)
                        .expect("a helper's import_dep must be a prelude import");
                    WirImport {
                        name: pi.name.clone(),
                        params: pi.params.iter().copied().map(wasmty_kind).collect(),
                        results: pi.results.iter().copied().map(wasmty_kind).collect(),
                    }
                })
                .collect();
            if custom_key_eq.is_some() {
                resolved.remove("key_eq");
            }
            let mut pruned_funcs: Vec<WirFunc> = resolved.into_values().map(|s| s.func).collect();
            if let Some(f) = custom_key_eq {
                pruned_funcs.push(f);
            }
            // The program-specific structural-equality / render helpers reached by
            // user `==` / generated interpolation render.
            for f in cg.eq_wir_helpers.values() {
                pruned_funcs.push(f.clone());
            }
            for f in cg.ts_wir_helpers.values() {
                pruned_funcs.push(f.clone());
            }
            // Generated per-shape region copy-out helpers reached by a pointer
            // `region:` reclaim. Gated on `uses_region` so a helper generated for a
            // region that then fell back to a plain block stays out of the module
            // (it references `$rcopy_*` globals only declared when `uses_region`).
            if cg.uses_region {
                for f in cg.rcopy_wir_helpers.values() {
                    pruned_funcs.push(f.clone());
                }
            }
            // Lifted lambda bodies, in table-index order (so `$__lamw{i}` lands at
            // table slot i, matching the code index baked into each closure object).
            for f in &cg.lambda_wir_funcs {
                pruned_funcs.push(f.clone());
            }
            for name in &user_order {
                pruned_funcs.push(cg.wir_funcs.get(name).expect("lowered above").clone());
            }
            // Each `Dir` param is minted from a distinct root grant in declaration
            // order as an unforgeable externref (RFC-0005 Stage 3).
            // Each `File` param is minted from the corresponding direct `--file`
            // grant as an unforgeable externref (RFC-0005 Stage 2). The root
            // `Secret` is minted from the host's signing-key grant as an opaque
            // externref; there is no guest-visible integer handle.
            let mut dir_grant_ord = 0i32;
            let mut file_grant_ord = 0i32;
            let mut net_grant_ord = 0i32;
            let mut user_cap_ord = 0i32;
            let mut main_args: Vec<WirExpr> = Vec::with_capacity(main_params);
            for i in 0..main_params {
                if main_param_is_args.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::Call { func: "build_args".into(), args: vec![] });
                } else if main_param_is_dir.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::CallHost {
                        import: "mint_dir".into(),
                        args: vec![WirExpr::ConstI32(dir_grant_ord)],
                    });
                    dir_grant_ord += 1;
                } else if main_param_is_file.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::CallHost {
                        import: "mint_file".into(),
                        args: vec![WirExpr::ConstI32(file_grant_ord)],
                    });
                    file_grant_ord += 1;
                } else if main_param_is_net.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::CallHost {
                        import: "mint_net".into(),
                        args: vec![WirExpr::ConstI32(net_grant_ord)],
                    });
                    net_grant_ord += 1;
                } else if main_param_is_secret.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::CallHost {
                        import: "mint_secret".into(),
                        args: vec![WirExpr::ConstI32(0)],
                    });
                } else if let Some((tn, nfields)) = main_param_user_cap.get(i).cloned().flatten() {
                    // RFC-0038: mint the sealed record from the grant —
                    // `mk{N}(tag, build_user_cap_field(k, 0), …, build_user_cap_field(k, N-1))`.
                    // `k` is the grantable param's ordinal (indexing the host's
                    // `user_cap_fields`); the tag is the ctor's variant discriminant;
                    // each field is a separately-alloc'd String widened to the i64 slot.
                    let k = user_cap_ord;
                    user_cap_ord += 1;
                    let tag = cg.ctors.get(&tn).map(|c| c.0).unwrap_or(0) as i32;
                    let mut mk_args: Vec<WirExpr> = Vec::with_capacity(nfields + 1);
                    mk_args.push(WirExpr::ConstI32(tag));
                    for fi in 0..nfields as i32 {
                        mk_args.push(WirExpr::Convert {
                            from: witchy_wir::wir::Kind::I32,
                            to: witchy_wir::wir::Kind::I64,
                            arg: Box::new(WirExpr::Call {
                                func: "build_user_cap_field".into(),
                                args: vec![WirExpr::ConstI32(k), WirExpr::ConstI32(fi)],
                            }),
                        });
                    }
                    main_args.push(WirExpr::Call { func: format!("mk{nfields}"), args: mk_args });
                } else {
                    main_args.push(WirExpr::ConstI32(0));
                }
            }
            // The `run` export calls `main`; an Int/Float result is printed (the
            // exit-code convention), anything else is dropped — matching the WAT
            // sink's `run` tail. Only synthesized when the module has a `main`; a
            // pure string-export library (no `main`) exports only `__galloc` + the
            // `__export_*` wrappers.
            if has_main {
                let main_call = WirExpr::Call { func: "main".into(), args: main_args };
                let run_body = if main_returns_int {
                    vec![WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![main_call] })]
                } else if main_returns_float {
                    vec![WirNode::Do(WirExpr::CallHost { import: "print_float".into(), args: vec![main_call] })]
                } else {
                    vec![WirNode::Drop(main_call)]
                };
                pruned_funcs.push(WirFunc {
                    name: "run".into(),
                    params: Vec::new(),
                    ret: Vec::new(),
                    locals: Vec::new(),
                    body: run_body,
                    raw_body: None,
                });
            }
            // String-export glue (RFC-0007 §"Data marshaling" / RFC-0008 run loop):
            // a JS host writes a witchy `String` header `[i32 len][bytes]` into guest
            // memory at a `__galloc`-returned pointer, then calls `__export_f(ptr,
            // len)`; the wrapper passes the pointer straight to the witchy fn (whose
            // single `String` param IS that header) and returns the result String
            // pointer. No import, no authority — only guest-memory reads/writes.
            if !string_exports.is_empty() {
                // __galloc(len) -> ptr — (RFC-0051 I2) the shared WIR `$__galloc`
                // (which delegates to `$bump_alloc`, the single ensure-prefixed
                // allocator) rather than an inline ensure+bump twin.
                pruned_funcs.push(witchy_wir::wir_helpers::galloc_helper());
                // One `__export_f(in_ptr, in_len) -> out_ptr` per string export. The
                // `in_len` param is accepted for ABI symmetry (and a future bounds
                // check) but the String header is self-describing, so the wrapper
                // forwards `in_ptr` to the witchy fn directly.
                for name in &string_exports {
                    // (RFC-0040) A cap-gated export mints its grantable cap host-side
                    // (`mk{N}(tag, i64(build_user_cap_field(0, i))…)`, mirroring the `run`
                    // wrapper for `main`), prepended before the input String pointer.
                    let mut call_args: Vec<WirExpr> = Vec::new();
                    if let Some((_, cap_ty, nfields)) = export_cap_info.iter().find(|(n, _, _)| n == name) {
                        let tag = cg.ctors.get(cap_ty).map(|c| c.0).unwrap_or(0) as i32;
                        let mut mk_args: Vec<WirExpr> = Vec::with_capacity(nfields + 1);
                        mk_args.push(WirExpr::ConstI32(tag));
                        for fi in 0..*nfields as i32 {
                            mk_args.push(WirExpr::Convert {
                                from: witchy_wir::wir::Kind::I32,
                                to: witchy_wir::wir::Kind::I64,
                                arg: Box::new(WirExpr::Call {
                                    func: "build_user_cap_field".into(),
                                    args: vec![WirExpr::ConstI32(0), WirExpr::ConstI32(fi)],
                                }),
                            });
                        }
                        call_args.push(WirExpr::Call { func: format!("mk{nfields}"), args: mk_args });
                    }
                    call_args.push(WirExpr::GetLocal("in_ptr".into()));
                    pruned_funcs.push(WirFunc {
                        name: string_export_name(name),
                        params: vec![
                            witchy_wir::wir::WirLocal { name: "in_ptr".into(), ty: witchy_wir::wir::WirTy::Bool },
                            witchy_wir::wir::WirLocal { name: "in_len".into(), ty: witchy_wir::wir::WirTy::Bool },
                        ],
                        ret: vec![witchy_wir::wir::WirTy::Bool], // i32 result String pointer
                        locals: vec![],
                        body: vec![WirNode::Push(WirExpr::Call {
                            func: name.clone(),
                            args: call_args,
                        })],
                        raw_body: None,
                    });
                }
            }
            // Static host-backed helpers already carry a source-site parameter,
            // and source/lambda bodies were instrumented statement by statement.
            // The remaining functions are compiler-synthesized after those
            // passes: run/export wrappers and per-shape render/equality helpers.
            // They have no lexical statement of their own, but must still satisfy
            // the augmented helper signatures. Thread the explicit "unknown"
            // site (0) through any host-backed path they contain. Existing precise
            // sites are idempotently preserved.
            for func in &mut pruned_funcs {
                if prepare_synthetic_diagnostic_sites(func) {
                    cg.uses_diagnostic_sites = true;
                }
            }
            let mut pruned_globals = if uses_heap {
                vec![
                    WirGlobal {
                        name: "heap".into(),
                        kind: WK::I32,
                        mutable: true,
                        init: GlobalInit::I32(cg.next_offset as i32),
                        // Exported so a long-lived host (the glamour MVU run loop, which calls a
                        // `String -> String` export once per event) can RESET the bump allocator to
                        // its base after each call. Every `export_*` call is pure — its input,
                        // working, and output allocations are all dead once the host has read the
                        // result String out — so without a reset the never-freeing bump allocator
                        // leaks one call's allocations forever and eventually exhausts memory
                        // (`__galloc` returns an out-of-bounds pointer). The host reads the global's
                        // initial value as the base and restores it; see witchy-runtime.mjs.
                        export: Some("__heap".into()),
                    },
                    // (RFC-0035) The immutable heap base = the initial `$heap` value (the
                    // first byte past the static data segment). Every `$rc_alloc` object
                    // lives at an address >= this; scalars, nullary/immediate values,
                    // capability handles and static-data pointers all sit BELOW it. The
                    // gated `$rc_dup`/`$rc_drop` guard on `ptr >= heap_base`, so emitting
                    // them for any `i32`-kinded value is a sound over-approximation — only
                    // a real refcounted heap object (which alone has the `[rc]` header at
                    // `ptr-8`) is ever touched.
                    WirGlobal {
                        name: "heap_base".into(),
                        kind: WK::I32,
                        mutable: false,
                        init: GlobalInit::I32(cg.next_offset as i32),
                        export: None,
                    },
                    WirGlobal {
                        name: "__witchy_reowns".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_reowns".into()),
                    },
                    // (RFC-0016) Head of the RC-floor size-classed free-list (0 = empty).
                    // `$rc_alloc` pops it, `$rc_free` pushes; declared with `heap` since
                    // they share the allocation path. Empty (no effect) unless the
                    // codegen free-at-overwrite (gated `rc-floor`) emits `$rc_free`.
                    WirGlobal {
                        name: "rc_freelist".into(),
                        kind: WK::I32,
                        mutable: true,
                        init: GlobalInit::I32(0),
                        export: None,
                    },
                    // (RFC-0016) DoD counter: bytes handed back out of the free-list by
                    // `$rc_alloc` (reused rather than freshly bumped). 0 unless the
                    // free-at-overwrite codegen (gated `rc-floor`) populated the list, so
                    // `witchy stats` proves the optimization actually fired and recycled.
                    WirGlobal {
                        name: "__rc_reused_bytes".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__rc_reused_bytes".into()),
                    },
                    // (RFC-0035) Live-cell counter: `$rc_alloc` +1 (each call yields one live
                    // object), `$rc_free` -1 (each freed object). At exit it is the number of
                    // rc_alloc objects NOT returned to the free-list — a leak metric. For a
                    // fully-reclaiming rc-floor program it stays bounded (→ the reachable roots);
                    // an unbounded leak makes it grow with the input. 0 unless a `$rc_free` fires.
                    WirGlobal {
                        name: "__witchy_live_cells".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_live_cells".into()),
                    },
                    // (RFC-0089) Monotonic operation counts let FIP tests prove
                    // that recursive depth adds no heap work.
                    WirGlobal {
                        name: "__witchy_rc_alloc_calls".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_rc_alloc_calls".into()),
                    },
                    WirGlobal {
                        name: "__witchy_bump_alloc_calls".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_bump_alloc_calls".into()),
                    },
                    WirGlobal {
                        name: "__witchy_rc_reuse_calls".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_rc_reuse_calls".into()),
                    },
                    WirGlobal {
                        name: "__witchy_rc_free_calls".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_rc_free_calls".into()),
                    },
                    WirGlobal {
                        name: "__witchy_region_rewind_calls".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_region_rewind_calls".into()),
                    },
                    WirGlobal {
                        name: "__witchy_extract_active".into(),
                        kind: WK::I32,
                        mutable: true,
                        init: GlobalInit::I32(0),
                        export: None,
                    },
                    WirGlobal {
                        name: "__witchy_extract_searches".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_extract_searches".into()),
                    },
                    WirGlobal {
                        name: "__witchy_extract_key_comparisons".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_extract_key_comparisons".into()),
                    },
                    WirGlobal {
                        name: "__witchy_extract_copied_bytes".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_extract_copied_bytes".into()),
                    },
                    WirGlobal {
                        name: "__witchy_extract_retains".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_extract_retains".into()),
                    },
                    WirGlobal {
                        name: "__witchy_extract_drops".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_extract_drops".into()),
                    },
                ]
            } else {
                Vec::new()
            };
            if cg.uses_diagnostic_sites {
                pruned_globals.push(WirGlobal {
                    name: "__witchy_diagnostic_site".into(),
                    kind: WK::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__witchy_diagnostic_site".into()),
                });
            }
            // Region copy-out scratch globals: the watermark / temp base / slide delta
            // the `$rcopy_*` helpers read, and the exported `$__region_copy_bytes`
            // counter. Declared only when a pointer `region:` reclaim is reached.
            if cg.uses_region {
                for (name, ex) in
                    [("rcopy_wm", false), ("rcopy_base", false), ("rcopy_delta", false)]
                {
                    pruned_globals.push(WirGlobal {
                        name: name.into(),
                        kind: WK::I32,
                        mutable: true,
                        init: GlobalInit::I32(0),
                        export: if ex { Some(name.into()) } else { None },
                    });
                }
                pruned_globals.push(WirGlobal {
                    name: "__region_copy_bytes".into(),
                    kind: WK::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__region_copy_bytes".into()),
                });
            }
            // (RFC-0032) When `vm.par_map` (scalar or String) is linked, emit + export
            // the `__call_idx` trampoline the host (incl. fresh worker VMs) re-enters to
            // apply the mapped closure to each element by its table index. The String
            // variant also needs `__galloc` so the host can place input strings into a
            // worker's memory — emit it unless string-export wrappers already do.
            // The String/Bytes variants copy buffers in via `__galloc`; all variants
            // invoke the closure by index via `__call_idx`.
            let has_par_map_buf = pruned_funcs.iter().any(|f| f.name == "vm_par_map_bytes");
            // (RFC-0032) `vm.with_dir` and `vm.serve` invoke a 2-arg closure via `__call2`,
            // and copy buffers into a worker via `__galloc`.
            let has_call2 = pruned_funcs
                .iter()
                .any(|f| f.name == "vm_with_dir" || f.name == "vm_serve");
            let has_with_dir = has_call2;
            let exports_call_idx =
                has_par_map_buf || pruned_funcs.iter().any(|f| f.name == "vm_par_map");
            if exports_call_idx {
                pruned_funcs.push(witchy_wir::wir_helpers::call_idx_helper());
            }
            if has_with_dir {
                pruned_funcs.push(witchy_wir::wir_helpers::call2_helper());
            }
            // The String/Bytes par_map variants and `vm.with_dir` all copy a buffer into
            // a worker via `__galloc`.
            let needs_galloc = has_par_map_buf || has_with_dir;
            if needs_galloc && string_exports.is_empty() {
                pruned_funcs.push(witchy_wir::wir_helpers::galloc_helper());
            }
            let data: Vec<DataSegment> = cg
                .strings
                .iter()
                .map(|(text, off)| {
                    let mut bytes = (text.len() as u32).to_le_bytes().to_vec();
                    bytes.extend_from_slice(text.as_bytes());
                    DataSegment { offset: *off, bytes }
                })
                .collect();
            let gc_structs = cg.gc_structs.clone();
            return Ok(Some((WirModule {
                imports: pruned_imports,
                funcs: pruned_funcs,
                memory_pages: 1,
                data,
                globals: pruned_globals,
                table: if cg.lambda_wir_funcs.is_empty() {
                    if uses_table { Some(WirTable { funcs: Vec::new() }) } else { None }
                } else {
                    // Slot i = `$__lamw{i}`, so a closure object's code index
                    // resolves to its lifted body through the element segment.
                    Some(WirTable { funcs: cg.lambda_wir_funcs.iter().map(|f| f.name.clone()).collect() })
                },
                exports: {
                    let mut exports: Vec<(String, String)> = Vec::new();
                    if has_main {
                        exports.push(("run".into(), "run".into()));
                    }
                    if exports_call_idx {
                        exports.push(("__call_idx".into(), "__call_idx".into()));
                    }
                    if has_with_dir {
                        exports.push(("__call2".into(), "__call2".into()));
                    }
                    if !string_exports.is_empty() || needs_galloc {
                        exports.push(("__galloc".into(), "__galloc".into()));
                    }
                    if !string_exports.is_empty() {
                        for name in &string_exports {
                            let ex = string_export_name(name);
                            exports.push((ex.clone(), ex));
                        }
                    }
                    exports
                },
            }, gc_structs)));
        }
    }

    // Otherwise the program reaches a prelude helper not yet migrated to a
    // WIR-native form (or directly calls a host import), so no capability-correct
    // binary can be built yet → return `Ok(None)`. The old raw-body
    // "all features on" splice path is RETIRED: it over-imported the full host
    // surface (incl. authority like crypto.sign/dir/net), which a minimal program
    // cannot instantiate under its real grant — the opposite of witchy's
    // capability model. Coverage grows by migrating helpers into `wir_helper`.
    Ok(None)
}

/// Collect every function name a `WirSeq` calls directly (`Call{func}`),
/// recursively. Used by `assemble_wir_module` to find which prelude helpers a
/// program reaches.
fn collect_called_funcs(seq: &[witchy_wir::wir::WirNode], out: &mut HashSet<String>) {
    use witchy_wir::wir::{WirExpr as E, WirNode as N};
    fn expr(e: &E, out: &mut HashSet<String>) {
        match e {
            E::Call { func, args } => {
                out.insert(func.clone());
                for a in args {
                    expr(a, out);
                }
            }
            E::CallHost { args, .. } => {
                for a in args {
                    expr(a, out);
                }
            }
            E::CallIndirect { args, index, .. } => {
                for a in args {
                    expr(a, out);
                }
                expr(index, out);
            }
            E::ToSlot(i, _)
            | E::FromSlot(i, _)
            | E::Unary { arg: i, .. }
            | E::Convert { arg: i, .. }
            | E::Load { ptr: i, .. }
            | E::Load8U { ptr: i, .. }
            | E::MemoryGrow(i) => expr(i, out),
            E::Binary { lhs, rhs, .. } => {
                expr(lhs, out);
                expr(rhs, out);
            }
            E::Control(n) => node(n, out),
            E::Seq(s) => collect_called_funcs(s, out),
            E::StructNew { args, .. } => {
                for a in args {
                    expr(a, out);
                }
            }
            E::StructGet { base, .. }
            | E::RefCast { value: base, .. }
            | E::RefIsNull(base) => expr(base, out),
            E::ConstI64(_) | E::ConstF64(_) | E::ConstI32(_) | E::StrPtr(_) | E::MemorySize
            | E::GetLocal(_) | E::GetGlobal(_) | E::RefNull(_) => {}
        }
    }
    fn node(n: &N, out: &mut HashSet<String>) {
        match n {
            N::SetLocal { value, .. } | N::SetGlobal { value, .. } => expr(value, out),
            N::Store { ptr, value, .. } | N::Store8 { ptr, value, .. } => {
                expr(ptr, out);
                expr(value, out);
            }
            N::CallStoreMulti { func, args, .. } => {
                out.insert(func.clone());
                for a in args {
                    expr(a, out);
                }
            }
            N::CallIndirectStoreMulti { args, index, .. } => {
                for a in args {
                    expr(a, out);
                }
                expr(index, out);
            }
            N::MemoryCopy { dest, src, len } => {
                expr(dest, out);
                expr(src, out);
                expr(len, out);
            }
            N::MemoryFill { dest, value, len } => {
                expr(dest, out);
                expr(value, out);
                expr(len, out);
            }
            N::If { cond, then_, els, .. } => {
                expr(cond, out);
                collect_called_funcs(then_, out);
                collect_called_funcs(els, out);
            }
            N::Block { body, .. } | N::Loop { body, .. } => collect_called_funcs(body, out),
            N::StructSet { base, value, .. } => {
                expr(base, out);
                expr(value, out);
            }
            N::Br { cond: Some(c), .. } => expr(c, out),
            N::Drop(e) | N::Do(e) | N::Push(e) | N::Return(Some(e)) => expr(e, out),
            N::Br { cond: None, .. } | N::Return(None) | N::Unreachable => {}
        }
    }
    for n in seq {
        node(n, out);
    }
}

/// Collect every host import a `WirSeq` calls directly (`CallHost{import}`),
/// recursively. Used by `assemble_wir_module` to detect direct host-authority
/// calls in USER code (e.g. `dir.subdir`, `now`, `recv_*`) — which the pruned
/// path can't account for, so such programs return `Ok(None)`. (Helper
/// host calls are accounted for via the registry's `import_deps` instead.)
fn collect_called_host_imports(seq: &[witchy_wir::wir::WirNode], out: &mut HashSet<String>) {
    use witchy_wir::wir::{WirExpr as E, WirNode as N};
    fn expr(e: &E, out: &mut HashSet<String>) {
        match e {
            E::CallHost { import, args } => {
                out.insert(import.clone());
                for a in args {
                    expr(a, out);
                }
            }
            E::Call { args, .. } => {
                for a in args {
                    expr(a, out);
                }
            }
            E::CallIndirect { args, index, .. } => {
                for a in args {
                    expr(a, out);
                }
                expr(index, out);
            }
            E::ToSlot(i, _)
            | E::FromSlot(i, _)
            | E::Unary { arg: i, .. }
            | E::Convert { arg: i, .. }
            | E::Load { ptr: i, .. }
            | E::Load8U { ptr: i, .. }
            | E::MemoryGrow(i) => expr(i, out),
            E::Binary { lhs, rhs, .. } => {
                expr(lhs, out);
                expr(rhs, out);
            }
            E::Control(n) => node(n, out),
            E::Seq(s) => collect_called_host_imports(s, out),
            E::StructNew { args, .. } => {
                for a in args {
                    expr(a, out);
                }
            }
            E::StructGet { base, .. }
            | E::RefCast { value: base, .. }
            | E::RefIsNull(base) => expr(base, out),
            E::ConstI64(_) | E::ConstF64(_) | E::ConstI32(_) | E::StrPtr(_) | E::MemorySize
            | E::GetLocal(_) | E::GetGlobal(_) | E::RefNull(_) => {}
        }
    }
    fn node(n: &N, out: &mut HashSet<String>) {
        match n {
            N::SetLocal { value, .. } | N::SetGlobal { value, .. } => expr(value, out),
            N::Store { ptr, value, .. } | N::Store8 { ptr, value, .. } => {
                expr(ptr, out);
                expr(value, out);
            }
            N::CallStoreMulti { args, .. } => {
                for a in args {
                    expr(a, out);
                }
            }
            N::CallIndirectStoreMulti { args, index, .. } => {
                for a in args {
                    expr(a, out);
                }
                expr(index, out);
            }
            N::MemoryCopy { dest, src, len } => {
                expr(dest, out);
                expr(src, out);
                expr(len, out);
            }
            N::MemoryFill { dest, value, len } => {
                expr(dest, out);
                expr(value, out);
                expr(len, out);
            }
            N::If { cond, then_, els, .. } => {
                expr(cond, out);
                collect_called_host_imports(then_, out);
                collect_called_host_imports(els, out);
            }
            N::Block { body, .. } | N::Loop { body, .. } => collect_called_host_imports(body, out),
            N::StructSet { base, value, .. } => {
                expr(base, out);
                expr(value, out);
            }
            N::Br { cond: Some(c), .. } => expr(c, out),
            N::Drop(e) | N::Do(e) | N::Push(e) | N::Return(Some(e)) => expr(e, out),
            N::Br { cond: None, .. } | N::Return(None) | N::Unreachable => {}
        }
    }
    for n in seq {
        node(n, out);
    }
}

/// Whether this lowered source statement can reach a host import. Helper calls
/// follow the WIR registry's declared dependency graph, the same source of truth
/// module assembly uses to link imports.
pub(super) fn wir_seq_needs_diagnostic_site(seq: &[witchy_wir::wir::WirNode]) -> bool {
    let mut imports = HashSet::new();
    collect_called_host_imports(seq, &mut imports);
    if !imports.is_empty() {
        return true;
    }

    let mut calls = HashSet::new();
    collect_called_funcs(seq, &mut calls);
    let mut seen = HashSet::new();
    calls.iter().any(|name| helper_needs_diagnostic_site(name, &mut seen))
}

const DIAGNOSTIC_SITE_PARAM: &str = "__witchy_diagnostic_site_arg";

fn helper_needs_diagnostic_site(name: &str, seen: &mut HashSet<String>) -> bool {
    if !seen.insert(name.to_string()) {
        return false;
    }
    let Some(spec) = witchy_wir::wir_helpers::wir_helper(name) else {
        return false;
    };
    !spec.import_deps.is_empty()
        || spec.helper_deps.iter().any(|dep| helper_needs_diagnostic_site(dep, seen))
}

fn registered_helper_needs_diagnostic_site(name: &str) -> bool {
    helper_needs_diagnostic_site(name, &mut HashSet::new())
}

fn append_helper_diagnostic_site(
    func: &str,
    args: &mut Vec<witchy_wir::wir::WirExpr>,
    site: &witchy_wir::wir::WirExpr,
) -> bool {
    if !registered_helper_needs_diagnostic_site(func) {
        return false;
    }
    let original_arity = witchy_wir::wir_helpers::wir_helper(func)
        .map(|spec| spec.func.params.len())
        .expect("a registered diagnostic helper must resolve");
    match args.len() {
        n if n == original_arity => args.push(site.clone()),
        n if n == original_arity + 1 => {}
        n => panic!("diagnostic helper `{func}` has {n} arguments; expected {original_arity}"),
    }
    true
}

/// Thread one packed source site into every host-backed helper call and publish
/// it only at the actual host edge. Passing the site as a normal argument is
/// compositional: nested calls evaluate first and cannot leave stale location
/// state for an outer operation.
pub(super) fn attach_diagnostic_sites(seq: &mut witchy_wir::wir::WirSeq, site: i64) -> bool {
    attach_diagnostic_site_expr(seq, &witchy_wir::wir::WirExpr::ConstI64(site))
}

fn attach_diagnostic_site_expr(
    seq: &mut witchy_wir::wir::WirSeq,
    site: &witchy_wir::wir::WirExpr,
) -> bool {
    use witchy_wir::wir::{WirExpr as E, WirNode as N};

    fn expr(e: &mut E, site: &E) -> bool {
        let mut reaches_host = false;
        match e {
            E::Call { func, args } => {
                for arg in args.iter_mut() {
                    reaches_host |= expr(arg, site);
                }
                reaches_host |= append_helper_diagnostic_site(func, args, site);
            }
            E::CallHost { import: _, args } => {
                for arg in args {
                    let _ = expr(arg, site);
                }
                reaches_host = true;
            }
            E::CallIndirect { args, index, .. } => {
                for arg in args {
                    reaches_host |= expr(arg, site);
                }
                reaches_host |= expr(index, site);
            }
            E::ToSlot(inner, _)
            | E::FromSlot(inner, _)
            | E::Unary { arg: inner, .. }
            | E::Convert { arg: inner, .. }
            | E::Load { ptr: inner, .. }
            | E::Load8U { ptr: inner, .. }
            | E::MemoryGrow(inner) => reaches_host |= expr(inner, site),
            E::Binary { lhs, rhs, .. } => {
                reaches_host |= expr(lhs, site);
                reaches_host |= expr(rhs, site);
            }
            E::Control(node) => reaches_host |= node_expr(node, site),
            E::Seq(inner) => reaches_host |= attach_diagnostic_site_expr(inner, site),
            E::StructNew { args, .. } => {
                for arg in args {
                    reaches_host |= expr(arg, site);
                }
            }
            E::StructGet { base, .. }
            | E::RefCast { value: base, .. }
            | E::RefIsNull(base) => reaches_host |= expr(base, site),
            E::ConstI64(_)
            | E::ConstF64(_)
            | E::ConstI32(_)
            | E::StrPtr(_)
            | E::GetLocal(_)
            | E::GetGlobal(_)
            | E::MemorySize
            | E::RefNull(_) => {}
        }
        reaches_host
    }

    fn node_expr(node: &mut N, site: &E) -> bool {
        let mut reaches_host = false;
        match node {
            N::SetLocal { value, .. } | N::SetGlobal { value, .. } => {
                reaches_host |= expr(value, site);
            }
            N::Store { ptr, value, .. } | N::Store8 { ptr, value, .. } => {
                reaches_host |= expr(ptr, site);
                reaches_host |= expr(value, site);
            }
            N::CallStoreMulti { func, args, .. } => {
                for arg in args.iter_mut() {
                    reaches_host |= expr(arg, site);
                }
                reaches_host |= append_helper_diagnostic_site(func, args, site);
            }
            N::CallIndirectStoreMulti { args, index, .. } => {
                for arg in args.iter_mut() {
                    reaches_host |= expr(arg, site);
                }
                reaches_host |= expr(index, site);
            }
            N::MemoryCopy { dest, src, len } => {
                reaches_host |= expr(dest, site);
                reaches_host |= expr(src, site);
                reaches_host |= expr(len, site);
            }
            N::MemoryFill { dest, value, len } => {
                reaches_host |= expr(dest, site);
                reaches_host |= expr(value, site);
                reaches_host |= expr(len, site);
            }
            N::If { cond, then_, els, .. } => {
                reaches_host |= expr(cond, site);
                reaches_host |= attach_diagnostic_site_expr(then_, site);
                reaches_host |= attach_diagnostic_site_expr(els, site);
            }
            N::Block { body, .. } | N::Loop { body, .. } => {
                reaches_host |= attach_diagnostic_site_expr(body, site);
            }
            N::Br { cond: Some(cond), .. } => reaches_host |= expr(cond, site),
            N::Drop(value) | N::Do(value) | N::Push(value) | N::Return(Some(value)) => {
                reaches_host |= expr(value, site);
            }
            N::StructSet { base, value, .. } => {
                reaches_host |= expr(base, site);
                reaches_host |= expr(value, site);
            }
            N::Br { cond: None, .. } | N::Return(None) | N::Unreachable => {}
        }
        reaches_host
    }

    let mut out = Vec::with_capacity(seq.len());
    let mut reaches_host = false;
    for mut node in std::mem::take(seq) {
        let is_host_edge = matches!(
            &node,
            N::SetLocal { value: E::CallHost { .. }, .. }
                | N::Push(E::CallHost { .. })
                | N::Do(E::CallHost { .. })
                | N::Return(Some(E::CallHost { .. }))
        );
        reaches_host |= node_expr(&mut node, site);
        let already_published = matches!(
            out.last(),
            Some(N::SetGlobal { global, .. }) if global == "__witchy_diagnostic_site"
        );
        if is_host_edge && !already_published {
            out.push(N::SetGlobal {
                global: "__witchy_diagnostic_site".into(),
                value: site.clone(),
            });
        }
        out.push(node);
    }
    *seq = out;
    reaches_host
}

fn prepare_diagnostic_helper(name: &str, func: &mut witchy_wir::wir::WirFunc) {
    use witchy_wir::wir::{WirExpr as E, WirLocal, WirTy};
    if !registered_helper_needs_diagnostic_site(name) {
        return;
    }
    if !func.params.iter().any(|param| param.name == DIAGNOSTIC_SITE_PARAM) {
        func.params.push(WirLocal { name: DIAGNOSTIC_SITE_PARAM.into(), ty: WirTy::Int });
    }
    let reached = attach_diagnostic_site_expr(
        &mut func.body,
        &E::GetLocal(DIAGNOSTIC_SITE_PARAM.into()),
    );
    debug_assert!(reached, "host-backed helper `{name}` has no diagnostic edge");
}

fn prepare_synthetic_diagnostic_sites(func: &mut witchy_wir::wir::WirFunc) -> bool {
    if func.params.iter().any(|param| param.name == DIAGNOSTIC_SITE_PARAM) {
        return false;
    }
    attach_diagnostic_sites(&mut func.body, 0)
}

#[cfg(test)]
mod diagnostic_site_tests {
    use super::{
        attach_diagnostic_sites, prepare_diagnostic_helper,
        prepare_synthetic_diagnostic_sites,
        wir_seq_needs_diagnostic_site, DIAGNOSTIC_SITE_PARAM,
    };
    use witchy_wir::wir::{WirExpr as E, WirNode as N};

    #[test]
    fn source_sites_follow_host_and_abort_dependencies() {
        let direct = [N::Do(E::CallHost {
            import: "__witchy_abort".into(),
            args: vec![],
        })];
        let helper = [N::Push(E::Call {
            func: "list_at".into(),
            args: vec![],
        })];
        let host_helper = [N::Push(E::Call {
            func: "net_listen".into(),
            args: vec![],
        })];
        let ordinary = [N::Push(E::Call {
            func: "concat".into(),
            args: vec![],
        })];

        assert!(wir_seq_needs_diagnostic_site(&direct));
        assert!(wir_seq_needs_diagnostic_site(&helper));
        assert!(wir_seq_needs_diagnostic_site(&host_helper));
        assert!(!wir_seq_needs_diagnostic_site(&ordinary));
    }

    #[test]
    fn source_sites_are_arguments_and_publish_only_at_host_edges() {
        let site = 0x1234_i64;
        let mut seq = vec![N::Push(E::Call {
            func: "list_at".into(),
            args: vec![E::ConstI32(8), E::ConstI64(1)],
        })];
        assert!(attach_diagnostic_sites(&mut seq, site));
        let N::Push(E::Call { args, .. }) = &seq[0] else { panic!("list_at call") };
        assert!(matches!(args.last(), Some(E::ConstI64(v)) if *v == site));
        assert!(!seq.iter().any(|node| matches!(node, N::SetGlobal { .. })));

        let mut helper = witchy_wir::wir_helpers::wir_helper("list_at").unwrap().func;
        prepare_diagnostic_helper("list_at", &mut helper);
        assert_eq!(helper.params.last().unwrap().name, DIAGNOSTIC_SITE_PARAM);
        let N::If { then_, .. } = &helper.body[0] else { panic!("list_at guard") };
        assert!(matches!(
            &then_[0],
            N::SetGlobal { global, value: E::GetLocal(local) }
                if global == "__witchy_diagnostic_site" && local == DIAGNOSTIC_SITE_PARAM
        ));

        let mut host_helper = witchy_wir::wir_helpers::wir_helper("net_listen").unwrap().func;
        prepare_diagnostic_helper("net_listen", &mut host_helper);
        assert_eq!(host_helper.params.last().unwrap().name, DIAGNOSTIC_SITE_PARAM);
        assert!(matches!(
            &host_helper.body[..],
            [
                N::SetGlobal { global, value: E::GetLocal(local) },
                N::Push(E::CallHost { import, .. })
            ] if global == "__witchy_diagnostic_site"
                && local == DIAGNOSTIC_SITE_PARAM
                && import == "net_listen"
        ));

        let mut direct = vec![
            N::SetLocal { local: "msg".into(), value: E::ConstI32(12) },
            N::Do(E::CallHost {
                import: "__witchy_abort".into(),
                args: vec![
                    E::ConstI32(5),
                    E::ConstI64(0),
                    E::ConstI64(0),
                    E::GetLocal("msg".into()),
                ],
            }),
        ];
        assert!(attach_diagnostic_sites(&mut direct, site));
        assert!(matches!(&direct[0], N::SetLocal { local, .. } if local == "msg"));
        assert!(matches!(&direct[1], N::SetGlobal { .. }));
        assert!(matches!(&direct[2], N::Do(E::CallHost { .. })));

        // An enclosing block walks the already-instrumented nested sequence
        // again. Its broader site must not overwrite the innermost statement.
        assert!(attach_diagnostic_sites(&mut direct, 0x9999));
        assert_eq!(direct.len(), 3);
        assert!(matches!(
            &direct[1],
            N::SetGlobal { value: E::ConstI64(v), .. } if *v == site
        ));
    }

    #[test]
    fn synthesized_callers_supply_an_unknown_site_to_host_backed_helpers() {
        let mut wrapper = witchy_wir::wir::WirFunc {
            name: "run".into(),
            params: Vec::new(),
            ret: Vec::new(),
            locals: Vec::new(),
            body: vec![N::Push(E::Call { func: "build_args".into(), args: Vec::new() })],
            raw_body: None,
        };
        assert!(prepare_synthetic_diagnostic_sites(&mut wrapper));
        assert!(matches!(
            &wrapper.body[..],
            [N::Push(E::Call { func, args })]
                if func == "build_args" && matches!(&args[..], [E::ConstI64(0)])
        ));
    }
}

/// Compile a rune's build step to a WASM binary that runs in the zero-ambient
/// build sandbox. The `build` entrypoint is renamed to `main` so the whole
/// `compile_module_binary` pipeline (the `run` export, marshaling, helpers) is
/// reused verbatim. The only build-specific code is the `write_out`/`read_build`
/// host calls (the `build_out_write`/`build_read` WIR helpers), which never
/// appear in an ordinary program (so parity is untouched). The host links only
/// `build_out_write`/`build_read_len`, confined to the granted output sandbox
/// and read roots — nothing else exists for the guest to call.
pub fn compile_build_module(module: &Module) -> Result<Vec<u8>, CodegenError> {
    let mut m = module.clone();
    // A build module ships no `main`; promote its `build` entrypoint to `main`.
    m.items.retain(|it| !matches!(it, Item::Function(f) if f.name == "main"));
    for item in &mut m.items {
        if let Item::Function(f) = item {
            if f.name.rsplit('.').next() == Some("build") {
                f.name = "main".to_string();
            }
        }
    }
    compile_module_binary(&m)?.ok_or_else(|| CodegenError {
        message: "build step uses a construct the binary backend does not support".into(),
    })
}
