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
fn grantable_cap_names(module: &Module) -> std::collections::HashSet<&str> {
    module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => Some(t.name.as_str()),
            _ => None,
        })
        .collect()
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

fn reachable_functions(module: &Module) -> HashSet<String> {
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
    // Collect parameter conventions up front so call sites can resolve `var`
    // write-back even for forward references.
    for item in &module.items {
        match item {
            Item::Function(f) => {
                cg.fn_conventions
                    .insert(f.name.clone(), f.params.iter().map(|p| p.convention).collect());
                cg.fn_params.insert(f.name.clone(), f.params.clone());
                let ret = f.ret.as_ref().map(ty_kind).unwrap_or(Kind::I32);
                cg.fn_ret.insert(f.name.clone(), ret);
                if let Some(t) = &f.ret {
                    cg.fn_ret_valtype.insert(f.name.clone(), ty_to_valtype(t));
                    cg.fn_ret_ty.insert(f.name.clone(), t.clone());
                }
                // A function returning a closure (`-> fn(...) -> RET`): record the
                // closure's return kind so a `let f = make(...)` then `f(x)` call
                // recovers the result at the right width.
                if let Some(Type::Fn(_, cret)) = &f.ret {
                    cg.fn_ret_closure_kind.insert(f.name.clone(), ty_kind(cret));
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
            Item::Type(t) => {
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
                    }
                }
            }
            Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    // Now that all record types are known, record which constructor fields are
    // records, so binding `Circle(p)` in a pattern lets `p.field` resolve.
    for item in &module.items {
        if let Item::Type(t) = item {
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
    let Some(mut wir_module) = assemble_wir_module(module)? else {
        return Ok(None);
    };
    witchy_wir::wir_opt::optimize(&mut wir_module);
    // Robustness net: if any reached `Call` names a func that didn't make it into
    // the module — an unregistered guest helper like `$string_from_code`, which
    // `assemble`'s prelude/wir-helper resolution doesn't account for — bail with
    // `Ok(None)` rather than panic in the encoder's func-index lookup.
    {
        let mut defined: std::collections::HashSet<String> = std::collections::HashSet::new();
        for imp in &wir_module.imports {
            defined.insert(imp.name.clone());
        }
        for f in &wir_module.funcs {
            defined.insert(f.name.clone());
        }
        let mut called: std::collections::HashSet<String> = std::collections::HashSet::new();
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
    let bytes = witchy_wir::wir_encode::encode(&wir_module);
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
    use witchy_wir::wir::{
        DataSegment, GlobalInit, Kind as WK, WirExpr, WirFunc, WirGlobal, WirImport, WirModule,
        WirNode, WirTable,
    };
    use witchy_wir::wir_prelude::WasmTy;
    // Front-end, identical to `compile_module_with`.
    let recs = witchy_syntax::records::lower(module.clone()).map_err(|message| CodegenError { message })?;
    let mut lowered = witchy_types::traits::lower_for_wasm(recs);
    witchy_syntax::parser::lower_sugar_module(&mut lowered);
    alpha_rename_module(&mut lowered);
    let mut cg = Codegen::new();
    cg.collect_wir = true;
    cg.type_table = witchy_types::typeck::annotate(&lowered);
    // `e ? "msg"` desugar (`__try_ctx`) is type-directed: an `Option` operand lowers
    // via `option.ok_or`, a `Result` via `result.map_err`. Rewrite it here — after
    // annotation (so the operand's type is known) and before the string-`+` flip +
    // lowering (so the synthesized `map_err` lambda's `+` flips to `Concat` and its
    // nodes get typed). Re-annotate so the freshly minted calls/lambda are in the
    // type table.
    if rewrite_try_ctx_module(&mut lowered, &cg.type_table) {
        cg.type_table = witchy_types::typeck::annotate(&lowered);
    }
    flip_string_add_module(&mut lowered, &cg.type_table);
    let module = &lowered;
    register_module_items(&mut cg, module);
    cg.summaries = analysis::Summaries::of_module(module);

    let reachable = reachable_functions(module);
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
    // RFC-0038: `Some((type_name, nfields))` for a grantable-capability `main` param
    // (its record is minted at the root); `None` otherwise.
    let mut main_param_user_cap: Vec<Option<(String, usize)>> = Vec::new();
    // Grantable capability name -> field count, to detect + size a grantable param.
    let grantable_caps: std::collections::HashMap<&str, usize> = module
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
    // Structural `==` / `__render` are fine when every legacy eq/ts helper has a
    // WIR twin; a shape the WIR generator couldn't build leaves its key without a
    // twin → bail to WAT.
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
        }
    };

    // --- Capability-minimal WIR-helper path (#35) -------------------------------
    // If every prelude helper the program reaches has a WIR-native form (the
    // `wir_helper` registry), build a PRUNED module that declares only those
    // helpers and imports only their authority — instead of splicing the full
    // "all features on" raw-body prelude (which would over-import and break the
    // capability model). Falls through to the raw-body path otherwise.
    {
        let helper_names: std::collections::HashSet<&str> =
            prelude.funcs.iter().map(|f| f.name.as_str()).collect();
        let mut called = std::collections::HashSet::new();
        let mut user_host_imports = std::collections::HashSet::new();
        for name in &user_order {
            if let Some(wf) = cg.wir_funcs.get(name) {
                collect_called_funcs(&wf.body, &mut called);
                collect_called_host_imports(&wf.body, &mut user_host_imports);
            }
        }
        // The generated structural-eq / render helpers (included below) call
        // prelude helpers themselves — a Str field eq via `$str_eq`, a renderer via
        // `$concat`/`$int_to_string`. Pull those (and nested eq_*/ts_* calls) into
        // the reached set so the resolution loop declares them.
        for f in cg.eq_wir_helpers.values() {
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
        // The `__galloc` allocator the string-export wrappers expose calls `$ensure`
        // and bumps `$heap`, so pull `ensure` into the reached set (it brings the
        // `$heap` global via `uses_heap` below). Harmless if a string-export body
        // already reaches it.
        if !string_exports.is_empty() {
            called.insert("ensure".to_string());
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
            let mut pruned_funcs: Vec<WirFunc> = resolved.into_values().map(|s| s.func).collect();
            // The program-specific structural-equality / render helpers reached by
            // user `==` / `__render`.
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
            // Each `Dir` param maps to a distinct host handle in declaration order
            // (0, 1, 2, …) so a `main` taking several `Dir`s gets several grants;
            // each `File` param maps to a file handle in declaration order (the host
            // pre-populates the files table from `--file` grants, RFC-0012); every
            // other cap is a right-less placeholder (handle 0).
            let mut dir_handle = 0i32;
            let mut file_handle = 0i32;
            let mut user_cap_ord = 0i32;
            let mut main_args: Vec<WirExpr> = Vec::with_capacity(main_params);
            for i in 0..main_params {
                if main_param_is_args.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::Call { func: "build_args".into(), args: vec![] });
                } else if main_param_is_dir.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::ConstI32(dir_handle));
                    dir_handle += 1;
                } else if main_param_is_file.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::ConstI32(file_handle));
                    file_handle += 1;
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
                // __galloc(len) -> ptr : ensure(len); p = heap; heap = heap + len; p
                pruned_funcs.push(WirFunc {
                    name: "__galloc".into(),
                    params: vec![witchy_wir::wir::WirLocal {
                        name: "len".into(),
                        ty: witchy_wir::wir::WirTy::Bool, // i32
                    }],
                    ret: vec![witchy_wir::wir::WirTy::Bool], // i32 pointer
                    locals: vec![witchy_wir::wir::WirLocal {
                        name: "p".into(),
                        ty: witchy_wir::wir::WirTy::Bool,
                    }],
                    body: vec![
                        WirNode::Do(WirExpr::Call {
                            func: "ensure".into(),
                            args: vec![WirExpr::GetLocal("len".into())],
                        }),
                        WirNode::SetLocal {
                            local: "p".into(),
                            value: WirExpr::GetGlobal("heap".into()),
                        },
                        WirNode::SetGlobal {
                            global: "heap".into(),
                            value: WirExpr::Binary {
                                op: witchy_wir::wir::BinOp::Add,
                                kind: WK::I32,
                                lhs: Box::new(WirExpr::GetGlobal("heap".into())),
                                rhs: Box::new(WirExpr::GetLocal("len".into())),
                            },
                        },
                        WirNode::Push(WirExpr::GetLocal("p".into())),
                    ],
                    raw_body: None,
                });
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
                ]
            } else {
                Vec::new()
            };
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
            return Ok(Some(WirModule {
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
            }));
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
fn collect_called_funcs(seq: &witchy_wir::wir::WirSeq, out: &mut std::collections::HashSet<String>) {
    use witchy_wir::wir::{WirExpr as E, WirNode as N};
    fn expr(e: &E, out: &mut std::collections::HashSet<String>) {
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
            E::ConstI64(_) | E::ConstF64(_) | E::ConstI32(_) | E::StrPtr(_) | E::MemorySize
            | E::GetLocal(_) | E::GetGlobal(_) => {}
        }
    }
    fn node(n: &N, out: &mut std::collections::HashSet<String>) {
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
fn collect_called_host_imports(seq: &witchy_wir::wir::WirSeq, out: &mut std::collections::HashSet<String>) {
    use witchy_wir::wir::{WirExpr as E, WirNode as N};
    fn expr(e: &E, out: &mut std::collections::HashSet<String>) {
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
            E::ConstI64(_) | E::ConstF64(_) | E::ConstI32(_) | E::StrPtr(_) | E::MemorySize
            | E::GetLocal(_) | E::GetGlobal(_) => {}
        }
    }
    fn node(n: &N, out: &mut std::collections::HashSet<String>) {
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
            N::Br { cond: Some(c), .. } => expr(c, out),
            N::Drop(e) | N::Do(e) | N::Push(e) | N::Return(Some(e)) => expr(e, out),
            N::Br { cond: None, .. } | N::Return(None) | N::Unreachable => {}
        }
    }
    for n in seq {
        node(n, out);
    }
}

/// Compile a rune's build step to a WASM binary that runs in the zero-ambient
/// build sandbox. The `build` entrypoint is renamed to `main` so the whole
/// `compile_module_binary` pipeline (the `run` export, marshaling, helpers) is
/// reused verbatim — its capability parameters lower to handle 0 exactly like
/// `main`'s, and the only build-specific code is the `write_out`/`read_build`
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
