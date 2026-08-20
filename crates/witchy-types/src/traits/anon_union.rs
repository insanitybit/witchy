//! Compiler-synthesized trait impls for anonymous-union types.
//!
//! Extracted verbatim from `traits.rs` (RFC-0046 typed trait dispatch). An
//! anonymous union `A | B | ...` gets `Show`/`Reflect`/`PartialEq` impls
//! generated automatically when its element types satisfy the corresponding
//! bound. `synthesize_anon_union_impls` walks the module for anon-union heads
//! and emits the matching `ImplDef`s; it is the sole entry point.

use foldhash::{HashMap, HashMapExt as _, HashSet};

use witchy_syntax::ast::*;

use super::{is_standard_trait_identity, named_type};

pub(crate) fn synthesize_anon_union_impls(
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
    let show_trait = trait_identity_with_method(trait_method_list, "show", "Show", "show");
    let reflect_trait =
        trait_identity_with_method(trait_method_list, "reflect", "Reflect", "reflect");
    let partial_eq_trait =
        trait_identity_with_method(trait_method_list, "cmp", "PartialEq", "eq");
    let mut heads: Vec<(String, usize)> = heads.into_iter().collect();
    heads.sort_by(|a, b| a.0.cmp(&b.0));

    let mut impls = Vec::new();
    for (name, arity) in heads {
        let Some(variants) = crate::typeck::anon_union_synthetic_variants(&name) else {
            continue;
        };
        if let Some(trait_name) = &show_trait
            && !existing.contains(&(trait_name.clone(), name.clone()))
        {
            impls.push(anon_union_show_impl(
                &name,
                &variants,
                arity,
                trait_name,
            ));
        }
        if reflect_runtime
            && let Some(trait_name) = &reflect_trait
            && !existing.contains(&(trait_name.clone(), name.clone()))
        {
            impls.push(anon_union_reflect_impl(
                &name,
                &variants,
                arity,
                trait_name,
            ));
        }
        if let Some(trait_name) = &partial_eq_trait
            && !existing.contains(&(trait_name.clone(), name.clone()))
        {
            impls.push(anon_union_partial_eq_impl(
                &name,
                &variants,
                arity,
                trait_name,
            ));
        }
    }
    impls
}

fn trait_identity_with_method(
    trait_method_list: &HashMap<String, Vec<MethodSig>>,
    module: &str,
    trait_name: &str,
    method_name: &str,
) -> Option<String> {
    trait_method_list
        .iter()
        .find(|(name, methods)| {
            is_standard_trait_identity(name, module, trait_name)
                && methods.iter().any(|method| method.name == method_name)
        })
        .map(|(name, _)| name.clone())
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
        Type::Dyn(_, args) => {
            for arg in args {
                collect_anon_union_heads_type(arg, out);
            }
        }
        Type::Fn(params, ret, _) => {
            for param in params {
                collect_anon_union_heads_type(param, out);
            }
            collect_anon_union_heads_type(ret, out);
        }
        Type::RecordCompose { base, fields } => {
            collect_anon_union_heads_type(base, out);
            for (_, ty) in fields {
                collect_anon_union_heads_type(ty, out);
            }
        }
        Type::Qualified(_, inner) => collect_anon_union_heads_type(inner, out),
        Type::Slice(inner) => collect_anon_union_heads_type(inner, out),
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
        Expr::LabeledMethodCall { receiver, args, .. } => {
            collect_anon_union_heads_expr(receiver, out);
            for (_, arg) in args {
                collect_anon_union_heads_expr(arg, out);
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
        Expr::ExistentialCall {
            receiver,
            args,
            ty,
            result,
            ..
        } => {
            collect_anon_union_heads_expr(receiver, out);
            for arg in args {
                collect_anon_union_heads_expr(arg, out);
            }
            collect_anon_union_heads_type(ty, out);
            collect_anon_union_heads_type(result, out);
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
        Expr::ExistentialPack { expr, ty, .. } => {
            collect_anon_union_heads_expr(expr, out);
            collect_anon_union_heads_type(ty, out);
        }
        Expr::ExistentialUpcast { expr, ty } => {
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

fn anon_union_show_impl(
    name: &str,
    variants: &[(String, usize)],
    arity: usize,
    trait_name: &str,
) -> ImplDef {
    ImplDef {
        origin: ImplOrigin::CompilerGenerated,
        trait_name: Some(trait_name.to_string()),
        trait_args: Vec::new(),
        type_name: name.to_string(),
        target_args: anon_union_target_args(arity),
        bounds: anon_union_bounds(arity, trait_name),
        methods: vec![Function {
            line: 0,
            public: true,
            comptime_only: false,
            attributes: Vec::new(),
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

fn anon_union_reflect_impl(
    name: &str,
    variants: &[(String, usize)],
    arity: usize,
    trait_name: &str,
) -> ImplDef {
    ImplDef {
        origin: ImplOrigin::CompilerGenerated,
        trait_name: Some(trait_name.to_string()),
        trait_args: Vec::new(),
        type_name: name.to_string(),
        target_args: anon_union_target_args(arity),
        bounds: anon_union_bounds(arity, trait_name),
        methods: vec![Function {
            line: 0,
            public: true,
            comptime_only: false,
            attributes: Vec::new(),
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

fn anon_union_partial_eq_impl(
    name: &str,
    variants: &[(String, usize)],
    arity: usize,
    trait_name: &str,
) -> ImplDef {
    ImplDef {
        origin: ImplOrigin::CompilerGenerated,
        trait_name: Some(trait_name.to_string()),
        trait_args: Vec::new(),
        type_name: name.to_string(),
        target_args: anon_union_target_args(arity),
        bounds: anon_union_bounds(arity, trait_name),
        methods: vec![Function {
            line: 0,
            public: true,
            comptime_only: false,
            attributes: Vec::new(),
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
