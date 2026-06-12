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
fn method_fn(name: String, mut params: Vec<Param>, ret: Option<Type>, body: Block, type_name: &str) -> Function {
    for p in &mut params {
        if let Some(t) = &p.ty {
            p.ty = Some(subst_self(t, type_name));
        }
    }
    if let Some(first) = params.first_mut() {
        if first.ty.is_none() {
            first.ty = Some(Type::Named(type_name.to_string(), vec![]));
        }
    }
    let ret = ret.map(|t| subst_self(&t, type_name));
    Function {
        public: true,
        name,
        params,
        ret,
        body,
        bounds: Vec::new(),
        is_gen: false,
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
    });
    if !needs_lowering {
        return (module, Vec::new());
    }

    // method name -> owning trait (increment 1 assumes a method name is unique
    // across traits), and each trait's full method list (for default bodies).
    let mut trait_methods: HashMap<String, String> = HashMap::new();
    let mut trait_method_list: HashMap<String, Vec<MethodSig>> = HashMap::new();
    for item in &module.items {
        if let Item::Trait(t) = item {
            for m in &t.methods {
                trait_methods.insert(m.name.clone(), t.name.clone());
            }
            trait_method_list.insert(t.name.clone(), t.methods.clone());
        }
    }

    // (method name, receiver type) -> mangled function, plus the generated
    // functions themselves (impl methods with `self` typed to the impl type).
    let mut impl_table: HashMap<(String, String), String> = HashMap::new();
    // (type name, method name) -> mangled fn, for self-less impl methods.
    let mut statics: HashMap<(String, String), String> = HashMap::new();
    let mut generated: Vec<Function> = Vec::new();
    for item in &module.items {
        if let Item::Impl(im) = item {
            let provided: HashSet<&str> = im.methods.iter().map(|m| m.name.as_str()).collect();
            // Methods the impl defines. A method whose first parameter is
            // `self` is an INSTANCE method (dispatched on a value); one
            // without is a STATIC, callable only as `Type.name(args)`.
            for method in &im.methods {
                let mangled = mangle(im.trait_name.as_deref(), &im.type_name, &method.name);
                let is_static =
                    method.params.first().map_or(true, |p| p.name != "self");
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
            impl_table: &impl_table,
            ctor_results: &ctor_results,
            fn_rets: &fn_rets,
            ctor_fields: &ctor_fields,
            free_fns: &free_fns,
            missing_impls: &quiet,
            statics: &statics,
            table: &empty_table,
        };
        for item in &mut items {
            match item {
                Item::Function(f) => {
                    let mut scope = Scope::new();
                    seed_params(&f.params, &mut scope);
                    ctx.rewrite_block(&mut f.body, &mut scope);
                }
                Item::Actor(a) => {
                    for field in &mut a.fields {
                        if let Some(init) = &mut field.init {
                            ctx.rewrite_expr(init, &mut Scope::new());
                        }
                    }
                    for h in &mut a.handlers {
                        let mut scope = Scope::new();
                        seed_params(&h.params, &mut scope);
                        ctx.rewrite_block(&mut h.body, &mut scope);
                    }
                }
                _ => {}
            }
        }
    }

    let (items_back, type_table) = {
        let probe = Module {
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
    if !templates.is_empty() {
        let (ctor_results, fn_rets) = build_tables(&items);
        let ctor_fields = build_ctor_fields(&items);
        let mut mono = Mono {
            templates: &templates,
            ctor_results: &ctor_results,
            ctor_fields: &ctor_fields,
            fn_rets,
            memo: HashMap::new(),
            generated: Vec::new(),
            table: &type_table,
        };
        mono.run(&mut items);
        items.extend(mono.generated.into_iter().map(Item::Function));
    }

    // Tables used to determine a receiver's type at a trait-method call site.
    let (ctor_results, fn_rets) = build_tables(&items);
    let ctor_fields = build_ctor_fields(&items);
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
        impl_table: &impl_table,
        ctor_results: &ctor_results,
        fn_rets: &fn_rets,
        ctor_fields: &ctor_fields,
        free_fns: &free_fns,
        missing_impls: &missing_impls,
        statics: &statics,
        table: &type_table,
    };
    for item in &mut items {
        match item {
            Item::Function(f) => {
                let mut scope = Scope::new();
                seed_params(&f.params, &mut scope);
                ctx.rewrite_block(&mut f.body, &mut scope);
            }
            Item::Actor(a) => {
                for field in &mut a.fields {
                    if let Some(init) = &mut field.init {
                        ctx.rewrite_expr(init, &mut Scope::new());
                    }
                }
                for h in &mut a.handlers {
                    let mut scope = Scope::new();
                    seed_params(&h.params, &mut scope);
                    ctx.rewrite_block(&mut h.body, &mut scope);
                }
            }
            _ => {}
        }
    }

    (
        Module { imports, items, import_lines: Vec::new(), item_lines: Vec::new() },
        missing_impls.into_inner(),
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
    impl_table: &'a HashMap<(String, String), String>,
    ctor_results: &'a HashMap<String, String>,
    fn_rets: &'a HashMap<String, String>,
    ctor_fields: &'a HashMap<String, Vec<Type>>,
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
        head_type_name(e, scope, self.ctor_results, self.fn_rets)
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
                Stmt::Assign { value, .. } | Stmt::LetTuple { value, .. } => {
                    self.rewrite_expr(value, scope)
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
                            match self.impl_table.get(&(name.clone(), tn.clone())) {
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
            | Expr::Tuple(args)
            | Expr::Spawn { args, .. } => {
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
                    if let Some(mangled) = self.impl_table.get(&(method.clone(), tn.clone())) {
                        let mut call_args = vec![std::mem::replace(
                            receiver.as_mut(),
                            Expr::Bool(false),
                        )];
                        call_args.append(args);
                        *e = Expr::Call { name: mangled.clone(), args: call_args };
                        return;
                    }
                }
                // A trait method on a generic (bound) receiver dispatches
                // after monomorphization: lower to the bare trait call.
                let receiver_is_generic =
                    tn.as_deref().is_none_or(|n| n.chars().next().is_some_and(char::is_lowercase));
                if self.trait_methods.contains_key(method.as_str()) && receiver_is_generic {
                    let mut call_args =
                        vec![std::mem::replace(receiver.as_mut(), Expr::Bool(false))];
                    call_args.append(args);
                    *e = Expr::Call { name: method.clone(), args: call_args };
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
            Expr::Lambda { params, body } => {
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
        "int_to_string" | "to_string" => "String",
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
) -> Option<String> {
    match e {
        Expr::Int(_) => Some("Int".into()),
        Expr::Float(_) => Some("Float".into()),
        Expr::Str(_) => Some("String".into()),
        Expr::Bool(_) => Some("Bool".into()),
        Expr::Duration(_) => Some("Duration".into()),
        Expr::Var(n) => scope.get(n).cloned(),
        Expr::Ctor { name, .. } => ctor_results.get(name).cloned(),
        Expr::Call { name, .. } => fn_rets.get(name).cloned().or_else(|| builtin_ret(name)),
        Expr::RecordUpdate { base, .. } => head_type_name(base, scope, ctor_results, fn_rets),
        // `!` yields Bool; `-`/`~` preserve the operand's type (so `-5` is Int).
        Expr::Unary { op, expr } => match op {
            UnOp::Not => Some("Bool".into()),
            UnOp::Neg | UnOp::BitNot | UnOp::Move => head_type_name(expr, scope, ctor_results, fn_rets),
        },
        // Comparisons/logic yield Bool; `<>` yields String; arithmetic and
        // bitwise ops have the type of their (left) operand.
        Expr::Binary { op, lhs, .. } => match op {
            BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
            | BinOp::And | BinOp::Or => Some("Bool".into()),
            BinOp::Concat => Some("String".into()),
            _ => head_type_name(lhs, scope, ctor_results, fn_rets),
        },
        // A list literal's type encodes its element type when determinable from
        // the first element, e.g. `List<Int>`, so a `for` loop over it can type
        // the loop variable. `list_elem` reads the element back out.
        Expr::List(items) => Some(
            match items
                .first()
                .and_then(|e| head_type_name(e, scope, ctor_results, fn_rets))
            {
                Some(elem) => format!("List<{elem}>"),
                None => "List".to_string(),
            },
        ),
        _ => None,
    }
}

/// The element type encoded in a `List<...>` scope name, if any.
fn list_elem(type_name: &str) -> Option<&str> {
    type_name.strip_prefix("List<")?.strip_suffix('>')
}

/// The scope name for a declared parameter type, encoding a list's element type
/// (`List<Int>`) so loop-variable typing works on annotated list parameters.
fn type_to_scope_name(t: &Type) -> Option<String> {
    match t {
        Type::Named(n, args) if n == "List" => {
            Some(match args.first().and_then(type_to_scope_name) {
                Some(elem) => format!("List<{elem}>"),
                None => "List".to_string(),
            })
        }
        Type::Named(n, _) => Some(n.clone()),
        _ => None,
    }
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
fn subst_vars(t: &Type, subst: &HashMap<&str, String>) -> Type {
    match t {
        Type::Named(n, args) if args.is_empty() && subst.contains_key(n.as_str()) => {
            Type::Named(subst[n.as_str()].clone(), vec![])
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
fn type_var_list(f: &Function) -> Vec<String> {
    if f.bounds.is_empty() {
        signature_type_vars(f)
    } else {
        f.bounds.iter().map(|(v, _)| v.clone()).collect()
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
    ctor_results: &'a HashMap<String, String>,
    ctor_fields: &'a HashMap<String, Vec<Type>>,
    fn_rets: HashMap<String, String>,
    memo: HashMap<(String, Vec<String>), String>,
    generated: Vec<Function>,
    /// typeck's resolved types for this module instance: the fallback when
    /// the head-name scope can't resolve a type argument.
    table: &'a crate::typeck::TypeTable,
}

impl Mono<'_> {
    fn run(&mut self, items: &mut [Item]) {
        for item in items.iter_mut() {
            match item {
                Item::Function(f) => {
                    let mut s = Scope::new();
                    seed_params(&f.params, &mut s);
                    self.walk_block(&mut f.body, &mut s);
                }
                Item::Actor(a) => {
                    for field in &mut a.fields {
                        if let Some(init) = &mut field.init {
                            self.walk_expr(init, &mut Scope::new());
                        }
                    }
                    for h in &mut a.handlers {
                        let mut s = Scope::new();
                        seed_params(&h.params, &mut s);
                        self.walk_block(&mut h.body, &mut s);
                    }
                }
                _ => {}
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
            self.walk_block(&mut body, &mut s);
            self.generated[i].body = body;
            i += 1;
        }
    }

    fn type_name(&self, e: &Expr, scope: &Scope) -> Option<String> {
        head_type_name(e, scope, self.ctor_results, &self.fn_rets)
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
            Expr::Lambda { params, body } => {
                let mut s = scope.clone();
                seed_params(params, &mut s);
                match body.stmts.last() {
                    Some(Stmt::Expr(e)) | Some(Stmt::Return(Some(e))) => {
                        head_type_name(e, &s, self.ctor_results, &self.fn_rets)
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
                        if let Some(tn) = self.type_name(arg, scope) {
                            found = Some(tn);
                            break;
                        }
                    }
                    Some(Type::Named(n, a))
                        if n == "List"
                            && matches!(a.first(), Some(Type::Named(vn, va)) if *vn == var && va.is_empty()) =>
                    {
                        if let Some(elem) = self
                            .type_name(arg, scope)
                            .as_deref()
                            .and_then(list_elem)
                        {
                            found = Some(elem.to_string());
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
            let from_table = match (&found, &table_name) {
                (None, Some(tn)) => {
                    found = Some(tn.clone());
                    true
                }
                (Some(f), Some(tn)) => f == tn,
                _ => false,
            };
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
        let mangled = format!("{name}__{}", type_args.join("__"));
        self.memo.insert(key, mangled.clone());

        let mut f = self.templates[name].clone();
        f.name = mangled.clone();
        // Substitute over the same variable list `resolve_type_args` resolved:
        // the `where`-bound variables for a bounded generic, otherwise the free
        // type variables of the signature.
        let vars = type_var_list(&f);
        let subst: HashMap<&str, String> =
            vars.iter().map(|v| v.as_str()).zip(type_args).collect();
        for p in &mut f.params {
            if let Some(t) = &p.ty {
                p.ty = Some(subst_vars(t, &subst));
            }
        }
        f.ret = f.ret.as_ref().map(|t| subst_vars(t, &subst));
        if let Some(Type::Named(n, _)) = &f.ret {
            self.fn_rets.insert(mangled.clone(), n.clone());
        }
        drop(subst);
        // Monomorphization discharges the `where` bounds: every bound type
        // variable is now a concrete type, and the trait obligation is satisfied
        // by the impl whose method this specialization's body resolves to.
        // Clearing them lets the (fully concrete) specialization compile on the
        // compiled backend, which has no notion of an unsatisfied generic bound.
        f.bounds = Vec::new();
        self.generated.push(f);
        mangled
    }

    fn walk_block(&mut self, b: &mut Block, scope: &mut Scope) {
        for stmt in &mut b.stmts {
            match stmt {
                Stmt::Let { name, value, .. } => {
                    self.walk_expr(value, scope);
                    match self.type_name(value, scope) {
                        Some(t) => {
                            scope.insert(name.clone(), t);
                        }
                        None => {
                            scope.remove(name.as_str());
                        }
                    }
                }
                Stmt::Assign { value, .. } | Stmt::LetTuple { value, .. } => {
                    self.walk_expr(value, scope)
                }
                Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => self.walk_expr(e, scope),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    fn walk_expr(&mut self, e: &mut Expr, scope: &mut Scope) {
        match e {
            Expr::Call { name, args } => {
                for a in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
                if let Some(template) = self.templates.get(name.as_str()).cloned() {
                    if let Some(type_args) = self.resolve_type_args(&template, args, scope) {
                        *name = self.specialize(name, type_args);
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
            | Expr::Tuple(args)
            | Expr::Spawn { args, .. } => {
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
            Expr::Lambda { params, body } => {
                let mut s = scope.clone();
                seed_params(params, &mut s);
                self.walk_block(body, &mut s);
            }
            Expr::Block(b) => self.walk_block(b, &mut scope.clone()),
            Expr::Var(_) | Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
        }
    }
}
