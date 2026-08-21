//! Local item reachability for compile-time helper programs.
//!
//! `comptime:` blocks and tagged literals both execute a synthetic Witchy module.
//! That module should contain the helper code the generator actually calls, but
//! not the consumer's whole runtime module. Keeping this traversal shared prevents
//! the two compile-time paths from drifting.

use std::collections::{HashMap, HashSet};

use witchy_syntax::ast::{
    collect_type_names, Block, Expr, Function, Item, Module, Pattern, Stmt, Type,
};

const TAGGED_LITERAL_REF: &str = "@compiler:tagged-literal";
const IMPL_REF_PREFIX: &str = "@compiler:impl:";

pub(crate) fn impl_item_identity(index: usize) -> String {
    format!("{IMPL_REF_PREFIX}{index}")
}

/// Module-qualified item identities reachable from one function.
pub(crate) fn reachable_from_module_function(
    modules: &[(String, Module)],
    root_module: &str,
    root: &str,
) -> HashSet<(String, String)> {
    let mut keep = HashSet::new();
    let mut work = Vec::new();
    push_module_ref(modules, root_module, root, &mut keep, &mut work);

    while let Some((module_name, item_name)) = work.pop() {
        let Some((_, module)) = modules.iter().find(|(name, _)| name == &module_name) else {
            continue;
        };
        let mut refs = HashSet::new();
        if let Some(item) = module
            .items
            .iter()
            .enumerate()
            .find_map(|(index, item)| item_owns_name(item, index, &item_name).then_some(item))
        {
            match item {
                Item::Function(function) => collect_function_refs(function, &mut refs),
                Item::Type(ty) => {
                    for variant in &ty.variants {
                        for field in &variant.fields {
                            collect_type_names(field, &mut refs);
                        }
                    }
                }
                Item::TypeAlias { ty, .. } => collect_type_names(ty, &mut refs),
                Item::Trait(trait_) => {
                    refs.extend(trait_.supertraits.iter().cloned());
                    for method in &trait_.methods {
                        for param in &method.params {
                            if let Some(ty) = &param.ty {
                                collect_type_names(ty, &mut refs);
                            }
                        }
                        if let Some(ty) = &method.ret {
                            collect_type_names(ty, &mut refs);
                        }
                        if let Some(default) = &method.default {
                            collect_refs_block(default, &mut refs);
                        }
                    }
                }
                Item::Impl(impl_) => {
                    if let Some(trait_name) = &impl_.trait_name {
                        refs.insert(trait_name.clone());
                    }
                    refs.insert(impl_.type_name.clone());
                    for ty in impl_.trait_args.iter().chain(&impl_.target_args) {
                        collect_type_names(ty, &mut refs);
                    }
                    for (_, trait_name, args) in &impl_.bounds {
                        refs.insert(trait_name.clone());
                        for ty in args {
                            collect_type_names(ty, &mut refs);
                        }
                    }
                    for method in &impl_.methods {
                        collect_function_refs(method, &mut refs);
                    }
                }
                Item::Const { value, .. } => collect_refs_expr(value, &mut refs),
                _ => {}
            }
            if matches!(item, Item::Type(_) | Item::Trait(_)) {
                enqueue_matching_impls(
                    modules,
                    &module_name,
                    &item_name,
                    &mut keep,
                    &mut work,
                );
            }
        }
        for name in refs {
            push_module_ref(modules, &module_name, &name, &mut keep, &mut work);
        }
    }
    keep
}

fn collect_function_refs(function: &Function, refs: &mut HashSet<String>) {
    collect_refs_block(&function.body, refs);
    for param in &function.params {
        if let Some(ty) = &param.ty {
            collect_type_names(ty, refs);
        }
    }
    if let Some(ty) = &function.ret {
        collect_type_names(ty, refs);
    }
    for (_, trait_name, args) in &function.bounds {
        refs.insert(trait_name.clone());
        for ty in args {
            collect_type_names(ty, refs);
        }
    }
}

fn enqueue_matching_impls(
    modules: &[(String, Module)],
    item_module: &str,
    item_name: &str,
    keep: &mut HashSet<(String, String)>,
    work: &mut Vec<(String, String)>,
) {
    let wanted = (item_module.to_string(), item_name.to_string());
    for (module_name, module) in modules {
        for (index, item) in module.items.iter().enumerate() {
            let Item::Impl(impl_) = item else { continue };
            let type_matches = resolve_named_identity(modules, module_name, &impl_.type_name)
                .is_some_and(|identity| identity == wanted);
            let trait_matches = impl_
                .trait_name
                .as_deref()
                .and_then(|name| resolve_named_identity(modules, module_name, name))
                .is_some_and(|identity| identity == wanted);
            if !type_matches && !trait_matches {
                continue;
            }
            let key = (module_name.clone(), impl_item_identity(index));
            if keep.insert(key.clone()) {
                work.push(key);
            }
        }
    }
}

pub(crate) fn block_contains_tagged_literal(block: &Block) -> bool {
    let mut refs = HashSet::new();
    collect_refs_block(block, &mut refs);
    refs.contains(TAGGED_LITERAL_REF)
}

pub(crate) fn expr_contains_tagged_literal(expr: &Expr) -> bool {
    let mut refs = HashSet::new();
    collect_refs_expr(expr, &mut refs);
    refs.contains(TAGGED_LITERAL_REF)
}

fn resolve_named_identity(
    modules: &[(String, Module)],
    owner: &str,
    reference: &str,
) -> Option<(String, String)> {
    if let Some((module_name, name)) = reference.split_once('.') {
        let (_, module) = modules
            .iter()
            .find(|(candidate, _)| candidate == module_name)?;
        return owned_item_name(module, name).map(|name| (module_name.to_string(), name));
    }

    let (_, module) = modules.iter().find(|(name, _)| name == owner)?;
    if let Some(name) = owned_item_name(module, reference) {
        return Some((owner.to_string(), name));
    }

    let mut imported = HashSet::new();
    for (source, names) in &module.from_imports {
        if !names.iter().any(|name| name == reference) {
            continue;
        }
        let Some((_, source_module)) = modules.iter().find(|(name, _)| name == source) else {
            continue;
        };
        if let Some(name) = owned_item_name(source_module, reference) {
            imported.insert((source.clone(), name));
        }
    }
    (imported.len() == 1).then(|| imported.into_iter().next()).flatten()
}

fn push_module_ref(
    modules: &[(String, Module)],
    owner: &str,
    reference: &str,
    keep: &mut HashSet<(String, String)>,
    work: &mut Vec<(String, String)>,
) {
    let mut enqueue = |module: &str, name: String| {
        let key = (module.to_string(), name);
        if keep.insert(key.clone()) {
            work.push(key);
        }
    };

    if let Some((module_name, name)) = reference.split_once('.') {
        if let Some((_, module)) = modules.iter().find(|(candidate, _)| candidate == module_name) {
            if let Some(owned) = owned_item_name(module, name) {
                enqueue(module_name, owned);
            }
        }
        return;
    }

    let Some((_, module)) = modules.iter().find(|(name, _)| name == owner) else {
        return;
    };
    if let Some(owned) = owned_item_name(module, reference) {
        enqueue(owner, owned);
        return;
    }
    for (source, names) in &module.from_imports {
        if !names.iter().any(|name| name == reference) {
            continue;
        }
        if let Some((_, imported)) = modules.iter().find(|(name, _)| name == source) {
            if let Some(owned) = owned_item_name(imported, reference) {
                enqueue(source, owned);
            }
        }
    }
}

fn owned_item_name(module: &Module, name: &str) -> Option<String> {
    module.items.iter().find_map(|item| match item {
        Item::Function(function) if function.name == name => Some(function.name.clone()),
        Item::Type(ty) if ty.name == name => Some(ty.name.clone()),
        Item::Type(ty) if ty.variants.iter().any(|variant| variant.name == name) => {
            Some(ty.name.clone())
        }
        Item::Trait(trait_) if trait_.name == name => Some(trait_.name.clone()),
        Item::Const { name: constant, .. } if constant == name => Some(constant.clone()),
        Item::TypeAlias { name: alias, .. } if alias == name => Some(alias.clone()),
        _ => None,
    })
}

pub(crate) fn module_item_identity(
    modules: &[(String, Module)],
    module_name: &str,
    name: &str,
) -> Option<String> {
    let (_, module) = modules.iter().find(|(candidate, _)| candidate == module_name)?;
    owned_item_name(module, name)
}

fn item_owns_name(item: &Item, index: usize, name: &str) -> bool {
    match item {
        Item::Function(function) => function.name == name,
        Item::Type(ty) => ty.name == name,
        Item::Trait(trait_) => trait_.name == name,
        Item::Impl(_) => name == impl_item_identity(index),
        Item::Const { name: constant, .. } => constant == name,
        Item::TypeAlias { name: alias, .. } => alias == name,
        _ => false,
    }
}

/// The names of every local item reachable from a root block.
pub(crate) fn reachable_from_block(items: &[Item], root: &Block) -> HashSet<String> {
    let ctx = Reachability::new(items);
    let mut keep = HashSet::new();
    let mut work = Vec::new();
    let mut names = HashSet::new();
    collect_refs_block(root, &mut names);
    for name in names {
        ctx.push_ref(&name, &mut keep, &mut work);
    }
    ctx.drain(&mut keep, &mut work);
    keep
}

struct Reachability<'a> {
    fns: HashMap<&'a str, &'a Function>,
    types: HashMap<&'a str, &'a witchy_syntax::ast::TypeDef>,
    aliases: HashMap<&'a str, &'a Type>,
    ctor_owner: HashMap<&'a str, &'a str>,
}

impl<'a> Reachability<'a> {
    fn new(items: &'a [Item]) -> Self {
        let mut fns = HashMap::new();
        let mut types = HashMap::new();
        let mut aliases = HashMap::new();
        let mut ctor_owner = HashMap::new();
        for item in items {
            match item {
                Item::Function(f) => {
                    fns.insert(f.name.as_str(), f);
                }
                Item::Type(t) => {
                    types.insert(t.name.as_str(), t);
                    for v in &t.variants {
                        ctor_owner.insert(v.name.as_str(), t.name.as_str());
                    }
                }
                Item::TypeAlias { name, ty, .. } => {
                    aliases.insert(name.as_str(), ty);
                }
                _ => {}
            }
        }
        Self { fns, types, aliases, ctor_owner }
    }

    fn push_ref(&self, name: &str, keep: &mut HashSet<String>, work: &mut Vec<String>) {
        let mut enqueue = |n: &str| {
            if keep.insert(n.to_string()) {
                work.push(n.to_string());
            }
        };
        if self.fns.contains_key(name) {
            enqueue(name);
        }
        if self.types.contains_key(name) {
            enqueue(name);
        }
        if self.aliases.contains_key(name) {
            enqueue(name);
        }
        if let Some(owner) = self.ctor_owner.get(name) {
            enqueue(owner);
        }
    }

    fn drain(&self, keep: &mut HashSet<String>, work: &mut Vec<String>) {
        while let Some(name) = work.pop() {
            if let Some(f) = self.fns.get(name.as_str()) {
                let mut names = HashSet::new();
                collect_refs_block(&f.body, &mut names);
                for p in &f.params {
                    if let Some(t) = &p.ty {
                        collect_type_names(t, &mut names);
                    }
                }
                if let Some(t) = &f.ret {
                    collect_type_names(t, &mut names);
                }
                for r in names {
                    self.push_ref(&r, keep, work);
                }
            }
            if let Some(t) = self.types.get(name.as_str()) {
                let mut names = HashSet::new();
                for v in &t.variants {
                    for field in &v.fields {
                        collect_type_names(field, &mut names);
                    }
                }
                for r in names {
                    self.push_ref(&r, keep, work);
                }
            }
            if let Some(t) = self.aliases.get(name.as_str()) {
                let mut names = HashSet::new();
                collect_type_names(t, &mut names);
                for r in names {
                    self.push_ref(&r, keep, work);
                }
            }
        }
    }
}

/// Collect every name a block references: callees, variables, constructors,
/// constructor names in patterns, and types named in annotations.
fn collect_refs_block(b: &Block, out: &mut HashSet<String>) {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { ty, value, .. } => {
                if let Some(t) = ty {
                    collect_type_names(t, out);
                }
                collect_refs_expr(value, out);
            }
            Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } => {
                collect_refs_expr(value, out)
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => collect_refs_expr(e, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_refs_pattern(p: &Pattern, out: &mut HashSet<String>) {
    match p {
        Pattern::Ctor { name, args } => {
            out.insert(name.clone());
            for a in args {
                collect_refs_pattern(a, out);
            }
        }
        Pattern::AnonCtor { args, .. } => {
            for a in args {
                collect_refs_pattern(a, out);
            }
        }
        Pattern::Tuple(args) | Pattern::List { elems: args, .. } | Pattern::Or(args) => {
            for a in args {
                collect_refs_pattern(a, out);
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

fn collect_refs_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Call { name, args } | Expr::Ctor { name, args } => {
            out.insert(name.clone());
            for a in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::AnonCtor { args, .. } => {
            for a in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::LabeledCall { name, args } => {
            out.insert(name.clone());
            for (_, a) in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            collect_refs_expr(receiver, out);
            for (_, a) in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::Var(name) => {
            out.insert(name.clone());
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_refs_expr(receiver, out);
            for a in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::Apply { func, args } => {
            collect_refs_expr(func, out);
            for a in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                collect_refs_expr(x, out);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::Field { base: expr, .. } => {
            collect_refs_expr(expr, out)
        }
        Expr::As { expr, ty } => {
            collect_refs_expr(expr, out);
            collect_type_names(ty, out);
        }
        Expr::ExistentialPack { expr, ty, .. }
        | Expr::ExistentialUpcast { expr, ty } => {
            collect_refs_expr(expr, out);
            collect_type_names(ty, out);
        }
        Expr::ExistentialCall { receiver, args, ty, result, .. } => {
            collect_refs_expr(receiver, out);
            for arg in args {
                collect_refs_expr(arg, out);
            }
            collect_type_names(ty, out);
            collect_type_names(result, out);
        }
        Expr::RecordUpdate { base, fields, .. } => {
            collect_refs_expr(base, out);
            for (_, v) in fields {
                collect_refs_expr(v, out);
            }
        }
        Expr::Record { name, fields, spread } => {
            out.insert(name.clone());
            for (_, v) in fields {
                collect_refs_expr(v, out);
            }
            if let Some(s) = spread {
                collect_refs_expr(s, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_refs_expr(lhs, out);
            collect_refs_expr(rhs, out);
        }
        Expr::If { cond, then_block, else_block } => {
            collect_refs_expr(cond, out);
            collect_refs_block(then_block, out);
            if let Some(b) = else_block {
                collect_refs_block(b, out);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_refs_expr(scrutinee, out);
            for arm in arms {
                collect_refs_pattern(&arm.pattern, out);
                if let Some(g) = &arm.guard {
                    collect_refs_expr(g, out);
                }
                collect_refs_expr(&arm.body, out);
            }
        }
        Expr::While { cond, body } => {
            collect_refs_expr(cond, out);
            collect_refs_block(body, out);
        }
        Expr::For { iter, body, .. } => {
            collect_refs_expr(iter, out);
            collect_refs_block(body, out);
        }
        Expr::Range { lo, hi, .. } => {
            collect_refs_expr(lo, out);
            collect_refs_expr(hi, out);
        }
        Expr::Index { base, index } => {
            collect_refs_expr(base, out);
            collect_refs_expr(index, out);
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            collect_refs_pattern(pattern, out);
            collect_refs_expr(scrutinee, out);
            collect_refs_block(body, out);
        }
        Expr::Lambda { params, body, ret, .. } => {
            for p in params {
                if let Some(t) = &p.ty {
                    collect_type_names(t, out);
                }
            }
            if let Some(t) = ret {
                collect_type_names(t, out);
            }
            collect_refs_block(body, out);
        }
        Expr::Block(b) => collect_refs_block(b, out),
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_) => {}
        Expr::TaggedLit { .. } => {
            out.insert(TAGGED_LITERAL_REF.to_string());
        }
    }
}
