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

const MAX_COMPTIME_BLOCKS: usize = 256;

/// Expand every `comptime:` block in `module` (consuming the items), running
/// each and appending the items its output parses to. `name` is the module's
/// name, for error messages.
pub fn expand(name: &str, module: &mut Module) -> Result<(), String> {
    let mut i = 0;
    let mut expanded = 0;
    while i < module.items.len() {
        if !matches!(module.items[i], Item::Comptime(_)) {
            i += 1;
            continue;
        }
        expanded += 1;
        if expanded > MAX_COMPTIME_BLOCKS {
            return Err(format!(
                "module `{name}`: comptime expansion exceeded {MAX_COMPTIME_BLOCKS} blocks"
            ));
        }
        // Use the REAL source line of the `comptime:` declaration
        // (`u32::MAX`/absent -> 0, "unknown"), so errors in emitted code can be
        // attributed to the block that produced it instead of a phantom offset into
        // the invisible emitted text (BUG-341).
        let Item::Comptime(mut body) = module.items.remove(i) else {
            unreachable!()
        };
        let block_line = if module.item_lines.len() > i {
            let l = module.item_lines.remove(i);
            if l == u32::MAX { 0 } else { l }
        } else {
            0
        };

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
        // Expose the module's current type structures to the block as
        // `module_types` (the comptime `typeInfo` reflection primitive).
        // Rebuild this per block so later generators see types emitted by
        // earlier generators in the same module.
        body.stmts.insert(
            0,
            Stmt::Let {
                ty: None,
                name: "module_types".into(),
                mutable: false,
                value: Expr::List(witchy_syntax::reflect::module_type_info_exprs(module)),
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
        merge_emitted_module(module, emitted, block_line);
        *module = normalize_generated_module(module.clone())
            .map_err(|e| format!("module `{name}`: comptime generated source: {e}"))?;
        // Do not increment `i`: removing the block shifted the next original item
        // into this slot, and normalization may also have appended derive-generated
        // comptime blocks for this same pass to consume.
    }
    Ok(())
}

fn normalize_generated_module(module: Module) -> Result<Module, String> {
    let module = witchy_syntax::generators::lower(module)?;
    let module = witchy_syntax::async_lower::lower(module)?;
    witchy_syntax::records::lower_lenient(module)
}

fn merge_emitted_module(module: &mut Module, emitted: Module, block_line: u32) {
    for imp in emitted.imports {
        if !module.imports.contains(&imp) {
            module.imports.push(imp);
            if !module.import_lines.is_empty() {
                module.import_lines.push(u32::MAX);
            }
        }
    }
    merge_from_imports(&mut module.from_imports, emitted.from_imports);
    let n = emitted.items.len();
    for mut item in emitted.items {
        // The emitted items were parsed from a standalone blob, so every line
        // number they carry is relative to that invisible text — a phantom offset
        // that can point past the real file's EOF in a later type error (BUG-341).
        // Re-stamp them to the `comptime:` block's own source line.
        stamp_item_lines(&mut item, block_line);
        module.items.push(item);
    }
    for _ in 0..n {
        if !module.item_lines.is_empty() {
            module.item_lines.push(block_line);
        }
    }
}

fn merge_from_imports(
    target: &mut Vec<(String, Vec<String>)>,
    emitted: Vec<(String, Vec<String>)>,
) {
    for (module, names) in emitted {
        if let Some((_, existing)) = target.iter_mut().find(|(m, _)| m == &module) {
            for name in names {
                if !existing.contains(&name) {
                    existing.push(name);
                }
            }
        } else {
            target.push((module, names));
        }
    }
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

#[cfg(test)]
mod tests {
    #[test]
    fn module_types_include_types_emitted_by_earlier_comptime_blocks() {
        let src = r#"
comptime:
    emit("type Generated:")
    emit("    value: Int")

type Handwritten:
    value: Int

comptime:
    var saw_generated = false
    var saw_handwritten = false
    for t in module_types:
        if t.name == "Generated":
            saw_generated = true
        if t.name == "Handwritten":
            saw_handwritten = true
    emit("fn saw_generated() -> Bool:")
    emit("    " + __render(saw_generated))
    emit("fn saw_handwritten() -> Bool:")
    emit("    " + __render(saw_handwritten))

fn main(console: Console):
    print(console, __render(saw_generated()))
    print(console, __render(saw_handwritten()))
    let g = Generated(value: 7)
    print(console, __render(g.value))
"#;

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        witchy_types::typeck::check(&linked).expect("typecheck");
        let out = crate::interpreter::run_module(linked, ".", Vec::new()).expect("run");
        assert_eq!(out, ["true", "true", "7"]);
    }

    #[test]
    fn emitted_from_imports_bind_generated_items() {
        let src = r#"
comptime:
    emit("from json import Json")
    emit("")
    emit("pub fn generated(j: Json) -> Int:")
    emit("    1")

fn main(console: Console):
    print(console, "ok")
"#;

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        witchy_types::typeck::check(&linked).expect("typecheck");
    }

    #[test]
    fn emitted_named_field_construction_is_lowered() {
        let src = r#"
type Point:
    x: Int
    y: Int

comptime:
    emit("fn made() -> Point:")
    emit("    Point(x: 1, y: 2)")

fn main(console: Console):
    let p = made()
    print(console, __render(p.x))
    print(console, __render(p.y))
"#;

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        witchy_types::typeck::check(&linked).expect("typecheck");
        let out = crate::interpreter::run_module(linked, ".", Vec::new()).expect("run");
        assert_eq!(out, ["1", "2"]);
    }

    #[test]
    fn emitted_derive_blocks_are_expanded() {
        let src = r#"
import show

comptime:
    emit("type Generated derive(Show):")
    emit("    value: Int")

fn main(console: Console):
    print(console, show.render(Generated(value: 7)))
"#;

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        witchy_types::typeck::check(&linked).expect("typecheck");
        let out = crate::interpreter::run_module(linked, ".", Vec::new()).expect("run");
        assert_eq!(out, ["Generated(7)"]);
    }

    #[test]
    fn emitted_generators_are_lowered() {
        let src = r#"
import iter

comptime:
    emit("gen fn generated() -> Iter(Int):")
    emit("    yield 1")
    emit("    yield 2")

fn main(console: Console):
    let xs: List(Int) = iter.collect(generated())
    print(console, __render(xs))
"#;

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        witchy_types::typeck::check(&linked).expect("typecheck");
        let out = crate::interpreter::run_module(linked, ".", Vec::new()).expect("run");
        assert_eq!(out, ["[1, 2]"]);
    }

    #[test]
    fn emitted_async_functions_are_lowered() {
        let src = r#"
comptime:
    emit("async fn generated() -> Int:")
    emit("    7")

async fn main(console: Console):
    let n = generated().await
    print(console, __render(n))
"#;

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        witchy_types::typeck::check(&linked).expect("typecheck");
        let out = crate::interpreter::run_module(linked, ".", Vec::new()).expect("run");
        assert_eq!(out, ["7"]);
    }
}
