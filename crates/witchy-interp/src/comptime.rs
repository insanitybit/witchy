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

use witchy_syntax::ast::{Block, Expr, Function, Item, Module, Param, Stmt, Type};

/// Expand every `comptime:` block in `module` (consuming the items), running
/// each and appending the items its output parses to. `name` is the module's
/// name, for error messages.
pub fn expand(name: &str, module: &mut Module) -> Result<(), String> {
    // Each block paired with the REAL source line of its `comptime:` declaration
    // (`u32::MAX`/absent → 0, "unknown"), so a type error in the emitted code can
    // be attributed to the block that produced it instead of a phantom offset into
    // the invisible emitted text (BUG-341).
    let blocks: Vec<(Block, u32)> = {
        let mut out = Vec::new();
        let mut i = 0;
        while i < module.items.len() {
            if matches!(module.items[i], Item::Comptime(_)) {
                let Item::Comptime(b) = module.items.remove(i) else {
                    unreachable!()
                };
                let block_line = if module.item_lines.len() > i {
                    let l = module.item_lines.remove(i);
                    if l == u32::MAX { 0 } else { l }
                } else {
                    0
                };
                out.push((b, block_line));
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
            Item::Type(t) => Some(witchy_syntax::reflect::type_info_expr(t)),
            _ => None,
        })
        .collect();
    for (mut body, block_line) in blocks {
        // The block becomes `fn main(console: Console)` of a synthetic
        // program carrying the enclosing module's imports. `emit(line)` is
        // the surface emit channel (a prepended closure over the console);
        // `print(console, ...)` works too. Linked recursively (a comptime
        // block cannot itself contain `comptime` — it is an item, not a
        // statement).
        body.stmts.insert(
            0,
            witchy_syntax::ast::Stmt::Let {
                ty: None,
                name: "emit".into(),
                mutable: false,
                value: witchy_syntax::ast::Expr::Lambda {
                    params: vec![Param {
                        name: "line".into(),
                        ty: Some(Type::Named("String".into(), Vec::new())),
                        convention: Default::default(),
                        default: None,
                    }],
                    body: Block {
                        stmts: vec![witchy_syntax::ast::Stmt::Expr(witchy_syntax::ast::Expr::Call {
                            name: "print".into(),
                            args: vec![
                                witchy_syntax::ast::Expr::Var("console".into()),
                                witchy_syntax::ast::Expr::Var("line".into()),
                            ],
                        })],
                        lines: vec![0],
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
        // still run its comptime. Preserve matching STD `from X import Y` bindings
        // so the block sees the same unqualified std names as its enclosing source.
        // `module_types` is `meta.TypeInfo`s, so meta is always present.
        let mut prog_imports: Vec<String> = module
            .imports
            .iter()
            .filter(|i| witchy_syntax::linker::STD_MODULES.contains(&i.as_str()))
            .cloned()
            .collect();
        let prog_from_imports: Vec<(String, Vec<String>)> = module
            .from_imports
            .iter()
            .filter(|(m, _)| witchy_syntax::linker::STD_MODULES.contains(&m.as_str()))
            .cloned()
            .collect();
        if !prog_imports.iter().any(|i| i == "meta") {
            prog_imports.push("meta".into());
        }
        let prog = Module {
            modes: Vec::new(),
            imports: prog_imports,
            from_imports: prog_from_imports,
            items: vec![Item::Function(Function {
                public: false,
                name: "main".into(),
                params: vec![Param {
                    name: "console".into(),
                    ty: Some(Type::Named("Console".into(), Vec::new())),
                    convention: Default::default(),
                    default: None,
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
        let linked = crate::pipeline::link(vec![("comptime".into(), prog)], "comptime")
            .map_err(|e| format!("module `{name}`: comptime block: {e}"))?;
        witchy_types::typeck::check(&linked)
            .map_err(|e| format!("module `{name}`: comptime block: {e}"))?;
        let lines = crate::interpreter::run_module_budgeted(
            linked,
            ".",
            crate::interpreter::COMPTIME_STEP_LIMIT,
        )
            .map_err(|e| format!("module `{name}`: comptime block: {e}"))?;
        let src = lines.join("\n");
        let emitted = witchy_syntax::parser::parse_module(&src).map_err(|e| {
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
        for mut item in emitted.items {
            // The emitted items were parsed from a standalone blob, so every line
            // number they carry is relative to that invisible text — a phantom
            // offset that can point PAST the real file's EOF in a later type error
            // (BUG-341). Re-stamp them to the `comptime:` block's own source line, so
            // a body type error is attributed to the block that generated the code
            // rather than a nonexistent absolute line. (The parse-error path already
            // shows the emitted source; the type-error path now at least reports a
            // real, in-file location.)
            stamp_item_lines(&mut item, block_line);
            module.items.push(item);
        }
        for _ in 0..n {
            if !module.item_lines.is_empty() {
                module.item_lines.push(block_line);
            }
        }
    }
    Ok(())
}

/// Re-stamp every source line an item carries to `line`. See the call site: the
/// emitted AST's line numbers are relative to the emitted blob (they can exceed
/// the real file's length), so a later type error must be pinned to the `comptime:`
/// block instead of a phantom offset (BUG-341).
fn stamp_item_lines(item: &mut Item, line: u32) {
    match item {
        Item::Function(f) => stamp_block(&mut f.body, line),
        Item::Impl(im) => {
            for m in &mut im.methods {
                stamp_block(&mut m.body, line);
            }
        }
        Item::Trait(t) => {
            for m in &mut t.methods {
                if let Some(body) = &mut m.default {
                    stamp_block(body, line);
                }
            }
        }
        Item::Const { value, .. } => stamp_expr(value, line),
        Item::Type(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
    }
}

fn stamp_block(b: &mut Block, line: u32) {
    for l in b.lines.iter_mut() {
        *l = line;
    }
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Expr(value)
            | Stmt::Yield(value)
            | Stmt::Return(Some(value)) => stamp_expr(value, line),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

/// Descend into every block-bearing subexpression so nested `if`/`while`/`for`/
/// `match`/lambda bodies are re-stamped too (their block lines drive `cur_line`).
fn stamp_expr(e: &mut Expr, line: u32) {
    match e {
        Expr::If { cond, then_block, else_block } => {
            stamp_expr(cond, line);
            stamp_block(then_block, line);
            if let Some(b) = else_block {
                stamp_block(b, line);
            }
        }
        Expr::While { cond, body } => {
            stamp_expr(cond, line);
            stamp_block(body, line);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            stamp_expr(scrutinee, line);
            stamp_block(body, line);
        }
        Expr::For { iter, body, .. } => {
            stamp_expr(iter, line);
            stamp_block(body, line);
        }
        Expr::Match { scrutinee, arms } => {
            stamp_expr(scrutinee, line);
            for a in arms.iter_mut() {
                if let Some(g) = &mut a.guard {
                    stamp_expr(g, line);
                }
                stamp_expr(&mut a.body, line);
            }
        }
        Expr::Lambda { body, .. } => stamp_block(body, line),
        Expr::Block(b) => stamp_block(b, line),
        Expr::Binary { lhs, rhs, .. } => {
            stamp_expr(lhs, line);
            stamp_expr(rhs, line);
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::Field { base: expr, .. } => stamp_expr(expr, line),
        Expr::Index { base, index } => {
            stamp_expr(base, line);
            stamp_expr(index, line);
        }
        Expr::Range { lo, hi, .. } => {
            stamp_expr(lo, line);
            stamp_expr(hi, line);
        }
        Expr::Call { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args)
        | Expr::Ctor { args, .. } => {
            for a in args {
                stamp_expr(a, line);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            stamp_expr(receiver, line);
            for a in args {
                stamp_expr(a, line);
            }
        }
        Expr::Apply { func, args } => {
            stamp_expr(func, line);
            for a in args {
                stamp_expr(a, line);
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, v) in args {
                stamp_expr(v, line);
            }
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            stamp_expr(base, line);
            for (_, v) in fields {
                stamp_expr(v, line);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                stamp_expr(v, line);
            }
            if let Some(s) = spread {
                stamp_expr(s, line);
            }
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::Var(_) | Expr::TaggedLit { .. } => {}
    }
}

/// Run both compile-time expansion passes for one module, in order: `comptime:`
/// blocks first, then `tag"…"` tagged literals. This is the expander callback
/// the linker invokes per module — the linker stays agnostic of how compile-time
/// code is evaluated (RFC-0018 dependency inversion), so it never names
/// `comptime`/`tagged`; the wiring lives here and in `crate::pipeline`.
pub fn expand_compile_time(
    name: &str,
    module: &mut Module,
    siblings: &[(String, Module)],
) -> Result<(), String> {
    expand(name, module)?;
    crate::tagged::expand(name, module, siblings)
}
