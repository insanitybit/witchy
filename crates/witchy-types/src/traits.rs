//! Lowering of `trait`/`impl` declarations to ordinary functions.
//!
//! Traits are a front-end feature. An `impl Show for Int { fn show(self) {...} }`
//! becomes a plain function named `Show__Int__show`, and a trait-method call
//! `show(x)` is rewritten to that function once the receiver's type is known.
//! After lowering, a module contains no `Item::Trait`/`Item::Impl`, so the type
//! checker, interpreter, and WASM backend all see identical ordinary functions —
//! which is what keeps the two backends in agreement.
//!
//! Increment 1 resolves dispatch at *concrete* call sites: the receiver's type
//! is found by a light, local analysis (literals, constructors, annotated
//! parameters, `let` bindings, function return types). Generic bounds
//! (`where a: Show`) and dispatch through type variables are a later increment; a
//! call whose receiver type can't be determined is left untouched and the type
//! checker reports it as an unknown function.
//!
//! (RFC-0043) Statement-position mutating-method write-back lives HERE, not in
//! the linker's old name census (`rewrite_mut_method_stmts`, deleted). This is
//! the point where `place.method(args)` resolves to a concrete callee BY THE
//! RECEIVER'S TYPE, so the write-back decision reads the resolved callee's
//! declaration (`Function::is_mutator`) rather than a whole-program name census.
//! A statement `xs.push(1)` on a `var` place whose resolved callee is a mutator
//! becomes `list.push(xs, 1)`; a non-mutator statement call whose result is
//! non-Nil is a discard error. The rewrite edits the single AST both backends
//! consume (via `lower`/`lower_for_wasm`) and the checker consumes (via
//! `lower_checked`), so parity holds by construction.

// foldhash (not SipHash): all keys are compiler-internal names/ids, never
// attacker-chosen collections — see the note in typeck.rs.
use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};

use witchy_syntax::{ast::*, cap_ops};

#[derive(Clone, Debug)]
struct ImplTraitMethod {
    trait_args: Vec<Type>,
    mangled: String,
}

type TraitImplTable = HashMap<(String, String, String), Vec<ImplTraitMethod>>;

/// Mangled name for an impl method: `Trait__Type__method`, or
/// `Trait__Arg__Type__method` for parameterized trait impls such as `From(Arg)`.
fn mangle(trait_name: Option<&str>, trait_args: &[Type], type_name: &str, method: &str) -> String {
    match trait_name {
        Some(t) if trait_args.is_empty() => format!("{t}__{type_name}__{method}"),
        Some(t) => {
            let args = trait_args
                .iter()
                .map(|arg| mangle_type_key(&type_key(arg.unqualified())))
                .collect::<Vec<_>>()
                .join("__");
            format!("{t}__{args}__{type_name}__{method}")
        }
        // Inherent method: no trait segment, still dispatched by receiver type.
        None => format!("{type_name}__{method}"),
    }
}

fn push_trait_impl(
    table: &mut TraitImplTable,
    trait_name: &str,
    trait_args: &[Type],
    method: &str,
    type_name: &str,
    mangled: String,
) {
    table
        .entry((trait_name.to_string(), method.to_string(), type_name.to_string()))
        .or_default()
        .push(ImplTraitMethod {
            trait_args: trait_args.to_vec(),
            mangled,
        });
}

fn static_bound_marker(receiver: &str, method: &str) -> String {
    format!("__trait_static__{receiver}__{method}")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TraitMethodInfo {
    owner: String,
    is_static: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum MethodResolution {
    Found(String),
    Ambiguous(Vec<String>),
}

/// Build the ordinary function a (possibly defaulted) method lowers to.
/// `Self` in any annotation refers to the implementing type (so a trait can
/// write `fn eq(self, other: Self) -> Bool`), and an unannotated receiver
/// (`self`) takes the implementing type too. Without this, an untyped non-self
/// parameter would default to i32 in codegen and clash with, e.g., an f64 arg.
fn method_fn(
    name: String,
    mut params: Vec<Param>,
    ret: Option<Type>,
    body: Block,
    type_name: &str,
    target_args: &[Type],
    bounds: Vec<(String, String, Vec<Type>)>,
) -> Function {
    // The target type the method's `self` stands for: `List(a)` for a generic impl
    // (so monomorphization can recover the element), bare `List`/`Point` otherwise.
    let self_ty = if type_name.starts_with("Tuple") {
        Type::Tuple(target_args.to_vec())
    } else {
        Type::Named(type_name.to_string(), target_args.to_vec())
    };
    for p in &mut params {
        if let Some(t) = &p.ty {
            p.ty = Some(subst_self(t, &self_ty));
        }
    }
    if let Some(first) = params.first_mut() {
        if first.ty.is_none() {
            first.ty = Some(self_ty.clone());
        }
    }
    let ret = ret.map(|t| subst_self(&t, &self_ty));
    Function {
        public: true,
        comptime_only: false,
        name,
        params,
        ret,
        body,
        // A conditional impl's `where` bounds become the generated method's
        // bounds, so its body's bounded calls resolve and it monomorphizes per
        // concrete type (discharging the bound).
        bounds,
        is_gen: false,
        is_async: false,
    }
}

/// Replace every `Self` in a type with the implementing type. `self_ty` carries
/// the type's parameters for a generic impl (`Pair(a, b)`, `(a, b)`), so a
/// default method's `other: Self` is typed `Pair(a, b)` to match the receiver —
/// not the bare head `Pair`, which would clash with a real `Pair(Int, String)`.
fn subst_self(t: &Type, self_ty: &Type) -> Type {
    match t {
        Type::Qualified(q, inner) => Type::Qualified(*q, Box::new(subst_self(inner, self_ty))),
        Type::Named(n, args) if n == "Self" && args.is_empty() => self_ty.clone(),
        Type::Named(n, args) => {
            Type::Named(n.clone(), args.iter().map(|a| subst_self(a, self_ty)).collect())
        }
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|a| subst_self(a, self_ty)).collect()),
        Type::Fn(ps, r, conventions) => Type::Fn(
            ps.iter().map(|a| subst_self(a, self_ty)).collect(),
            Box::new(subst_self(r, self_ty)),
            conventions.clone(),
        ),
    }
}

/// Desugar all traits and impls in `module` into ordinary functions, rewriting
/// trait-method call sites to the resolved impl. A no-op (returns the module
/// unchanged) when there are no traits or impls, so non-trait programs — every
/// existing one — are unaffected.
/// Lower traits/impls and monomorphize `where`-bounded generics — needed by every
/// backend. The interpreter and native (Rust) backends call this; the WASM
/// backend calls [`lower_for_wasm`], which additionally monomorphizes *unbounded*
/// generics on primitive type arguments (the interpreter and Rust handle generic
/// `==` and 64-bit Ints natively, so only the lossy WASM i32 generic ABI needs it).
pub fn lower(module: Module) -> Module {
    lower_with(module, false).0
}

/// [`lower`] that surfaces unsatisfiable trait dispatch: a trait-method call
/// whose receiver type is known but has no impl. The type checker runs THIS
/// flavor so the error reads "`Float` does not implement `Show`" at check
/// time instead of "unknown function `show`" after lowering.
pub fn lower_checked(module: Module) -> Result<Module, String> {
    let (lowered, missing) = lower_with(module, false);
    match missing.into_iter().next() {
        Some(msg) => Err(msg),
        None => Ok(lowered),
    }
}

/// Like [`lower`], but also monomorphizes unbounded generics on primitive type
/// arguments — for the WASM backend, whose generic ABI otherwise pointer-compares
/// strings and truncates large Ints.
pub fn lower_for_wasm(module: Module) -> Module {
    lower_with(module, true).0
}

/// Start a mono-phase profiling timer, but ONLY when `WITCHY_DEBUG_MONO_TIMING`
/// is set (the same gate the matching `eprintln!`s use). Returns `None`
/// otherwise, so the timestamp is never taken on the hot path — and, crucially,
/// never on `wasm32-unknown-unknown`, where `std::time::Instant::now()` panics
/// ("time not implemented on this platform"). The in-browser playground compiles
/// through `lower_with`, so an unconditional timer there traps compilation of
/// every program (BUG-015).
fn mono_timing_start() -> Option<std::time::Instant> {
    std::env::var_os("WITCHY_DEBUG_MONO_TIMING").map(|_| std::time::Instant::now())
}

fn lower_with(module: Module, mono_unbounded: bool) -> (Module, Vec<String>) {
    // Expand type aliases and inline module-level constants first (a no-op once
    // the linker has done so, but covers single-module paths like `check_str`).
    let module = witchy_syntax::aliases::resolve(witchy_syntax::consts::inline(module));
    let needs_lowering = module.items.iter().any(|it| {
        matches!(it, Item::Trait(_) | Item::Impl(_))
            || matches!(it, Item::Function(f) if !f.bounds.is_empty()
                || (mono_unbounded && !signature_type_vars(f).is_empty()))
    }) || module_needs_lowering(&module.items);
    if !needs_lowering {
        return (module, Vec::new());
    }

    // method name -> owning trait(s), and each trait's full method list (for
    // default bodies). A method name is not a global namespace: unrelated traits
    // may both declare `name`, `from`, etc.; bounded dispatch chooses by trait
    // identity and concrete dispatch rejects ambiguous trait-method calls.
    let mut trait_methods: HashMap<String, Vec<TraitMethodInfo>> = HashMap::new();
    let mut trait_method_list: HashMap<String, Vec<MethodSig>> = HashMap::new();
    let mut trait_type_params: HashMap<String, Vec<String>> = HashMap::new();
    // trait name -> its DIRECT supertraits; closed under transitivity below.
    let mut trait_supertraits: HashMap<String, Vec<String>> = HashMap::new();
    for item in &module.items {
        if let Item::Trait(t) = item {
            for m in &t.methods {
                trait_methods
                    .entry(m.name.clone())
                    .or_default()
                    .push(TraitMethodInfo {
                        owner: t.name.clone(),
                        is_static: m.params.first().is_none_or(|p| p.name != "self"),
                    });
            }
            trait_method_list.insert(t.name.clone(), t.methods.clone());
            trait_type_params.insert(t.name.clone(), t.typarams.clone());
            trait_supertraits.insert(t.name.clone(), t.supertraits.clone());
        }
    }
    // A `where a: Ord` bound must discharge Eq/PartialOrd/PartialEq methods too,
    // so each trait maps to ALL of its supertraits (direct and inherited).
    let trait_supertraits = transitive_supertraits(&trait_supertraits);

    // Trait methods are keyed by (trait, method, receiver type). Inherent methods
    // are a deliberate separate namespace keyed by (method, receiver type).
    let mut trait_impl_table: TraitImplTable = HashMap::new();
    let mut inherent_impl_table: HashMap<(String, String), String> = HashMap::new();
    let mut inherent_methods: HashSet<String> = HashSet::new();
    // (type name, method name) -> mangled fn, for self-less impl methods.
    let mut statics: HashMap<(String, String), String> = HashMap::new();
    // (trait name, impl head) present, to check supertrait obligations below.
    let mut impl_pairs: HashSet<(String, String)> = HashSet::new();
    let mut impl_contract_diags: Vec<String> = Vec::new();
    let mut generated: Vec<Function> = Vec::new();
    let synthetic_impls = synthesize_anon_union_impls(&module.items, &trait_method_list);
    let source_impls = module.items.iter().filter_map(|item| match item {
        Item::Impl(im) => Some(im),
        _ => None,
    });
    for im in source_impls.chain(synthetic_impls.iter()) {
        if let Some(t) = &im.trait_name {
            impl_pairs.insert((t.clone(), im.type_name.clone()));
            if let Some(methods) = trait_method_list.get(t) {
                let params = trait_type_params.get(t).map(Vec::as_slice).unwrap_or(&[]);
                impl_contract_diags.extend(validate_trait_impl(im, methods, params));
            }
        }
        let provided: HashSet<&str> = im.methods.iter().map(|m| m.name.as_str()).collect();
        // Methods the impl defines. A method whose first parameter is
        // `self` is an INSTANCE method (dispatched on a value); one
        // without is a STATIC, callable only as `Type.name(args)`.
        for method in &im.methods {
            let mangled = mangle(
                im.trait_name.as_deref(),
                &im.trait_args,
                &im.type_name,
                &method.name,
            );
            let is_static =
                method.params.first().is_none_or(|p| p.name != "self");
            if is_static {
                if let Some(trait_name) = &im.trait_name {
                    push_trait_impl(
                        &mut trait_impl_table,
                        trait_name,
                        &im.trait_args,
                        &method.name,
                        &im.type_name,
                        mangled.clone(),
                    );
                }
                statics.insert(
                    (im.type_name.clone(), method.name.clone()),
                    mangled.clone(),
                );
                generated.push(method_fn(
                    mangled,
                    method.params.clone(),
                    method.ret.clone(),
                    method.body.clone(),
                    &im.type_name,
                    &im.target_args,
                    im.bounds.clone(),
                ));
                continue;
            }
            if let Some(trait_name) = &im.trait_name {
                push_trait_impl(
                    &mut trait_impl_table,
                    trait_name,
                    &im.trait_args,
                    &method.name,
                    &im.type_name,
                    mangled.clone(),
                );
            } else {
                // Ambient std-owned types (`List`, `Dict`, `String`, ...)
                // are always in scope through prelude modules. Their future
                // inherent methods must be reachable as receiver methods,
                // but must not resurrect retired global builtins such as
                // bare `push([1], 2)`.
                if !is_ambient_std_owned_name(&im.type_name) {
                    inherent_methods.insert(method.name.clone());
                }
                inherent_impl_table
                    .insert((method.name.clone(), im.type_name.clone()), mangled.clone());
            }
            generated.push(method_fn(
                mangled,
                method.params.clone(),
                method.ret.clone(),
                method.body.clone(),
                &im.type_name,
                &im.target_args,
                im.bounds.clone(),
            ));
        }
        // Methods the impl omits but the trait provides a default for. Only
        // trait impls inherit defaults; inherent impls have no trait.
        if let Some(trait_name) = &im.trait_name {
            if let Some(methods) = trait_method_list.get(trait_name) {
                for ms in methods {
                    if provided.contains(ms.name.as_str()) {
                        continue;
                    }
                    if let Some(body) = &ms.default {
                        let mangled = mangle(
                            Some(trait_name),
                            &im.trait_args,
                            &im.type_name,
                            &ms.name,
                        );
                        push_trait_impl(
                            &mut trait_impl_table,
                            trait_name,
                            &im.trait_args,
                            &ms.name,
                            &im.type_name,
                            mangled.clone(),
                        );
                        generated.push(method_fn(
                            mangled,
                            ms.params.clone(),
                            ms.ret.clone(),
                            body.clone(),
                            &im.type_name,
                            &im.target_args,
                            im.bounds.clone(),
                        ));
                    }
                }
            }
        }
    }
    // `?` conversion is a semantic use of a resolved `From.from` trait impl,
    // not a property of whatever compiler-generated spelling its function
    // happens to receive. Keep that identity explicit through annotation and
    // rewriting below.
    let from_conversion_fns = trait_impl_table
        .iter()
        .filter(|((trait_name, method, _), _)| trait_name == "From" && method == "from")
        .flat_map(|(_, methods)| methods.iter().map(|method| method.mangled.clone()))
        .collect::<HashSet<_>>();

    // Supertrait obligations: `impl Ord for T` requires `impl Eq for T`,
    // `impl PartialOrd for T`, etc. (the transitive closure). Surfaced through the
    // same diagnostics channel as missing dispatch impls.
    let mut supertrait_diags: Vec<String> = Vec::new();
    let source_impls = module.items.iter().filter_map(|item| match item {
        Item::Impl(im) => Some(im),
        _ => None,
    });
    for im in source_impls.chain(synthetic_impls.iter()) {
        if let Some(t) = &im.trait_name {
            if let Some(supers) = trait_supertraits.get(t) {
                for sup in supers {
                    if !impl_pairs.contains(&(sup.clone(), im.type_name.clone())) {
                        supertrait_diags.push(format!(
                            "`{ty}` implements `{t}` but not its supertrait `{sup}` \
                             (add `impl {sup} for {ty}`)",
                            ty = im.type_name
                        ));
                    }
                }
            }
        }
    }

    // Keep everything that isn't a trait/impl, then append the lowered methods.
    let imports = module.imports;
    let mut items: Vec<Item> = module
        .items
        .into_iter()
        .filter(|it| !matches!(it, Item::Trait(_) | Item::Impl(_)))
        .collect();
    items.extend(generated.into_iter().map(Item::Function));

    // Phase 0 (typed lowering): annotate this exact items instance so
    // monomorphization can resolve type arguments the declaration-only local
    // judgment cannot
    // (e.g. `dict.get(d, k)` needs `v` from `d: Dict(String, String)`).
    // Node pointers stay valid through the moves below (statements live in
    // each function's own heap allocations).
    //
    // QUIET pre-mono dispatch pass: resolve trait-method calls and method
    // syntax at every CONCRETE site (including instantiated trait defaults
    // like `Ord__Int__less` calling `compare`), so the annotate probe below
    // sees a checkable module. Diagnostics are discarded here — anything
    // genuinely unresolvable is re-found loudly by the post-mono pass.
    //
    // (RFC-0043) Discarded-non-Nil-method-result errors, however, ARE surfaced
    // (unlike `missing_impls`): each statement-position method call is resolved
    // exactly once — by whichever pass first knows its receiver type — so a
    // discard is detected once. Deduplicated at the end (a generic site the
    // quiet pass leaves unresolved is flagged per specialization by the final
    // pass; the same message need only appear once).
    let discard_errors = std::cell::RefCell::new(Vec::new());
    {
        let fn_sigs = build_fn_sigs(&items);
        let ctor_infos = build_ctor_infos(&items);
        let record_fields = build_record_fields(&items);
        let free_fns: HashSet<String> = items
            .iter()
            .filter_map(|it| match it {
                Item::Function(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect();
        let owner_methods = build_owner_methods(&items);
        let quiet = std::cell::RefCell::new(Vec::new());
        let (var_calls, returns_nil) = build_mutation_tables(&items);
        let empty_table = crate::typeck::TypeTable::default();
        let ctx = Ctx {
            trait_methods: &trait_methods,
            inherent_methods: &inherent_methods,
            supertraits: &trait_supertraits,
            trait_impl_table: &trait_impl_table,
            inherent_impl_table: &inherent_impl_table,
            ctor_infos: &ctor_infos,
            fn_sigs: &fn_sigs,
            record_fields: &record_fields,
            free_fns: &free_fns,
            owner_methods: &owner_methods,
            missing_impls: &quiet,
            statics: &statics,
            var_calls: &var_calls,
            returns_nil: &returns_nil,
            discard_errors: &discard_errors,
            table: &empty_table,
            bound_traits: std::cell::RefCell::new(HashMap::new()),
            current_func: std::cell::RefCell::new(String::new()),
            current_line: std::cell::Cell::new(0),
        };
        for item in &mut items {
            if let Item::Function(f) = item {
                *ctx.current_func.borrow_mut() = f.name.clone();
                ctx.set_bounds(&f.bounds);
                let mut scope = Scope::new();
                seed_typed_params(&f.params, &mut scope);
                // (RFC-0043) A function body's tail statement is its return
                // value (value position); write-back skips it.
                ctx.rewrite_block(&mut f.body, &mut scope, true);
            }
        }
    }

    let __t = mono_timing_start();
    let mut typed = crate::typeck::annotate_with_from_conversions(
        Module {
            modes: Vec::new(),
            imports: imports.clone(),
            from_imports: Vec::new(),
            items,
            import_lines: Vec::new(),
            item_lines: Vec::new(),
        },
        &from_conversion_fns,
    );
    if let Some(__t) = __t {
        eprintln!(
            "annotate first_table: items={} took={:?}",
            typed.module().items.len(),
            __t.elapsed()
        );
    }

    // The no-fallback template set: bounded generics PLUS the generic helpers that
    // transitively call them (RFC-0046 §2). Both are kept in `items` through the
    // fixpoint so each re-annotate sees their signatures, then removed afterwards —
    // they have no runnable generic form (their bounded call can't resolve while
    // generic). Their concrete specializations are what gets emitted.
    let no_fallback = no_fallback_template_names(&typed.module().items);
    let template_body_diag = if no_fallback.is_empty() {
        None
    } else {
        let probe = Module {
            modes: Vec::new(),
            imports: imports.clone(),
            from_imports: Vec::new(),
            items: typed.module().items.clone(),
            import_lines: Vec::new(),
            item_lines: Vec::new(),
        };
        crate::typeck::check_selected_lowered(&probe, &no_fallback, &from_conversion_fns)
            .err()
            .and_then(|e| {
                // Some bounded trait calls are intentionally unresolved until a
                // concrete specialization exists (`compare` for `where a: Ord`,
                // etc.). Keep those lazy, but surface ordinary template-body type
                // errors now so an uncalled generic cannot make `check` lie.
                let lazy_template_placeholder = e.message.contains("call to unknown function")
                    || e.message.contains("cannot infer the result type")
                    || e.message.contains("could not resolve the `")
                    || e.message.contains("requires `Ord`");
                (!lazy_template_placeholder).then_some(e.message)
            })
    };
    let mut templates: HashMap<String, Function> = HashMap::new();
    for it in &typed.module().items {
        if let Item::Function(f) = it {
            if no_fallback.contains(&f.name) {
                templates.insert(f.name.clone(), f.clone());
            }
        }
    }
    // Unbounded generic functions are ALSO templates, but stay in `items` as a
    // fallback: a call whose primitive type argument resolves is rewritten to a
    // specialization (so `==` is content-correct and `Int` stays 64-bit), while a
    // call that can't be resolved keeps calling the generic version unchanged.
    if mono_unbounded {
        for it in &typed.module().items {
            if let Item::Function(f) = it {
                if f.bounds.is_empty()
                    && !signature_type_vars(f).is_empty()
                    && !crate::typeck::intrinsic(&f.name)
                {
                    templates.entry(f.name.clone()).or_insert_with(|| f.clone());
                }
            }
        }
    }
    // (RFC-0053) The interpolation flip's inputs. `show_types` is the bare name
    // of every type carrying a `Show` impl (`trait_impl_table`'s `Show.show` keys). A value
    // whose concrete type is here, or whose container transitively contains one,
    // can render through `show.render`. The linker preludes that helper, so imports
    // never select semantics; the availability guard only protects direct users of
    // this stage API that bypass linking.
    let show_types: HashSet<String> = trait_impl_table
        .keys()
        .filter(|(owner, method, _)| owner == "Show" && method == "show")
        .map(|(_, _, ty)| ty.rsplit_once('.').map_or(ty.clone(), |(_, s)| s.to_string()))
        .collect();
    let render_available = templates.contains_key("show.render");
    let mut mono_diags: Vec<String> = Vec::new();
    if !templates.is_empty() {
        // Annotate + monomorphize to a FIXPOINT (RFC-0046 §2): each round types the
        // concrete specializations the previous round generated, unlocking the
        // bounded calls inside them (a generic helper's `iter.collect` resolves
        // only once the helper itself is specialized to a concrete type). Two
        // rounds resolve every known case — one to specialize the helper, one to
        // type its result and resolve its inner bounded call — and the loop stops
        // as soon as a round generates nothing new. Every round is a whole-module,
        // deterministic pass, so both backends reach the same fixpoint. The memo
        // persists across rounds so a specialization is never generated twice.
        const MONO_ROUNDS: usize = 4;
        let mut memo: HashMap<(String, Vec<String>), String> = HashMap::new();
        for round in 0..MONO_ROUNDS {
            let fn_sigs = build_fn_sigs(&typed.module().items);
            let ctor_infos = build_ctor_infos(&typed.module().items);
            let record_fields = build_record_fields(&typed.module().items);
            let known_fns: HashSet<String> = typed
                .module()
                .items
                .iter()
                .filter_map(|it| match it {
                    Item::Function(f) => Some(f.name.clone()),
                    _ => None,
                })
                .collect();
            let __t_mono = mono_timing_start();
            let (next_memo, next_diags, generated) = typed.rewrite_preserving_nodes(
                |table, module| {
                    let mut mono = Mono {
                        templates: &templates,
                        known_fns: &known_fns,
                        trait_methods: &trait_methods,
                        supertraits: &trait_supertraits,
                        trait_impl_table: &trait_impl_table,
                        diagnostics: Vec::new(),
                        ctor_infos: &ctor_infos,
                        record_fields: &record_fields,
                        fn_sigs,
                        memo: std::mem::take(&mut memo),
                        generated: Vec::new(),
                        table,
                        skip_walk: &no_fallback,
                        show_types: &show_types,
                        render_available,
                    };
                    mono.run(&mut module.items);
                    (mono.memo, mono.diagnostics, mono.generated)
                },
            );
            memo = next_memo;
            mono_diags = next_diags;
            let progressed = !generated.is_empty();
            if let Some(__t_mono) = __t_mono {
                eprintln!(
                    "mono round {round}: items={} generated={} mono_walk={:?}",
                    typed.module().items.len(), generated.len(), __t_mono.elapsed()
                );
            }
            if !progressed {
                break;
            }
            // Generated nodes have no facts in the current table. Consume the
            // typed owner before extending the AST, then annotate the new exact
            // module before either the next round or the final dispatch pass.
            let mut module = typed.into_module();
            module.items.extend(generated.into_iter().map(Item::Function));
            let __t = mono_timing_start();
            typed =
                crate::typeck::annotate_with_from_conversions(module, &from_conversion_fns);
            if let Some(__t) = __t {
                eprintln!(
                    "annotate round {round}: items={} took={:?}",
                    typed.module().items.len(),
                    __t.elapsed()
                );
            }
            if round + 1 == MONO_ROUNDS {
                break;
            }
        }
    }

    // Tables used to determine a receiver's type at a trait-method call site.
    let fn_sigs = build_fn_sigs(&typed.module().items);
    let ctor_infos = build_ctor_infos(&typed.module().items);
    let record_fields = build_record_fields(&typed.module().items);
    let free_fns: HashSet<String> = typed
        .module()
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Function(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect();
    let owner_methods = build_owner_methods(&typed.module().items);
    let missing_impls = std::cell::RefCell::new(Vec::new());
    let (var_calls, returns_nil) = build_mutation_tables(&typed.module().items);
    let (mut lowered, ()) = typed.rewrite_into_module(|type_table, module| {
        let ctx = Ctx {
            trait_methods: &trait_methods,
            inherent_methods: &inherent_methods,
            supertraits: &trait_supertraits,
            trait_impl_table: &trait_impl_table,
            inherent_impl_table: &inherent_impl_table,
            ctor_infos: &ctor_infos,
            fn_sigs: &fn_sigs,
            record_fields: &record_fields,
            free_fns: &free_fns,
            owner_methods: &owner_methods,
            missing_impls: &missing_impls,
            statics: &statics,
            var_calls: &var_calls,
            returns_nil: &returns_nil,
            discard_errors: &discard_errors,
            table: type_table,
            bound_traits: std::cell::RefCell::new(HashMap::new()),
            current_func: std::cell::RefCell::new(String::new()),
            current_line: std::cell::Cell::new(0),
        };
        for item in &mut module.items {
            if let Item::Function(f) = item {
                if no_fallback.contains(&f.name) {
                    continue;
                }
                *ctx.current_func.borrow_mut() = f.name.clone();
                ctx.set_bounds(&f.bounds);
                let mut scope = Scope::new();
                seed_typed_params(&f.params, &mut scope);
                // (RFC-0043) The body's tail statement is the return value.
                ctx.rewrite_block(&mut f.body, &mut scope, true);
            }
        }
        // This replacement changes expression structure. It is deliberately the
        // final table-dependent operation in the closure; the table is consumed
        // together with the typed owner immediately afterwards.
        rewrite_try_from_conversions(&mut module.items, type_table, &from_conversion_fns);
    });
    lowered
        .items
        .retain(|it| !matches!(it, Item::Function(f) if no_fallback.contains(&f.name)));
    lowered.imports = imports;
    // (RFC-0056; BUG-210) A defaulted parameter on an `impl`/`trait` method
    // (`p.scaled()` where `fn scaled(self, k: Int = 2)`) is unreachable through the
    // linker's keyword-argument pass, which ran BEFORE method calls were resolved.
    // Now that every dispatchable method is a positional `Call` to a mangled
    // function that carries the same defaults, splice the trailing constant
    // defaults with the exact free-function mechanism — in the shared `lower_with`,
    // so both backends fill them identically (parity by construction). Labels never
    // reach a method call, so this only ever splices omitted trailing defaults.
    let kw = witchy_syntax::keyword_args::resolve(&mut lowered);
    (
        lowered,
        {
            let mut d = impl_contract_diags;
            d.extend(supertrait_diags);
            if let Some(msg) = template_body_diag {
                d.push(msg);
            }
            // (RFC-0043) A discarded-result error is a definitive diagnostic on a
            // fully-resolved callee — surface it FIRST (ahead of any spurious
            // missing-impl the quiet pass would otherwise report), deduplicated.
            let mut seen = HashSet::new();
            let mut discards = discard_errors.into_inner();
            discards.retain(|m| seen.insert(m.clone()));
            d.extend(discards);
            d.extend(missing_impls.into_inner());
            d.extend(mono_diags);
            if let Err(msg) = kw {
                d.push(msg);
            }
            d
        },
    )
}

fn bare(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

fn display_type(t: &Type) -> String {
    match t {
        Type::Qualified(q, inner) => format!("{} {}", q.as_str(), display_type(inner)),
        Type::Named(n, args) if args.is_empty() => bare(n).to_string(),
        Type::Named(n, args) => {
            format!(
                "{}({})",
                bare(n),
                args.iter().map(display_type).collect::<Vec<_>>().join(", ")
            )
        }
        Type::Tuple(ts) => {
            format!("({})", ts.iter().map(display_type).collect::<Vec<_>>().join(", "))
        }
        Type::Fn(ps, r, conventions) => {
            let rendered = ps
                .iter()
                .enumerate()
                .map(|(i, ty)| {
                    let prefix = match conventions.get(i).copied().unwrap_or_default() {
                        Convention::Let => "",
                        Convention::Borrow => "let ",
                        Convention::Var => "var ",
                        Convention::Own => "own ",
                    };
                    format!("{prefix}{}", display_type(ty))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "fn({}) -> {}",
                rendered,
                display_type(r)
            )
        }
    }
}

fn nil_type() -> Type {
    Type::Named("Nil".to_string(), Vec::new())
}

fn impl_self_type(im: &ImplDef) -> Type {
    if im.type_name.starts_with("Tuple") {
        Type::Tuple(im.target_args.clone())
    } else {
        Type::Named(im.type_name.clone(), im.target_args.clone())
    }
}

fn subst_trait_params(t: &Type, vars: &HashMap<String, Type>) -> Type {
    match t {
        Type::Qualified(q, inner) => Type::Qualified(*q, Box::new(subst_trait_params(inner, vars))),
        Type::Named(n, args) if args.is_empty() => {
            vars.get(n).cloned().unwrap_or_else(|| Type::Named(n.clone(), Vec::new()))
        }
        Type::Named(n, args) => {
            Type::Named(n.clone(), args.iter().map(|a| subst_trait_params(a, vars)).collect())
        }
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|a| subst_trait_params(a, vars)).collect()),
        Type::Fn(ps, r, conventions) => Type::Fn(
            ps.iter().map(|a| subst_trait_params(a, vars)).collect(),
            Box::new(subst_trait_params(r, vars)),
            conventions.clone(),
        ),
    }
}

fn expected_method_type(t: &Type, im: &ImplDef, trait_params: &HashMap<String, Type>) -> Type {
    subst_trait_params(&subst_self(t, &impl_self_type(im)), trait_params)
}

fn ret_type(ret: &Option<Type>, im: &ImplDef, trait_params: &HashMap<String, Type>) -> Type {
    ret.as_ref()
        .map(|t| expected_method_type(t, im, trait_params))
        .unwrap_or_else(nil_type)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.bytes().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.bytes().enumerate() {
            cur[j + 1] = if ca == cb {
                prev[j]
            } else {
                1 + prev[j].min(prev[j + 1]).min(cur[j])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn closest_method<'a>(name: &str, methods: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    methods
        .map(|cand| (edit_distance(name, cand), cand))
        .filter(|(dist, _)| *dist <= 6)
        .min_by_key(|(dist, cand)| (*dist, cand.len()))
        .map(|(_, cand)| cand)
}

fn validate_trait_impl(im: &ImplDef, methods: &[MethodSig], trait_params: &[String]) -> Vec<String> {
    let Some(trait_name) = &im.trait_name else { return Vec::new() };
    let trait_bare = bare(trait_name);
    let type_bare = bare(&im.type_name);
    let known: HashMap<&str, &MethodSig> = methods.iter().map(|m| (m.name.as_str(), m)).collect();
    let trait_param_map: HashMap<String, Type> = trait_params
        .iter()
        .cloned()
        .zip(im.trait_args.iter().cloned())
        .collect();
    let mut provided: HashSet<&str> = HashSet::new();
    let mut diags = Vec::new();

    for method in &im.methods {
        let name = method.name.as_str();
        let Some(sig) = known.get(name) else {
            let suggestion = closest_method(name, known.keys().copied())
                .map(|m| format!("; did you mean `{m}`?"))
                .unwrap_or_default();
            diags.push(format!(
                "`{name}` is not a `{trait_bare}` method in `impl {trait_bare} for {type_bare}`{suggestion}"
            ));
            continue;
        };
        provided.insert(name);

        if method.params.len() != sig.params.len() {
            diags.push(format!(
                "`impl {trait_bare} for {type_bare}` method `{name}` has {} parameter(s), but the trait requires {}",
                method.params.len(),
                sig.params.len()
            ));
            continue;
        }

        for (idx, (actual, expected)) in method.params.iter().zip(&sig.params).enumerate() {
            if actual.convention != expected.convention {
                diags.push(format!(
                    "`impl {trait_bare} for {type_bare}` method `{name}` parameter {} has convention `{:?}`, but the trait requires `{:?}`",
                    idx + 1,
                    actual.convention,
                    expected.convention
                ));
            }
            if let (Some(actual_ty), Some(expected_ty)) = (&actual.ty, &expected.ty) {
                let actual_ty = expected_method_type(actual_ty, im, &trait_param_map);
                let expected_ty = expected_method_type(expected_ty, im, &trait_param_map);
                if actual_ty != expected_ty {
                    diags.push(format!(
                        "`impl {trait_bare} for {type_bare}` method `{name}` parameter {} has type `{}`, but the trait requires `{}`",
                        idx + 1,
                        display_type(&actual_ty),
                        display_type(&expected_ty)
                    ));
                }
            }
        }

        let actual_ret = ret_type(&method.ret, im, &trait_param_map);
        let expected_ret = ret_type(&sig.ret, im, &trait_param_map);
        if actual_ret != expected_ret {
            diags.push(format!(
                "`impl {trait_bare} for {type_bare}` method `{name}` returns `{}`, but the trait requires `{}`",
                display_type(&actual_ret),
                display_type(&expected_ret)
            ));
        }
    }

    for sig in methods {
        if sig.default.is_none() && !provided.contains(sig.name.as_str()) {
            diags.push(format!(
                "`impl {trait_bare} for {type_bare}` is missing required method `{}`",
                sig.name
            ));
        }
    }

    diags
}

/// Close a direct-supertrait map under transitivity: each trait maps to ALL of
/// its supertraits (direct and inherited), so a `where a: Ord` bound knows it
/// also provides `Eq`, `PartialOrd`, and `PartialEq`.
fn transitive_supertraits(direct: &HashMap<String, Vec<String>>) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for name in direct.keys() {
        let mut seen: Vec<String> = Vec::new();
        let mut stack: Vec<String> = direct.get(name).cloned().unwrap_or_default();
        while let Some(s) = stack.pop() {
            if seen.contains(&s) {
                continue;
            }
            seen.push(s.clone());
            if let Some(more) = direct.get(&s) {
                stack.extend(more.iter().cloned());
            }
        }
        out.insert(name.clone(), seen);
    }
    out
}

fn synthesize_anon_union_impls(
    items: &[Item],
    trait_method_list: &HashMap<String, Vec<MethodSig>>,
) -> Vec<ImplDef> {
    let mut heads = HashMap::new();
    collect_anon_union_heads(items, &mut heads);
    if heads.is_empty() {
        return Vec::new();
    }

    let existing: HashSet<(String, String)> = items
        .iter()
        .filter_map(|item| match item {
            Item::Impl(im) => im.trait_name.as_ref().map(|tr| (tr.clone(), im.type_name.clone())),
            _ => None,
        })
        .collect();
    let reflect_runtime = has_function(items, "reflect.reflect_one")
        && has_variant(items, "reflect.MVariant");
    let has_show = trait_declares_method(trait_method_list, "Show", "show");
    let has_reflect = trait_declares_method(trait_method_list, "Reflect", "reflect");
    let has_partial_eq = trait_declares_method(trait_method_list, "PartialEq", "eq");
    let mut heads: Vec<(String, usize)> = heads.into_iter().collect();
    heads.sort_by(|a, b| a.0.cmp(&b.0));

    let mut impls = Vec::new();
    for (name, arity) in heads {
        let Some(variants) = crate::typeck::anon_union_synthetic_variants(&name) else {
            continue;
        };
        if has_show
            && !existing.contains(&("Show".to_string(), name.clone()))
        {
            impls.push(anon_union_show_impl(&name, &variants, arity));
        }
        if reflect_runtime
            && has_reflect
            && !existing.contains(&("Reflect".to_string(), name.clone()))
        {
            impls.push(anon_union_reflect_impl(&name, &variants, arity));
        }
        if has_partial_eq
            && !existing.contains(&("PartialEq".to_string(), name.clone()))
        {
            impls.push(anon_union_partial_eq_impl(&name, &variants, arity));
        }
    }
    impls
}

fn trait_declares_method(
    trait_method_list: &HashMap<String, Vec<MethodSig>>,
    trait_name: &str,
    method_name: &str,
) -> bool {
    trait_method_list
        .get(trait_name)
        .is_some_and(|methods| methods.iter().any(|method| method.name == method_name))
}

fn collect_anon_union_heads(items: &[Item], out: &mut HashMap<String, usize>) {
    for item in items {
        match item {
            Item::Function(f) => collect_anon_union_heads_function(f, out),
            Item::Trait(t) => {
                for method in &t.methods {
                    collect_anon_union_heads_method_sig(method, out);
                }
            }
            Item::Impl(im) => {
                for ty in &im.trait_args {
                    collect_anon_union_heads_type(ty, out);
                }
                for ty in &im.target_args {
                    collect_anon_union_heads_type(ty, out);
                }
                for (_, _, args) in &im.bounds {
                    for ty in args {
                        collect_anon_union_heads_type(ty, out);
                    }
                }
                for method in &im.methods {
                    collect_anon_union_heads_function(method, out);
                }
            }
            Item::Type(t) => {
                for variant in &t.variants {
                    for field in &variant.fields {
                        collect_anon_union_heads_type(field, out);
                    }
                }
            }
            Item::Const { value, .. } => collect_anon_union_heads_expr(value, out),
            Item::TypeAlias { ty, .. } => collect_anon_union_heads_type(ty, out),
            Item::Comptime(block) => collect_anon_union_heads_block(block, out),
        }
    }
}

fn collect_anon_union_heads_function(f: &Function, out: &mut HashMap<String, usize>) {
    for param in &f.params {
        if let Some(ty) = &param.ty {
            collect_anon_union_heads_type(ty, out);
        }
        if let Some(default) = &param.default {
            collect_anon_union_heads_expr(default, out);
        }
    }
    if let Some(ret) = &f.ret {
        collect_anon_union_heads_type(ret, out);
    }
    for (_, _, args) in &f.bounds {
        for ty in args {
            collect_anon_union_heads_type(ty, out);
        }
    }
    collect_anon_union_heads_block(&f.body, out);
}

fn collect_anon_union_heads_method_sig(ms: &MethodSig, out: &mut HashMap<String, usize>) {
    for param in &ms.params {
        if let Some(ty) = &param.ty {
            collect_anon_union_heads_type(ty, out);
        }
    }
    if let Some(ret) = &ms.ret {
        collect_anon_union_heads_type(ret, out);
    }
    if let Some(default) = &ms.default {
        collect_anon_union_heads_block(default, out);
    }
}

fn collect_anon_union_heads_type(ty: &Type, out: &mut HashMap<String, usize>) {
    match ty {
        Type::Named(name, args) => {
            if let Some(variants) = crate::typeck::anon_union_synthetic_variants(name) {
                let arity = variants.iter().map(|(_, arity)| *arity).sum();
                if arity == args.len() {
                    out.insert(name.clone(), arity);
                }
            }
            for arg in args {
                collect_anon_union_heads_type(arg, out);
            }
        }
        Type::Tuple(items) => {
            for item in items {
                collect_anon_union_heads_type(item, out);
            }
        }
        Type::Fn(params, ret, _) => {
            for param in params {
                collect_anon_union_heads_type(param, out);
            }
            collect_anon_union_heads_type(ret, out);
        }
        Type::Qualified(_, inner) => collect_anon_union_heads_type(inner, out),
    }
}

fn collect_anon_union_heads_block(block: &Block, out: &mut HashMap<String, usize>) {
    if let Some(region) = &block.region {
        if let Some(ty) = &region.ty {
            collect_anon_union_heads_type(ty, out);
        }
    }
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { ty, value, .. } => {
                if let Some(ty) = ty {
                    collect_anon_union_heads_type(ty, out);
                }
                collect_anon_union_heads_expr(value, out);
            }
            Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => collect_anon_union_heads_expr(value, out),
            Stmt::Return(Some(value)) => collect_anon_union_heads_expr(value, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_anon_union_heads_expr(expr: &Expr, out: &mut HashMap<String, usize>) {
    match expr {
        Expr::List(items) | Expr::Tuple(items) | Expr::Ctor { args: items, .. }
        | Expr::AnonCtor { args: items, .. } => {
            for item in items {
                collect_anon_union_heads_expr(item, out);
            }
        }
        Expr::Call { args, .. } | Expr::MethodCall { args, .. } => {
            for arg in args {
                collect_anon_union_heads_expr(arg, out);
            }
            if let Expr::MethodCall { receiver, .. } = expr {
                collect_anon_union_heads_expr(receiver, out);
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, arg) in args {
                collect_anon_union_heads_expr(arg, out);
            }
        }
        Expr::Apply { func, args } => {
            collect_anon_union_heads_expr(func, out);
            for arg in args {
                collect_anon_union_heads_expr(arg, out);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::Field { base: expr, .. } => {
            collect_anon_union_heads_expr(expr, out);
        }
        Expr::As { expr, ty } => {
            collect_anon_union_heads_expr(expr, out);
            collect_anon_union_heads_type(ty, out);
        }
        Expr::Lambda { params, body, ret } => {
            for param in params {
                if let Some(ty) = &param.ty {
                    collect_anon_union_heads_type(ty, out);
                }
                if let Some(default) = &param.default {
                    collect_anon_union_heads_expr(default, out);
                }
            }
            if let Some(ret) = ret {
                collect_anon_union_heads_type(ret, out);
            }
            collect_anon_union_heads_block(body, out);
        }
        Expr::RecordUpdate { base, fields, .. } => {
            collect_anon_union_heads_expr(base, out);
            for (_, value) in fields {
                collect_anon_union_heads_expr(value, out);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                collect_anon_union_heads_expr(value, out);
            }
            if let Some(spread) = spread {
                collect_anon_union_heads_expr(spread, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } | Expr::Range { lo: lhs, hi: rhs, .. } => {
            collect_anon_union_heads_expr(lhs, out);
            collect_anon_union_heads_expr(rhs, out);
        }
        Expr::If { cond, then_block, else_block } => {
            collect_anon_union_heads_expr(cond, out);
            collect_anon_union_heads_block(then_block, out);
            if let Some(block) = else_block {
                collect_anon_union_heads_block(block, out);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_anon_union_heads_expr(scrutinee, out);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_anon_union_heads_expr(guard, out);
                }
                collect_anon_union_heads_expr(&arm.body, out);
            }
        }
        Expr::Block(block) => collect_anon_union_heads_block(block, out),
        Expr::While { cond, body } => {
            collect_anon_union_heads_expr(cond, out);
            collect_anon_union_heads_block(body, out);
        }
        Expr::For { iter, body, .. } => {
            collect_anon_union_heads_expr(iter, out);
            collect_anon_union_heads_block(body, out);
        }
        Expr::Index { base, index } => {
            collect_anon_union_heads_expr(base, out);
            collect_anon_union_heads_expr(index, out);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            collect_anon_union_heads_expr(scrutinee, out);
            collect_anon_union_heads_block(body, out);
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }
}

fn has_function(items: &[Item], name: &str) -> bool {
    items.iter().any(|item| matches!(item, Item::Function(f) if f.name == name))
}

fn has_variant(items: &[Item], name: &str) -> bool {
    items.iter().any(|item| {
        matches!(
            item,
            Item::Type(t) if t.variants.iter().any(|variant| variant.name == name)
        )
    })
}

fn anon_union_show_impl(name: &str, variants: &[(String, usize)], arity: usize) -> ImplDef {
    ImplDef {
        origin: ImplOrigin::CompilerGenerated,
        trait_name: Some("Show".to_string()),
        trait_args: Vec::new(),
        type_name: name.to_string(),
        target_args: anon_union_target_args(arity),
        bounds: anon_union_bounds(arity, "Show"),
        methods: vec![Function {
            public: true,
            comptime_only: false,
            name: "show".to_string(),
            params: vec![self_param()],
            ret: Some(named_type("String")),
            body: expr_block(Expr::Match {
                scrutinee: Box::new(Expr::Var("self".to_string())),
                arms: anon_union_show_arms(variants),
            }),
            bounds: Vec::new(),
            is_gen: false,
            is_async: false,
        }],
    }
}

fn anon_union_reflect_impl(name: &str, variants: &[(String, usize)], arity: usize) -> ImplDef {
    ImplDef {
        origin: ImplOrigin::CompilerGenerated,
        trait_name: Some("Reflect".to_string()),
        trait_args: Vec::new(),
        type_name: name.to_string(),
        target_args: anon_union_target_args(arity),
        bounds: anon_union_bounds(arity, "Reflect"),
        methods: vec![Function {
            public: true,
            comptime_only: false,
            name: "reflect".to_string(),
            params: vec![self_param()],
            ret: Some(Type::Named("reflect.Mirror".to_string(), Vec::new())),
            body: expr_block(Expr::Match {
                scrutinee: Box::new(Expr::Var("self".to_string())),
                arms: anon_union_reflect_arms(variants),
            }),
            bounds: Vec::new(),
            is_gen: false,
            is_async: false,
        }],
    }
}

fn anon_union_partial_eq_impl(name: &str, variants: &[(String, usize)], arity: usize) -> ImplDef {
    ImplDef {
        origin: ImplOrigin::CompilerGenerated,
        trait_name: Some("PartialEq".to_string()),
        trait_args: Vec::new(),
        type_name: name.to_string(),
        target_args: anon_union_target_args(arity),
        bounds: anon_union_bounds(arity, "PartialEq"),
        methods: vec![Function {
            public: true,
            comptime_only: false,
            name: "eq".to_string(),
            params: vec![
                self_param(),
                Param {
                    name: "other".to_string(),
                    ty: Some(Type::Named("Self".to_string(), Vec::new())),
                    convention: Convention::Let,
                    default: None,
                },
            ],
            ret: Some(named_type("Bool")),
            body: expr_block(Expr::Match {
                scrutinee: Box::new(Expr::Var("self".to_string())),
                arms: anon_union_eq_arms(variants),
            }),
            bounds: Vec::new(),
            is_gen: false,
            is_async: false,
        }],
    }
}

fn anon_union_target_args(arity: usize) -> Vec<Type> {
    (0..arity).map(|idx| named_type(&format!("t{idx}"))).collect()
}

fn anon_union_bounds(arity: usize, trait_name: &str) -> Vec<(String, String, Vec<Type>)> {
    (0..arity)
        .map(|idx| (format!("t{idx}"), trait_name.to_string(), Vec::new()))
        .collect()
}

fn self_param() -> Param {
    Param {
        name: "self".to_string(),
        ty: None,
        convention: Convention::Let,
        default: None,
    }
}

fn expr_block(expr: Expr) -> Block {
    Block {
        stmts: vec![Stmt::Expr(expr)],
        lines: vec![u32::MAX],
        region: None,
    }
}

fn anon_union_show_arms(variants: &[(String, usize)]) -> Vec<MatchArm> {
    let mut offset = 0usize;
    let mut arms = Vec::new();
    for (tag, arity) in variants {
        let names = payload_names("u", offset, *arity);
        let body = if names.is_empty() {
            Expr::Str(format!(".{tag}"))
        } else {
            let mut parts = vec![Expr::Str(format!(".{tag}("))];
            for (index, name) in names.iter().enumerate() {
                if index > 0 {
                    parts.push(Expr::Str(", ".to_string()));
                }
                parts.push(Expr::Call {
                    name: "show".to_string(),
                    args: vec![Expr::Var(name.clone())],
                });
            }
            parts.push(Expr::Str(")".to_string()));
            concat_expr(parts)
        };
        arms.push(MatchArm {
            line: u32::MAX,
            pattern: Pattern::AnonCtor {
                tag: tag.clone(),
                args: names.into_iter().map(Pattern::Var).collect(),
            },
            guard: None,
            body,
        });
        offset += arity;
    }
    arms
}

fn anon_union_reflect_arms(variants: &[(String, usize)]) -> Vec<MatchArm> {
    let mut offset = 0usize;
    let mut arms = Vec::new();
    for (tag, arity) in variants {
        let names = payload_names("u", offset, *arity);
        let payload = names
            .iter()
            .map(|name| Expr::Call {
                name: "reflect.reflect_one".to_string(),
                args: vec![Expr::Var(name.clone())],
            })
            .collect();
        arms.push(MatchArm {
            line: u32::MAX,
            pattern: Pattern::AnonCtor {
                tag: tag.clone(),
                args: names.into_iter().map(Pattern::Var).collect(),
            },
            guard: None,
            body: Expr::Ctor {
                name: "reflect.MVariant".to_string(),
                args: vec![Expr::Str(String::new()), Expr::Str(format!(".{tag}")), Expr::List(payload)],
            },
        });
        offset += arity;
    }
    arms
}

fn anon_union_eq_arms(variants: &[(String, usize)]) -> Vec<MatchArm> {
    let mut offset = 0usize;
    let mut arms = Vec::new();
    for (tag, arity) in variants {
        let left = payload_names("l", offset, *arity);
        let right = payload_names("r", offset, *arity);
        let same_tag = if left.is_empty() {
            Expr::Bool(true)
        } else {
            let checks = left
                .iter()
                .zip(&right)
                .map(|(l, r)| Expr::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(Expr::Var(l.clone())),
                    rhs: Box::new(Expr::Var(r.clone())),
                })
                .collect();
            and_expr(checks)
        };
        let other_match = Expr::Match {
            scrutinee: Box::new(Expr::Var("other".to_string())),
            arms: vec![
                MatchArm {
                    line: u32::MAX,
                    pattern: Pattern::AnonCtor {
                        tag: tag.clone(),
                        args: right.into_iter().map(Pattern::Var).collect(),
                    },
                    guard: None,
                    body: same_tag,
                },
                MatchArm {
                    line: u32::MAX,
                    pattern: Pattern::Wildcard,
                    guard: None,
                    body: Expr::Bool(false),
                },
            ],
        };
        arms.push(MatchArm {
            line: u32::MAX,
            pattern: Pattern::AnonCtor {
                tag: tag.clone(),
                args: left.into_iter().map(Pattern::Var).collect(),
            },
            guard: None,
            body: other_match,
        });
        offset += arity;
    }
    arms
}

fn payload_names(prefix: &str, offset: usize, arity: usize) -> Vec<String> {
    (0..arity).map(|idx| format!("{prefix}{}", offset + idx)).collect()
}

fn concat_expr(mut parts: Vec<Expr>) -> Expr {
    let first = parts.remove(0);
    parts.into_iter().fold(first, |lhs, rhs| Expr::Binary {
        op: BinOp::Add,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    })
}

fn and_expr(mut parts: Vec<Expr>) -> Expr {
    let first = parts.remove(0);
    parts.into_iter().fold(first, |lhs, rhs| Expr::Binary {
        op: BinOp::And,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    })
}

/// The dispatch pass's lexical environment: each bound name's (known) head type,
/// plus the set of ALL names bound as locals (params, `let`s, `for`/pattern
/// binders, lambda params) — including those whose type is a function or is
/// otherwise unknown. The type map drives receiver typing; the local set answers
/// "is this call name a bound local?", so a call on a parameter that happens to
/// share a trait method's name (`less`/`greater`/`show`, e.g. a comparator
/// parameter) is invoked as the first-class function it is, never rewritten to a
/// trait dispatch.
#[derive(Clone)]
struct Scope<T> {
    types: HashMap<String, T>,
    locals: HashSet<String>,
    /// (RFC-0043) Names bound to a *mutable* place — a `var` parameter or `var`
    /// let. A statement-position mutator method call (`xs.push(1)`) writes back
    /// only when its receiver's base is one of these. Any other binding form for
    /// the same name (a `let`, a loop/pattern binder) shadows it out.
    mutables: HashSet<String>,
}

impl<T> Default for Scope<T> {
    fn default() -> Self {
        Self {
            types: HashMap::new(),
            locals: HashSet::new(),
            mutables: HashSet::new(),
        }
    }
}

impl<T> Scope<T> {
    fn new() -> Self {
        Scope::default()
    }

    /// The type evidence bound for `name`, if known.
    fn get(&self, name: &str) -> Option<&T> {
        self.types.get(name)
    }

    /// Bind `name` to type evidence `ty` — and record it as a local. Immutable by
    /// default (a `let`); use [`Scope::insert_mut`] for a `var`.
    fn insert(&mut self, name: String, ty: T) {
        self.locals.insert(name.clone());
        self.mutables.remove(&name);
        self.types.insert(name, ty);
    }

    /// Bind `name` to type evidence `ty` as a MUTABLE place (a `var`) — a write-back
    /// target for a statement-position mutator method call.
    fn insert_mut(&mut self, name: String, ty: T) {
        self.locals.insert(name.clone());
        self.mutables.insert(name.clone());
        self.types.insert(name, ty);
    }

    /// Record `name` as a bound local whose type is unknown here (a function-
    /// typed parameter, an untyped `let`), clearing any stale type binding. Also
    /// clears mutability — an untyped binding form shadows any outer mutable.
    fn bind_local(&mut self, name: &str) {
        self.locals.insert(name.to_string());
        self.mutables.remove(name);
        self.types.remove(name);
    }

    /// Record `name` as a bound local that is a mutable place whose type is
    /// unknown here (an untyped `var`).
    fn bind_local_mut(&mut self, name: &str) {
        self.locals.insert(name.to_string());
        self.mutables.insert(name.to_string());
        self.types.remove(name);
    }

    /// Whether `name` is bound as a local in this scope (so a call on it is a
    /// first-class function invocation, not a trait-method dispatch).
    fn is_local(&self, name: &str) -> bool {
        self.locals.contains(name)
    }
}

fn merge_refined_outer_ast_types(parent: &mut Scope<Type>, child: &Scope<Type>) {
    for (name, refined) in &child.types {
        refine_ast_scope_type(parent, name, refined);
    }
}

fn refine_ast_scope_type(scope: &mut Scope<Type>, name: &str, refined: &Type) {
    let Some(existing) = scope.types.get(name) else { return };
    let same_head = match (existing.unqualified(), refined.unqualified()) {
        (Type::Named(left, _), Type::Named(right, _)) => left == right,
        (Type::Tuple(left), Type::Tuple(right)) => left.len() == right.len(),
        (Type::Fn(left, _, lc), Type::Fn(right, _, rc)) => {
            left.len() == right.len() && lc == rc
        }
        _ => false,
    };
    let carries_arguments = match refined.unqualified() {
        Type::Named(_, args) => !args.is_empty(),
        Type::Tuple(_) | Type::Fn(_, _, _) => true,
        Type::Qualified(_, _) => unreachable!("unqualified strips qualifiers"),
    };
    if existing != refined && same_head && carries_arguments {
        scope.types.insert(name.to_string(), refined.clone());
    }
}

fn seed_typed_params(params: &[Param], scope: &mut Scope<Type>) {
    for param in params {
        let mutable = param.convention == Convention::Var;
        match (&param.ty, mutable) {
            (Some(ty), true) => scope.insert_mut(param.name.clone(), ty.clone()),
            (Some(ty), false) => scope.insert(param.name.clone(), ty.clone()),
            (None, true) => scope.bind_local_mut(&param.name),
            (None, false) => scope.bind_local(&param.name),
        }
    }
}

fn iterable_item_type(iter_type: &Type) -> Option<Type> {
    match iter_type.unqualified() {
        Type::Named(name, args) if matches!(name.as_str(), "List" | "Set") => {
            args.first().cloned()
        }
        Type::Named(name, args) if name == "Dict" && args.len() == 2 => {
            Some(Type::Tuple(args.clone()))
        }
        _ => None,
    }
}

/// Bind every variable in a refutable pattern from its structured scrutinee
/// type. Constructor generics are substituted once and then propagated through
/// nested patterns; builtin Option/Result payloads follow the same path. An
/// unknown type still records names as locals, preventing accidental dispatch
/// through a same-named free function.
fn bind_typed_pattern(
    pat: &Pattern,
    ctor_infos: &HashMap<String, CtorInfo>,
    expected: Option<&Type>,
    scope: &mut Scope<Type>,
) {
    match pat {
        Pattern::Var(name) if name != "_" => match expected {
            Some(ty) => scope.insert(name.clone(), ty.clone()),
            None => scope.bind_local(name),
        },
        Pattern::Tuple(parts) => {
            let slots = match expected.map(Type::unqualified) {
                Some(Type::Tuple(slots)) => Some(slots.as_slice()),
                Some(Type::Named(name, slots)) if name.starts_with("Tuple") => {
                    Some(slots.as_slice())
                }
                _ => None,
            };
            for (index, part) in parts.iter().enumerate() {
                bind_typed_pattern(
                    part,
                    ctor_infos,
                    slots.and_then(|items| items.get(index)),
                    scope,
                );
            }
        }
        Pattern::Ctor { name, args } => {
            if let Some(info) = ctor_infos.get(name) {
                let mut subst = HashMap::new();
                if let Some(actual) = expected {
                    let nominal = Type::Named(
                        info.owner.clone(),
                        info.params
                            .iter()
                            .map(|param| named_type(param))
                            .collect(),
                    );
                    let _ = bind_ast_type_vars(&nominal, actual, &mut subst);
                }
                for (index, arg) in args.iter().enumerate() {
                    let field_ty = info
                        .fields
                        .get(index)
                        .map(|field| subst_trait_params(field, &subst));
                    bind_typed_pattern(arg, ctor_infos, field_ty.as_ref(), scope);
                }
                return;
            }

            let payload = match (name.as_str(), expected.map(Type::unqualified)) {
                ("Some", Some(Type::Named(owner, types))) if owner == "Option" => types.first(),
                ("Ok", Some(Type::Named(owner, types))) if owner == "Result" => types.first(),
                ("Err", Some(Type::Named(owner, types))) if owner == "Result" => types.get(1),
                _ => None,
            };
            for (index, arg) in args.iter().enumerate() {
                bind_typed_pattern(
                    arg,
                    ctor_infos,
                    (index == 0).then_some(payload).flatten(),
                    scope,
                );
            }
        }
        Pattern::AnonCtor { tag, args } => {
            let mut fields: Option<Vec<Type>> = None;
            if let Some(Type::Named(name, types)) = expected.map(Type::unqualified) {
                if let Some(variants) = crate::typeck::anon_union_synthetic_variants(name) {
                    let mut offset = 0usize;
                    for (variant, arity) in variants {
                        let end = offset.saturating_add(arity);
                        if variant == *tag && arity == args.len() && end <= types.len() {
                            fields = Some(types[offset..end].to_vec());
                            break;
                        }
                        offset = end;
                    }
                }
            }
            for (index, arg) in args.iter().enumerate() {
                bind_typed_pattern(
                    arg,
                    ctor_infos,
                    fields.as_ref().and_then(|items| items.get(index)),
                    scope,
                );
            }
        }
        Pattern::List { elems, rest } => {
            let elem_ty = match expected.map(Type::unqualified) {
                Some(Type::Named(name, args)) if name == "List" => args.first(),
                _ => None,
            };
            for elem in elems {
                bind_typed_pattern(elem, ctor_infos, elem_ty, scope);
            }
            if let Some(Some(name)) = rest {
                match expected {
                    Some(ty) => scope.insert(name.clone(), ty.clone()),
                    None => scope.bind_local(name),
                }
            }
        }
        Pattern::Or(alternatives) => {
            if let Some(first) = alternatives.first() {
                bind_typed_pattern(first, ctor_infos, expected, scope);
            }
        }
        Pattern::Wildcard
        | Pattern::Var(_)
        | Pattern::Int(_)
        | Pattern::Str(_)
        | Pattern::Bool(_)
        | Pattern::Duration(_)
        | Pattern::IntRange { .. } => {}
    }
}

struct Ctx<'a> {
    trait_methods: &'a HashMap<String, Vec<TraitMethodInfo>>,
    inherent_methods: &'a HashSet<String>,
    /// trait -> its transitive supertraits, so a `where a: Ord` bound discharges
    /// the methods of `Eq`/`PartialOrd`/`PartialEq` too.
    supertraits: &'a HashMap<String, Vec<String>>,
    trait_impl_table: &'a TraitImplTable,
    inherent_impl_table: &'a HashMap<(String, String), String>,
    ctor_infos: &'a HashMap<String, CtorInfo>,
    /// Function -> (param types, return type), for recovering a generic call's
    /// concrete result type (e.g. the element of `list.at(xs, i)`).
    fn_sigs: &'a HashMap<String, FnSig>,
    /// Record type name -> its named field types (for typing `x.field`).
    record_fields: &'a HashMap<String, Vec<(String, Type)>>,
    /// Plain (non-method) function names: a trait-method call that ALSO names
    /// a free function may legitimately resolve to it, so it is never a
    /// missing-impl error.
    free_fns: &'a HashSet<String>,
    /// Public receiver-first functions whose first parameter is owned by the
    /// function's module. These are the RFC-0050 Part 1 owner methods.
    owner_methods: &'a HashSet<String>,
    /// Trait-method calls whose receiver type is KNOWN but has no impl —
    /// surfaced by the type checker as a clean "T does not implement Trait"
    /// instead of a post-lowering unknown-function error.
    missing_impls: &'a std::cell::RefCell<Vec<String>>,
    /// Self-less impl methods: `Type.name(args)` statics.
    statics: &'a HashMap<(String, String), String>,
    /// Resolved functions with at least one `var` parameter. Their ordinary
    /// result may be discarded because write-back is the statement's effect.
    var_calls: &'a HashSet<String>,
    /// (RFC-0043) Resolved function name -> whether it returns `Nil`/nothing.
    /// A statement-position non-mutator method call whose callee returns a
    /// non-Nil value is a discard error (the RFC's Failure-2 fix).
    returns_nil: &'a HashMap<String, bool>,
    /// (RFC-0043) Diagnostics for a discarded non-Nil method result. Kept
    /// SEPARATE from `missing_impls`: the quiet pre-mono pass discards its
    /// `missing_impls` (unresolved calls are re-found post-mono), but a discard
    /// error is a real, final diagnostic surfaced through `lower_checked`.
    discard_errors: &'a std::cell::RefCell<Vec<String>>,
    /// typeck's resolved types — receiver typing for method resolution.
    table: &'a crate::typeck::TypeTable,
    /// The current function's type-variable bounds (var -> trait names), so a
    /// comparison operator on a type-variable operand desugars to a trait call
    /// ONLY when that variable is bound by the relevant comparison trait — an
    /// UNbounded generic `==` keeps the native structural comparison.
    bound_traits: std::cell::RefCell<HashMap<String, Vec<String>>>,
    /// The function currently being rewritten, for diagnostics emitted before
    /// the ordinary type checker can attach its own `at_loc` prefix.
    current_func: std::cell::RefCell<String>,
    /// The enclosing statement's source line while rewriting an expression.
    current_line: std::cell::Cell<u32>,
}

fn table_ast_type(table: &crate::typeck::TypeTable, e: &Expr) -> Option<Type> {
    table.type_of(e).and_then(crate::typeck::ty_to_ast)
}

fn named_type(name: &str) -> Type {
    Type::Named(name.to_string(), Vec::new())
}

/// Recover an expression type from language declarations and local syntax.
/// This is the empty-table half of dispatch: the loud pass normally reads
/// typeck's table, while the quiet pre-pass must bootstrap enough information
/// to make the module checkable. Types stay structured throughout; only the
/// final impl-table lookup encodes a name.
fn declared_expr_type(
    e: &Expr,
    fn_sigs: &HashMap<String, FnSig>,
    type_of: &dyn Fn(&Expr) -> Option<Type>,
) -> Option<Type> {
    match e {
        Expr::Index { base, .. } => match type_of(base)?.unqualified() {
            Type::Named(name, args) if name == "List" && args.len() == 1 => {
                Some(args[0].clone())
            }
            _ => None,
        },
        Expr::Try(inner) => match type_of(inner)?.unqualified() {
            Type::Named(name, args)
                if matches!(name.as_str(), "Option" | "Result") && !args.is_empty() =>
            {
                Some(args[0].clone())
            }
            _ => None,
        },
        Expr::Call { name, args } => {
            let (params, ret, _) = fn_sigs.get(name)?;
            let mut binds = HashMap::new();
            for (param, arg) in params.iter().zip(args) {
                let (Some(param), Some(arg_ty)) = (param, type_of(arg)) else {
                    continue;
                };
                let _ = bind_ast_type_vars(param, &arg_ty, &mut binds);
            }
            // Keep unresolved variables in place. The nominal declaration is
            // still authoritative enough to resolve owner methods (`dict.new()`
            // is `Dict(k, v)` before its insert arguments constrain k/v), while
            // typeck remains responsible for proving those variables concrete.
            Some(subst_trait_params(ret, &binds))
        }
        _ => None,
    }
}

fn ctor_info_for_owner<'a>(
    infos: &'a HashMap<String, CtorInfo>,
    owner: &str,
) -> Option<&'a CtorInfo> {
    infos
        .get(owner)
        .filter(|info| info.owner == owner)
        .or_else(|| infos.values().find(|info| info.owner == owner))
}

fn inferred_nominal_type(
    info: &CtorInfo,
    fields: impl Iterator<Item = (Type, Type)>,
) -> Type {
    if info.params.is_empty() {
        return named_type(&info.owner);
    }
    let mut binds = HashMap::new();
    for (declared, actual) in fields {
        let _ = bind_ast_type_vars(&declared, &actual, &mut binds);
    }
    let args = info
        .params
        .iter()
        .map(|param| binds.get(param).cloned())
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default();
    Type::Named(info.owner.clone(), args)
}

fn local_expr_type(
    e: &Expr,
    scope: &Scope<Type>,
    ctor_infos: &HashMap<String, CtorInfo>,
    fn_sigs: &HashMap<String, FnSig>,
    record_fields: &HashMap<String, Vec<(String, Type)>>,
    type_of: &dyn Fn(&Expr) -> Option<Type>,
) -> Option<Type> {
    match e {
        Expr::Int(_) => Some(named_type("Int")),
        Expr::Float(_) => Some(named_type("Float")),
        Expr::Duration(_) => Some(named_type("Duration")),
        Expr::Str(_) => Some(named_type("String")),
        Expr::Bool(_) => Some(named_type("Bool")),
        Expr::Var(name) => scope.get(name).cloned().or_else(|| {
            let (params, ret, conventions) = fn_sigs.get(name)?;
            Some(Type::Fn(
                params.iter().cloned().collect::<Option<Vec<_>>>()?,
                Box::new(ret.clone()),
                conventions.clone(),
            ))
        }),
        Expr::Field { base, field } => {
            let base_ty = type_of(base)?;
            let Type::Named(owner, args) = base_ty.unqualified() else {
                return None;
            };
            let (_, field_ty) = record_fields.get(owner)?.iter().find(|(name, _)| name == field)?;
            let Some(info) = ctor_info_for_owner(ctor_infos, owner) else {
                return Some(field_ty.clone());
            };
            let subst = info
                .params
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect();
            Some(subst_trait_params(field_ty, &subst))
        }
        Expr::Ctor { name, args } => {
            let info = ctor_infos.get(name)?;
            let inferred = info
                .fields
                .iter()
                .zip(args)
                .filter_map(|(declared, arg)| Some((declared.clone(), type_of(arg)?)));
            Some(inferred_nominal_type(info, inferred))
        }
        Expr::Record { name, fields, .. } => {
            let info = ctor_infos
                .get(name)
                .or_else(|| ctor_info_for_owner(ctor_infos, name))?;
            let declared = record_fields.get(&info.owner)?;
            let inferred = fields.iter().filter_map(|(field, value)| {
                let (_, declared_ty) = declared.iter().find(|(name, _)| name == field)?;
                Some((declared_ty.clone(), type_of(value)?))
            });
            Some(inferred_nominal_type(info, inferred))
        }
        Expr::Call { .. } | Expr::Index { .. } | Expr::Try(_) => {
            declared_expr_type(e, fn_sigs, type_of)
        }
        Expr::Apply { func, .. } => match type_of(func)?.unqualified() {
            Type::Fn(_, ret, _) => Some((**ret).clone()),
            _ => None,
        },
        Expr::RecordUpdate { base, .. } => type_of(base),
        Expr::As { ty, .. } => Some(ty.clone()),
        Expr::Unary { op, expr } => match op {
            UnOp::Not => Some(named_type("Bool")),
            UnOp::Neg | UnOp::BitNot | UnOp::Move | UnOp::Await => type_of(expr),
        },
        Expr::Binary { op, lhs, rhs } => match op {
            BinOp::Eq
            | BinOp::NotEq
            | BinOp::Lt
            | BinOp::LtEq
            | BinOp::Gt
            | BinOp::GtEq
            | BinOp::And
            | BinOp::Or => Some(named_type("Bool")),
            BinOp::Concat => Some(named_type("String")),
            BinOp::Coalesce => type_of(rhs),
            _ => type_of(lhs),
        },
        Expr::List(items) => Some(Type::Named(
            "List".to_string(),
            items.first().and_then(type_of).into_iter().collect(),
        )),
        Expr::Tuple(items) => match items.iter().map(type_of).collect::<Option<Vec<_>>>() {
            Some(types) => Some(Type::Tuple(types)),
            None => Some(named_type(&format!("Tuple{}", items.len()))),
        },
        Expr::Range { .. } => Some(Type::Named("List".to_string(), vec![named_type("Int")])),
        Expr::Lambda { params, body, ret } => {
            let conventions = params.iter().map(|param| param.convention).collect();
            let params = params
                .iter()
                .map(|param| param.ty.clone())
                .collect::<Option<Vec<_>>>()?;
            let ret = ret.clone().or_else(|| match body.stmts.last() {
                Some(Stmt::Expr(expr) | Stmt::Return(Some(expr))) => type_of(expr),
                Some(Stmt::Return(None)) => Some(named_type("Nil")),
                _ => None,
            })?;
            Some(Type::Fn(params, Box::new(ret), conventions))
        }
        Expr::AnonCtor { .. }
        | Expr::LabeledCall { .. }
        | Expr::MethodCall { .. }
        | Expr::If { .. }
        | Expr::Match { .. }
        | Expr::Block(_)
        | Expr::While { .. }
        | Expr::For { .. }
        | Expr::WhileLet { .. }
        | Expr::TaggedLit { .. } => None,
    }
}

fn cap_op_result_type(e: &Expr, type_of: &dyn Fn(&Expr) -> Option<Type>) -> Option<Type> {
    let Expr::Call { name, args } = e else { return None };
    match cap_ops::result_shape(name, args.len())? {
        cap_ops::ResultShape::SameReceiver => args.first().and_then(type_of),
        cap_ops::ResultShape::Nil => Some(named_type("Nil")),
        cap_ops::ResultShape::Int => Some(named_type("Int")),
        cap_ops::ResultShape::String => Some(named_type("String")),
        cap_ops::ResultShape::Bool => Some(named_type("Bool")),
        cap_ops::ResultShape::ListString => {
            Some(Type::Named("List".to_string(), vec![named_type("String")]))
        }
        cap_ops::ResultShape::OptionString => {
            Some(Type::Named("Option".to_string(), vec![named_type("String")]))
        }
        cap_ops::ResultShape::Dir => Some(named_type("Dir")),
        cap_ops::ResultShape::File => Some(named_type("File")),
        cap_ops::ResultShape::Socket => Some(named_type("Socket")),
        cap_ops::ResultShape::OptionSocket => {
            Some(Type::Named("Option".to_string(), vec![named_type("Socket")]))
        }
        cap_ops::ResultShape::Listener => Some(named_type("Listener")),
    }
}

impl Ctx<'_> {
    fn type_ast(&self, e: &Expr, scope: &Scope<Type>) -> Option<Type> {
        table_ast_type(self.table, e)
            // Declaration-driven judgment for call results — what the QUIET pass
            // (empty table) relies on to type a let bound to a generic call
            // (`let above = table.at(i - 1)`), so a method call on it resolves
            // BEFORE annotate needs a fully-resolved module.
            .or_else(|| declared_expr_type(e, self.fn_sigs, &|a| self.type_ast(a, scope)))
            .or_else(|| {
                local_expr_type(
                    e,
                    scope,
                    self.ctor_infos,
                    self.fn_sigs,
                    self.record_fields,
                    &|a| self.type_ast(a, scope),
                )
            })
            // A host capability OP is a BARE intrinsic (`net.deny`, `dir.subtree`),
            // so the QUIET pre-mono pass (which runs with an empty table) cannot
            // type its result from the table — it needs this to resolve a chained
            // method call on a cap-op result (`net.deny(...).only(...)`). The loud
            // pass gets the same fact from the checker's table; this is the empty-
            // table residual. See RFC-0046 step-4 note.
            .or_else(|| cap_op_result_type(e, &|a| self.type_ast(a, scope)))
    }

    fn refine_var_call_args(&self, name: &str, args: &[Expr], scope: &mut Scope<Type>) {
        let Some((params, _, conventions)) = self.fn_sigs.get(name) else { return };
        let mut bindings = HashMap::new();
        for (param, arg) in params.iter().zip(args) {
            let (Some(pattern), Some(actual)) = (param, self.type_ast(arg, scope)) else {
                continue;
            };
            let _ = bind_ast_type_vars(pattern, &actual, &mut bindings);
        }
        for ((param, convention), arg) in params.iter().zip(conventions).zip(args) {
            if *convention != Convention::Var {
                continue;
            }
            let (Some(pattern), Expr::Var(binding)) = (param, arg) else { continue };
            let refined = subst_trait_params(pattern, &bindings);
            refine_ast_scope_type(scope, binding, &refined);
        }
    }

    /// Resolve an owner-specific trait method to its mangled impl for a receiver
    /// type. A concrete generic type falls back to its head, where generic impls
    /// are registered:
    /// `List<Int>` matches `impl … for List(a)`, `Option<String>` matches
    /// `impl … for Option(a)`. The impl method stays generic and monomorphizes per
    /// element exactly as a `where`-bounded free function would. Last, a BLANKET impl
    /// — `impl Into(b) for a where b: From(a)` — is registered under a type-variable
    /// head (lowercase); it applies to any receiver, with its `where` bound
    /// discharged at monomorphization.
    fn lookup_trait_impls(&self, owner: &str, method: &str, ty: &Type) -> Vec<String> {
        let table_key = |ty: String| (owner.to_string(), method.to_string(), ty);
        let exact = type_key(ty.unqualified());
        let head = type_head_key(ty);
        for candidate_key in std::iter::once(exact).chain(head).map(table_key) {
            if let Some(candidates) = self.trait_impl_table.get(&candidate_key) {
                return candidates.iter().map(|c| c.mangled.clone()).collect();
            }
        }
        // A BLANKET impl is registered under a type-VARIABLE head (a bare
        // lowercase name like `a`). A module-qualified concrete head
        // (`geometry.Coord`) also starts lowercase but is NOT a variable —
        // exclude it, or every qualified impl would masquerade as blanket
        // and a generic receiver would dispatch to an arbitrary one (RFC-0042).
        self.trait_impl_table
            .iter()
            .filter(|((tr, m, k), _)| {
                tr == owner
                    && m == method
                    && k.chars().next().is_some_and(char::is_lowercase)
                    && !k.contains('.')
            })
            .flat_map(|(_, candidates)| candidates.iter().map(|c| c.mangled.clone()))
            .collect()
    }

    fn lookup_inherent_impl(&self, method: &str, ty: &Type) -> Option<String> {
        let exact = type_key(ty.unqualified());
        let head = type_head_key(ty);
        std::iter::once(exact)
            .chain(head)
            .find_map(|key| self.inherent_impl_table.get(&(method.to_string(), key)))
            .cloned()
    }

    /// Resolve a concrete receiver method. Inherent methods take precedence; if
    /// multiple trait impls with the same method apply to the same receiver, the
    /// source needs a bound to provide trait identity and the call is ambiguous.
    fn lookup_impl(&self, method: &str, ty: &Type) -> Option<MethodResolution> {
        if let Some(mangled) = self.lookup_inherent_impl(method, ty) {
            return Some(MethodResolution::Found(mangled));
        }
        let mut matches = Vec::new();
        if let Some(infos) = self.trait_methods.get(method) {
            for info in infos {
                if info.is_static {
                    continue;
                }
                for mangled in self.lookup_trait_impls(&info.owner, method, ty) {
                    matches.push((info.owner.clone(), mangled));
                }
            }
        }
        matches.sort();
        matches.dedup();
        match matches.len() {
            0 => None,
            1 => Some(MethodResolution::Found(matches.pop().unwrap().1)),
            _ => Some(MethodResolution::Ambiguous(matches.into_iter().map(|(owner, _)| owner).collect())),
        }
    }

    fn trait_method_infos_for_receiver(
        &self,
        method: &str,
        receiver_ty: Option<&Type>,
    ) -> Vec<TraitMethodInfo> {
        let Some(infos) = self.trait_methods.get(method) else {
            return Vec::new();
        };
        let Some(var) = receiver_ty.and_then(type_variable_name) else {
            return infos.clone();
        };
        let bounds = self.bound_traits.borrow();
        let Some(active_bounds) = bounds.get(var) else { return infos.clone() };
        infos
            .iter()
            .filter(|info| {
                active_bounds.iter().any(|b| {
                    b == &info.owner || self.supertraits.get(b).is_some_and(|s| s.contains(&info.owner))
                })
            })
            .cloned()
            .collect()
    }

    fn ambiguous_method_msg(method: &str, tn: &str, owners: &[String]) -> String {
        let disp = tn.rsplit_once('.').map_or(tn, |(_, s)| s);
        format!(
            "method `{method}` on `{disp}` is ambiguous between trait impls: {}",
            owners.join(", ")
        )
    }

    fn is_dispatch_method(&self, method: &str) -> bool {
        self.trait_methods.contains_key(method) || self.inherent_methods.contains(method)
    }

    fn std_owner_method_alias(&self, method: &str, ty: &Type) -> Option<String> {
        if !is_ambient_std_owned_ast_type(ty) || self.lookup_inherent_impl(method, ty).is_none() {
            return None;
        }
        let module = type_owner_module_ast(ty)?;
        let func = format!("{module}.{method}");
        self.free_fns.contains(&func).then_some(func)
    }

    /// Record the current function's type-variable bounds, so the operator
    /// rewrite can tell a bounded generic (dispatch) from an unbounded one (keep
    /// the native structural comparison).
    fn set_bounds(&self, bounds: &[(String, String, Vec<Type>)]) {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for (var, trait_name, _) in bounds {
            map.entry(var.clone()).or_default().push(trait_name.clone());
        }
        *self.bound_traits.borrow_mut() = map;
    }

    /// Whether the current function bounds type variable `var` by any of `traits`
    /// (or a comparison trait that has one of them as a supertrait — `Ord` and
    /// `PartialOrd` both imply `PartialEq`).
    fn var_bounded_by(&self, var: &str, traits: &[&str]) -> bool {
        self.bound_traits
            .borrow()
            .get(var)
            .is_some_and(|bs| bs.iter().any(|b| traits.contains(&b.as_str())))
    }

    /// Whether a comparison operator on an operand of head type `head` should
    /// desugar to a trait call. Primitives keep the native operator. For
    /// equality, structural tuples, records without a `PartialEq` impl, and
    /// UNbounded type variables stay native (backwards compatible); a type
    /// variable bound by a comparison trait dispatches so compiled generic `==`
    /// is content-correct. Ordering has no native path for non-primitives, so a
    /// concrete non-primitive always dispatches and a type variable dispatches
    /// when bound by `PartialOrd`/`Ord` (an unbounded one is left for the type
    /// checker to reject with its clear "ordering requires Int/…" message).
    fn operator_dispatches(&self, op: BinOp, ty: Option<&Type>) -> bool {
        let Some(ty) = ty else { return false };
        let Some(head) = type_head_key(ty) else { return false };
        if is_specializable_type_arg(&head) {
            return false;
        }
        let type_var = type_variable_name(ty);
        if matches!(op, BinOp::Eq | BinOp::NotEq) {
            if matches!(ty.unqualified(), Type::Tuple(_))
                || matches!(ty.unqualified(), Type::Named(name, _) if name == "List")
            {
                return false;
            }
            if let Some(var) = type_var {
                return self.var_bounded_by(var, &["PartialEq", "Eq", "PartialOrd", "Ord"]);
            }
            let method = if op == BinOp::Eq { "eq" } else { "ne" };
            self.lookup_impl(method, ty).is_some()
        } else if let Some(var) = type_var {
            self.var_bounded_by(var, &["PartialOrd", "Ord"])
        } else {
            true
        }
    }

    fn current_location_prefix(&self) -> String {
        let line = self.current_line.get();
        if line == 0 {
            return String::new();
        }
        let func = self.current_func.borrow();
        let display = func.rsplit('.').next().unwrap_or(&func);
        if display.is_empty() {
            format!("line {line}: ")
        } else {
            format!("`{display}`, line {line}: ")
        }
    }

    /// `tail_is_value` is whether this block's final statement is in VALUE
    /// position — a function/closure body, or an `if`/`match` arm used as an
    /// expression — so its result is consumed and must NOT be turned into a
    /// write-back or flagged as a discard. (RFC-0043)
    fn rewrite_block(&self, b: &mut Block, scope: &mut Scope<Type>, tail_is_value: bool) {
        let last = b.stmts.len().wrapping_sub(1);
        for (i, stmt) in b.stmts.iter_mut().enumerate() {
            self.current_line.set(b.lines.get(i).copied().unwrap_or(0));
            // The final statement of a value-position block IS the block's value;
            // its result is used, so it is never a write-back or a discard.
            let value_used = i == last && tail_is_value;
            match stmt {
                // (RFC-0043) A statement-position method call whose result is
                // NOT consumed: after normal resolution, decide write-back (a
                // mutator on a mutable place) or a discard error (a non-Nil
                // result thrown away). The decision reads the RESOLVED callee.
                Stmt::Expr(Expr::MethodCall { .. }) if !value_used => {
                    self.rewrite_expr_stmt_method(stmt, scope);
                }
                Stmt::Let { name, ty, value, mutable, .. } => {
                    self.rewrite_expr(value, scope);
                    // The declared ascription is the binding's type — a local,
                    // typed declaration (RFC-0046) — and the value's recovered
                    // type is the fallback (`Mono::walk_block` does the same).
                    // Without it, `let cs: Set(Int) = iter.collect(...)` left
                    // `cs` untyped in the QUIET pass (a bounded call's result-
                    // position variable is unrecoverable before annotate), so a
                    // bare trait call on `cs` (`show(cs)`) stayed unresolved and
                    // made annotate fail — emptying the table for everyone.
                    let resolved = ty.as_ref().cloned().or_else(|| self.type_ast(value, scope));
                    match resolved {
                        // A `var` let is a mutable place — a write-back target.
                        Some(t) if *mutable => scope.insert_mut(name.clone(), t),
                        Some(t) => scope.insert(name.clone(), t),
                        // Untypeable, but still a bound local: a call on it
                        // is a first-class invocation, not a trait dispatch.
                        None if *mutable => scope.bind_local_mut(name),
                        None => scope.bind_local(name),
                    }
                }
                Stmt::Assign { value, .. } => self.rewrite_expr(value, scope),
                // Seed each destructured name from the tuple's slot types so a
                // trait call on a tuple part (`x0.show()`) dispatches.
                Stmt::LetPattern { pattern, value } => {
                    self.rewrite_expr(value, scope);
                    // Seed each destructured name from the value's type so a trait
                    // call on a part (`x0.show()`, `a < b`) dispatches. A
                    // tuple-returning call (`let (a, b) = pair()`) has no head name
                    // for `type_name` to recover, so fall back to the typeck table
                    // — otherwise the destructured names stay untyped. Any name we
                    // can't type is bound untyped (`bind_local`); this is a
                    // best-effort dispatch aid, so an untyped fallback is always
                    // sound (the checker re-verifies).
                    let value_ty = self.type_ast(value, scope);
                    bind_typed_pattern(pattern, self.ctor_infos, value_ty.as_ref(), scope);
                }
                // A `return`/`yield` value is always consumed. A bare expression
                // statement is consumed only when it is this block's value tail
                // (`value_used`) — otherwise its result is discarded, so a nested
                // block's tail (`if cond: xs.push(1)` as a statement) is a
                // write-back / discard site, not a value.
                Stmt::Return(Some(e)) | Stmt::Yield(e) => self.rewrite_expr_vp(e, scope, true),
                Stmt::Expr(e) => {
                    self.rewrite_expr_vp(e, scope, value_used);
                    // A discarded non-Nil call is legal only when its resolved
                    // declaration carries a var write-back. Free and method forms
                    // use the same convention table.
                    if !value_used {
                        if let Expr::Call { name, .. } = e {
                            let returns_nil = self.returns_nil.get(name).copied().unwrap_or(true);
                            if !returns_nil && !self.var_calls.contains(name) {
                                let bare = name.rsplit('.').next().unwrap_or(name);
                                self.discard_errors.borrow_mut().push(discarded_result_msg(bare));
                            }
                        }
                        // Preserve the proven RFC-0051 storage paths for the common
                        // discarded std mutators. This is a typed optimization: the
                        // source call has already resolved to a concrete var callee,
                        // and both source forms retain uniform write-back semantics.
                        if let Some(writeback) = discarded_std_var_writeback(e, self.var_calls) {
                            *stmt = writeback;
                        }
                    }
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    /// (RFC-0043) Resolve a statement-position method call and decide its fate by
    /// the RESOLVED callee — the write-back rule that replaces the linker's
    /// name census. `stmt` is `Stmt::Expr(MethodCall{ receiver, method, args })`
    /// and `place` is a clone of its receiver expression (still in method form).
    ///
    /// After ordinary resolution (`rewrite_expr` turns the `MethodCall` into a
    /// `Call { name, .. }`), the resolved function name tells us:
    /// - a MUTATOR (`is_mutator`, e.g. `list.push`) on a mutable place -> the
    ///   write-back rewrite `xs.push(1)` => `list.push(xs, 1)`;
    /// - a mutator on an immutable place / non-place -> a "declare it `var`, or
    ///   bind the result" error;
    /// - a non-mutator returning `Nil` -> a plain statement (as today);
    /// - a non-mutator returning non-Nil -> a discarded-result error naming the
    ///   method (the RFC's Failure-2 fix; `let _ = …` is the discard escape).
    ///
    /// A call this pass can't resolve to a `Call` (its receiver type isn't known
    /// yet) is left untouched — a later pass (per specialization) resolves and
    /// decides it, or the checker reports the unresolved method.
    fn rewrite_expr_stmt_method(&self, stmt: &mut Stmt, scope: &mut Scope<Type>) {
        // Read the method name before resolution consumes the node (for the
        // discard/immutable-place diagnostics).
        let method = match stmt {
            Stmt::Expr(Expr::MethodCall { method, .. }) => method.clone(),
            _ => return,
        };
        let Stmt::Expr(value) = stmt else { return };
        self.rewrite_expr(value, scope);
        // Resolution turns a method call into a `Call { name, args }`. Anything
        // else (still a MethodCall, or a non-call) is left for a later pass /
        // the checker.
        let Expr::Call { name, .. } = value else { return };
        let name = name.clone();

        if self.var_calls.contains(&name) {
            // The call itself carries write-back in every expression context.
            // Statement position only discards its independent ordinary result.
            return;
        }

        // Not a mutator. A resolved callee whose DECLARED return is non-Nil and
        // is thrown away is a discard error (the Failure-2 fix); `let _ = …` is
        // the explicit-discard escape (parsed as a wildcard `LetPattern`, so it
        // never reaches this arm). A `Nil`-returning call is a plain side-
        // effecting statement, as today. A callee NOT in the table (a bare
        // intrinsic / capability op the resolver produced, never a std/user
        // function) has no declared return here — leave it a plain statement
        // rather than risk a false positive.
        let returns_nil = self.returns_nil.get(&name).copied().unwrap_or(true);
        if !returns_nil {
            self.discard_errors.borrow_mut().push(discarded_result_msg(&method));
        }
    }

    /// Rewrite a single expression that occupies statement position but lives in
    /// an expression-shaped AST slot, such as a `match` arm body whose enclosing
    /// match is itself a statement. Reusing `rewrite_block` keeps RFC-0043's
    /// mutator/discard rules in one place: if the expression turns into an
    /// assignment, the arm stays expression-shaped by becoming a one-statement
    /// block whose value is `Nil`.
    fn rewrite_discarded_expr(&self, e: &mut Expr, scope: &mut Scope<Type>, line: u32) {
        let expr = std::mem::replace(e, Expr::Bool(false));
        let mut block = Block {
            stmts: vec![Stmt::Expr(expr)],
            lines: vec![line],
            region: None,
        };
        self.rewrite_block(&mut block, scope, false);
        let stmt = block.stmts.pop().expect("single-statement wrapper remains single");
        let line = block.lines.pop().unwrap_or(line);
        match stmt {
            Stmt::Expr(expr) => *e = expr,
            stmt => {
                *e = Expr::Block(Block {
                    stmts: vec![stmt],
                    lines: vec![line],
                    region: None,
                });
            }
        }
    }

    /// Resolve method syntax / trait calls within an expression. (RFC-0043)
    /// `value_position` flows to nested blocks so each knows whether its tail
    /// statement is consumed as a value: an `if`/`match` arm inherits the
    /// surrounding position (a tail `if` in a function body is a return value),
    /// while a loop body's tail is always discarded. Sub-expression operands are
    /// always in value position — the thin `rewrite_expr` wrapper passes `true`.
    fn rewrite_expr(&self, e: &mut Expr, scope: &mut Scope<Type>) {
        self.rewrite_expr_vp(e, scope, true);
    }

    fn rewrite_expr_vp(&self, e: &mut Expr, scope: &mut Scope<Type>, value_position: bool) {
        match e {
            Expr::Call { name, args } => {
                for a in args.iter_mut() {
                    self.rewrite_expr(a, scope);
                }
                // A call on a bound LOCAL (a function-typed parameter, a `let`
                // holding a closure) is a first-class invocation — never a
                // trait-method dispatch, even when the local's name coincides
                // with a trait method (`less`/`greater`/`show`, e.g. a
                // comparator parameter named `less`). Without this guard, a
                // comparator's own `less(best, x)` would be rewritten to the
                // element type's `Ord::less`, silently discarding the passed-in
                // function.
                if self.is_dispatch_method(name) && !scope.is_local(name) {
                    if let Some(recv) = args.first() {
                        if let Some(recv_ty) = self.type_ast(recv, scope) {
                            let tn = type_key(recv_ty.unqualified());
                            match self.lookup_impl(name, &recv_ty) {
                                Some(MethodResolution::Found(mangled)) => *name = mangled,
                                Some(MethodResolution::Ambiguous(owners)) => self
                                    .missing_impls
                                    .borrow_mut()
                                    .push(Self::ambiguous_method_msg(name, &tn, &owners)),
                                // The receiver's type is known and no impl
                                // exists; unless a plain function of this name
                                // can take the call, that is a bound the
                                // program cannot satisfy — report it cleanly.
                                // A TYPE-VARIABLE receiver (lowercase name) is
                                // a bounded generic: dispatch resolves after
                                // monomorphization, never an error here.
                                None if !self.free_fns.contains(name.as_str())
                                    && type_variable_name(&recv_ty).is_none() => {
                                    if let Some(infos) = self.trait_methods.get(name.as_str()) {
                                        // Render the unqualified type name a reader wrote
                                        // (`Blob`, not the canonical `main.Blob`) (RFC-0042).
                                        let disp = tn.rsplit_once('.').map_or(tn.as_str(), |(_, s)| s);
                                        let trait_name = if infos.len() == 1 {
                                            infos[0].owner.clone()
                                        } else {
                                            format!(
                                                "one of {}",
                                                infos
                                                    .iter()
                                                    .map(|i| i.owner.as_str())
                                                    .collect::<Vec<_>>()
                                                    .join(", ")
                                            )
                                        };
                                        self.missing_impls.borrow_mut().push(format!(
                                            "`{disp}` does not implement `{trait_name}` \
                                             (no `impl {trait_name} for {disp}`) — required by a call to `{name}`"
                                        ));
                                    }
                                }
                                None => {}
                            }
                        }
                    }
                }
                self.refine_var_call_args(name, args, scope);
            }
            Expr::Apply { func, args } => {
                self.rewrite_expr(func, scope);
                for a in args.iter_mut() {
                    self.rewrite_expr(a, scope);
                }
            }
            Expr::Ctor { args, .. }
            | Expr::AnonCtor { args, .. }
            | Expr::List(args)
            | Expr::Tuple(args) => {
                for a in args.iter_mut() {
                    self.rewrite_expr(a, scope);
                }
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
                self.rewrite_expr(expr, scope)
            }
            Expr::RecordUpdate { name: _, base, fields } => {
                self.rewrite_expr(base, scope);
                for (_, v) in fields.iter_mut() {
                    self.rewrite_expr(v, scope);
                }
            }
            // (RFC-0056) Lowered to positional `Call` at the link layer; recurse
            // defensively over argument values (mirrors `Record` above).
            Expr::LabeledCall { args, .. } => {
                for (_, a) in args.iter_mut() {
                    self.rewrite_expr(a, scope);
                }
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields.iter_mut() {
                    self.rewrite_expr(v, scope);
                }
                if let Some(s) = spread {
                    self.rewrite_expr(s, scope);
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                // Comparison operators on non-primitive operands desugar to their
                // trait method (`a > b` -> `greater(a, b)`), which the call arm
                // below then dispatches to the concrete impl. Primitives — and, for
                // equality, structural tuples and records lacking a `PartialEq`
                // impl — keep the native operator.
                if let Some(method) = operator_trait_method(*op) {
                    // `==`/`!=`/`<`/… require both operands to share a type, so the
                    // receiver's concrete type can be recovered from EITHER side.
                    // Recovering only from the left misses a pattern-bound left
                    // operand (`Ok(p2) -> p2 == p`) whose type the scope/table can't
                    // surface but the right operand can. A wrong guess only ever
                    // yields a type error (never wrong code), so trying both is safe.
                    let operand_ty = self
                        .type_ast(lhs, scope)
                        .or_else(|| self.type_ast(rhs, scope));
                    if self.operator_dispatches(*op, operand_ty.as_ref()) {
                        // Rewrite children while they still occupy the addresses
                        // recorded by the typed owner. Once moved into the new
                        // Call's Vec their addresses change, so the replacement
                        // must not be revisited with the old table.
                        self.rewrite_expr(lhs, scope);
                        self.rewrite_expr(rhs, scope);
                        let l = std::mem::replace(lhs.as_mut(), Expr::Bool(false));
                        let r = std::mem::replace(rhs.as_mut(), Expr::Bool(false));
                        // Mangle to the concrete impl directly from the recovered
                        // head. The Call arm below otherwise re-recovers the receiver
                        // type from the FIRST argument, which fails for a pattern-bound
                        // operand (`Ok(p2) -> p2 == p`); since both operands share
                        // `head`, use it. A type-variable head (lowercase) stays a
                        // generic trait call for monomorphization to specialize.
                        let resolved = operand_ty
                            .as_ref()
                            .filter(|ty| type_variable_name(ty).is_none())
                            .and_then(|ty| match self.lookup_impl(method, ty) {
                                Some(MethodResolution::Found(mangled)) => Some(mangled),
                                _ => None,
                            });
                        *e = Expr::Call {
                            name: resolved.unwrap_or_else(|| method.to_string()),
                            args: vec![l, r],
                        };
                        return;
                    }
                }
                self.rewrite_expr(lhs, scope);
                self.rewrite_expr(rhs, scope);
            }
            Expr::Range { lo, hi, .. } => {
                self.rewrite_expr(lo, scope);
                self.rewrite_expr(hi, scope);
            }
            Expr::Index { base, index } => {
                self.rewrite_expr(base, scope);
                self.rewrite_expr(index, scope);
                let receiver_ty = self.type_ast(base, scope);
                if receiver_ty.as_ref().and_then(type_owner_module_ast) == Some("dict") {
                    let base = std::mem::replace(base.as_mut(), Expr::Bool(false));
                    let index = std::mem::replace(index.as_mut(), Expr::Bool(false));
                    *e = Expr::Call { name: "dict.at".to_string(), args: vec![base, index] };
                }
            }
            // METHOD RESOLUTION (rfcs/language-evolution.md Phase 3):
            // `x.f(a)` resolves to a real method — an impl for x's type, a
            // trait method on a bound receiver, or a `Type.f(a)` static. It
            // is NOT sugar for arbitrary free functions; an unresolvable
            // method is a loud check-time error naming the function spelling.
            Expr::MethodCall { receiver, method, args } => {
                self.rewrite_expr(receiver, scope);
                for a in args.iter_mut() {
                    self.rewrite_expr(a, scope);
                }
                // `Type.name(args)` — a static call on the TYPE itself.
                if let Expr::Ctor { name: tyname, args: cargs } = receiver.as_ref() {
                    if cargs.is_empty() {
                        if let Some(mangled) = self.statics.get(&(tyname.clone(), method.clone())) {
                            let mut call_args = Vec::new();
                            call_args.append(args);
                            *e = Expr::Call { name: mangled.clone(), args: call_args };
                            return;
                        }
                        // `Dog.greet()` where `Dog` is a NULLARY CONSTRUCTOR
                        // is a method call on that value, not a static access:
                        // fall through to instance dispatch.
                        let is_value = self
                            .ctor_infos
                            .get(tyname.as_str())
                            .is_some_and(|info| info.fields.is_empty());
                        if self.lookup_impl(method, &named_type(tyname)).is_some() && !is_value
                        {
                            self.missing_impls.borrow_mut().push(format!(
                                "`{tyname}.{method}` is an INSTANCE method (it takes `self`) — \
                                 call it on a value: `value.{method}(…)`"
                            ));
                            return;
                        }
                    }
                }
                let receiver_ty = self.type_ast(receiver, scope);
                if let Some(ty) = &receiver_ty {
                    let tn = type_key(ty.unqualified());
                    // Host-capability intrinsics are authority-bearing methods,
                    // even when the capability type also has a std owner module
                    // (`Rand` -> `rand`). Try this before owner-module UFCS so
                    // `rand.rand_u64()` lowers to the host op, while ordinary
                    // std helpers like `rand.hex(n)` still resolve below.
                    if cap_receiver_kind_ast(ty)
                        .is_some_and(|receiver| cap_ops::receiver_supports(method, receiver))
                    {
                        let mut call_args = vec![std::mem::replace(
                            receiver.as_mut(),
                            Expr::Bool(false),
                        )];
                        call_args.append(args);
                        *e = Expr::Call { name: cap_ops::call_name(method), args: call_args };
                        return;
                    }
                    if let Some(func) = self.std_owner_method_alias(method, ty) {
                        let mut call_args = vec![std::mem::replace(
                            receiver.as_mut(),
                            Expr::Bool(false),
                        )];
                        call_args.append(args);
                        self.refine_var_call_args(&func, &call_args, scope);
                        *e = Expr::Call { name: func, args: call_args };
                        return;
                    }
                    match self.lookup_impl(method, ty) {
                        Some(MethodResolution::Found(mangled)) => {
                            let mut call_args = vec![std::mem::replace(
                                receiver.as_mut(),
                                Expr::Bool(false),
                            )];
                            call_args.append(args);
                            self.refine_var_call_args(&mangled, &call_args, scope);
                            *e = Expr::Call { name: mangled, args: call_args };
                            return;
                        }
                        Some(MethodResolution::Ambiguous(owners)) => {
                            self.missing_impls
                                .borrow_mut()
                                .push(Self::ambiguous_method_msg(method, &tn, &owners));
                            return;
                        }
                        None => {}
                    }
                }
                // A trait method on a generic (bound) receiver dispatches after
                // monomorphization: lower to the bare trait call. A STATIC trait
                // method (`b.from(x)`, no `self`) takes no receiver — the receiver is
                // the type itself, resolved through the bound at mono — so only the
                // explicit arguments are passed; an instance method prepends `self`.
                let receiver_is_generic = receiver_ty
                    .as_ref()
                    .is_none_or(|ty| type_variable_name(ty).is_some());
                let active_trait_methods =
                    self.trait_method_infos_for_receiver(method, receiver_ty.as_ref());
                if !active_trait_methods.is_empty() && receiver_is_generic {
                    if active_trait_methods.len() > 1 {
                        self.missing_impls.borrow_mut().push(format!(
                            "method `{method}` on a generic receiver is ambiguous between trait bounds: {}",
                            active_trait_methods
                                .iter()
                                .map(|info| info.owner.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                        return;
                    }
                    let method_info = &active_trait_methods[0];
                    let mut call_args = if method_info.is_static {
                        if let Expr::Var(receiver_name) = receiver.as_ref() {
                            *e = Expr::Call {
                                name: static_bound_marker(receiver_name, method),
                                args: std::mem::take(args),
                            };
                            return;
                        }
                        Vec::new()
                    } else {
                        vec![std::mem::replace(receiver.as_mut(), Expr::Bool(false))]
                    };
                    call_args.append(args);
                    *e = Expr::Call { name: method.clone(), args: call_args };
                    return;
                }
                // UFCS fallback: a receiver may call public functions from its
                // owning module (`matrix.Matrix.scale(m, n)` as `m.scale(n)`).
                // RFC-0042 gives ordinary user types canonical `module.Type`
                // names, so the owner is derived from the type itself. Ambient
                // builtin types still use a small declaration table until they
                // acquire ordinary source declarations.
                if let Some(module) = receiver_ty.as_ref().and_then(type_owner_module_ast) {
                    let mut call_args =
                        vec![std::mem::replace(receiver.as_mut(), Expr::Bool(false))];
                    call_args.append(args);
                    // Place-assignment desugaring uses a private value-rebuild
                    // operation. Dict and List keep distinct internal spellings;
                    // their public setters are uniform `var`/`Nil` calls.
                    let func = if module == "dict" && method == "__set_at" {
                        "dict.__insert".to_string()
                    } else {
                        format!("{module}.{method}")
                    };
                    if receiver_ty.as_ref().is_some_and(is_ambient_owned_ast_type)
                        || self.owner_methods.contains(&func)
                    {
                        *e = Expr::Call { name: func, args: call_args };
                        return;
                    }
                }
                // UFCS for host-capability operations. Keep a private marker on
                // the lowered call so the compiler still knows the user wrote
                // method syntax (`dir.read("x")`) rather than the legacy bare
                // intrinsic form (`dir.read("x")`).
                if receiver_ty
                    .as_ref()
                    .and_then(cap_receiver_kind_ast)
                    .is_some_and(|receiver| cap_ops::receiver_supports(method, receiver))
                {
                    let mut call_args =
                        vec![std::mem::replace(receiver.as_mut(), Expr::Bool(false))];
                    call_args.append(args);
                    *e = Expr::Call { name: cap_ops::call_name(method), args: call_args };
                    return;
                }
                match receiver_ty {
                    Some(ty) => self.missing_impls.borrow_mut().push(format!(
                        "no method `{method}` on `{}` — methods come from `impl` blocks; \
                         a plain function is called as `{method}(value, …)` (or module-qualified, \
                         e.g. `list.{method}(value, …)`)",
                        type_key(ty.unqualified()),
                    )),
                    // `json.stringify(x)` with no `import json` parses as a method
                    // call on the bare name `json`; if that name is actually a std
                    // module the user just forgot the import, so say so rather than
                    // talk about method resolution.
                    None if matches!(receiver.as_ref(), Expr::Var(m) if witchy_syntax::linker::STD_MODULES.contains(&m.as_str())) => {
                        let Expr::Var(m) = receiver.as_ref() else { unreachable!() };
                        let loc = self.current_location_prefix();
                        self.missing_impls.borrow_mut().push(format!(
                            "{loc}`{m}.{method}` looks like a module-qualified call, but `{m}` is \
                             not imported — add `import {m}`"
                        ));
                    }
                    // Let the ordinary checker infer the receiver expression so
                    // unbound variable receivers keep function/line context.
                    None if matches!(receiver.as_ref(), Expr::Var(_)) => {}
                    None => self.missing_impls.borrow_mut().push(format!(
                        "cannot resolve the method call `.{method}(…)` — the receiver's type \
                         is not known here; call the function directly: `{method}(value, …)`"
                    )),
                }
            }
            Expr::WhileLet { pattern, scrutinee, body } => {
                self.rewrite_expr(scrutinee, scope);
                let mut s = scope.clone();
                let scrutinee_ty = self.type_ast(scrutinee, scope);
                bind_typed_pattern(
                    pattern,
                    self.ctor_infos,
                    scrutinee_ty.as_ref(),
                    &mut s,
                );
                // A loop evaluates to Nil, so its body's tail value is discarded.
                self.rewrite_block(body, &mut s, false);
                merge_refined_outer_ast_types(scope, &s);
            }
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                self.rewrite_expr(cond, scope);
                // An `if` used as an expression yields its arms' tails, so each
                // arm inherits the surrounding value position.
                let mut then_scope = scope.clone();
                self.rewrite_block(then_block, &mut then_scope, value_position);
                merge_refined_outer_ast_types(scope, &then_scope);
                if let Some(b) = else_block {
                    let mut else_scope = scope.clone();
                    self.rewrite_block(b, &mut else_scope, value_position);
                    merge_refined_outer_ast_types(scope, &else_scope);
                }
            }
            Expr::While { cond, body } => {
                self.rewrite_expr(cond, scope);
                let mut body_scope = scope.clone();
                self.rewrite_block(body, &mut body_scope, false);
                merge_refined_outer_ast_types(scope, &body_scope);
            }
            Expr::For { var, iter, body } => {
                self.rewrite_expr(iter, scope);
                let iter_ty = self.type_ast(iter, scope);
                // `for ... in <dict>` iterates the dict's (key, value) pairs;
                // `for x in <set>` iterates the set's members — rewrite the
                // iterand to `dict.pairs(...)` / `set.to_list(...)` respectively.
                let view = match iter_ty.as_ref().map(Type::unqualified) {
                    Some(Type::Named(name, _)) if name == "Dict" => Some("dict.pairs"),
                    Some(Type::Named(name, _)) if name == "Set" => Some("set.to_list"),
                    _ => None,
                };
                if let Some(view_fn) = view {
                    let inner = std::mem::replace(iter.as_mut(), Expr::Bool(false));
                    **iter = Expr::Call { name: view_fn.to_string(), args: vec![inner] };
                }
                let mut s = scope.clone();
                match iter_ty.as_ref().and_then(iterable_item_type) {
                    Some(item_ty) => s.insert(var.clone(), item_ty),
                    None => s.bind_local(var),
                }
                self.rewrite_block(body, &mut s, false);
                merge_refined_outer_ast_types(scope, &s);
            }
            Expr::Match { scrutinee, arms } => {
                self.rewrite_expr(scrutinee, scope);
                let scrutinee_ty = self.type_ast(scrutinee, scope);
                for arm in arms.iter_mut() {
                    let mut s = scope.clone();
                    bind_typed_pattern(
                        &arm.pattern,
                        self.ctor_infos,
                        scrutinee_ty.as_ref(),
                        &mut s,
                    );
                    if let Some(g) = &mut arm.guard {
                        self.rewrite_expr(g, &mut s);
                    }
                    // A match arm's body inherits the surrounding value position.
                    // If the surrounding match is itself a statement, the arm is
                    // a statement-position expression even though `MatchArm`
                    // stores it as `Expr`.
                    if value_position {
                        self.rewrite_expr_vp(&mut arm.body, &mut s, true);
                    } else {
                        self.rewrite_discarded_expr(&mut arm.body, &mut s, arm.line);
                    }
                    merge_refined_outer_ast_types(scope, &s);
                }
            }
            Expr::Lambda { params, body, .. } => {
                let mut s = scope.clone();
                seed_typed_params(params, &mut s);
                // A closure body's tail IS its return value.
                self.rewrite_block(body, &mut s, true);
            }
            Expr::Block(b) => {
                let mut block_scope = scope.clone();
                self.rewrite_block(b, &mut block_scope, value_position);
                merge_refined_outer_ast_types(scope, &block_scope);
            }
            Expr::Var(_) | Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::TaggedLit { .. } => {}
        }
    }
}

fn nominal_type_name(ty: &Type) -> Option<&str> {
    match ty.unqualified() {
        Type::Named(name, _) => Some(name),
        Type::Tuple(_) | Type::Fn(_, _, _) => None,
        Type::Qualified(_, _) => unreachable!("unqualified strips qualifiers"),
    }
}

fn type_variable_name(ty: &Type) -> Option<&str> {
    let Type::Named(name, args) = ty.unqualified() else { return None };
    (args.is_empty()
        && name.chars().next().is_some_and(char::is_lowercase)
        && !name.contains('.'))
    .then_some(name)
}

fn cap_receiver_kind_ast(ty: &Type) -> Option<cap_ops::ReceiverKind> {
    let name = nominal_type_name(ty)?;
    match name {
        "Console" => Some(cap_ops::ReceiverKind::Console),
        "Clock" => Some(cap_ops::ReceiverKind::Clock),
        "Rand" => Some(cap_ops::ReceiverKind::Rand),
        "Env" => Some(cap_ops::ReceiverKind::Env),
        "Exec" => Some(cap_ops::ReceiverKind::Exec),
        "BuildOut" => Some(cap_ops::ReceiverKind::BuildOut),
        "BuildRead" => Some(cap_ops::ReceiverKind::BuildRead),
        "BuildEnv" => Some(cap_ops::ReceiverKind::BuildEnv),
        "BuildNet" => Some(cap_ops::ReceiverKind::BuildNet),
        "BuildExec" => Some(cap_ops::ReceiverKind::BuildExec),
        "File" => Some(cap_ops::ReceiverKind::File),
        "Dir" => Some(cap_ops::ReceiverKind::Dir),
        "Net" => Some(cap_ops::ReceiverKind::Net),
        "Socket" => Some(cap_ops::ReceiverKind::Socket),
        "Listener" => Some(cap_ops::ReceiverKind::Listener),
        _ => None,
    }
}

fn type_owner_module_name(name: &str) -> Option<&str> {
    if let Some((module, ty)) = name.rsplit_once('.') {
        if !module.is_empty() && !ty.is_empty() {
            return Some(module);
        }
    }
    match name {
        "Bytes" => Some("bytes"),
        "Duration" => Some("duration"),
        "List" => Some("list"),
        "Dict" => Some("dict"),
        "String" => Some("string"),
        "Set" => Some("set"),
        "Option" => Some("option"),
        "Result" => Some("result"),
        "Iter" => Some("iter"),
        "Rand" => Some("rand"),
        // `key.sign(msg)` / `key.public_key()` / `key.reveal()` -> crypto.*;
        // `store.get(name)` -> secretstore.get.
        "Secret" => Some("crypto"),
        "SecretStore" => Some("secretstore"),
        _ => None,
    }
}

fn type_owner_module_ast(ty: &Type) -> Option<&str> {
    type_owner_module_name(nominal_type_name(ty)?)
}

fn is_ambient_owned_ast_type(ty: &Type) -> bool {
    nominal_type_name(ty).is_some_and(|name| !name.contains('.'))
}

fn is_ambient_std_owned_ast_type(ty: &Type) -> bool {
    is_ambient_owned_ast_type(ty) && type_owner_module_ast(ty).is_some()
}

fn is_ambient_std_owned_name(name: &str) -> bool {
    !name.contains('.') && type_owner_module_name(name).is_some()
}

// A `MethodCall` anywhere means the lowering pass must run (to resolve it via
// impl/trait/static dispatch or UFCS), even in a module with no traits or impls.
fn module_needs_lowering(items: &[Item]) -> bool {
    items.iter().any(|it| match it {
        Item::Function(f) => block_needs_lowering(&f.body),
        Item::Const { value, .. } => expr_needs_lowering(value),
        _ => false,
    })
}

fn block_needs_lowering(b: &Block) -> bool {
    b.stmts.iter().any(|s| match s {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Yield(value)
        | Stmt::Expr(value) => expr_needs_lowering(value),
        Stmt::Return(opt) => opt.as_ref().is_some_and(expr_needs_lowering),
        Stmt::Break | Stmt::Continue => false,
    })
}

fn expr_needs_lowering(e: &Expr) -> bool {
    match e {
        Expr::MethodCall { .. } => true,
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::Var(_) | Expr::TaggedLit { .. } => false,
        Expr::List(xs) | Expr::Tuple(xs) => xs.iter().any(expr_needs_lowering),
        Expr::Call { args, .. } | Expr::Ctor { args, .. }
        | Expr::AnonCtor { args, .. } => {
            args.iter().any(expr_needs_lowering)
        }
        Expr::LabeledCall { args, .. } => args.iter().any(|(_, a)| expr_needs_lowering(a)),
        Expr::Apply { func, args } => {
            expr_needs_lowering(func) || args.iter().any(expr_needs_lowering)
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => {
            expr_needs_lowering(expr)
        }
        Expr::Field { base, .. } => expr_needs_lowering(base),
        Expr::Lambda { body, .. } | Expr::Block(body) => block_needs_lowering(body),
        Expr::RecordUpdate { name: _, base, fields } => {
            expr_needs_lowering(base) || fields.iter().any(|(_, v)| expr_needs_lowering(v))
        }
        Expr::Record { fields, spread, .. } => {
            fields.iter().any(|(_, v)| expr_needs_lowering(v))
                || spread.as_deref().is_some_and(expr_needs_lowering)
        }
        Expr::Binary { lhs, rhs, .. } => {
            expr_needs_lowering(lhs) || expr_needs_lowering(rhs)
        }
        Expr::If { cond, then_block, else_block } => {
            expr_needs_lowering(cond)
                || block_needs_lowering(then_block)
                || else_block.as_ref().is_some_and(block_needs_lowering)
        }
        Expr::Match { scrutinee, arms } => {
            expr_needs_lowering(scrutinee)
                || arms.iter().any(|a| {
                    a.guard.as_ref().is_some_and(expr_needs_lowering)
                        || expr_needs_lowering(&a.body)
                })
        }
        Expr::While { cond, body } => {
            expr_needs_lowering(cond) || block_needs_lowering(body)
        }
        // A for-loop may iterate a dict, which the lowering pass desugars to
        // `dict.pairs(...)`, so any for-loop needs the pass to run.
        Expr::For { .. } => true,
        Expr::Range { lo, hi, .. } => expr_needs_lowering(lo) || expr_needs_lowering(hi),
        Expr::Index { base, index } => {
            expr_needs_lowering(base) || expr_needs_lowering(index)
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            expr_needs_lowering(scrutinee) || block_needs_lowering(body)
        }
    }
}

/// Canonical terminal key for impl lookup, memoization, and mangling. The key is
/// never parsed back into a type; all structural reasoning happens before this
/// boundary on [`Type`].
fn type_key(t: &Type) -> String {
    enum Part<'a> {
        Ty(&'a Type),
        Char(char),
        Text(&'static str),
    }

    fn push_items<'a>(stack: &mut Vec<Part<'a>>, items: &'a [Type]) {
        for (index, item) in items.iter().enumerate().rev() {
            stack.push(Part::Ty(item));
            if index > 0 {
                stack.push(Part::Char(','));
            }
        }
    }

    // `Type` is a finite owned tree: recursive declarations stay nominal rather
    // than embedding themselves. Walk it iteratively so arbitrary source nesting
    // cannot overflow this renderer, and never truncate a key in a way that makes
    // two distinct types select the same specialization.
    let mut key = String::new();
    let mut stack = vec![Part::Ty(t)];
    while let Some(part) = stack.pop() {
        match part {
            Part::Ty(Type::Qualified(_, inner)) => stack.push(Part::Ty(inner)),
            Part::Ty(Type::Named(name, args)) => {
                key.push_str(name);
                if !args.is_empty() {
                    key.push('<');
                    stack.push(Part::Char('>'));
                    push_items(&mut stack, args);
                }
            }
            Part::Ty(Type::Tuple(items)) => {
                key.push_str(&format!("Tuple{}<", items.len()));
                stack.push(Part::Char('>'));
                push_items(&mut stack, items);
            }
            Part::Ty(Type::Fn(params, ret, conventions)) => {
                key.push_str("fn[");
                for convention in conventions {
                    key.push(match convention {
                        Convention::Let => 'l',
                        Convention::Borrow => 'b',
                        Convention::Var => 'v',
                        Convention::Own => 'o',
                    });
                }
                key.push_str("](");
                stack.push(Part::Ty(ret));
                stack.push(Part::Text(")->"));
                push_items(&mut stack, params);
            }
            Part::Char(ch) => key.push(ch),
            Part::Text(text) => key.push_str(text),
        }
    }
    key
}

/// Encode a canonical type key into one compiler-private symbol segment without
/// losing identity. Escaping every non-ASCII-alphanumeric byte (including `_`)
/// keeps punctuation, module qualification, Unicode, and literal underscores
/// distinct; the old replace-with-underscore scheme could emit one function name
/// for both `pkg.T` and `pkg_T`.
fn mangle_type_key(key: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut mangled = String::with_capacity(key.len());
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() {
            mangled.push(char::from(byte));
        } else {
            mangled.push('_');
            mangled.push(char::from(HEX[usize::from(byte >> 4)]));
            mangled.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    mangled
}

/// A discarded non-Nil result from a call without write-back. `let _ = …` is
/// the explicit-discard escape.
fn discarded_result_msg(method: &str) -> String {
    format!(
        "result of `{method}` is discarded — bind it or discard explicitly with `let _ = …`. \
         A call with a declared `var` write-back may discard its ordinary result directly."
    )
}

/// A function's parameter types (None for an unannotated param) and return type,
/// kept so a generic call's result type can be recovered by binding the return
/// type variable from an argument — e.g. `list.at(xs: List(a), Int) -> a`.
type FnSig = (Vec<Option<Type>>, Type, Vec<Convention>);

#[derive(Clone, Debug)]
struct CtorInfo {
    owner: String,
    params: Vec<String>,
    fields: Vec<Type>,
}

fn build_fn_sigs(items: &[Item]) -> HashMap<String, FnSig> {
    let mut fn_sigs = HashMap::new();
    for item in items {
        if let Item::Function(f) = item {
            if let Some(ret) = &f.ret {
                let ptys = f.params.iter().map(|p| p.ty.clone()).collect();
                let conventions = f.params.iter().map(|p| p.convention).collect();
                fn_sigs.insert(f.name.clone(), (ptys, ret.clone(), conventions));
            }
        }
    }
    fn_sigs
}

fn build_ctor_infos(items: &[Item]) -> HashMap<String, CtorInfo> {
    let mut map = HashMap::new();
    for item in items {
        let Item::Type(t) = item else { continue };
        let mut implicit_params = Vec::new();
        if t.params.is_empty() {
            for v in &t.variants {
                for field in &v.fields {
                    collect_type_vars(field, &mut implicit_params);
                }
            }
        }
        let params = if t.params.is_empty() {
            implicit_params
        } else {
            t.params.clone()
        };
        for v in &t.variants {
            map.insert(
                v.name.clone(),
                CtorInfo {
                    owner: t.name.clone(),
                    params: params.clone(),
                    fields: v.fields.clone(),
                },
            );
        }
    }
    map
}

fn build_owner_methods(items: &[Item]) -> HashSet<String> {
    let mut methods = HashSet::new();
    for item in items {
        let Item::Function(f) = item else { continue };
        if !f.public {
            continue;
        }
        let Some((module, _)) = f.name.rsplit_once('.') else {
            continue;
        };
        let Some(first_ty) = f.params.first().and_then(|p| p.ty.as_ref()) else {
            continue;
        };
        if type_owner_module_ast(first_ty) == Some(module) {
            methods.insert(f.name.clone());
        }
    }
    methods
}

/// The write-back tables read at a resolved call site:
/// - `var_calls`: every function with at least one `var` parameter. Its ordinary
///   result may be discarded because the call still performs write-back.
/// - `returns_nil`: function name -> whether it returns `Nil`/nothing, so a
///   statement-position non-mutator method call whose result is non-Nil is a
///   discard error. A function absent from this map has an unknown (generic or
///   inferred) return; treated as non-Nil (conservatively an error candidate).
///
/// Keyed by the FULLY-QUALIFIED / mangled function name the dispatch pass
/// resolves a `place.method(args)` to (`list.push`, `Bag.push`'s mangling), so
/// the decision consults the exact resolved callee — never a bare name census.
fn build_mutation_tables(
    items: &[Item],
) -> (HashSet<String>, HashMap<String, bool>) {
    let mut var_calls = HashSet::new();
    let mut returns_nil = HashMap::new();
    for item in items {
        if let Item::Function(f) = item {
            if f.params.iter().any(|p| p.convention == Convention::Var) {
                var_calls.insert(f.name.clone());
            }
            let nil = match f.ret.as_ref() {
                None => true,
                Some(t) => matches!(t.unqualified(), Type::Named(n, _) if n == "Nil"),
            };
            returns_nil.insert(f.name.clone(), nil);
        }
    }
    (var_calls, returns_nil)
}

fn discarded_std_var_writeback(
    expr: &Expr,
    var_calls: &HashSet<String>,
) -> Option<Stmt> {
    let Expr::Call { name, args } = expr else { return None };
    if !var_calls.contains(name) {
        return None;
    }
    let place = args.first()?.clone();
    let private = if name == "list.push" || name.starts_with("list.push__") {
        "list.__push"
    } else if name == "list.set_at" || name.starts_with("list.set_at__") {
        "list.__set_at"
    } else if name == "dict.insert" || name.starts_with("dict.insert__") {
        "dict.__insert"
    } else if name == "dict.update" || name.starts_with("dict.update__") {
        "dict.__update"
    } else if name == "dict.remove" || name.starts_with("dict.remove__") {
        "dict.__remove"
    } else {
        return None;
    };
    let value = Expr::Call { name: private.to_string(), args: args.clone() };
    witchy_syntax::parser::desugar_place_assign(place, value).ok()
}

/// Record type name -> its named field types, for typing `x.field` receivers.
fn build_record_fields(items: &[Item]) -> HashMap<String, Vec<(String, Type)>> {
    let mut map = HashMap::new();
    for item in items {
        if let Item::Type(t) = item {
            if let [v] = t.variants.as_slice() {
                if !v.field_names.is_empty() {
                    map.insert(
                        t.name.clone(),
                        v.field_names.iter().cloned().zip(v.fields.iter().cloned()).collect(),
                    );
                }
            }
        }
    }
    map
}

/// Match a declared type pattern against a concrete type transactionally.
/// Failed shape matches cannot leak partial bindings into later judgments.
fn bind_ast_type_vars(
    pattern: &Type,
    concrete: &Type,
    out: &mut HashMap<String, Type>,
) -> bool {
    let mut trial = out.clone();
    if bind_ast_type_vars_inner(pattern, concrete, &mut trial) {
        *out = trial;
        true
    } else {
        false
    }
}

fn bind_ast_type_vars_inner(
    pattern: &Type,
    concrete: &Type,
    out: &mut HashMap<String, Type>,
) -> bool {
    let pattern = pattern.unqualified();
    let concrete = concrete.unqualified();
    match (pattern, concrete) {
        (Type::Named(var, args), concrete)
            if args.is_empty()
                && var.chars().next().is_some_and(char::is_lowercase)
                && !var.contains('.') =>
        {
            // An unresolved actual variable is absence of evidence, not a
            // concrete binding. `set.new()` yields `Set(a)`; a later
            // `set.insert(s, 1.5)` must refine that `a` from the Float argument
            // instead of conflicting with a meaningless `a -> a` binding.
            if matches!(concrete, Type::Named(name, args)
                if args.is_empty()
                    && name.chars().next().is_some_and(char::is_lowercase)
                    && !name.contains('.'))
            {
                return true;
            }
            match out.get(var) {
                Some(previous) => previous.unqualified() == concrete,
                None => {
                    out.insert(var.clone(), concrete.clone());
                    true
                }
            }
        }
        (Type::Named(pattern_name, pattern_args), Type::Named(name, args)) => {
            pattern_name == name
                // Empty literals initially carry only their container head. They
                // are incomplete evidence, so let another argument bind the
                // element variables before rejecting the call's full shape.
                && (args.is_empty()
                    || (pattern_args.len() == args.len()
                        && pattern_args.iter().zip(args).all(|(pattern, concrete)| {
                            bind_ast_type_vars_inner(pattern, concrete, out)
                        })))
        }
        (Type::Tuple(pattern_items), Type::Tuple(items)) => {
            pattern_items.len() == items.len()
                && pattern_items
                    .iter()
                    .zip(items)
                    .all(|(pattern, concrete)| {
                        bind_ast_type_vars_inner(pattern, concrete, out)
                    })
        }
        (
            Type::Fn(pattern_params, pattern_ret, pattern_conventions),
            Type::Fn(params, ret, conventions),
        ) => {
            pattern_params.len() == params.len()
                && pattern_conventions == conventions
                && pattern_params
                    .iter()
                    .zip(params)
                    .all(|(pattern, concrete)| {
                        bind_ast_type_vars_inner(pattern, concrete, out)
                    })
                && bind_ast_type_vars_inner(pattern_ret, ret, out)
        }
        _ => pattern == concrete,
    }
}

/// Substitute type variables in a block's type ANNOTATIONS (`let x: T`, `e as T`)
/// — the part `specialize` doesn't reach by rewriting only the signature.
fn subst_block_types(b: &mut Block, subst: &HashMap<String, Type>) {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { ty, value, .. } => {
                if let Some(t) = ty {
                    *t = subst_trait_params(t, subst);
                }
                subst_expr_types(value, subst);
            }
            Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Expr(value)
            | Stmt::Yield(value) => subst_expr_types(value, subst),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn subst_expr_types(e: &mut Expr, subst: &HashMap<String, Type>) {
    match e {
        Expr::Lambda { params, body, .. } => {
            for p in params.iter_mut() {
                if let Some(t) = &p.ty {
                    p.ty = Some(subst_trait_params(t, subst));
                }
            }
            subst_block_types(body, subst);
        }
        Expr::Block(b) => subst_block_types(b, subst),
        Expr::As { expr, ty } => {
            *ty = subst_trait_params(ty, subst);
            subst_expr_types(expr, subst);
        }
        Expr::If { cond, then_block, else_block } => {
            subst_expr_types(cond, subst);
            subst_block_types(then_block, subst);
            if let Some(eb) = else_block {
                subst_block_types(eb, subst);
            }
        }
        Expr::Match { scrutinee, arms } => {
            subst_expr_types(scrutinee, subst);
            for a in arms.iter_mut() {
                if let Some(g) = &mut a.guard {
                    subst_expr_types(g, subst);
                }
                subst_expr_types(&mut a.body, subst);
            }
        }
        Expr::While { cond, body } => {
            subst_expr_types(cond, subst);
            subst_block_types(body, subst);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            subst_expr_types(scrutinee, subst);
            subst_block_types(body, subst);
        }
        Expr::For { iter, body, .. } => {
            subst_expr_types(iter, subst);
            subst_block_types(body, subst);
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. }
        | Expr::AnonCtor { args, .. } => {
            for a in args {
                subst_expr_types(a, subst);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            subst_expr_types(receiver, subst);
            for a in args {
                subst_expr_types(a, subst);
            }
        }
        Expr::Apply { func, args } => {
            subst_expr_types(func, subst);
            for a in args {
                subst_expr_types(a, subst);
            }
        }
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                subst_expr_types(x, subst);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::Field { base: expr, .. } => {
            subst_expr_types(expr, subst)
        }
        Expr::Binary { lhs, rhs, .. } => {
            subst_expr_types(lhs, subst);
            subst_expr_types(rhs, subst);
        }
        Expr::Range { lo, hi, .. } => {
            subst_expr_types(lo, subst);
            subst_expr_types(hi, subst);
        }
        Expr::Index { base, index } => {
            subst_expr_types(base, subst);
            subst_expr_types(index, subst);
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            subst_expr_types(base, subst);
            for (_, v) in fields {
                subst_expr_types(v, subst);
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, a) in args {
                subst_expr_types(a, subst);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                subst_expr_types(v, subst);
            }
            if let Some(s) = spread {
                subst_expr_types(s, subst);
            }
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::Var(_) | Expr::TaggedLit { .. } => {}
    }
}

/// A type argument we will specialize an *unbounded* generic on. Restricted to
/// the primitive types: these are exactly the ones the generic i32 ABI gets
/// wrong — `String` (pointer-compared instead of content-compared by `==`) and
/// `Int` (truncated to 32 bits). For records/enums/parameterized types the head
/// name alone wouldn't capture the type, and codegen treats them as pointers
/// either way, so those calls keep using the generic version.
fn is_specializable_type_arg(n: &str) -> bool {
    matches!(n, "Int" | "Bool" | "Float" | "String" | "Duration")
}

/// The trait method a comparison operator desugars to, or `None` for the
/// non-comparison operators (which never dispatch through a trait).
fn operator_trait_method(op: BinOp) -> Option<&'static str> {
    Some(match op {
        BinOp::Eq => "eq",
        BinOp::NotEq => "ne",
        BinOp::Lt => "less",
        BinOp::Gt => "greater",
        BinOp::LtEq => "less_equal",
        BinOp::GtEq => "greater_equal",
        _ => return None,
    })
}

/// The type variables in a function's parameters and return type (deduplicated,
/// in order). A call must resolve every one of these to a concrete type to be
/// specialized — a variable that appears only in the return (so it can't be read
/// off an argument) therefore blocks specialization, keeping it sound.
fn signature_type_vars(f: &Function) -> Vec<String> {
    let mut out = Vec::new();
    for p in &f.params {
        if let Some(t) = &p.ty {
            collect_type_vars(t, &mut out);
        }
    }
    if let Some(r) = &f.ret {
        collect_type_vars(r, &mut out);
    }
    out
}

/// Collect the names of every free (`Expr::Call`) function this block calls,
/// recursively — used to discover which generic helpers transitively reach a
/// bounded template (and so themselves need monomorphization).
fn collect_call_names(b: &Block, out: &mut HashSet<String>) {
    fn walk(e: &Expr, out: &mut HashSet<String>) {
        match e {
            Expr::Call { name, args } => {
                out.insert(name.clone());
                for a in args {
                    walk(a, out);
                }
            }
            Expr::LabeledCall { name, args } => {
                out.insert(name.clone());
                for (_, a) in args {
                    walk(a, out);
                }
            }
            Expr::Apply { func, args } => {
                walk(func, out);
                for a in args {
                    walk(a, out);
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                walk(receiver, out);
                for a in args {
                    walk(a, out);
                }
            }
            Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. }
            | Expr::List(args) | Expr::Tuple(args) => {
                for a in args {
                    walk(a, out);
                }
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. }
            | Expr::Field { base: expr, .. } => walk(expr, out),
            Expr::Binary { lhs, rhs, .. } => {
                walk(lhs, out);
                walk(rhs, out);
            }
            Expr::Range { lo, hi, .. } => {
                walk(lo, out);
                walk(hi, out);
            }
            Expr::Index { base, index } => {
                walk(base, out);
                walk(index, out);
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields {
                    walk(v, out);
                }
                if let Some(sp) = spread {
                    walk(sp, out);
                }
            }
            Expr::RecordUpdate { name: _, base, fields } => {
                walk(base, out);
                for (_, v) in fields {
                    walk(v, out);
                }
            }
            Expr::If { cond, then_block, else_block } => {
                walk(cond, out);
                collect_call_names(then_block, out);
                if let Some(b) = else_block {
                    collect_call_names(b, out);
                }
            }
            Expr::Match { scrutinee, arms } => {
                walk(scrutinee, out);
                for a in arms {
                    if let Some(g) = &a.guard {
                        walk(g, out);
                    }
                    walk(&a.body, out);
                }
            }
            Expr::While { cond, body } => {
                walk(cond, out);
                collect_call_names(body, out);
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                walk(scrutinee, out);
                collect_call_names(body, out);
            }
            Expr::For { iter, body, .. } => {
                walk(iter, out);
                collect_call_names(body, out);
            }
            Expr::Lambda { body, .. } | Expr::Block(body) => collect_call_names(body, out),
            Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_)
            | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
        }
    }
    for st in &b.stmts {
        match st {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Expr(value)
            | Stmt::Yield(value) => walk(value, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

/// The set of function names that must be monomorphized WITHOUT a generic
/// fallback: the `where`-bounded templates, plus every generic helper that
/// (transitively) calls one. A bounded call's obligation can only be discharged
/// once the CALLER's type variables are concrete, so a generic function that
/// contains such a call cannot run generically — it propagates the need for
/// specialization up to its own concrete call sites (RFC-0046 §2). Closed to a
/// fixpoint over the call graph.
fn no_fallback_template_names(items: &[Item]) -> HashSet<String> {
    let mut names: HashSet<String> = items
        .iter()
        .filter_map(|it| match it {
            Item::Function(f) if !f.bounds.is_empty() && !crate::typeck::intrinsic(&f.name) => {
                Some(f.name.clone())
            }
            _ => None,
        })
        .collect();
    loop {
        let mut added = false;
        for it in items {
            if let Item::Function(f) = it {
                if names.contains(&f.name)
                    || crate::typeck::intrinsic(&f.name)
                    || signature_type_vars(f).is_empty()
                {
                    continue;
                }
                let mut calls = HashSet::new();
                collect_call_names(&f.body, &mut calls);
                if calls.iter().any(|n| names.contains(n)) {
                    names.insert(f.name.clone());
                    added = true;
                }
            }
        }
        if !added {
            break;
        }
    }
    names
}

/// The type variables a function is generic over: its `where`-bound variables if
/// bounded, otherwise the free type variables in its signature.
/// Rename bare calls per `renames`, recursively — the substitution-directed
/// dispatch rewrite over a specialization's body. `scope` tracks the names bound
/// as locals (params, `let`s, `for`/pattern binders, lambda params): a call on a
/// bound local is a first-class function invocation and is NEVER renamed, even
/// when its name collides with a `renames` key. Without this guard, a `where`-
/// bounded generic's own `fn`-typed parameter named like a trait method (a
/// comparator `less`, `eq`, …) would be silently rewritten to the trait impl,
/// discarding the passed function and computing the wrong answer (BUG-001).
type Renames = HashMap<(String, String), String>;
type ReceiverResolver<'a> = dyn Fn(&Expr, &Scope<Type>) -> Option<Type> + 'a;

fn type_head_key(ty: &Type) -> Option<String> {
    match ty.unqualified() {
        Type::Named(name, _) => Some(name.clone()),
        Type::Tuple(items) => Some(format!("Tuple{}", items.len())),
        Type::Fn(_, _, _) => None,
        Type::Qualified(_, _) => unreachable!("unqualified strips qualifiers"),
    }
}

/// Resolve `method` to its specialized impl for a call whose first argument (the
/// receiver, for the UFCS-lowered instance methods that get renamed) has head
/// type `recv`. When the receiver type is known, key on it exactly; otherwise
/// fall back to the method's unique target if there is only one across all bound
/// types (the single-bound case). Ambiguous with an unknown receiver → leave it.
fn pick_rename<'a>(
    renames: &'a Renames,
    method: &str,
    recv: Option<&Type>,
) -> Option<&'a String> {
    if let Some(recv) = recv {
        let key = type_key(recv.unqualified());
        if let Some(t) = renames.get(&(key, method.to_string())) {
            return Some(t);
        }
        if let Some(head) = type_head_key(recv) {
            if let Some(t) = renames.get(&(head, method.to_string())) {
                return Some(t);
            }
        }
    }
    let mut matches = renames.iter().filter(|((_, m), _)| m == method);
    let first = matches.next()?;
    matches.next().is_none().then_some(first.1)
}

struct RenameCallContext<'a> {
    renames: &'a Renames,
    resolve: &'a ReceiverResolver<'a>,
    ctor_infos: &'a HashMap<String, CtorInfo>,
}

fn rename_calls_block(
    b: &mut Block,
    scope: &mut Scope<Type>,
    ctx: &RenameCallContext<'_>,
) {
    fn walk_expr(e: &mut Expr, scope: &mut Scope<Type>, ctx: &RenameCallContext<'_>) {
        match e {
            Expr::Call { name, args } => {
                // A call on a bound LOCAL (a `fn`-typed parameter or `let` named
                // like a trait method) is a first-class invocation, so it is never
                // substituted to the impl (BUG-001). The receiver is the first
                // argument (UFCS-lowered `x.tag()` -> `tag(x)`); dispatch on its
                // concrete type so each same-trait bound picks its own impl.
                if !scope.is_local(name) {
                    let recv = args.first().and_then(|a| (ctx.resolve)(a, scope));
                    if let Some(to) = pick_rename(ctx.renames, name, recv.as_ref()) {
                        *name = to.clone();
                    }
                }
                for a in args {
                    walk_expr(a, scope, ctx);
                }
            }
            Expr::LabeledCall { name, args } => {
                if !scope.is_local(name) {
                    let recv = args.first().and_then(|(_, a)| (ctx.resolve)(a, scope));
                    if let Some(to) = pick_rename(ctx.renames, name, recv.as_ref()) {
                        *name = to.clone();
                    }
                }
                for (_, a) in args {
                    walk_expr(a, scope, ctx);
                }
            }
            Expr::Apply { func, args } => {
                walk_expr(func, scope, ctx);
                for a in args {
                    walk_expr(a, scope, ctx);
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                walk_expr(receiver, scope, ctx);
                for a in args {
                    walk_expr(a, scope, ctx);
                }
            }
            Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. }
            | Expr::List(args) | Expr::Tuple(args) => {
                for a in args {
                    walk_expr(a, scope, ctx);
                }
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. }
            | Expr::Field { base: expr, .. } => walk_expr(expr, scope, ctx),
            Expr::Binary { lhs, rhs, .. } => {
                walk_expr(lhs, scope, ctx);
                walk_expr(rhs, scope, ctx);
            }
            Expr::Range { lo, hi, .. } => {
                walk_expr(lo, scope, ctx);
                walk_expr(hi, scope, ctx);
            }
            Expr::Index { base, index } => {
                walk_expr(base, scope, ctx);
                walk_expr(index, scope, ctx);
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields {
                    walk_expr(v, scope, ctx);
                }
                if let Some(sp) = spread {
                    walk_expr(sp, scope, ctx);
                }
            }
            Expr::RecordUpdate { name: _, base, fields } => {
                walk_expr(base, scope, ctx);
                for (_, v) in fields {
                    walk_expr(v, scope, ctx);
                }
            }
            Expr::If { cond, then_block, else_block } => {
                walk_expr(cond, scope, ctx);
                rename_calls_block(then_block, &mut scope.clone(), ctx);
                if let Some(b) = else_block {
                    rename_calls_block(b, &mut scope.clone(), ctx);
                }
            }
            Expr::Match { scrutinee, arms } => {
                walk_expr(scrutinee, scope, ctx);
                let scrutinee_ty = (ctx.resolve)(scrutinee, scope);
                for a in arms {
                    let mut s = scope.clone();
                    bind_typed_pattern(
                        &a.pattern,
                        ctx.ctor_infos,
                        scrutinee_ty.as_ref(),
                        &mut s,
                    );
                    if let Some(g) = &mut a.guard {
                        walk_expr(g, &mut s, ctx);
                    }
                    walk_expr(&mut a.body, &mut s, ctx);
                }
            }
            Expr::While { cond, body } => {
                walk_expr(cond, scope, ctx);
                rename_calls_block(body, &mut scope.clone(), ctx);
            }
            Expr::WhileLet { pattern, scrutinee, body } => {
                walk_expr(scrutinee, scope, ctx);
                let mut s = scope.clone();
                let scrutinee_ty = (ctx.resolve)(scrutinee, scope);
                bind_typed_pattern(pattern, ctx.ctor_infos, scrutinee_ty.as_ref(), &mut s);
                rename_calls_block(body, &mut s, ctx);
            }
            Expr::For { var, iter, body } => {
                walk_expr(iter, scope, ctx);
                let mut s = scope.clone();
                match (ctx.resolve)(iter, scope).as_ref().and_then(iterable_item_type) {
                    Some(item_ty) => s.insert(var.clone(), item_ty),
                    None => s.bind_local(var),
                }
                rename_calls_block(body, &mut s, ctx);
            }
            Expr::Lambda { params, body, .. } => {
                let mut s = scope.clone();
                seed_typed_params(params, &mut s);
                rename_calls_block(body, &mut s, ctx);
            }
            Expr::Block(body) => rename_calls_block(body, &mut scope.clone(), ctx),
            Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_)
            | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
        }
    }
    for st in &mut b.stmts {
        match st {
            Stmt::Let { name, ty, value, mutable } => {
                walk_expr(value, scope, ctx);
                // A `let less = …` shadows a same-named trait method for the rest
                // of the block, so a later `less(…)` is its value, not a rename.
                let resolved = ty.clone().or_else(|| (ctx.resolve)(value, scope));
                match (resolved, *mutable) {
                    (Some(ty), true) => scope.insert_mut(name.clone(), ty),
                    (Some(ty), false) => scope.insert(name.clone(), ty),
                    (None, true) => scope.bind_local_mut(name),
                    (None, false) => scope.bind_local(name),
                }
            }
            Stmt::LetPattern { pattern, value } => {
                walk_expr(value, scope, ctx);
                let value_ty = (ctx.resolve)(value, scope);
                bind_typed_pattern(pattern, ctx.ctor_infos, value_ty.as_ref(), scope);
            }
            Stmt::Assign { name, value } => {
                walk_expr(value, scope, ctx);
                if let Some(ty) = (ctx.resolve)(value, scope) {
                    scope.insert(name.clone(), ty);
                }
            }
            Stmt::Return(Some(value))
            | Stmt::Expr(value)
            | Stmt::Yield(value) => walk_expr(value, scope, ctx),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn type_var_list(f: &Function) -> Vec<String> {
    if f.bounds.is_empty() {
        signature_type_vars(f)
    } else {
        // Bound variables first, then any other signature variables — a
        // bounded fn can be generic over more than its bounds (`collect`
        // is bounded on its RESULT `c` and free in its element `a`).
        let mut vars: Vec<String> = f.bounds.iter().map(|(v, _, _)| v.clone()).collect();
        for v in signature_type_vars(f) {
            if !vars.contains(&v) {
                vars.push(v);
            }
        }
        vars
    }
}

fn type_args_from_receiver(template: &Function, concrete_receiver: &Type) -> Option<Vec<Type>> {
    let recv = template.params.first()?.ty.as_ref()?;
    let mut bindings = HashMap::new();
    if !bind_ast_type_vars(recv, concrete_receiver, &mut bindings) {
        return None;
    }
    type_var_list(template)
        .into_iter()
        .map(|var| bindings.get(&var).cloned())
        .collect()
}

/// (RFC-0053) Whether a concrete type should render through `Show` instead of
/// interpolation's structural fallback. Primitive `Show` impls are byte-identical
/// to structural rendering, so they stay on the render intrinsic. `Set` is
/// different even over primitives (`Set([1, 2])` structurally versus `{1, 2}`
/// through `Show`).
fn render_needs_show(ty: &crate::typeck::Ty, show_types: &HashSet<String>) -> bool {
    use crate::typeck::Ty;
    match ty {
        Ty::Duration => true,
        Ty::List(elem) => render_needs_show(elem, show_types),
        Ty::Tuple(slots) => slots.iter().any(|slot| render_needs_show(slot, show_types)),
        Ty::Named(name, args) => {
            let bare = name.rsplit_once('.').map_or(name.as_str(), |(_, s)| s);
            match bare {
                "Option" | "Result" | "Dict" => {
                    args.iter().any(|arg| render_needs_show(arg, show_types))
                }
                "Set" => true,
                _ => show_types.contains(bare),
            }
        }
        _ => false,
    }
}

#[derive(Clone)]
struct FromConversion {
    src: Type,
    dst: Type,
    func: String,
}

fn build_from_conversions(
    items: &[Item],
    from_conversion_fns: &HashSet<String>,
) -> Vec<FromConversion> {
    items
        .iter()
        .filter_map(|it| match it {
            Item::Function(f) if from_conversion_fns.contains(&f.name) && f.params.len() == 1 => {
                Some(FromConversion {
                    src: f.params[0].ty.clone()?,
                    dst: f.ret.clone()?,
                    func: f.name.clone(),
                })
            }
            _ => None,
        })
        .collect()
}

fn result_error_from_ret(ret: &Option<Type>) -> Option<Type> {
    match ret {
        Some(Type::Named(n, args)) if n == "Result" && args.len() == 2 => Some(args[1].clone()),
        _ => None,
    }
}

fn result_error_from_ty(ty: &crate::typeck::Ty) -> Option<Type> {
    match crate::typeck::ty_to_ast(ty)? {
        Type::Named(n, args) if n == "Result" && args.len() == 2 => Some(args[1].clone()),
        _ => None,
    }
}

fn rewrite_try_from_conversions(
    items: &mut [Item],
    table: &crate::typeck::TypeTable,
    from_conversion_fns: &HashSet<String>,
) {
    let conversions = build_from_conversions(items, from_conversion_fns);
    if conversions.is_empty() {
        return;
    }
    for item in items {
        let Item::Function(f) = item else { continue };
        let Some(dst_err) = result_error_from_ret(&f.ret) else { continue };
        rewrite_try_from_block(&mut f.body, &dst_err, &conversions, table);
    }
}

fn rewrite_try_from_block(
    block: &mut Block,
    dst_err: &Type,
    conversions: &[FromConversion],
    table: &crate::typeck::TypeTable,
) {
    for stmt in &mut block.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Yield(value)
            | Stmt::Expr(value) => rewrite_try_from_expr(value, dst_err, conversions, table),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn rewrite_try_from_expr(
    expr: &mut Expr,
    dst_err: &Type,
    conversions: &[FromConversion],
    table: &crate::typeck::TypeTable,
) {
    match expr {
        Expr::List(xs) | Expr::Tuple(xs) | Expr::Ctor { args: xs, .. }
        | Expr::AnonCtor { args: xs, .. }
        | Expr::Call { args: xs, .. } => {
            for x in xs {
                rewrite_try_from_expr(x, dst_err, conversions, table);
            }
        }
        Expr::Apply { func, args } => {
            rewrite_try_from_expr(func, dst_err, conversions, table);
            for arg in args {
                rewrite_try_from_expr(arg, dst_err, conversions, table);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            rewrite_try_from_expr(receiver, dst_err, conversions, table);
            for arg in args {
                rewrite_try_from_expr(arg, dst_err, conversions, table);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_try_from_expr(lhs, dst_err, conversions, table);
            rewrite_try_from_expr(rhs, dst_err, conversions, table);
        }
        Expr::Unary { expr, .. } | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            rewrite_try_from_expr(expr, dst_err, conversions, table);
        }
        Expr::Try(inner) => {
            rewrite_try_from_expr(inner, dst_err, conversions, table);
            let Some(src_err) = table.type_of(inner).and_then(result_error_from_ty) else {
                return;
            };
            if &src_err == dst_err {
                return;
            }
            let Some(conv) = conversions.iter().find(|c| c.src == src_err && c.dst == *dst_err) else {
                return;
            };
            let err = "__try_err".to_string();
            let operand = std::mem::replace(inner.as_mut(), Expr::Bool(false));
            **inner = Expr::Call {
                name: "result.map_err".to_string(),
                args: vec![
                    operand,
                    Expr::Lambda {
                        params: vec![Param {
                            name: err.clone(),
                            ty: Some(src_err),
                            convention: Convention::default(),
                            default: None,
                        }],
                        body: Block {
                            stmts: vec![Stmt::Expr(Expr::Call {
                                name: conv.func.clone(),
                                args: vec![Expr::Var(err)],
                            })],
                            lines: vec![0],
                            region: None,
                        },
                        ret: Some(dst_err.clone()),
                    },
                ],
            };
        }
        Expr::Range { lo, hi, .. } => {
            rewrite_try_from_expr(lo, dst_err, conversions, table);
            rewrite_try_from_expr(hi, dst_err, conversions, table);
        }
        Expr::Index { base, index } => {
            rewrite_try_from_expr(base, dst_err, conversions, table);
            rewrite_try_from_expr(index, dst_err, conversions, table);
        }
        Expr::LabeledCall { .. } => {
            unreachable!("RFC-0056: labeled calls are lowered to positional Call before traits")
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                rewrite_try_from_expr(value, dst_err, conversions, table);
            }
            if let Some(spread) = spread {
                rewrite_try_from_expr(spread, dst_err, conversions, table);
            }
        }
        Expr::RecordUpdate { base, fields, .. } => {
            rewrite_try_from_expr(base, dst_err, conversions, table);
            for (_, value) in fields {
                rewrite_try_from_expr(value, dst_err, conversions, table);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            rewrite_try_from_expr(cond, dst_err, conversions, table);
            rewrite_try_from_block(then_block, dst_err, conversions, table);
            if let Some(block) = else_block {
                rewrite_try_from_block(block, dst_err, conversions, table);
            }
        }
        Expr::Match { scrutinee, arms } => {
            rewrite_try_from_expr(scrutinee, dst_err, conversions, table);
            for arm in arms {
                if let Some(guard) = &mut arm.guard {
                    rewrite_try_from_expr(guard, dst_err, conversions, table);
                }
                rewrite_try_from_expr(&mut arm.body, dst_err, conversions, table);
            }
        }
        Expr::While { cond, body } => {
            rewrite_try_from_expr(cond, dst_err, conversions, table);
            rewrite_try_from_block(body, dst_err, conversions, table);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            rewrite_try_from_expr(scrutinee, dst_err, conversions, table);
            rewrite_try_from_block(body, dst_err, conversions, table);
        }
        Expr::For { iter, body, .. } => {
            rewrite_try_from_expr(iter, dst_err, conversions, table);
            rewrite_try_from_block(body, dst_err, conversions, table);
        }
        Expr::Lambda { body, ret, .. } => {
            if let Some(lambda_dst) = result_error_from_ret(ret) {
                rewrite_try_from_block(body, &lambda_dst, conversions, table);
            }
        }
        Expr::Block(body) => {
            rewrite_try_from_block(body, dst_err, conversions, table);
        }
        Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_)
        | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
    }
}

/// Monomorphizes generic functions: each call to a template with a determinable
/// concrete type for its type variable(s) is rewritten to a per-instantiation
/// specialization (`max_of__Int`), generated by substituting the type variables
/// in the clone's signature. The body is otherwise unchanged, so once its
/// parameters are concrete, codegen infers the right representation (e.g. i64 for
/// `Int`, content `==` for `String`) and the trait-resolution pass resolves any
/// trait-method calls inside it. Covers both `where`-bounded generics and
/// unbounded ones specialized on a primitive type argument.
struct Mono<'a> {
    templates: &'a HashMap<String, Function>,
    /// Every function name in the module — a dispatch rewrite only fires
    /// when its target actually exists (a missing impl stays a bare call,
    /// which the post-mono pass diagnoses properly).
    known_fns: &'a HashSet<String>,
    /// method name -> owning trait(s), and the concrete impl methods available
    /// for substitution-directed dispatch through bounds.
    trait_methods: &'a HashMap<String, Vec<TraitMethodInfo>>,
    /// trait -> its transitive supertraits, so a `where a: Ord` bound discharges
    /// the methods of `Eq`/`PartialOrd`/`PartialEq` too.
    supertraits: &'a HashMap<String, Vec<String>>,
    trait_impl_table: &'a TraitImplTable,
    /// Loud failures (an uninferrable bounded call) surfaced as check errors.
    diagnostics: Vec<String>,
    ctor_infos: &'a HashMap<String, CtorInfo>,
    record_fields: &'a HashMap<String, Vec<(String, Type)>>,
    /// Function -> (param types, return type): the declared signatures behind
    /// `declared_expr_type` for expressions the table has no entry for
    /// (freshly-generated specialization bodies within a round).
    fn_sigs: HashMap<String, FnSig>,
    memo: HashMap<(String, Vec<String>), String>,
    generated: Vec<Function>,
    /// typeck's resolved types for this module instance. Generated bodies have
    /// no entries until the next fixpoint round, so declarations and typed local
    /// scopes provide their structured fallback.
    table: &'a crate::typeck::TypeTable,
    /// Names of the template functions that are kept in `items` ONLY so the
    /// fixpoint re-annotate can see their signatures (bounded templates + the
    /// generic helpers that transitively call them). Their bodies are still
    /// generic — walking them would try, and fail, to resolve their own bounded
    /// calls — so they are skipped here and removed from the module after the
    /// fixpoint. Their concrete SPECIALIZATIONS (in `generated`) are walked.
    skip_walk: &'a HashSet<String>,
    /// (RFC-0053) Bare type names carrying a `Show` impl. The typed interpolation
    /// rewrite uses this to route values with a meaningful display protocol
    /// through `show.render`.
    show_types: &'a HashSet<String>,
    /// Whether `show.render` is linked as a monomorphizable template. Production
    /// linking guarantees it; direct stage callers still avoid a dangling call.
    render_available: bool,
}

impl Mono<'_> {
    fn run(&mut self, items: &mut [Item]) {
        for item in items.iter_mut() {
            if let Item::Function(f) = item {
                if self.skip_walk.contains(&f.name) {
                    continue;
                }
                let mut s = Scope::new();
                seed_typed_params(&f.params, &mut s);
                self.walk_block(&mut f.body, &mut s);
            }
        }
        // A specialization may itself call a template; walk the bodies we
        // generate (the list grows as we go).
        let mut i = 0;
        while i < self.generated.len() {
            let params = self.generated[i].params.clone();
            let mut body = std::mem::replace(
                &mut self.generated[i].body,
                Block { stmts: Vec::new(), lines: Vec::new(), region: None },
            );
            let mut s = Scope::new();
            seed_typed_params(&params, &mut s);
            self.walk_block(&mut body, &mut s);
            self.generated[i].body = body;
            i += 1;
        }
    }

    fn type_ast(&self, e: &Expr, scope: &Scope<Type>) -> Option<Type> {
        table_ast_type(self.table, e)
            .or_else(|| declared_expr_type(e, &self.fn_sigs, &|arg| self.type_ast(arg, scope)))
            .or_else(|| {
                local_expr_type(
                    e,
                    scope,
                    self.ctor_infos,
                    &self.fn_sigs,
                    self.record_fields,
                    &|arg| self.type_ast(arg, scope),
                )
            })
            .or_else(|| cap_op_result_type(e, &|arg| self.type_ast(arg, scope)))
    }

    fn refine_var_call_args(&self, name: &str, args: &[Expr], scope: &mut Scope<Type>) {
        let Some((params, _, conventions)) = self.fn_sigs.get(name) else { return };
        let mut bindings = HashMap::new();
        for (param, arg) in params.iter().zip(args) {
            let (Some(pattern), Some(actual)) = (param, self.type_ast(arg, scope)) else {
                continue;
            };
            let _ = bind_ast_type_vars(pattern, &actual, &mut bindings);
        }
        for ((param, convention), arg) in params.iter().zip(conventions).zip(args) {
            if *convention != Convention::Var {
                continue;
            }
            let (Some(pattern), Expr::Var(binding)) = (param, arg) else { continue };
            let refined = subst_trait_params(pattern, &bindings);
            refine_ast_scope_type(scope, binding, &refined);
        }
    }

    fn resolve_type_args(
        &self,
        template: &Function,
        args: &[Expr],
        scope: &Scope<Type>,
        result_ty: Option<&Type>,
    ) -> Option<Vec<Type>> {
        let bounded = !template.bounds.is_empty();
        let mut bindings = HashMap::new();
        let mut table_confirmed = HashSet::new();

        for (param, arg) in template.params.iter().zip(args) {
            let Some(pattern) = &param.ty else { continue };
            let from_table = table_ast_type(self.table, arg);
            let actual = from_table.clone().or_else(|| self.type_ast(arg, scope));
            let Some(actual) = actual else { continue };
            if !bind_ast_type_vars(pattern, &actual, &mut bindings) {
                return None;
            }
            if let Some(table_ty) = from_table {
                let mut confirmed = HashMap::new();
                if bind_ast_type_vars(pattern, &table_ty, &mut confirmed) {
                    table_confirmed.extend(confirmed.into_keys());
                }
            }
        }

        if let (Some(ret), Some(actual)) = (&template.ret, result_ty) {
            if !bind_ast_type_vars(ret, actual, &mut bindings) {
                return None;
            }
            let mut confirmed = HashMap::new();
            if bind_ast_type_vars(ret, actual, &mut confirmed) {
                table_confirmed.extend(confirmed.into_keys());
            }
        }

        type_var_list(template)
            .into_iter()
            .map(|var| {
                let ty = bindings.get(&var)?.clone();
                let mut unresolved = Vec::new();
                collect_type_vars(&ty, &mut unresolved);
                if !unresolved.is_empty() {
                    return None;
                }
                let key = type_key(ty.unqualified());
                if !bounded
                    && !table_confirmed.contains(&var)
                    && !is_specializable_type_arg(&key)
                {
                    return None;
                }
                Some(ty)
            })
            .collect()
    }

    fn specialize(&mut self, name: &str, type_args: Vec<Type>) -> String {
        let type_keys = type_args
            .iter()
            .map(|ty| type_key(ty.unqualified()))
            .collect::<Vec<_>>();
        let key = (name.to_string(), type_keys.clone());
        if let Some(m) = self.memo.get(&key) {
            return m.clone();
        }
        // The canonical type rendering is emitted only as an identifier key; it
        // is never parsed back into a type.
        let safe: Vec<String> = type_keys
            .iter()
            .map(|key| mangle_type_key(key))
            .collect();
        let mangled = format!("{name}__{}", safe.join("__"));
        self.memo.insert(key, mangled.clone());

        let mut f = self.templates[name].clone();
        f.name = mangled.clone();
        // Substitute over the same variable list `resolve_type_args` resolved:
        // the `where`-bound variables for a bounded generic, otherwise the free
        // type variables of the signature.
        let subst: HashMap<String, Type> = type_var_list(&f).into_iter().zip(type_args).collect();
        for p in &mut f.params {
            if let Some(t) = &p.ty {
                p.ty = Some(subst_trait_params(t, &subst));
            }
        }
        f.ret = f.ret.as_ref().map(|t| subst_trait_params(t, &subst));
        if let Some(ret) = &f.ret {
            let params = f.params.iter().map(|param| param.ty.clone()).collect();
            let conventions = f.params.iter().map(|param| param.convention).collect();
            self.fn_sigs
                .insert(mangled.clone(), (params, ret.clone(), conventions));
        }
        // Substitute the body's type ANNOTATIONS too (`var items: List(a) = []`,
        // `x as T`), so a specialization's body type-checks at the concrete type.
        subst_block_types(&mut f.body, &subst);
        // Substitution-directed trait dispatch: a bound variable's trait
        // methods resolve by the SUBSTITUTED type — not by any argument — so a
        // constructor-style method (`from_iter`, which mentions its bound
        // variable only in the RESULT) dispatches correctly, and the impl's
        // own generic method is specialized at the bound's type arguments.
        let trait_method_pairs: Vec<(String, String)> = self
            .trait_methods
            .iter()
            .flat_map(|(m, infos)| infos.iter().map(|info| (m.clone(), info.owner.clone())))
            .collect();
        let bounds_snapshot = f.bounds.clone();
        // Keyed by (concrete receiver-type head, method) — NOT method alone — so
        // two same-trait bounds (`where a: Named, b: Named`) each rewrite their own
        // variable's calls to their own impl instead of the last bound clobbering
        // the target for every call site (BUG-298).
        let mut renames: HashMap<(String, String), String> = HashMap::new();
        for (bvar, btrait, btargs) in &bounds_snapshot {
            let Some(concrete) = subst.get(bvar.as_str()) else { continue };
            let Some(head) = type_head_key(concrete) else { continue };
            for (method, owner) in &trait_method_pairs {
                // The bound discharges its own trait's methods AND those of every
                // supertrait (a `where a: Ord` bound also supplies `eq`/`less`).
                let owned_by_bound = owner == btrait
                    || self.supertraits.get(btrait).is_some_and(|s| s.contains(owner));
                if !owned_by_bound {
                    continue;
                }
                // The impl that defines this method is registered under its actual
                // owning trait. Parameterized traits can have several impls for
                // the same receiver head (`From(JsonError) for String`,
                // `From(TomlDecodeError) for String`), so choose the candidate
                // whose trait arguments match the substituted bound arguments.
                let mut candidates = self
                    .trait_impl_table
                    .get(&(owner.clone(), method.clone(), head.clone()))
                    .cloned()
                    .unwrap_or_default();
                if candidates.is_empty() {
                    candidates = self
                        .trait_impl_table
                        .iter()
                        .filter(|((tr, m, k), _)| {
                            tr == owner
                                && m == method
                                && k.chars().next().is_some_and(char::is_lowercase)
                                && !k.contains('.')
                        })
                        .flat_map(|(_, methods)| methods.clone())
                        .collect();
                }
                for candidate in candidates {
                    let mangled = candidate.mangled;
                    // Bind the impl method's own type variables by STRUCTURAL
                    // matching: each impl trait-argument pattern against the
                    // bound's (substituted) concrete argument. Anything that
                    // doesn't bind falls back to the generic impl function.
                    let mut bound_map: HashMap<String, Type> = HashMap::new();
                    let mut ok = candidate.trait_args.len() == btargs.len();
                    if ok {
                        for (pat, targ) in candidate.trait_args.iter().zip(btargs) {
                            let concrete_arg = subst_trait_params(targ, &subst);
                            if !bind_ast_type_vars(pat, &concrete_arg, &mut bound_map) {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if !ok {
                        continue;
                    }

                    let mut target = mangled.clone();
                    let Some(tmpl) = self.templates.get(&mangled).cloned() else {
                        if self.known_fns.contains(&mangled) {
                            renames.insert((head.clone(), method.clone()), mangled.clone());
                            renames.insert((head.clone(), static_bound_marker(bvar, method)), mangled);
                        }
                        continue;
                    };
                    if ok {
                        let mut targs_out: Vec<Type> = Vec::new();
                        for v in type_var_list(&tmpl) {
                            match bound_map.get(&v) {
                                Some(c) => targs_out.push(c.clone()),
                                None => {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        if ok && !targs_out.is_empty() {
                            target = self.specialize(&mangled, targs_out);
                        }
                    }
                    if target == mangled {
                        if let Some(targs_out) =
                            type_args_from_receiver(&tmpl, concrete)
                                .filter(|args| !args.is_empty())
                        {
                            target = self.specialize(&mangled, targs_out);
                        }
                    }
                    if target != mangled {
                        renames.insert((head.clone(), mangled.clone()), target.clone());
                    }
                    renames.insert((head.clone(), method.clone()), target.clone());
                    renames.insert((head.clone(), static_bound_marker(bvar, method)), target);
                }
            }
        }
        if !renames.is_empty() {
            // Seed the specialization's own parameters as bound locals, so a
            // `fn`-typed parameter named like a trait method (a comparator) is
            // invoked as the passed function, not rewritten to the impl (BUG-001).
            let mut rename_scope = Scope::new();
            seed_typed_params(&f.params, &mut rename_scope);
            // Resolve a call's receiver type through the checker's tables and make
            // the (possibly generic) result concrete with THIS specialization's
            // substitution — so a field-access receiver (`self.fst`) resolves to
            // its instantiated type and each same-trait bound dispatches to its
            // own impl (BUG-298).
            let this = &*self;
            let resolve = move |e: &Expr, sc: &Scope<Type>| this.type_ast(e, sc);
            let rename_ctx = RenameCallContext {
                renames: &renames,
                resolve: &resolve,
                ctor_infos: self.ctor_infos,
            };
            rename_calls_block(&mut f.body, &mut rename_scope, &rename_ctx);
        }
        // Monomorphization discharges the `where` bounds: every bound type
        // variable is now a concrete type, and the trait obligation is satisfied
        // by the impl whose method this specialization's body resolves to.
        // Clearing them lets the (fully concrete) specialization compile on the
        // compiled backend, which has no notion of an unsatisfied generic bound.
        f.bounds = Vec::new();
        self.generated.push(f);
        mangled
    }

    fn walk_block(&mut self, b: &mut Block, scope: &mut Scope<Type>) {
        for stmt in &mut b.stmts {
            match stmt {
                Stmt::Let { name, ty, value, mutable } => {
                    self.walk_expr(value, scope);
                    // Prefer the type ascription (`var items: List(a) = []`): it
                    // carries the element type an empty/ambiguous value loses. The
                    // value's inferred type is the fallback.
                    let resolved = ty.clone().or_else(|| self.type_ast(value, scope));
                    match (resolved, *mutable) {
                        (Some(t), true) => scope.insert_mut(name.clone(), t),
                        (Some(t), false) => scope.insert(name.clone(), t),
                        (None, true) => scope.bind_local_mut(name),
                        (None, false) => scope.bind_local(name),
                    }
                }
                Stmt::Assign { name, value } => {
                    self.walk_expr(value, scope);
                    if let Some(t) = self.type_ast(value, scope) {
                        scope.insert(name.clone(), t);
                    }
                }
                // `let PAT = t` seeds each destructured name from the value's type
                // so a destructured part monomorphizes (e.g. a tuple impl's
                // `reflect_one(x0)`). A tuple pattern recurses per slot; other
                // patterns clear their names (untyped) so a stale outer binding
                // doesn't leak in.
                Stmt::LetPattern { pattern, value } => {
                    self.walk_expr(value, scope);
                    let ty = self.type_ast(value, scope);
                    bind_typed_pattern(pattern, self.ctor_infos, ty.as_ref(), scope);
                }
                Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => self.walk_expr(e, scope),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    fn walk_expr(&mut self, e: &mut Expr, scope: &mut Scope<Type>) {
        let result_ty = table_ast_type(self.table, e);
        match e {
            Expr::Call { name, args } => {
                for a in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
                self.refine_var_call_args(name, args, scope);
                // (RFC-0053) Interpolation desugars to the internal render intrinsic,
                // the structural fallback. At this point monomorphization has concrete
                // type evidence for `x`, so values whose public display model is `Show`
                // route through `show.render` and then specialize like any other
                // bounded generic. Production linking always supplies `show`.
                if is_render_intrinsic(name) && args.len() == 1 {
                    if self.render_available {
                        if let Some(ty) = self.table.type_of(&args[0]) {
                            if render_needs_show(ty, self.show_types) {
                                *name = "show.render".to_string();
                            }
                        }
                    }
                }
                if let Some(template) = self.templates.get(name.as_str()).cloned() {
                    match self.resolve_type_args(&template, args, scope, result_ty.as_ref()) {
                        Some(type_args) => {
                            let subst: HashMap<String, Type> = type_var_list(&template)
                                .into_iter()
                                .zip(type_args.iter().cloned())
                                .collect();
                            for (param, arg) in template.params.iter().zip(args.iter()) {
                                if param.convention != Convention::Var {
                                    continue;
                                }
                                let (Some(pattern), Expr::Var(binding)) = (&param.ty, arg) else {
                                    continue;
                                };
                                let refined = subst_trait_params(pattern, &subst);
                                refine_ast_scope_type(scope, binding, &refined);
                            }
                            *name = self.specialize(name, type_args);
                        }
                        // A BOUNDED template has no generic fallback (its body
                        // can't compile unresolved), so failing to infer is an
                        // error — and for a result-position variable the fix
                        // is an ascription.
                        None if !template.bounds.is_empty() => {
                            if std::env::var_os("WITCHY_DEBUG_MONO").is_some() {
                                eprintln!(
                                    "mono: `{name}` unresolved; result_ty={:?}; vars={:?}",
                                    result_ty,
                                    type_var_list(&template)
                                );
                            }
                            self.diagnostics.push(format!(
                                "cannot infer the result type for `{name}` — give the \
                                 expected type, e.g. ascribe the binding \
                                 (`let x: List(Int) = {name}(…)`)"
                            ));
                        }
                        None => {}
                    }
                }
            }
            Expr::Apply { func, args } => {
                self.walk_expr(func, scope);
                for a in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
            }
            Expr::Ctor { args, .. }
            | Expr::AnonCtor { args, .. }
            | Expr::List(args)
            | Expr::Tuple(args) => {
                for a in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
                self.walk_expr(expr, scope)
            }
            Expr::RecordUpdate { name: _, base, fields } => {
                self.walk_expr(base, scope);
                for (_, v) in fields.iter_mut() {
                    self.walk_expr(v, scope);
                }
            }
            Expr::LabeledCall { args, .. } => {
                for (_, a) in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields.iter_mut() {
                    self.walk_expr(v, scope);
                }
                if let Some(s) = spread {
                    self.walk_expr(s, scope);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.walk_expr(lhs, scope);
                self.walk_expr(rhs, scope);
            }
            Expr::Range { lo, hi, .. } => {
                self.walk_expr(lo, scope);
                self.walk_expr(hi, scope);
            }
            Expr::Index { base, index } => {
                self.walk_expr(base, scope);
                self.walk_expr(index, scope);
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.walk_expr(receiver, scope);
                for a in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
            }
            Expr::WhileLet { pattern, scrutinee, body } => {
                self.walk_expr(scrutinee, scope);
                let mut s = scope.clone();
                let scrutinee_ty = self.type_ast(scrutinee, scope);
                bind_typed_pattern(
                    pattern,
                    self.ctor_infos,
                    scrutinee_ty.as_ref(),
                    &mut s,
                );
                self.walk_block(body, &mut s);
                merge_refined_outer_ast_types(scope, &s);
            }
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                self.walk_expr(cond, scope);
                let mut then_scope = scope.clone();
                self.walk_block(then_block, &mut then_scope);
                merge_refined_outer_ast_types(scope, &then_scope);
                if let Some(b) = else_block {
                    let mut else_scope = scope.clone();
                    self.walk_block(b, &mut else_scope);
                    merge_refined_outer_ast_types(scope, &else_scope);
                }
            }
            Expr::While { cond, body } => {
                self.walk_expr(cond, scope);
                let mut body_scope = scope.clone();
                self.walk_block(body, &mut body_scope);
                merge_refined_outer_ast_types(scope, &body_scope);
            }
            Expr::For { var, iter, body } => {
                self.walk_expr(iter, scope);
                let mut s = scope.clone();
                match self.type_ast(iter, scope).as_ref().and_then(iterable_item_type) {
                    Some(item_ty) => s.insert(var.clone(), item_ty),
                    None => s.bind_local(var),
                }
                self.walk_block(body, &mut s);
                merge_refined_outer_ast_types(scope, &s);
            }
            Expr::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee, scope);
                let scrutinee_ty = self.type_ast(scrutinee, scope);
                for arm in arms.iter_mut() {
                    let mut s = scope.clone();
                    bind_typed_pattern(
                        &arm.pattern,
                        self.ctor_infos,
                        scrutinee_ty.as_ref(),
                        &mut s,
                    );
                    if let Some(g) = &mut arm.guard {
                        self.walk_expr(g, &mut s);
                    }
                    self.walk_expr(&mut arm.body, &mut s);
                    merge_refined_outer_ast_types(scope, &s);
                }
            }
            Expr::Lambda { params, body, .. } => {
                let mut s = scope.clone();
                seed_typed_params(params, &mut s);
                self.walk_block(body, &mut s);
            }
            Expr::Block(b) => {
                let mut block_scope = scope.clone();
                self.walk_block(b, &mut block_scope);
                merge_refined_outer_ast_types(scope, &block_scope);
            }
            Expr::Var(_) | Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::TaggedLit { .. } => {}
        }
    }
}

#[cfg(test)]
mod structured_dispatch_tests {
    use super::*;

    fn nominal(name: &str, args: Vec<Type>) -> Type {
        Type::Named(name.to_string(), args)
    }

    #[test]
    fn declared_result_substitutes_nested_types_without_scope_strings() {
        let a = named_type("a");
        let mut signatures = HashMap::new();
        signatures.insert(
            "head".to_string(),
            (
                vec![Some(nominal("List", vec![a.clone()]))],
                nominal("Option", vec![nominal("List", vec![a])]),
                vec![Convention::Let],
            ),
        );
        let expression = Expr::Call {
            name: "head".to_string(),
            args: vec![Expr::Var("values".to_string())],
        };
        let actual = declared_expr_type(&expression, &signatures, &|expr| match expr {
            Expr::Var(name) if name == "values" => {
                Some(nominal("List", vec![named_type("String")]))
            }
            _ => None,
        });

        assert_eq!(
            actual,
            Some(nominal(
                "Option",
                vec![nominal("List", vec![named_type("String")])],
            ))
        );

        signatures.insert(
            "empty_dict".to_string(),
            (
                Vec::new(),
                nominal("Dict", vec![named_type("k"), named_type("v")]),
                Vec::new(),
            ),
        );
        let unbound = declared_expr_type(
            &Expr::Call { name: "empty_dict".to_string(), args: Vec::new() },
            &signatures,
            &|_| None,
        );
        assert_eq!(
            unbound,
            Some(nominal("Dict", vec![named_type("k"), named_type("v")]))
        );
    }

    #[test]
    fn local_record_and_constructor_types_keep_generic_arguments() {
        let mut infos = HashMap::new();
        infos.insert(
            "Box".to_string(),
            CtorInfo {
                owner: "Box".to_string(),
                params: vec!["a".to_string()],
                fields: vec![named_type("a")],
            },
        );
        let mut fields = HashMap::new();
        fields.insert(
            "Box".to_string(),
            vec![("value".to_string(), named_type("a"))],
        );
        let mut scope = Scope::new();
        scope.insert(
            "box".to_string(),
            nominal("Box", vec![nominal("List", vec![named_type("Int")])]),
        );
        let no_signatures = HashMap::new();
        let field = Expr::Field {
            base: Box::new(Expr::Var("box".to_string())),
            field: "value".to_string(),
        };
        let field_ty = local_expr_type(
            &field,
            &scope,
            &infos,
            &no_signatures,
            &fields,
            &|expr| match expr {
                Expr::Var(name) => scope.get(name).cloned(),
                _ => None,
            },
        );
        assert_eq!(field_ty, Some(nominal("List", vec![named_type("Int")])));

        let ctor = Expr::Ctor {
            name: "Box".to_string(),
            args: vec![Expr::Str("value".to_string())],
        };
        let ctor_ty = local_expr_type(
            &ctor,
            &scope,
            &infos,
            &no_signatures,
            &fields,
            &|expr| matches!(expr, Expr::Str(_)).then(|| named_type("String")),
        );
        assert_eq!(ctor_ty, Some(nominal("Box", vec![named_type("String")])));
    }

    #[test]
    fn constructor_pattern_propagates_nested_payload_types() {
        let mut infos = HashMap::new();
        infos.insert(
            "Wrapped".to_string(),
            CtorInfo {
                owner: "Envelope".to_string(),
                params: vec!["a".to_string()],
                fields: vec![nominal("List", vec![named_type("a")])],
            },
        );
        let pattern = Pattern::Ctor {
            name: "Wrapped".to_string(),
            args: vec![Pattern::List {
                elems: vec![Pattern::Var("first".to_string())],
                rest: Some(Some("rest".to_string())),
            }],
        };
        let expected = nominal("Envelope", vec![named_type("String")]);
        let mut scope = Scope::new();
        bind_typed_pattern(&pattern, &infos, Some(&expected), &mut scope);

        assert_eq!(scope.get("first"), Some(&named_type("String")));
        assert_eq!(
            scope.get("rest"),
            Some(&nominal("List", vec![named_type("String")]))
        );
    }

    #[test]
    fn structured_unification_discards_partial_failed_bindings() {
        let pattern = Type::Tuple(vec![named_type("a"), named_type("String")]);
        let concrete = Type::Tuple(vec![named_type("Int"), named_type("Bool")]);
        let mut bindings = HashMap::new();

        assert!(!bind_ast_type_vars(&pattern, &concrete, &mut bindings));
        assert!(bindings.is_empty());

        let pattern = nominal("Set", vec![named_type("a")]);
        assert!(bind_ast_type_vars(&pattern, &pattern, &mut bindings));
        assert!(bindings.is_empty(), "an unresolved actual is not evidence");
        assert!(bind_ast_type_vars(
            &pattern,
            &nominal("Set", vec![named_type("Float")]),
            &mut bindings,
        ));
        assert_eq!(bindings.get("a"), Some(&named_type("Float")));

        let empty_container = nominal("List", Vec::new());
        bindings.clear();
        assert!(bind_ast_type_vars(
            &nominal("List", vec![named_type("a")]),
            &empty_container,
            &mut bindings,
        ));
        assert!(bindings.is_empty(), "a bare container head is incomplete evidence");
    }

    #[test]
    fn terminal_type_keys_never_collapse_deep_distinct_types() {
        fn nested_list(mut inner: Type) -> Type {
            for _ in 0..64 {
                inner = nominal("List", vec![inner]);
            }
            inner
        }

        let ints = type_key(&nested_list(named_type("Int")));
        let strings = type_key(&nested_list(named_type("String")));

        assert_ne!(ints, strings);
        assert_eq!(ints.matches("List<").count(), 64);
        assert!(ints.ends_with(&format!("Int{}", ">".repeat(64))));
    }

    #[test]
    fn terminal_type_key_mangling_is_injective_for_old_collisions() {
        for (left, right) in [
            ("pkg.T", "pkg_T"),
            ("List<Int>", "List_Int_"),
            ("Tuple2<Int,String>", "Tuple2_Int_String_"),
        ] {
            assert_ne!(mangle_type_key(left), mangle_type_key(right));
        }

        let qualified = mangle(
            Some("From"),
            &[named_type("pkg.T")],
            "Target",
            "from",
        );
        let underscored = mangle(
            Some("From"),
            &[named_type("pkg_T")],
            "Target",
            "from",
        );
        assert_ne!(qualified, underscored);
    }
}
