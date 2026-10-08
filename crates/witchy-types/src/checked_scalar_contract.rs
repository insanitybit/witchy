use witchy_syntax::ast::{Block, Expr, Item, Module};
use crate::runtime_type::{DeclarationKind, PackageSource, RuntimeDeclarationCatalog};

fn normalized(mut body: Block) -> Block {
    body.lines.clear();
    witchy_syntax::ast::visit::visit_block_mut(&mut body, &mut |expression| {
        if let Expr::MethodCall { receiver, method, args } = expression {
            let mut arguments = vec![(**receiver).clone()];
            arguments.extend(args.clone());
            *expression = Expr::Call { name: method.clone(), args: arguments };
        }
        match expression {
            Expr::Call { name, .. } | Expr::Ctor { name, .. } => {
                if let Some(specification) = witchy_syntax::intrinsics::lookup(name) {
                    *name = format!("%intrinsic:{:?}", specification.id);
                }
            }
            Expr::Block(block) | Expr::Lambda { body: block, .. }
            | Expr::While { body: block, .. } | Expr::For { body: block, .. }
            | Expr::WhileLet { body: block, .. } => block.lines.clear(),
            Expr::If { then_block, else_block, .. } => {
                then_block.lines.clear();
                if let Some(block) = else_block { block.lines.clear(); }
            }
            Expr::Match { arms, .. } => {
                for arm in arms { arm.line = 0; }
            }
            _ => {}
        }
        if let Expr::Ctor { name, args } = expression {
            *expression = Expr::Call { name: name.clone(), args: args.clone() };
        }
        Ok::<_, ()>(())
    }).expect("infallible scalar scheduler recipe normalization");
    body
}

pub(crate) fn function_hash(function: &witchy_syntax::ast::Function) -> String {
    let mut function = function.clone();
    function.line = 0;
    function.body = normalized(function.body);
    blake3::hash(format!("{function:?}").as_bytes()).to_hex().to_string()
}

pub(crate) fn type_hash(definition: &witchy_syntax::ast::TypeDef) -> String {
    let mut definition = definition.clone();
    for variant in &mut definition.variants {
        variant.line = 0;
        variant.field_lines.clear();
    }
    blake3::hash(format!("{definition:?}").as_bytes()).to_hex().to_string()
}

include!("checked_scalar_contract_hashes.rs");

pub(crate) fn scheduler_contract(module: &Module, original: &Module, catalog: &RuntimeDeclarationCatalog) -> Result<(), String> {
    for &(name, runtime_hash, original_hash) in RECIPES {
        let kind = if matches!(name, "task.Task" | "task.Step" | "task.Slot" | "task.ChannelId" | "chan.Sender" | "chan.Receiver") {
            DeclarationKind::Type
        } else { DeclarationKind::Function };
        let identity = catalog.resolve(name, kind).ok_or_else(|| format!("missing scalar scheduler declaration {name}"))?;
        let (namespace, declaration) = name.split_once('.').ok_or("invalid compiler scheduler declaration")?;
        if identity.package().source() != &PackageSource::Toolchain
            || identity.package().name() != "witchy/std"
            || identity.module() != ["std", namespace] || identity.name() != declaration {
            return Err(format!("unauthenticated scalar scheduler declaration {name}"));
        }

        for (projection, (actual, expected_hash)) in [(module, runtime_hash), (original, original_hash)].into_iter().enumerate() {
            let mut projection_found = false;
            for item in &actual.items {
                let actual_hash = match item {
                    Item::Function(function) if kind == DeclarationKind::Function && function.name == name => function_hash(function),
                    Item::Type(definition) if kind == DeclarationKind::Type && definition.name == name => type_hash(definition),
                    _ => continue,
                };
                projection_found = true;
                if actual_hash != expected_hash { return Err(format!("changed scalar scheduler recipe {name}")); }
            }
            // The implicit-receive marker is removed only from the runtime projection.
            if !projection_found && !(projection == 0 && name == "chan.__recv_stream") {
                return Err(format!("missing scalar scheduler projection recipe {name}"));
            }
        }
    }
    Ok(())
}
