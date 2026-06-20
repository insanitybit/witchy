//! `comptime:` — compile-time, capability-free, ADDITIVE item generation
//! (rfcs/language-evolution.md Phase 5).
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

use crate::ast::{Block, Expr, Function, Item, Module, Param, Stmt, Type, TypeDef};

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
    // The module's type structures, exposed to every block as `module_types`
    // (the comptime `typeInfo` primitive). Built once from the module's types.
    let type_infos: Vec<Expr> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) => Some(type_info_expr(t)),
            _ => None,
        })
        .collect();
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
                    ret: None,
                },
            },
        );
        if let Some(first) = body.lines.first().copied() {
            body.lines.insert(0, first);
        } else {
            body.lines.push(0);
        }
        // Expose the module's type structures to the block as `module_types`
        // (the comptime `typeInfo` reflection primitive).
        body.stmts.insert(
            0,
            Stmt::Let {
                ty: None,
                name: "module_types".into(),
                mutable: false,
                value: Expr::List(type_infos.clone()),
            },
        );
        if let Some(first) = body.lines.first().copied() {
            body.lines.insert(0, first);
        } else {
            body.lines.push(0);
        }
        // The comptime program carries only the enclosing module's STD imports: it
        // runs in the isolated, zero-capability `comptime` link, which resolves the
        // bundled std modules but not the project's own sibling modules — and a
        // comptime block (a link-time, capability-free eval) cannot use sibling
        // runtime code in any case. Dropping the project-local imports lets a module
        // that both `derive`s and imports a sibling (e.g. a rune's test module)
        // still run its comptime. `module_types` is `meta.TypeInfo`s, so meta is
        // always present.
        let mut prog_imports: Vec<String> = module
            .imports
            .iter()
            .filter(|i| crate::linker::STD_MODULES.contains(&i.as_str()))
            .cloned()
            .collect();
        if !prog_imports.iter().any(|i| i == "meta") {
            prog_imports.push("meta".into());
        }
        let prog = Module {
            modes: Vec::new(),
            imports: prog_imports,
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
                is_async: false,
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

/// Render a declared type to the string form `meta.TypeInfo` exposes — `Int`,
/// `List(String)`, `Option(Point)`, `(Int, String)`, `fn(Int) -> Bool`.
fn type_to_string(t: &Type) -> String {
    match t {
        Type::Named(n, args) if args.is_empty() => n.clone(),
        Type::Named(n, args) => {
            let inner: Vec<String> = args.iter().map(type_to_string).collect();
            format!("{n}({})", inner.join(", "))
        }
        Type::Tuple(ts) => {
            let inner: Vec<String> = ts.iter().map(type_to_string).collect();
            format!("({})", inner.join(", "))
        }
        Type::Fn(ps, r) => {
            let inner: Vec<String> = ps.iter().map(type_to_string).collect();
            format!("fn({}) -> {}", inner.join(", "), type_to_string(r))
        }
    }
}

/// Build the `meta.TypeInfo(...)` constructor expression describing `t`, injected
/// into comptime blocks as an element of `module_types` so a block can read its
/// module's type structure as data (the `typeInfo` reflection primitive). Also
/// used by `derive` to embed a type's structure in the generator call it desugars to.
pub(crate) fn type_info_expr(t: &TypeDef) -> Expr {
    let s = |v: &str| Expr::Str(v.to_string());
    let str_list = |xs: &[String]| Expr::List(xs.iter().map(|x| Expr::Str(x.clone())).collect());
    let is_record = t.variants.len() == 1 && !t.variants[0].field_names.is_empty();
    let kind = if is_record {
        "record"
    } else if t.variants.is_empty() {
        "unit"
    } else {
        "sum"
    };
    let fields = if is_record {
        let v = &t.variants[0];
        v.field_names
            .iter()
            .zip(&v.fields)
            .map(|(name, ty)| Expr::Ctor {
                name: "FieldInfo".into(),
                args: vec![s(name), s(&type_to_string(ty))],
            })
            .collect()
    } else {
        Vec::new()
    };
    let variants = if is_record {
        Vec::new()
    } else {
        t.variants
            .iter()
            .map(|v| Expr::Ctor {
                name: "VariantInfo".into(),
                args: vec![
                    s(&v.name),
                    Expr::List(v.fields.iter().map(|ty| s(&type_to_string(ty))).collect()),
                ],
            })
            .collect()
    };
    Expr::Ctor {
        name: "TypeInfo".into(),
        args: vec![
            s(&t.name),
            s(kind),
            str_list(&t.params),
            Expr::List(fields),
            Expr::List(variants),
        ],
    }
}
