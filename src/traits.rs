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
fn mangle(trait_name: &str, type_name: &str, method: &str) -> String {
    format!("{trait_name}__{type_name}__{method}")
}

/// Build the ordinary function a (possibly defaulted) method lowers to, giving
/// the receiver (`self`, the first parameter) the implementing type if it was
/// left unannotated.
fn method_fn(name: String, mut params: Vec<Param>, ret: Option<Type>, body: Block, type_name: &str) -> Function {
    if let Some(first) = params.first_mut() {
        if first.ty.is_none() {
            first.ty = Some(Type::Named(type_name.to_string(), vec![]));
        }
    }
    Function {
        public: true,
        name,
        params,
        ret,
        body,
    }
}

/// Desugar all traits and impls in `module` into ordinary functions, rewriting
/// trait-method call sites to the resolved impl. A no-op (returns the module
/// unchanged) when there are no traits or impls, so non-trait programs — every
/// existing one — are unaffected.
pub fn lower(module: Module) -> Module {
    let has_traits = module
        .items
        .iter()
        .any(|it| matches!(it, Item::Trait(_) | Item::Impl(_)));
    if !has_traits {
        return module;
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
    let mut generated: Vec<Function> = Vec::new();
    for item in &module.items {
        if let Item::Impl(im) = item {
            let provided: HashSet<&str> = im.methods.iter().map(|m| m.name.as_str()).collect();
            // Methods the impl defines.
            for method in &im.methods {
                let mangled = mangle(&im.trait_name, &im.type_name, &method.name);
                impl_table.insert((method.name.clone(), im.type_name.clone()), mangled.clone());
                generated.push(method_fn(
                    mangled,
                    method.params.clone(),
                    method.ret.clone(),
                    method.body.clone(),
                    &im.type_name,
                ));
            }
            // Methods the impl omits but the trait provides a default for.
            if let Some(methods) = trait_method_list.get(&im.trait_name) {
                for ms in methods {
                    if provided.contains(ms.name.as_str()) {
                        continue;
                    }
                    if let Some(body) = &ms.default {
                        let mangled = mangle(&im.trait_name, &im.type_name, &ms.name);
                        impl_table.insert((ms.name.clone(), im.type_name.clone()), mangled.clone());
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

    // Keep everything that isn't a trait/impl, then append the lowered methods.
    let imports = module.imports;
    let mut items: Vec<Item> = module
        .items
        .into_iter()
        .filter(|it| !matches!(it, Item::Trait(_) | Item::Impl(_)))
        .collect();
    items.extend(generated.into_iter().map(Item::Function));

    // Tables used to determine a receiver's type at a call site.
    let mut ctor_results: HashMap<String, String> = HashMap::new();
    let mut fn_rets: HashMap<String, String> = HashMap::new();
    for item in &items {
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

    let ctx = Ctx {
        trait_methods: &trait_methods,
        impl_table: &impl_table,
        ctor_results: &ctor_results,
        fn_rets: &fn_rets,
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

    Module { imports, items }
}

/// Variable name -> the head name of its (known) type.
type Scope = HashMap<String, String>;

fn seed_params(params: &[Param], scope: &mut Scope) {
    for p in params {
        if let Some(Type::Named(n, _)) = &p.ty {
            scope.insert(p.name.clone(), n.clone());
        }
    }
}

struct Ctx<'a> {
    trait_methods: &'a HashMap<String, String>,
    impl_table: &'a HashMap<(String, String), String>,
    ctor_results: &'a HashMap<String, String>,
    fn_rets: &'a HashMap<String, String>,
}

impl Ctx<'_> {
    /// Best-effort head type name of an expression, or `None` if undeterminable
    /// without full inference.
    fn type_name(&self, e: &Expr, scope: &Scope) -> Option<String> {
        match e {
            Expr::Int(_) => Some("Int".into()),
            Expr::Float(_) => Some("Float".into()),
            Expr::Str(_) => Some("String".into()),
            Expr::Bool(_) => Some("Bool".into()),
            Expr::Var(n) => scope.get(n).cloned(),
            Expr::Ctor { name, .. } => self.ctor_results.get(name).cloned(),
            Expr::Call { name, .. } => {
                self.fn_rets.get(name).cloned().or_else(|| builtin_ret(name))
            }
            Expr::RecordUpdate { base, .. } => self.type_name(base, scope),
            _ => None,
        }
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
                Stmt::Return(Some(e)) | Stmt::Expr(e) => self.rewrite_expr(e, scope),
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
                if self.trait_methods.contains_key(name.as_str()) {
                    if let Some(recv) = args.first() {
                        if let Some(tn) = self.type_name(recv, scope) {
                            if let Some(mangled) = self.impl_table.get(&(name.clone(), tn)) {
                                *name = mangled.clone();
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
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::Field { base: expr, .. } => {
                self.rewrite_expr(expr, scope)
            }
            Expr::RecordUpdate { base, fields } => {
                self.rewrite_expr(base, scope);
                for (_, v) in fields.iter_mut() {
                    self.rewrite_expr(v, scope);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.rewrite_expr(lhs, scope);
                self.rewrite_expr(rhs, scope);
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
            Expr::For { iter, body, .. } => {
                self.rewrite_expr(iter, scope);
                self.rewrite_block(body, &mut scope.clone());
            }
            Expr::Match { scrutinee, arms } => {
                self.rewrite_expr(scrutinee, scope);
                for arm in arms.iter_mut() {
                    let mut s = scope.clone();
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
            Expr::Var(_) | Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
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
