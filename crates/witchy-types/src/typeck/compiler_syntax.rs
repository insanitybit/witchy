//! Compiler-syntax gating: reject compile-time-only `meta.*` syntax types in
//! runtime code.
//!
//! The `meta.ItemSyntax`/`meta.ExprSyntax`/… family models generated program
//! fragments and is legal only inside `comptime:`/`std/meta` helpers. This
//! self-contained cluster (extracted verbatim from the main checker) recognizes
//! those type names, walks declaration signatures for them, and rejects any
//! occurrence in a runtime module. It also identifies the compiler-generated
//! structural `impl`s (anon-record/anon-union derives) that check-type-names
//! must exempt.

use witchy_syntax::ast::{self, ImplOrigin, Item, Module};

use super::{
    anon_union_synthetic_variants, detect_entry_module, dequalify_home, diagnostic_callable_name,
    is_anon_record_synthetic_name, terr, TypeError,
};

pub(super) fn compiler_syntax_type_name(name: &str) -> Option<&'static str> {
    match name {
        "meta.ItemSyntax" => Some("meta.ItemSyntax"),
        "meta.TypeSyntax" => Some("meta.TypeSyntax"),
        "meta.ExprSyntax" => Some("meta.ExprSyntax"),
        "meta.PatternSyntax" => Some("meta.PatternSyntax"),
        "meta.SyntaxHole" => Some("meta.SyntaxHole"),
        "meta.StmtSyntax" => Some("meta.StmtSyntax"),
        "meta.BlockSyntax" => Some("meta.BlockSyntax"),
        "meta.MatchArmSyntax" => Some("meta.MatchArmSyntax"),
        "meta.ParamSyntax" => Some("meta.ParamSyntax"),
        "meta.Ident" => Some("meta.Ident"),
        _ => None,
    }
}

pub(super) fn compiler_syntax_allowed_module(module: &str) -> bool {
    matches!(module, "comptime" | "meta")
}

fn decl_module<'a>(name: &'a str, entry_module: &'a str) -> &'a str {
    name.rsplit_once('.').map_or(entry_module, |(module, _)| module)
}

fn compiler_syntax_in_ast_type(t: &ast::Type) -> Option<&'static str> {
    match t {
        ast::Type::Qualified(_, inner) => compiler_syntax_in_ast_type(inner),
        ast::Type::Tuple(items) => items.iter().find_map(compiler_syntax_in_ast_type),
        ast::Type::Fn(params, ret, _) => params
            .iter()
            .chain(std::iter::once(ret.as_ref()))
            .find_map(compiler_syntax_in_ast_type),
        ast::Type::Dyn(_, args) => args.iter().find_map(compiler_syntax_in_ast_type),
        ast::Type::RecordCompose { base, fields } => compiler_syntax_in_ast_type(base)
            .or_else(|| {
                fields
                    .iter()
                    .find_map(|(_, ty)| compiler_syntax_in_ast_type(ty))
            }),
        ast::Type::Named(name, args) => {
            compiler_syntax_type_name(name).or_else(|| args.iter().find_map(compiler_syntax_in_ast_type))
        }
    }
}

fn reject_runtime_compiler_syntax_ast_type(
    t: &ast::Type,
    home_module: &str,
    context: &str,
) -> Result<(), TypeError> {
    if compiler_syntax_allowed_module(home_module) {
        return Ok(());
    }
    if let Some(name) = compiler_syntax_in_ast_type(t) {
        let module = if home_module.is_empty() { "this module" } else { home_module };
        return terr(format!(
            "compiler syntax type `{name}` is compile-time-only; `{context}` is in runtime module `{module}`. Use it only inside `comptime:`/`std/meta` helpers and pass generated items to `emit_item`"
        ));
    }
    Ok(())
}

pub(super) fn check_compiler_syntax_declarations(module: &Module) -> Result<(), TypeError> {
    let entry_module = detect_entry_module(module);
    for item in &module.items {
        match item {
            Item::Function(f) => {
                if f.comptime_only {
                    continue;
                }
                let home = decl_module(&f.name, &entry_module);
                for p in &f.params {
                    if let Some(ty) = &p.ty {
                        reject_runtime_compiler_syntax_ast_type(
                            ty,
                            home,
                            &format!("parameter `{}` of `{}`", p.name, diagnostic_callable_name(&f.name)),
                        )?;
                    }
                }
                if let Some(ret) = &f.ret {
                    reject_runtime_compiler_syntax_ast_type(
                        ret,
                        home,
                        &format!("return type of `{}`", diagnostic_callable_name(&f.name)),
                    )?;
                }
            }
            Item::Type(t) => {
                let home = decl_module(&t.name, &entry_module);
                for variant in &t.variants {
                    for (idx, field) in variant.fields.iter().enumerate() {
                        let field_name = variant
                            .field_names
                            .get(idx)
                            .map_or_else(|| format!("field {}", idx + 1), |name| format!("field `{name}`"));
                        reject_runtime_compiler_syntax_ast_type(
                            field,
                            home,
                            &format!("{field_name} of type `{}`", dequalify_home(&t.name, home)),
                        )?;
                    }
                }
            }
            Item::Trait(tr) => {
                let home = decl_module(&tr.name, &entry_module);
                for method in &tr.methods {
                    for p in &method.params {
                        if let Some(ty) = &p.ty {
                            reject_runtime_compiler_syntax_ast_type(
                                ty,
                                home,
                                &format!("parameter `{}` of trait method `{}`", p.name, method.name),
                            )?;
                        }
                    }
                    if let Some(ret) = &method.ret {
                        reject_runtime_compiler_syntax_ast_type(
                            ret,
                            home,
                            &format!("return type of trait method `{}`", method.name),
                        )?;
                    }
                }
            }
            Item::Impl(im) => {
                let home = decl_module(&im.type_name, &entry_module);
                for method in &im.methods {
                    for p in &method.params {
                        if let Some(ty) = &p.ty {
                            reject_runtime_compiler_syntax_ast_type(
                                ty,
                                home,
                                &format!("parameter `{}` of method `{}`", p.name, method.name),
                            )?;
                        }
                    }
                    if let Some(ret) = &method.ret {
                        reject_runtime_compiler_syntax_ast_type(
                            ret,
                            home,
                            &format!("return type of method `{}`", method.name),
                        )?;
                    }
                }
            }
            Item::TypeAlias { name, ty, .. } => {
                let home = decl_module(name, &entry_module);
                reject_runtime_compiler_syntax_ast_type(
                    ty,
                    home,
                    &format!("type alias `{}`", dequalify_home(name, home)),
                )?;
            }
            Item::Const { .. } | Item::Comptime(_) => {}
        }
    }
    Ok(())
}

pub(super) fn is_compiler_generated_structural_impl(im: &ast::ImplDef) -> bool {
    if im.origin != ImplOrigin::CompilerGenerated {
        return false;
    }
    match im
        .trait_name
        .as_deref()
        .map(|name| name.rsplit('.').next().unwrap_or(name))
    {
        Some("Reflect" | "PartialEq" | "Eq")
            if is_anon_record_synthetic_name(&im.type_name) => true,
        Some("Show" | "Reflect" | "PartialEq")
            if anon_union_synthetic_variants(&im.type_name).is_some() => true,
        _ => false,
    }
}
