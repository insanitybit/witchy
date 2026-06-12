//! `comptime:` — compile-time, capability-free, ADDITIVE item generation
//! (docs/language-evolution.md Phase 5).
//!
//! A comptime block is an ordinary witchy block executed AT COMPILE TIME with
//! exactly one ambient ability: `print` (its emit channel). It cannot reach a
//! capability — there is no parameter list to receive one — so its execution
//! is deterministic by construction. Everything it prints is concatenated,
//! parsed as witchy source, and APPENDED to the enclosing module before type
//! checking and footprint analysis run: generated code is analyzed exactly
//! like handwritten code, and nothing existing can be rewritten or removed,
//! so no comptime block can launder authority out of a signature.
//!
//! v1 executes the block on the reference interpreter (the same zero-grant
//! determinism contract the WASM build sandbox enforces structurally); the
//! hard-isolation upgrade is mechanical because the channel is already
//! "printed source in, items out".

use crate::ast::{Block, Function, Item, Module, Param, Type};

/// Expand every `comptime:` block in `module` (consuming the items), running
/// each and appending the items its output parses to. `name` is the module's
/// name, for error messages.
pub fn expand(name: &str, module: &mut Module) -> Result<(), String> {
    let blocks: Vec<Block> = {
        let mut out = Vec::new();
        let mut i = 0;
        while i < module.items.len() {
            if matches!(module.items[i], Item::Comptime(_)) {
                let Item::Comptime(b) = module.items.remove(i) else {
                    unreachable!()
                };
                if module.item_lines.len() > i {
                    module.item_lines.remove(i);
                }
                out.push(b);
            } else {
                i += 1;
            }
        }
        out
    };
    if blocks.is_empty() {
        return Ok(());
    }
    for mut body in blocks {
        // The block becomes `fn main(console: Console)` of a synthetic
        // program carrying the enclosing module's imports. `emit(line)` is
        // the surface emit channel (a prepended closure over the console);
        // `print(console, ...)` works too. Linked recursively (a comptime
        // block cannot itself contain `comptime` — it is an item, not a
        // statement).
        body.stmts.insert(
            0,
            crate::ast::Stmt::Let {
                ty: None,
                name: "emit".into(),
                mutable: false,
                value: crate::ast::Expr::Lambda {
                    params: vec![Param {
                        name: "line".into(),
                        ty: Some(Type::Named("String".into(), Vec::new())),
                        convention: Default::default(),
                    }],
                    body: Block {
                        stmts: vec![crate::ast::Stmt::Expr(crate::ast::Expr::Call {
                            name: "print".into(),
                            args: vec![
                                crate::ast::Expr::Var("console".into()),
                                crate::ast::Expr::Var("line".into()),
                            ],
                        })],
                        lines: vec![0],
                        restrict: None,
                        region: None,
                    },
                },
            },
        );
        if let Some(first) = body.lines.first().copied() {
            body.lines.insert(0, first);
        } else {
            body.lines.push(0);
        }
        let prog = Module {
            imports: module.imports.clone(),
            items: vec![Item::Function(Function {
                public: false,
                name: "main".into(),
                params: vec![Param {
                    name: "console".into(),
                    ty: Some(Type::Named("Console".into(), Vec::new())),
                    convention: Default::default(),
                }],
                ret: None,
                body,
                bounds: Vec::new(),
                is_gen: false,
            })],
            import_lines: Vec::new(),
            item_lines: Vec::new(),
        };
        let linked = crate::linker::link(vec![("comptime".into(), prog)], "comptime")
            .map_err(|e| format!("module `{name}`: comptime block: {e}"))?;
        crate::typeck::check(&linked)
            .map_err(|e| format!("module `{name}`: comptime block: {e}"))?;
        let lines = crate::interpreter::run_module_budgeted(
            linked,
            ".",
            crate::interpreter::COMPTIME_STEP_LIMIT,
        )
            .map_err(|e| format!("module `{name}`: comptime block: {e}"))?;
        let src = lines.join("\n");
        let emitted = crate::parser::parse_module(&src).map_err(|e| {
            format!(
                "module `{name}`: comptime block emitted source that does not \
                 parse: {e}\n--- emitted ---\n{src}"
            )
        })?;
        if emitted.items.iter().any(|it| matches!(it, Item::Comptime(_))) {
            return Err(format!(
                "module `{name}`: a comptime block may not emit another `comptime` block"
            ));
        }
        for imp in emitted.imports {
            if !module.imports.contains(&imp) {
                module.imports.push(imp);
                if !module.import_lines.is_empty() {
                    module.import_lines.push(u32::MAX);
                }
            }
        }
        let n = emitted.items.len();
        module.items.extend(emitted.items);
        for _ in 0..n {
            if !module.item_lines.is_empty() {
                module.item_lines.push(u32::MAX);
            }
        }
    }
    Ok(())
}
