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

use std::collections::{HashMap, HashSet};

use crate::ast::*;

/// Mangled name for an impl method: `Trait__Type__method`.
fn mangle(trait_name: Option<&str>, type_name: &str, method: &str) -> String {
    match trait_name {
        Some(t) => format!("{t}__{type_name}__{method}"),
        // Inherent method: no trait segment, still dispatched by receiver type.
        None => format!("{type_name}__{method}"),
    }
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
            p.ty = Some(subst_self(t, type_name));
        }
    }
    if let Some(first) = params.first_mut() {
        if first.ty.is_none() {
            first.ty = Some(self_ty);
        }
    }
    let ret = ret.map(|t| subst_self(&t, type_name));
    Function {
        public: true,
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

/// Replace every `Self` in a type with the implementing type.
fn subst_self(t: &Type, impl_type: &str) -> Type {
    match t {
        Type::Named(n, args) if n == "Self" && args.is_empty() => {
            Type::Named(impl_type.to_string(), vec![])
        }
        Type::Named(n, args) => {
            Type::Named(n.clone(), args.iter().map(|a| subst_self(a, impl_type)).collect())
        }
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|a| subst_self(a, impl_type)).collect()),
        Type::Fn(ps, r) => Type::Fn(
            ps.iter().map(|a| subst_self(a, impl_type)).collect(),
            Box::new(subst_self(r, impl_type)),
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

fn lower_with(module: Module, mono_unbounded: bool) -> (Module, Vec<String>) {
    // Expand type aliases and inline module-level constants first (a no-op once
    // the linker has done so, but covers single-module paths like `check_str`).
    let module = crate::aliases::resolve(crate::consts::inline(module));
    let needs_lowering = module.items.iter().any(|it| {
        matches!(it, Item::Trait(_) | Item::Impl(_))
            || matches!(it, Item::Function(f) if !f.bounds.is_empty()
                || (mono_unbounded && !signature_type_vars(f).is_empty()))
    }) || module_needs_lowering(&module.items);
    if !needs_lowering {
        return (module, Vec::new());
    }

    // method name -> owning trait (increment 1 assumes a method name is unique
    // across traits), and each trait's full method list (for default bodies).
    let mut trait_methods: HashMap<String, String> = HashMap::new();
    let mut trait_method_list: HashMap<String, Vec<MethodSig>> = HashMap::new();
    // Trait methods whose first parameter is NOT `self` are STATIC (`From::from`,
    // `FromIterator::from_iter`): a call on a bound type variable (`b.from(x)`)
    // takes no receiver — the receiver IS the type, resolved via the bound at
    // monomorphization. Tracked so the generic-receiver dispatch doesn't prepend a
    // phantom `self`.
    let mut static_trait_methods: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in &module.items {
        if let Item::Trait(t) = item {
            for m in &t.methods {
                trait_methods.insert(m.name.clone(), t.name.clone());
                if m.params.first().is_none_or(|p| p.name != "self") {
                    static_trait_methods.insert(m.name.clone());
                }
            }
            trait_method_list.insert(t.name.clone(), t.methods.clone());
        }
    }

    // (method name, receiver type) -> mangled function, plus the generated
    // functions themselves (impl methods with `self` typed to the impl type).
    let mut impl_table: HashMap<(String, String), String> = HashMap::new();
    // (trait name, impl head) -> the impl's trait type-arguments
    // (`impl FromIterator(a) for List(a)` registers ("FromIterator","List")
    // -> [a]) — the variable map for substitution-directed dispatch.
    let mut impl_trait_args: HashMap<(String, String), Vec<Type>> = HashMap::new();
    // (type name, method name) -> mangled fn, for self-less impl methods.
    let mut statics: HashMap<(String, String), String> = HashMap::new();
    let mut generated: Vec<Function> = Vec::new();
    for item in &module.items {
        if let Item::Impl(im) = item {
            if let Some(t) = &im.trait_name {
                if !im.trait_args.is_empty() {
                    impl_trait_args
                        .insert((t.clone(), im.type_name.clone()), im.trait_args.clone());
                }
            }
            let provided: HashSet<&str> = im.methods.iter().map(|m| m.name.as_str()).collect();
            // Methods the impl defines. A method whose first parameter is
            // `self` is an INSTANCE method (dispatched on a value); one
            // without is a STATIC, callable only as `Type.name(args)`.
            for method in &im.methods {
                let mangled = mangle(im.trait_name.as_deref(), &im.type_name, &method.name);
                let is_static =
                    method.params.first().is_none_or(|p| p.name != "self");
                if is_static {
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
                impl_table.insert((method.name.clone(), im.type_name.clone()), mangled.clone());
                // An inherent method dispatches by receiver type too, so register
                // its name as dispatchable (trait methods are already in the map).
                trait_methods
                    .entry(method.name.clone())
                    .or_insert_with(|| im.trait_name.clone().unwrap_or_default());
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
                            let mangled = mangle(Some(trait_name), &im.type_name, &ms.name);
                            impl_table
                                .insert((ms.name.clone(), im.type_name.clone()), mangled.clone());
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
    // monomorphization can resolve type arguments the head-name scope cannot
    // (e.g. `dict.get(d, k)` needs `v` from `d: Dict(String, String)`).
    // Node pointers stay valid through the moves below (statements live in
    // each function's own heap allocations).
    //
    // QUIET pre-mono dispatch pass: resolve trait-method calls and method
    // syntax at every CONCRETE site (including instantiated trait defaults
    // like `Ord__Int__less` calling `compare`), so the annotate probe below
    // sees a checkable module. Diagnostics are discarded here — anything
    // genuinely unresolvable is re-found loudly by the post-mono pass.
    {
        let (ctor_results, fn_rets) = build_tables(&items);
        let ctor_fields = build_ctor_fields(&items);
        let record_fields = build_record_fields(&items);
        let free_fns: std::collections::HashSet<String> = items
            .iter()
            .filter_map(|it| match it {
                Item::Function(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect();
        let quiet = std::cell::RefCell::new(Vec::new());
        let empty_table = crate::typeck::TypeTable::default();
        let ctx = Ctx {
            trait_methods: &trait_methods,
            static_trait_methods: &static_trait_methods,
            impl_table: &impl_table,
            ctor_results: &ctor_results,
            fn_rets: &fn_rets,
            ctor_fields: &ctor_fields,
            record_fields: &record_fields,
            free_fns: &free_fns,
            missing_impls: &quiet,
            statics: &statics,
            table: &empty_table,
        };
        for item in &mut items {
            if let Item::Function(f) = item {
                let mut scope = Scope::new();
                seed_params(&f.params, &mut scope);
                ctx.rewrite_block(&mut f.body, &mut scope);
            }
        }
    }

    let (items_back, type_table) = {
        let probe = Module {
            modes: Vec::new(),
            imports: imports.clone(),
            items,
            import_lines: Vec::new(),
            item_lines: Vec::new(),
        };
        let t = crate::typeck::annotate(&probe);
        (probe.items, t)
    };
    let mut items = items_back;

    // Pull out bounded generic functions (templates). Only their concrete
    // specializations are emitted, generated next.
    let mut templates: HashMap<String, Function> = HashMap::new();
    items.retain(|it| match it {
        Item::Function(f) if !f.bounds.is_empty() && !crate::typeck::intrinsic(&f.name) => {
            templates.insert(f.name.clone(), f.clone());
            false
        }
        _ => true,
    });
    // Unbounded generic functions are ALSO templates, but stay in `items` as a
    // fallback: a call whose primitive type argument resolves is rewritten to a
    // specialization (so `==` is content-correct and `Int` stays 64-bit), while a
    // call that can't be resolved keeps calling the generic version unchanged.
    if mono_unbounded {
        for it in &items {
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
    let mut mono_diags: Vec<String> = Vec::new();
    if !templates.is_empty() {
        let (ctor_results, fn_rets) = build_tables(&items);
        let ctor_fields = build_ctor_fields(&items);
        let record_fields = build_record_fields(&items);
        let known_fns: std::collections::HashSet<String> = items
            .iter()
            .filter_map(|it| match it {
                Item::Function(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect();
        let mut mono = Mono {
            templates: &templates,
            known_fns: &known_fns,
            trait_methods: &trait_methods,
            impl_trait_args: &impl_trait_args,
            diagnostics: Vec::new(),
            ctor_results: &ctor_results,
            ctor_fields: &ctor_fields,
            record_fields: &record_fields,
            fn_rets,
            memo: HashMap::new(),
            generated: Vec::new(),
            generated_subst: Vec::new(),
            cur_subst: HashMap::new(),
            table: &type_table,
        };
        mono.run(&mut items);
        items.extend(mono.generated.into_iter().map(Item::Function));
        mono_diags = mono.diagnostics;
    }

    // Tables used to determine a receiver's type at a trait-method call site.
    let (ctor_results, fn_rets) = build_tables(&items);
    let ctor_fields = build_ctor_fields(&items);
        let record_fields = build_record_fields(&items);
    let free_fns: std::collections::HashSet<String> = items
        .iter()
        .filter_map(|it| match it {
            Item::Function(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect();
    let missing_impls = std::cell::RefCell::new(Vec::new());
    let ctx = Ctx {
        trait_methods: &trait_methods,
        static_trait_methods: &static_trait_methods,
        impl_table: &impl_table,
        ctor_results: &ctor_results,
        fn_rets: &fn_rets,
        ctor_fields: &ctor_fields,
        record_fields: &record_fields,
        free_fns: &free_fns,
        missing_impls: &missing_impls,
        statics: &statics,
        table: &type_table,
    };
    for item in &mut items {
        if let Item::Function(f) = item {
            let mut scope = Scope::new();
            seed_params(&f.params, &mut scope);
            ctx.rewrite_block(&mut f.body, &mut scope);
        }
    }

    (
        Module { modes: Vec::new(), imports, items, import_lines: Vec::new(), item_lines: Vec::new() },
        {
            let mut d = missing_impls.into_inner();
            d.extend(mono_diags);
            d
        },
    )
}

/// Variable name -> the head name of its (known) type.
type Scope = HashMap<String, String>;

fn seed_params(params: &[Param], scope: &mut Scope) {
    for p in params {
        if let Some(name) = p.ty.as_ref().and_then(type_to_scope_name) {
            scope.insert(p.name.clone(), name);
        }
    }
}

/// Bind a `for`-loop variable to the element type of the iterable, when the
/// iterable's type is a known `List<...>`.
fn bind_loop_var(var: &str, iter_type: Option<String>, scope: &mut Scope) {
    if let Some(elem) = iter_type.as_deref().and_then(list_elem) {
        scope.insert(var.to_string(), elem.to_string());
    }
}

struct Ctx<'a> {
    trait_methods: &'a HashMap<String, String>,
    /// Trait methods that take no `self` — a call on a bound type variable passes
    /// no receiver.
    static_trait_methods: &'a std::collections::HashSet<String>,
    impl_table: &'a HashMap<(String, String), String>,
    ctor_results: &'a HashMap<String, String>,
    fn_rets: &'a HashMap<String, String>,
    ctor_fields: &'a HashMap<String, Vec<Type>>,
    /// Record type name -> its named field types (for typing `x.field`).
    record_fields: &'a HashMap<String, Vec<(String, Type)>>,
    /// Plain (non-method) function names: a trait-method call that ALSO names
    /// a free function may legitimately resolve to it, so it is never a
    /// missing-impl error.
    free_fns: &'a std::collections::HashSet<String>,
    /// Trait-method calls whose receiver type is KNOWN but has no impl —
    /// surfaced by the type checker as a clean "T does not implement Trait"
    /// instead of a post-lowering unknown-function error.
    missing_impls: &'a std::cell::RefCell<Vec<String>>,
    /// Self-less impl methods: `Type.name(args)` statics.
    statics: &'a HashMap<(String, String), String>,
    /// typeck's resolved types — receiver typing for method resolution.
    table: &'a crate::typeck::TypeTable,
}

impl Ctx<'_> {
    fn type_name(&self, e: &Expr, scope: &Scope) -> Option<String> {
        head_type_name(e, scope, self.ctor_results, self.fn_rets, self.record_fields)
    }

    /// Resolve a trait method to its mangled impl for a receiver type. A concrete
    /// generic type falls back to its head, where generic impls are registered:
    /// `List<Int>` matches `impl … for List(a)`, `Option<String>` matches
    /// `impl … for Option(a)`. The impl method stays generic and monomorphizes per
    /// element exactly as a `where`-bounded free function would. Last, a BLANKET impl
    /// — `impl Into(b) for a where b: From(a)` — is registered under a type-variable
    /// head (lowercase); it applies to any receiver, with its `where` bound
    /// discharged at monomorphization.
    fn lookup_impl(&self, method: &str, tn: &str) -> Option<String> {
        self.impl_table
            .get(&(method.to_string(), tn.to_string()))
            .or_else(|| self.impl_table.get(&(method.to_string(), head_of(tn).to_string())))
            .or_else(|| {
                self.impl_table
                    .iter()
                    .find(|((m, k), _)| {
                        m == method && k.chars().next().is_some_and(char::is_lowercase)
                    })
                    .map(|(_, v)| v)
            })
            .cloned()
    }

    fn rewrite_block(&self, b: &mut Block, scope: &mut Scope) {
        for stmt in &mut b.stmts {
            match stmt {
                Stmt::Let { name, value, .. } => {
                    self.rewrite_expr(value, scope);
                    match self.type_name(value, scope) {
                        Some(t) => {
                            scope.insert(name.clone(), t);
                        }
                        None => {
                            scope.remove(name.as_str());
                        }
                    }
                }
                Stmt::Assign { value, .. } => self.rewrite_expr(value, scope),
                // Seed each destructured name from the tuple's slot types so a
                // trait call on a tuple part (`x0.show()`) dispatches.
                Stmt::LetTuple { names, value } => {
                    self.rewrite_expr(value, scope);
                    match self.type_name(value, scope).as_deref().and_then(tuple_args) {
                        Some(args) => {
                            for (n, t) in names.iter().zip(args) {
                                scope.insert(n.clone(), t.to_string());
                            }
                        }
                        None => {
                            for n in names {
                                scope.remove(n.as_str());
                            }
                        }
                    }
                }
                Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => self.rewrite_expr(e, scope),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    fn rewrite_expr(&self, e: &mut Expr, scope: &mut Scope) {
        match e {
            Expr::Call { name, args } => {
                for a in args.iter_mut() {
                    self.rewrite_expr(a, scope);
                }
                if let Some(trait_name) = self.trait_methods.get(name.as_str()) {
                    if let Some(recv) = args.first() {
                        if let Some(tn) = self.type_name(recv, scope) {
                            match self.lookup_impl(name, &tn) {
                                Some(mangled) => *name = mangled.clone(),
                                // The receiver's type is known and no impl
                                // exists; unless a plain function of this name
                                // can take the call, that is a bound the
                                // program cannot satisfy — report it cleanly.
                                // A TYPE-VARIABLE receiver (lowercase name) is
                                // a bounded generic: dispatch resolves after
                                // monomorphization, never an error here.
                                None if !self.free_fns.contains(name.as_str())
                                    && !tn.chars().next().is_some_and(|c| c.is_lowercase()) => {
                                    self.missing_impls.borrow_mut().push(format!(
                                        "`{tn}` does not implement `{trait_name}` \
                                         (no `impl {trait_name} for {tn}`) — required by a call to `{name}`"
                                    ));
                                }
                                None => {}
                            }
                        }
                    }
                }
            }
            Expr::Apply { func, args } => {
                self.rewrite_expr(func, scope);
                for a in args.iter_mut() {
                    self.rewrite_expr(a, scope);
                }
            }
            Expr::Ctor { args, .. }
            | Expr::List(args)
            | Expr::Tuple(args) => {
                for a in args.iter_mut() {
                    self.rewrite_expr(a, scope);
                }
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
                self.rewrite_expr(expr, scope)
            }
            Expr::RecordUpdate { base, fields } => {
                self.rewrite_expr(base, scope);
                for (_, v) in fields.iter_mut() {
                    self.rewrite_expr(v, scope);
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
            Expr::Binary { lhs, rhs, .. } => {
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
            }
            // METHOD RESOLUTION (docs/language-evolution.md Phase 3):
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
                            .ctor_fields
                            .get(tyname.as_str())
                            .is_some_and(|fs| fs.is_empty());
                        if self
                            .impl_table
                            .contains_key(&(method.clone(), tyname.clone()))
                            && !is_value
                        {
                            self.missing_impls.borrow_mut().push(format!(
                                "`{tyname}.{method}` is an INSTANCE method (it takes `self`) — \
                                 call it on a value: `value.{method}(…)`"
                            ));
                            return;
                        }
                    }
                }
                let tn = self
                    .type_name(receiver, scope)
                    .or_else(|| {
                        self.table
                            .type_of(receiver)
                            .and_then(crate::typeck::ty_to_ast)
                            .and_then(|t| type_to_scope_name(&t))
                    });
                if let Some(tn) = &tn {
                    if let Some(mangled) = self.lookup_impl(method, tn) {
                        let mut call_args = vec![std::mem::replace(
                            receiver.as_mut(),
                            Expr::Bool(false),
                        )];
                        call_args.append(args);
                        *e = Expr::Call { name: mangled.clone(), args: call_args };
                        return;
                    }
                }
                // A trait method on a generic (bound) receiver dispatches after
                // monomorphization: lower to the bare trait call. A STATIC trait
                // method (`b.from(x)`, no `self`) takes no receiver — the receiver is
                // the type itself, resolved through the bound at mono — so only the
                // explicit arguments are passed; an instance method prepends `self`.
                let receiver_is_generic =
                    tn.as_deref().is_none_or(|n| n.chars().next().is_some_and(char::is_lowercase));
                if self.trait_methods.contains_key(method.as_str()) && receiver_is_generic {
                    let mut call_args = if self.static_trait_methods.contains(method.as_str()) {
                        Vec::new()
                    } else {
                        vec![std::mem::replace(receiver.as_mut(), Expr::Bool(false))]
                    };
                    call_args.append(args);
                    *e = Expr::Call { name: method.clone(), args: call_args };
                    return;
                }
                // UFCS fallback: on a built-in collection type, `recv.method(args)`
                // lowers to the module-qualified free function
                // `module.method(recv, args)` — so `d.keys()` is `dict.keys(d)`,
                // `xs.length()` is `list.length(xs)`, and so on. A wrong name is
                // validated downstream ("module `dict` has no function `keys`").
                if let Some(module) = tn.as_deref().and_then(builtin_method_module) {
                    let mut call_args =
                        vec![std::mem::replace(receiver.as_mut(), Expr::Bool(false))];
                    call_args.append(args);
                    *e = Expr::Call { name: format!("{module}.{method}"), args: call_args };
                    return;
                }
                match tn {
                    Some(tn) => self.missing_impls.borrow_mut().push(format!(
                        "no method `{method}` on `{tn}` — methods come from `impl` blocks; \
                         a plain function is called as `{method}(value, …)` (or module-qualified, \
                         e.g. `list.{method}(value, …)`)"
                    )),
                    None => self.missing_impls.borrow_mut().push(format!(
                        "cannot resolve the method call `.{method}(…)` — the receiver's type \
                         is not known here; call the function directly: `{method}(value, …)`"
                    )),
                }
            }
            Expr::WhileLet { pattern, scrutinee, body } => {
                self.rewrite_expr(scrutinee, scope);
                let mut s = scope.clone();
                bind_ctor_pattern(pattern, self.ctor_fields, &mut s);
                self.rewrite_block(body, &mut s);
            }
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                self.rewrite_expr(cond, scope);
                self.rewrite_block(then_block, &mut scope.clone());
                if let Some(b) = else_block {
                    self.rewrite_block(b, &mut scope.clone());
                }
            }
            Expr::While { cond, body } => {
                self.rewrite_expr(cond, scope);
                self.rewrite_block(body, &mut scope.clone());
            }
            Expr::For { var, iter, body } => {
                self.rewrite_expr(iter, scope);
                // `for ... in <dict>` iterates the dict's (key, value) pairs;
                // `for x in <set>` iterates the set's members — rewrite the
                // iterand to `dict.pairs(...)` / `set.to_list(...)` respectively.
                let iter_head = self.type_name(iter, scope);
                let head = iter_head.as_deref().and_then(|t| t.split('<').next());
                let view = match head {
                    Some("Dict") => Some("dict.pairs"),
                    Some("Set") => Some("set.to_list"),
                    _ => None,
                };
                if let Some(view_fn) = view {
                    let inner = std::mem::replace(iter.as_mut(), Expr::Bool(false));
                    **iter = Expr::Call { name: view_fn.to_string(), args: vec![inner] };
                }
                let mut s = scope.clone();
                bind_loop_var(var, self.type_name(iter, scope), &mut s);
                self.rewrite_block(body, &mut s);
            }
            Expr::Match { scrutinee, arms } => {
                self.rewrite_expr(scrutinee, scope);
                for arm in arms.iter_mut() {
                    let mut s = scope.clone();
                    bind_ctor_pattern(&arm.pattern, self.ctor_fields, &mut s);
                    if let Some(g) = &mut arm.guard {
                        self.rewrite_expr(g, &mut s);
                    }
                    self.rewrite_expr(&mut arm.body, &mut s);
                }
            }
            Expr::Lambda { params, body, .. } => {
                let mut s = scope.clone();
                seed_params(params, &mut s);
                self.rewrite_block(body, &mut s);
            }
            Expr::Block(b) => self.rewrite_block(b, &mut scope.clone()),
            Expr::Var(_) | Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
        }
    }
}

/// Return types of the handful of builtins common enough to want as trait-call
/// receivers. A wrong guess only ever produces a type error (never wrong code),
/// so the table stays conservative.
fn builtin_ret(name: &str) -> Option<String> {
    let t = match name {
        "int_to_string" | "__render" => "String",
        "string_length" | "char_count" => "Int",
        _ => return None,
    };
    Some(t.into())
}

/// Best-effort head type name of an expression, or `None` if undeterminable
/// without full inference. Shared by trait-call resolution and monomorphization.
fn head_type_name(
    e: &Expr,
    scope: &Scope,
    ctor_results: &HashMap<String, String>,
    fn_rets: &HashMap<String, String>,
    record_fields: &HashMap<String, Vec<(String, Type)>>,
) -> Option<String> {
    match e {
        Expr::Int(_) => Some("Int".into()),
        Expr::Float(_) => Some("Float".into()),
        Expr::Str(_) => Some("String".into()),
        Expr::Bool(_) => Some("Bool".into()),
        Expr::Duration(_) => Some("Duration".into()),
        Expr::Var(n) => scope.get(n).cloned(),
        // `self.home` — a record field of a known record type, when the field
        // type is concrete (so generated method bodies dispatch on fields).
        Expr::Field { base, field } => {
            let base_ty = head_type_name(base, scope, ctor_results, fn_rets, record_fields)?;
            // The base may be an encoded generic (`Box<Int>`, `Set<String>`); record
            // fields are keyed by the bare head. The field's declared type stays
            // generic (`a`) and the caller's substitution makes it concrete.
            let fields = record_fields.get(head_of(&base_ty))?;
            let (_, ft) = fields.iter().find(|(n, _)| n == field)?;
            type_to_scope_name(ft)
        }
        // `Some(x)` encodes its payload (`Option<Int>`), mirroring a list literal,
        // so monomorphization recovers an option's element from the call site.
        Expr::Ctor { name, args } if name == "Some" => {
            let elem = args
                .first()
                .and_then(|a| head_type_name(a, scope, ctor_results, fn_rets, record_fields))
                .unwrap_or_else(|| "_".to_string());
            Some(format!("Option<{elem}>"))
        }
        Expr::Ctor { name, .. } => ctor_results.get(name).cloned(),
        Expr::Call { name, .. } => fn_rets.get(name).cloned().or_else(|| builtin_ret(name)),
        Expr::RecordUpdate { base, .. } => head_type_name(base, scope, ctor_results, fn_rets, record_fields),
        // `!` yields Bool; `-`/`~` preserve the operand's type (so `-5` is Int).
        Expr::Unary { op, expr } => match op {
            UnOp::Not => Some("Bool".into()),
            UnOp::Neg | UnOp::BitNot | UnOp::Move | UnOp::Await => head_type_name(expr, scope, ctor_results, fn_rets, record_fields),
        },
        // Comparisons/logic yield Bool; `<>` yields String; arithmetic and
        // bitwise ops have the type of their (left) operand.
        Expr::Binary { op, lhs, .. } => match op {
            BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
            | BinOp::And => Some("Bool".into()),
            BinOp::Concat => Some("String".into()),
            // Non-Bool `||` (truthy fallback) has its (left) operand's type.
            _ => head_type_name(lhs, scope, ctor_results, fn_rets, record_fields),
        },
        // A list literal's type encodes its element type when determinable from
        // the first element, e.g. `List<Int>`, so a `for` loop over it can type
        // the loop variable. `list_elem` reads the element back out.
        Expr::List(items) => Some(
            match items
                .first()
                .and_then(|e| head_type_name(e, scope, ctor_results, fn_rets, record_fields))
            {
                Some(elem) => format!("List<{elem}>"),
                None => "List".to_string(),
            },
        ),
        // A tuple literal encodes its slot types (`Tuple2<Int,String>`), mirroring a
        // list literal, so a tuple value dispatches + monomorphizes per slot.
        Expr::Tuple(items) => Some(
            match items
                .iter()
                .map(|e| head_type_name(e, scope, ctor_results, fn_rets, record_fields))
                .collect::<Option<Vec<_>>>()
            {
                Some(es) => format!("Tuple{}<{}>", items.len(), es.join(",")),
                None => format!("Tuple{}", items.len()),
            },
        ),
        _ => None,
    }
}

/// The element type encoded in a `List<...>` scope name, if any.
/// Rewrite type-variable tokens in a scope-name string to their concrete types,
/// e.g. `apply_subst("List<a>", {a: "Int"})` is `"List<Int>"`. Whole identifier
/// tokens are matched, so only an exact type-variable name is replaced.
fn apply_subst(name: &str, subst: &HashMap<String, String>) -> String {
    if subst.is_empty() {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut tok = String::new();
    let flush = |tok: &mut String, out: &mut String| {
        if !tok.is_empty() {
            out.push_str(subst.get(tok.as_str()).map_or(tok.as_str(), |s| s.as_str()));
            tok.clear();
        }
    };
    for c in name.chars() {
        if c.is_alphanumeric() || c == '_' {
            tok.push(c);
        } else {
            flush(&mut tok, &mut out);
            out.push(c);
        }
    }
    flush(&mut tok, &mut out);
    out
}

fn list_elem(type_name: &str) -> Option<&str> {
    type_name.strip_prefix("List<")?.strip_suffix('>')
}

/// The argument of a single-arg generic scope name — `List<Int>`/`Option<Int>` ->
/// `Int`, `List<List<Int>>` -> `List<Int>`. Lets monomorphization recover the
/// element of any one-parameter generic (List, Option, a user `Box(a)`) uniformly.
fn generic_arg(type_name: &str) -> Option<&str> {
    let start = type_name.find('<')? + 1;
    let end = type_name.rfind('>')?;
    (start <= end).then(|| &type_name[start..end])
}

/// The top-level slot scope names of a tuple scope name — `Tuple2<Int,String>` ->
/// `["Int", "String"]`, respecting nesting (`Tuple2<List<Int>,String>`). Lets
/// monomorphization recover each slot type of a tuple impl's `self`.
fn tuple_args(type_name: &str) -> Option<Vec<&str>> {
    let inner = generic_arg(type_name)?;
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in inner.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(inner[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(inner[start..].trim());
    Some(out)
}

/// The scope name for a declared parameter type, encoding a list's element type
/// (`List<Int>`) so loop-variable typing works on annotated list parameters.
// The stdlib module that backs UFCS method calls on a built-in type, so
// `recv.method(args)` can lower to `module.method(recv, args)`. `tn` may carry a
// generic suffix (`List<Int>`), so match on the head.
fn builtin_method_module(tn: &str) -> Option<&'static str> {
    match tn.split('<').next().unwrap_or(tn) {
        "List" => Some("list"),
        "Dict" => Some("dict"),
        "String" => Some("string"),
        "Set" => Some("set"),
        "Option" => Some("option"),
        "Result" => Some("result"),
        "Iter" => Some("iter"),
        // `key.sign(msg)` / `key.public_key()` / `key.reveal()` -> crypto.*;
        // `store.get(name)` -> secretstore.get.
        "Secret" => Some("crypto"),
        "SecretStore" => Some("secretstore"),
        _ => None,
    }
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
        | Stmt::LetTuple { value, .. }
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
        | Expr::Var(_) => false,
        Expr::List(xs) | Expr::Tuple(xs) => xs.iter().any(expr_needs_lowering),
        Expr::Call { args, .. } | Expr::Ctor { args, .. } => {
            args.iter().any(expr_needs_lowering)
        }
        Expr::Apply { func, args } => {
            expr_needs_lowering(func) || args.iter().any(expr_needs_lowering)
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => {
            expr_needs_lowering(expr)
        }
        Expr::Field { base, .. } => expr_needs_lowering(base),
        Expr::Lambda { body, .. } | Expr::Block(body) => block_needs_lowering(body),
        Expr::RecordUpdate { base, fields } => {
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

fn type_to_scope_name(t: &Type) -> Option<String> {
    type_to_scope_name_d(t, 0)
}

/// A legitimate type is shallow (its depth is the program's nesting); a self-
/// referential type's NOMINAL form is finite (`Json`, not its expansion). Past this
/// depth the input is a degenerate/cyclic type the encoding can't represent, so bail
/// to the head rather than recurse without bound — the compiler must never overflow.
const SCOPE_NAME_MAX_DEPTH: usize = 32;

fn type_to_scope_name_d(t: &Type, depth: usize) -> Option<String> {
    match t {
        // A generic encodes its arguments (`List<Int>`, `Box<Int>`,
        // `Dict<String,Int>`) so monomorphization can recover each from a receiver's
        // scope name; the dispatch lookup strips them back to the head.
        Type::Named(n, args) if !args.is_empty() => {
            if depth >= SCOPE_NAME_MAX_DEPTH {
                return Some(n.clone());
            }
            Some(match args.iter().map(|a| type_to_scope_name_d(a, depth + 1)).collect::<Option<Vec<_>>>() {
                Some(es) => format!("{n}<{}>", es.join(",")),
                None => n.clone(),
            })
        }
        Type::Named(n, _) => Some(n.clone()),
        // A tuple's head is its arity (`Tuple2`, `Tuple3`) — the head `impl Trait for
        // (a, b)` registers under and a value dispatches to — and it encodes its
        // slot types (`Tuple2<Int,String>`) so monomorphization recovers each.
        Type::Tuple(ts) => {
            if depth >= SCOPE_NAME_MAX_DEPTH {
                return Some(format!("Tuple{}", ts.len()));
            }
            Some(
                match ts.iter().map(|a| type_to_scope_name_d(a, depth + 1)).collect::<Option<Vec<_>>>() {
                    Some(es) => format!("Tuple{}<{}>", ts.len(), es.join(",")),
                    None => format!("Tuple{}", ts.len()),
                },
            )
        }
        _ => None,
    }
}

/// The head of a scope type name — `List<Int>` -> `List`, `Point` -> `Point`,
/// `Tuple2` -> `Tuple2`. Generic impls register by head, so a concrete receiver
/// type falls back to it during dispatch.
fn head_of(tn: &str) -> &str {
    tn.split('<').next().unwrap_or(tn)
}

/// Constructor -> its type name, and function -> its (named) return type.
fn build_tables(items: &[Item]) -> (HashMap<String, String>, HashMap<String, String>) {
    let mut ctor_results = HashMap::new();
    let mut fn_rets = HashMap::new();
    for item in items {
        match item {
            Item::Type(t) => {
                for v in &t.variants {
                    ctor_results.insert(v.name.clone(), t.name.clone());
                }
            }
            Item::Function(f) => {
                if let Some(Type::Named(n, _)) = &f.ret {
                    fn_rets.insert(f.name.clone(), n.clone());
                }
            }
            _ => {}
        }
    }
    (ctor_results, fn_rets)
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

/// Constructor name -> the types of its fields, for typing the variables bound
/// by a constructor pattern in a `match` arm.
fn build_ctor_fields(items: &[Item]) -> HashMap<String, Vec<Type>> {
    let mut map = HashMap::new();
    for item in items {
        if let Item::Type(t) = item {
            for v in &t.variants {
                map.insert(v.name.clone(), v.fields.clone());
            }
        }
    }
    map
}

/// The scope name for a field type that is a *concrete* (non-type-variable)
/// type. Witchy spells type variables in lowercase and concrete types
/// capitalized, so a capitalized, argument-free head name is concrete (`Int`,
/// `Coord`). Generic fields (a lowercase var like `a`, or a parameterized type
/// whose arguments we don't track here) return `None`, so their bound variable
/// stays untyped rather than risk a wrong dispatch.
fn concrete_scope_name(t: &Type) -> Option<String> {
    match t {
        Type::Named(n, args)
            if args.is_empty() && n.chars().next().is_some_and(|c| c.is_uppercase()) =>
        {
            Some(n.clone())
        }
        _ => None,
    }
}

/// Bind the variables of a constructor pattern to their concrete field types, so
/// a trait-method call on one (e.g. a recursive `show(x)` inside a `Show` impl)
/// resolves. Recurses into nested constructor patterns. Conservative: only
/// concrete fields are bound (see `concrete_scope_name`); a wrong guess would in
/// any case be caught by the type checker, never miscompiled.
fn bind_ctor_pattern(pat: &Pattern, ctor_fields: &HashMap<String, Vec<Type>>, scope: &mut Scope) {
    if let Pattern::Ctor { name, args } = pat {
        if let Some(fields) = ctor_fields.get(name) {
            for (arg, fty) in args.iter().zip(fields) {
                match arg {
                    Pattern::Var(v) => {
                        if let Some(sn) = concrete_scope_name(fty) {
                            scope.insert(v.clone(), sn);
                        }
                    }
                    Pattern::Ctor { .. } => bind_ctor_pattern(arg, ctor_fields, scope),
                    _ => {}
                }
            }
        }
    }
}

/// Replace each bound type variable in a type with its concrete instantiation.
/// Encode a CONCRETE `Type` as the scope naming ("List<Int>", "(String, Int)").
/// None when a type variable or unencodable form remains.
fn encode_scope_type(t: &Type) -> Option<String> {
    match t {
        Type::Named(n, args) if args.is_empty() => {
            if n.chars().next().is_some_and(char::is_lowercase) {
                None // a type variable, not a concrete type
            } else {
                Some(n.clone())
            }
        }
        Type::Named(n, args) => {
            let inner: Option<Vec<String>> = args.iter().map(encode_scope_type).collect();
            Some(format!("{n}<{}>", inner?.join(", ")))
        }
        Type::Tuple(ts) => {
            let inner: Option<Vec<String>> = ts.iter().map(encode_scope_type).collect();
            Some(format!("({})", inner?.join(", ")))
        }
        _ => None,
    }
}

/// Match a type PATTERN (an impl's trait argument, possibly containing type
/// variables) against a concrete type, collecting variable bindings as scope
/// encodings. False when the shapes disagree or a leaf can't encode.
fn bind_type_vars(pattern: &Type, concrete: &Type, out: &mut HashMap<String, String>) -> bool {
    match (pattern, concrete) {
        (Type::Named(v, a), c) if a.is_empty() && v.chars().next().is_some_and(char::is_lowercase) => {
            match encode_scope_type(c) {
                Some(enc) => match out.get(v) {
                    Some(prev) => prev == &enc,
                    None => {
                        out.insert(v.clone(), enc);
                        true
                    }
                },
                None => false,
            }
        }
        (Type::Named(pn, pa), Type::Named(cn, ca)) => {
            pn == cn
                && pa.len() == ca.len()
                && pa.iter().zip(ca).all(|(p, c)| bind_type_vars(p, c, out))
        }
        (Type::Tuple(ps), Type::Tuple(cs)) => {
            ps.len() == cs.len() && ps.iter().zip(cs).all(|(p, c)| bind_type_vars(p, c, out))
        }
        _ => pattern == concrete,
    }
}

/// Decode a scope-encoded type name ("List<Int>", "Dict<String, Int>",
/// "Int") back into a structured `Type` — the inverse of `simple_ty_name`.
fn decode_scope_type(name: &str) -> Type {
    // A tuple encoding: "(String, Int)".
    if let Some(inner) = name.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        let mut args = Vec::new();
        let mut depth = 0usize;
        let mut start = 0usize;
        for (i, c) in inner.char_indices() {
            match c {
                '<' | '(' => depth += 1,
                '>' | ')' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    args.push(decode_scope_type(inner[start..i].trim()));
                    start = i + 1;
                }
                _ => {}
            }
        }
        if start < inner.len() {
            args.push(decode_scope_type(inner[start..].trim()));
        }
        return Type::Tuple(args);
    }
    match name.split_once('<') {
        Some((head, rest)) if rest.ends_with('>') => {
            let inner = &rest[..rest.len() - 1];
            // Split on top-level commas only (nested encodings nest brackets).
            let mut args = Vec::new();
            let mut depth = 0usize;
            let mut start = 0usize;
            for (i, c) in inner.char_indices() {
                match c {
                    '<' | '(' => depth += 1,
                    '>' | ')' => depth = depth.saturating_sub(1),
                    ',' if depth == 0 => {
                        args.push(decode_scope_type(inner[start..i].trim()));
                        start = i + 1;
                    }
                    _ => {}
                }
            }
            if start < inner.len() {
                args.push(decode_scope_type(inner[start..].trim()));
            }
            if head == "List" && args.len() == 1 {
                Type::Named("List".into(), args)
            } else if head
                .strip_prefix("Tuple")
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            {
                // `Tuple2<String,Int>` decodes to the tuple type `(String, Int)`, so
                // its slot types survive the round-trip and each one monomorphizes.
                Type::Tuple(args)
            } else {
                Type::Named(head.to_string(), args)
            }
        }
        _ => Type::Named(name.to_string(), vec![]),
    }
}

/// Substitute type variables in a block's type ANNOTATIONS (`let x: T`, `e as T`)
/// — the part `specialize` doesn't reach by rewriting only the signature.
fn subst_block_types(b: &mut Block, subst: &HashMap<&str, String>) {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { ty, value, .. } => {
                if let Some(t) = ty {
                    *t = subst_vars(t, subst);
                }
                subst_expr_types(value, subst);
            }
            Stmt::Assign { value, .. }
            | Stmt::LetTuple { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Expr(value)
            | Stmt::Yield(value) => subst_expr_types(value, subst),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn subst_expr_types(e: &mut Expr, subst: &HashMap<&str, String>) {
    match e {
        Expr::Lambda { params, body, .. } => {
            for p in params.iter_mut() {
                if let Some(t) = &p.ty {
                    p.ty = Some(subst_vars(t, subst));
                }
            }
            subst_block_types(body, subst);
        }
        Expr::Block(b) => subst_block_types(b, subst),
        Expr::As { expr, ty } => {
            *ty = subst_vars(ty, subst);
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
        Expr::Call { args, .. } | Expr::Ctor { args, .. } => {
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
        Expr::RecordUpdate { base, fields } => {
            subst_expr_types(base, subst);
            for (_, v) in fields {
                subst_expr_types(v, subst);
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
        | Expr::Var(_) => {}
    }
}

fn subst_vars(t: &Type, subst: &HashMap<&str, String>) -> Type {
    match t {
        Type::Named(n, args) if args.is_empty() && subst.contains_key(n.as_str()) => {
            decode_scope_type(&subst[n.as_str()])
        }
        Type::Named(n, args) => {
            Type::Named(n.clone(), args.iter().map(|a| subst_vars(a, subst)).collect())
        }
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|a| subst_vars(a, subst)).collect()),
        Type::Fn(ps, r) => Type::Fn(
            ps.iter().map(|a| subst_vars(a, subst)).collect(),
            Box::new(subst_vars(r, subst)),
        ),
    }
}

/// Witchy spells type variables lowercase and concrete types capitalized.
fn is_type_var_name(n: &str) -> bool {
    n.chars().next().is_some_and(|c| c.is_lowercase())
}

/// A type argument we will specialize an *unbounded* generic on. Restricted to
/// the primitive types: these are exactly the ones the generic i32 ABI gets
/// wrong — `String` (pointer-compared instead of content-compared by `==`) and
/// `Int` (truncated to 32 bits). For records/enums/parameterized types the head
/// name alone wouldn't capture the type, and codegen treats them as pointers
/// either way, so those calls keep using the generic version.
/// Unify a declared parameter type against a typeck-resolved type, binding
/// `var` if it appears. Only SIMPLE named bindings (no type arguments) are
/// returned — monomorphization's name-keyed substitution can't represent a
/// composite type argument.
fn bind_var_simple(param: &Type, concrete: &crate::typeck::Ty, var: &str) -> Option<String> {
    use crate::typeck::Ty;
    match (param, concrete) {
        (Type::Named(n, args), _) if n == var && args.is_empty() => simple_ty_name(concrete),
        (Type::Named(n, pargs), Ty::Named(cn, cargs))
            if n == cn && pargs.len() == cargs.len() =>
        {
            pargs.iter().zip(cargs).find_map(|(p, c)| bind_var_simple(p, c, var))
        }
        (Type::Named(n, pargs), Ty::List(e)) if n == "List" && pargs.len() == 1 => {
            bind_var_simple(&pargs[0], e, var)
        }
        (Type::Tuple(ps), Ty::Tuple(cs)) if ps.len() == cs.len() => {
            ps.iter().zip(cs).find_map(|(p, c)| bind_var_simple(p, c, var))
        }
        _ => None,
    }
}

/// The plain name of a resolved type, when it has one (no type arguments).
fn simple_ty_name(t: &crate::typeck::Ty) -> Option<String> {
    use crate::typeck::Ty;
    match t {
        Ty::Int => Some("Int".into()),
        Ty::Float => Some("Float".into()),
        Ty::Bool => Some("Bool".into()),
        Ty::String => Some("String".into()),
        Ty::Duration => Some("Duration".into()),
        Ty::Named(n, args) if args.is_empty() => Some(n.clone()),
        // Compound types use the same scope encoding `head_type_name`
        // produces ("List<Int>"), so list_elem/head-splitting reads them.
        Ty::List(e) => Some(format!("List<{}>", simple_ty_name(e)?)),
        Ty::Tuple(ts) => {
            let inner: Option<Vec<String>> = ts.iter().map(simple_ty_name).collect();
            Some(format!("({})", inner?.join(", ")))
        }
        Ty::Named(n, args) => {
            let inner: Option<Vec<String>> = args.iter().map(simple_ty_name).collect();
            Some(format!("{n}<{}>", inner?.join(", ")))
        }
        _ => None,
    }
}

fn is_specializable_type_arg(n: &str) -> bool {
    matches!(n, "Int" | "Bool" | "Float" | "String" | "Duration")
}

/// Collect the type-variable names appearing in a type (lowercase, argument-free
/// `Named`s), in order of first appearance.
fn collect_type_vars(t: &Type, out: &mut Vec<String>) {
    match t {
        Type::Named(n, args) => {
            if args.is_empty() && is_type_var_name(n) && !out.iter().any(|v| v == n) {
                out.push(n.clone());
            }
            for a in args {
                collect_type_vars(a, out);
            }
        }
        Type::Tuple(ts) => {
            for a in ts {
                collect_type_vars(a, out);
            }
        }
        Type::Fn(ps, r) => {
            for a in ps {
                collect_type_vars(a, out);
            }
            collect_type_vars(r, out);
        }
    }
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

/// The type variables a function is generic over: its `where`-bound variables if
/// bounded, otherwise the free type variables in its signature.
/// Rename bare calls per `renames`, recursively — the substitution-directed
/// dispatch rewrite over a specialization's body.
fn rename_calls_block(b: &mut Block, renames: &HashMap<String, String>) {
    fn walk_expr(e: &mut Expr, renames: &HashMap<String, String>) {
        match e {
            Expr::Call { name, args } => {
                if let Some(to) = renames.get(name.as_str()) {
                    *name = to.clone();
                }
                for a in args {
                    walk_expr(a, renames);
                }
            }
            Expr::Apply { func, args } => {
                walk_expr(func, renames);
                for a in args {
                    walk_expr(a, renames);
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                walk_expr(receiver, renames);
                for a in args {
                    walk_expr(a, renames);
                }
            }
            Expr::Ctor { args, .. } | Expr::List(args) | Expr::Tuple(args) => {
                for a in args {
                    walk_expr(a, renames);
                }
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. }
            | Expr::Field { base: expr, .. } => walk_expr(expr, renames),
            Expr::Binary { lhs, rhs, .. } => {
                walk_expr(lhs, renames);
                walk_expr(rhs, renames);
            }
            Expr::Range { lo, hi, .. } => {
                walk_expr(lo, renames);
                walk_expr(hi, renames);
            }
            Expr::Index { base, index } => {
                walk_expr(base, renames);
                walk_expr(index, renames);
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields {
                    walk_expr(v, renames);
                }
                if let Some(sp) = spread {
                    walk_expr(sp, renames);
                }
            }
            Expr::RecordUpdate { base, fields } => {
                walk_expr(base, renames);
                for (_, v) in fields {
                    walk_expr(v, renames);
                }
            }
            Expr::If { cond, then_block, else_block } => {
                walk_expr(cond, renames);
                rename_calls_block(then_block, renames);
                if let Some(b) = else_block {
                    rename_calls_block(b, renames);
                }
            }
            Expr::Match { scrutinee, arms } => {
                walk_expr(scrutinee, renames);
                for a in arms {
                    if let Some(g) = &mut a.guard {
                        walk_expr(g, renames);
                    }
                    walk_expr(&mut a.body, renames);
                }
            }
            Expr::While { cond, body } => {
                walk_expr(cond, renames);
                rename_calls_block(body, renames);
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                walk_expr(scrutinee, renames);
                rename_calls_block(body, renames);
            }
            Expr::For { iter, body, .. } => {
                walk_expr(iter, renames);
                rename_calls_block(body, renames);
            }
            Expr::Lambda { body, .. } | Expr::Block(body) => {
                rename_calls_block(body, renames)
            }
            Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_)
            | Expr::Bool(_) | Expr::Var(_) => {}
        }
    }
    for st in &mut b.stmts {
        match st {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetTuple { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Expr(value)
            | Stmt::Yield(value) => walk_expr(value, renames),
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
    known_fns: &'a std::collections::HashSet<String>,
    /// method name -> owning trait, and (trait, impl head) -> the impl's
    /// trait type-arguments — substitution-directed dispatch for bounds.
    trait_methods: &'a HashMap<String, String>,
    impl_trait_args: &'a HashMap<(String, String), Vec<Type>>,
    /// Loud failures (an uninferrable bounded call) surfaced as check errors.
    diagnostics: Vec<String>,
    ctor_results: &'a HashMap<String, String>,
    ctor_fields: &'a HashMap<String, Vec<Type>>,
    record_fields: &'a HashMap<String, Vec<(String, Type)>>,
    fn_rets: HashMap<String, String>,
    memo: HashMap<(String, Vec<String>), String>,
    generated: Vec<Function>,
    /// Per-generated-instance type-variable substitution (var -> concrete scope
    /// name), parallel to `generated`. Lets the walk of an instance resolve a
    /// field-access type the head-name scope leaves generic (`s.items` on a
    /// `Set(Int)` is `List(Int)`, not `List(a)`).
    generated_subst: Vec<HashMap<String, String>>,
    /// The substitution of the instance currently being walked — empty when
    /// walking an original (non-specialized) function.
    cur_subst: HashMap<String, String>,
    /// typeck's resolved types for this module instance: the fallback when
    /// the head-name scope can't resolve a type argument.
    table: &'a crate::typeck::TypeTable,
}

impl Mono<'_> {
    fn run(&mut self, items: &mut [Item]) {
        for item in items.iter_mut() {
            if let Item::Function(f) = item {
                let mut s = Scope::new();
                seed_params(&f.params, &mut s);
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
                Block { stmts: Vec::new(), lines: Vec::new(), restrict: None, region: None },
            );
            let mut s = Scope::new();
            seed_params(&params, &mut s);
            // This instance's type-variable substitution, so the body walk can
            // make a generic field-access type concrete.
            self.cur_subst = self.generated_subst[i].clone();
            self.walk_block(&mut body, &mut s);
            self.cur_subst = HashMap::new();
            self.generated[i].body = body;
            i += 1;
        }
    }

    fn type_name(&self, e: &Expr, scope: &Scope) -> Option<String> {
        head_type_name(e, scope, self.ctor_results, &self.fn_rets, self.record_fields)
    }

    /// Like `type_name`, but rewrites the current instantiation's type variables
    /// to their concrete types — so a field access whose declared type is generic
    /// (`s.items: List(a)`) resolves concretely (`List(Int)`) inside `foo__Int`.
    fn type_name_subst(&self, e: &Expr, scope: &Scope) -> Option<String> {
        self.type_name(e, scope).map(|t| apply_subst(&t, &self.cur_subst))
    }

    /// The concrete type of field `pos` of the element TUPLE of a list argument,
    /// where the argument is a list literal of tuples (e.g. `unzip([(big, 1)])`).
    fn list_elem_tuple_field_type(&self, arg: &Expr, pos: usize, scope: &Scope) -> Option<String> {
        if let Expr::List(items) = arg {
            if let Some(Expr::Tuple(fields)) = items.first() {
                if let Some(e) = fields.get(pos) {
                    return self.type_name(e, scope);
                }
            }
        }
        None
    }

    /// The return type name of a function-valued argument: a lambda's body type
    /// (its parameters seeded into scope) or a named function's return type.
    /// Resolves a `fn(...) -> b` parameter's `b` for monomorphization.
    fn closure_ret_type(&self, arg: &Expr, scope: &Scope) -> Option<String> {
        match arg {
            Expr::Lambda { params, body, .. } => {
                let mut s = scope.clone();
                seed_params(params, &mut s);
                match body.stmts.last() {
                    Some(Stmt::Expr(e)) | Some(Stmt::Return(Some(e))) => {
                        head_type_name(e, &s, self.ctor_results, &self.fn_rets, self.record_fields)
                    }
                    _ => None,
                }
            }
            Expr::Var(f) => self.fn_rets.get(f).cloned(),
            _ => None,
        }
    }

    fn resolve_type_args(
        &self,
        template: &Function,
        args: &[Expr],
        scope: &Scope,
        result_ty: Option<&crate::typeck::Ty>,
    ) -> Option<Vec<String>> {
        let mut result = Vec::new();
        let bounded = !template.bounds.is_empty();
        for var in type_var_list(template) {
            // The variable must appear in a parameter's type, either directly
            // (`x: a`) or as a list element (`xs: List(a)`); take the concrete
            // type from the matching argument.
            let mut found = None;
            for (i, p) in template.params.iter().enumerate() {
                let Some(arg) = args.get(i) else { continue };
                match &p.ty {
                    Some(Type::Named(n, a)) if *n == var && a.is_empty() => {
                        if let Some(tn) = self.type_name_subst(arg, scope) {
                            found = Some(tn);
                            break;
                        }
                    }
                    // `xs: List(a)` / `b: Box(a, c)` / any generic `G(…var…)`: take
                    // `var` from its position among the argument's encoded scope
                    // arguments `G<arg0,arg1,…>`.
                    Some(Type::Named(_, slots))
                        if slots.iter().any(
                            |t| matches!(t, Type::Named(vn, va) if *vn == var && va.is_empty()),
                        ) =>
                    {
                        let pos = slots
                            .iter()
                            .position(|t| matches!(t, Type::Named(vn, va) if *vn == var && va.is_empty()))
                            .unwrap();
                        if let Some(elem) = self
                            .type_name_subst(arg, scope)
                            .as_deref()
                            .and_then(tuple_args)
                            .and_then(|a| a.get(pos).map(|s| s.to_string()))
                        {
                            found = Some(elem);
                            break;
                        }
                    }
                    // `xs: List((.., var, ..))` (e.g. `unzip`): resolve `var` from
                    // the argument list's element tuple at `var`'s position.
                    Some(Type::Named(n, a)) if n == "List" => {
                        if let Some(Type::Tuple(slots)) = a.first() {
                            if let Some(pos) = slots.iter().position(
                                |t| matches!(t, Type::Named(vn, va) if *vn == var && va.is_empty()),
                            ) {
                                if let Some(tn) = self.list_elem_tuple_field_type(arg, pos, scope) {
                                    found = Some(tn);
                                    break;
                                }
                            }
                        }
                    }
                    // `f: fn(...) -> var` (e.g. `map`'s mapper, whose result is the
                    // element type of the returned list): take `var` from the
                    // closure argument's return type.
                    Some(Type::Fn(_, ret))
                        if matches!(ret.as_ref(), Type::Named(vn, va) if *vn == var && va.is_empty()) =>
                    {
                        if let Some(tn) = self.closure_ret_type(arg, scope) {
                            found = Some(tn);
                            break;
                        }
                    }
                    // `self: (a, b, ...)` — a tuple impl's receiver: recover `var`
                    // from its slot in the argument tuple's encoded scope name.
                    Some(Type::Tuple(slots)) => {
                        if let Some(pos) = slots.iter().position(
                            |t| matches!(t, Type::Named(vn, va) if *vn == var && va.is_empty()),
                        ) {
                            if let Some(elem) = self
                                .type_name_subst(arg, scope)
                                .as_deref()
                                .and_then(tuple_args)
                                .and_then(|a| a.get(pos).map(|s| s.to_string()))
                            {
                                found = Some(elem);
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            // The typed-lowering resolution: unify each parameter's declared
            // type against the argument's typeck-resolved type. Table
            // bindings are exact, so any simple named type is trusted — this
            // is what lets `dict.get(d, key)` specialize its VALUE type, and
            // generic helpers specialize on user record types (content
            // equality). It both fills in what the head-name scope missed
            // and CONFIRMS a non-primitive name the scope guessed.
            let mut table_name = None;
            for (i, p) in template.params.iter().enumerate() {
                let (Some(pty), Some(arg)) = (&p.ty, args.get(i)) else { continue };
                if let Some(ty) = self.table.type_of(arg) {
                    if let Some(tn) = bind_var_simple(pty, ty, &var) {
                        table_name = Some(tn);
                        break;
                    }
                }
            }
            let mut from_table = match (&found, &table_name) {
                (None, Some(tn)) => {
                    found = Some(tn.clone());
                    true
                }
                (Some(f), Some(tn)) if f == tn => true,
                // The head-name scope saw only the bare head (`Box`, from a
                // constructor) while the table carries the full encoded type
                // (`Box<Int>`); the table is more specific, so trust it — this is
                // what recovers a generic constructor's element without a per-
                // constructor head-name special case.
                (Some(f), Some(tn)) if head_of(tn) == f => {
                    found = Some(tn.clone());
                    true
                }
                (Some(_), Some(_)) => false,
                _ => false,
            };
            // A RETURN-POSITION variable (`fn collect(...) -> c`): no argument
            // mentions it, so it binds from the call site's EXPECTED type —
            // typeck's table, fed by unification (an ascribed binding, a typed
            // parameter the result is passed to). This is what makes
            // annotation-driven instantiation work.
            if found.is_none() {
                if let (Some(ret), Some(ty)) = (&template.ret, result_ty) {
                    if let Some(tn) = bind_var_simple(ret, ty, &var) {
                        found = Some(tn);
                        from_table = true;
                    }
                }
            }
            // A `where`-bounded generic resolves to whatever concrete type the
            // trait dispatch picked (any named type). An *unbounded* generic is
            // only specialized on a primitive type argument — the cases the i32
            // generic ABI miscompiles — unless the TABLE confirmed the type
            // exactly; anything else falls back to the generic version rather
            // than producing an unsound specialization.
            let tn = found?;
            if !bounded && !from_table && !is_specializable_type_arg(&tn) {
                return None;
            }
            result.push(tn);
        }
        Some(result)
    }

    fn specialize(&mut self, name: &str, type_args: Vec<String>) -> String {
        let key = (name.to_string(), type_args.clone());
        if let Some(m) = self.memo.get(&key) {
            return m.clone();
        }
        // Type arguments may carry scope encodings ("List<Int>"); mangle
        // segments stay identifier-safe.
        let safe: Vec<String> = type_args
            .iter()
            .map(|t| {
                t.chars()
                    .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
                    .collect()
            })
            .collect();
        let mangled = format!("{name}__{}", safe.join("__"));
        self.memo.insert(key, mangled.clone());

        let mut f = self.templates[name].clone();
        f.name = mangled.clone();
        // Substitute over the same variable list `resolve_type_args` resolved:
        // the `where`-bound variables for a bounded generic, otherwise the free
        // type variables of the signature.
        let vars = type_var_list(&f);
        let subst: HashMap<&str, String> =
            vars.iter().map(|v| v.as_str()).zip(type_args).collect();
        // Owned copy stored parallel to the generated function, so its body walk
        // can resolve field-access types this instantiation makes concrete.
        let owned_subst: HashMap<String, String> =
            subst.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        for p in &mut f.params {
            if let Some(t) = &p.ty {
                p.ty = Some(subst_vars(t, &subst));
            }
        }
        f.ret = f.ret.as_ref().map(|t| subst_vars(t, &subst));
        if let Some(Type::Named(n, _)) = &f.ret {
            self.fn_rets.insert(mangled.clone(), n.clone());
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
            .map(|(m, t)| (m.clone(), t.clone()))
            .collect();
        let bounds_snapshot = f.bounds.clone();
        let mut renames: HashMap<String, String> = HashMap::new();
        for (bvar, btrait, btargs) in &bounds_snapshot {
            let Some(concrete) = subst.get(bvar.as_str()) else { continue };
            let head = concrete.split('<').next().unwrap_or(concrete).to_string();
            let impl_vars = self.impl_trait_args.get(&(btrait.clone(), head.clone())).cloned();
            for (method, owner) in &trait_method_pairs {
                if owner != btrait {
                    continue;
                }
                let mangled = format!("{btrait}__{head}__{method}");
                let mut target = mangled.clone();
                if let (Some(vars), Some(tmpl)) =
                    (&impl_vars, self.templates.get(&mangled).cloned())
                {
                    // Bind the impl method's own type variables by STRUCTURAL
                    // matching: each impl trait-argument pattern against the
                    // bound's (substituted) concrete argument. Anything that
                    // doesn't bind falls back to the generic impl function.
                    let mut bound_map: HashMap<String, String> = HashMap::new();
                    let mut ok = vars.len() == btargs.len();
                    if ok {
                        for (pat, targ) in vars.iter().zip(btargs) {
                            let concrete = subst_vars(targ, &subst);
                            if !bind_type_vars(pat, &concrete, &mut bound_map) {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        let mut targs_out: Vec<String> = Vec::new();
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
                }
                if target != mangled
                    || self.known_fns.contains(&mangled)
                    || self.templates.contains_key(&mangled)
                {
                    renames.insert(method.clone(), target);
                }
            }
        }
        if !renames.is_empty() {
            rename_calls_block(&mut f.body, &renames);
        }
        drop(subst);
        // Monomorphization discharges the `where` bounds: every bound type
        // variable is now a concrete type, and the trait obligation is satisfied
        // by the impl whose method this specialization's body resolves to.
        // Clearing them lets the (fully concrete) specialization compile on the
        // compiled backend, which has no notion of an unsatisfied generic bound.
        f.bounds = Vec::new();
        self.generated.push(f);
        self.generated_subst.push(owned_subst);
        mangled
    }

    fn walk_block(&mut self, b: &mut Block, scope: &mut Scope) {
        for stmt in &mut b.stmts {
            match stmt {
                Stmt::Let { name, ty, value, .. } => {
                    self.walk_expr(value, scope);
                    // Prefer the type ascription (`var items: List(a) = []`): it
                    // carries the element type an empty/ambiguous value loses. The
                    // value's inferred type is the fallback.
                    let resolved = ty
                        .as_ref()
                        .and_then(type_to_scope_name)
                        .or_else(|| self.type_name(value, scope));
                    match resolved {
                        Some(t) => {
                            scope.insert(name.clone(), t);
                        }
                        None => {
                            scope.remove(name.as_str());
                        }
                    }
                }
                Stmt::Assign { value, .. } => self.walk_expr(value, scope),
                // `let (x0, x1) = t` seeds each name from the tuple's slot types, so
                // a destructured tuple value's parts monomorphize (e.g. a tuple
                // impl's `reflect_one(x0)`).
                Stmt::LetTuple { names, value } => {
                    self.walk_expr(value, scope);
                    match self.type_name_subst(value, scope).as_deref().and_then(tuple_args) {
                        Some(args) => {
                            for (n, t) in names.iter().zip(args) {
                                scope.insert(n.clone(), t.to_string());
                            }
                        }
                        None => {
                            for n in names {
                                scope.remove(n.as_str());
                            }
                        }
                    }
                }
                Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => self.walk_expr(e, scope),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    fn walk_expr(&mut self, e: &mut Expr, scope: &mut Scope) {
        let result_ty = self.table.type_of(e).cloned();
        match e {
            Expr::Call { name, args } => {
                for a in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
                if let Some(template) = self.templates.get(name.as_str()).cloned() {
                    match self.resolve_type_args(&template, args, scope, result_ty.as_ref()) {
                        Some(type_args) => *name = self.specialize(name, type_args),
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
            | Expr::List(args)
            | Expr::Tuple(args) => {
                for a in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
                self.walk_expr(expr, scope)
            }
            Expr::RecordUpdate { base, fields } => {
                self.walk_expr(base, scope);
                for (_, v) in fields.iter_mut() {
                    self.walk_expr(v, scope);
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
                bind_ctor_pattern(pattern, self.ctor_fields, &mut s);
                self.walk_block(body, &mut s);
            }
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                self.walk_expr(cond, scope);
                self.walk_block(then_block, &mut scope.clone());
                if let Some(b) = else_block {
                    self.walk_block(b, &mut scope.clone());
                }
            }
            Expr::While { cond, body } => {
                self.walk_expr(cond, scope);
                self.walk_block(body, &mut scope.clone());
            }
            Expr::For { var, iter, body } => {
                self.walk_expr(iter, scope);
                let mut s = scope.clone();
                bind_loop_var(var, self.type_name(iter, scope), &mut s);
                self.walk_block(body, &mut s);
            }
            Expr::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee, scope);
                for arm in arms.iter_mut() {
                    let mut s = scope.clone();
                    bind_ctor_pattern(&arm.pattern, self.ctor_fields, &mut s);
                    if let Some(g) = &mut arm.guard {
                        self.walk_expr(g, &mut s);
                    }
                    self.walk_expr(&mut arm.body, &mut s);
                }
            }
            Expr::Lambda { params, body, .. } => {
                let mut s = scope.clone();
                seed_params(params, &mut s);
                self.walk_block(body, &mut s);
            }
            Expr::Block(b) => self.walk_block(b, &mut scope.clone()),
            Expr::Var(_) | Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
        }
    }
}
