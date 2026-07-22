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

use witchy_syntax::ast::{
    BinOp, Block, Expr, Function, ImplOrigin, Item, Module, Param, Stmt, Type,
};
use witchy_syntax::origin::{ExpansionOrigin, OriginTable, SourceSpan};

const MAX_COMPTIME_BLOCKS: usize = 256;
// Per module, after generated `gen`/`async` helpers are lowered into real items.
const MAX_COMPTIME_GENERATED_ITEMS: usize = 4096;
const SOURCE_OUTPUT_MARKER: &str = "\u{1e}witchy:comptime:source:";

/// Expand every `comptime:` block in `module` (consuming the items), running
/// each and appending the items its output parses to. `name` is the module's
/// name, for error messages.
pub fn expand(name: &str, module: &mut Module) -> Result<(), String> {
    expand_with_origins(name, module).map(|_| ())
}

/// Expand while retaining the typed source provenance that runtime linking and
/// backend compilation intentionally ignore. Tooling uses this entry point.
pub(crate) fn expand_with_origins(name: &str, module: &mut Module) -> Result<OriginTable, String> {
    expand_with_item_limit_and_origins(name, module, MAX_COMPTIME_GENERATED_ITEMS)
}

#[cfg(test)]
fn expand_with_item_limit(
    name: &str,
    module: &mut Module,
    max_generated_items: usize,
) -> Result<(), String> {
    expand_with_item_limit_and_origins(name, module, max_generated_items).map(|_| ())
}

fn expand_with_item_limit_and_origins(
    name: &str,
    module: &mut Module,
    max_generated_items: usize,
) -> Result<OriginTable, String> {
    let mut i = 0;
    let mut expanded = 0;
    let mut generated_items = 0usize;
    let mut projected_item_count: Option<usize> = None;
    let mut origins = OriginTable::default();
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
        let mut retained = vec![true; module.items.len() + 1];
        retained[i] = false;
        origins.retain_items(name, &retained);
        let block_line = if module.item_lines.len() > i {
            let l = module.item_lines.remove(i);
            if l == u32::MAX { 0 } else { l }
        } else {
            0
        };

        // The block becomes `fn main(console: Console)` of a synthetic program
        // carrying the enclosing module's imports. `emit(line)` is the legacy
        // source emit channel; `emit_item(meta.ItemSyntax)` is the typed
        // RFC-0080 migration boundary. `emit_item` calls an unspellable compiler
        // operation so a compiler-owned item can remain AST all the way back to
        // this expander. Direct `console.print(...)`
        // remains legacy source output for compatibility. Linked recursively
        // (a comptime block cannot itself contain `comptime` — it is an item,
        // not a statement).
        body.stmts.insert(
            0,
            witchy_syntax::ast::Stmt::Let {
                ty: None,
                name: "emit_item".into(),
                mutable: false,
                value: witchy_syntax::ast::Expr::Lambda {
                    params: vec![Param {
                        name: "syntax_value".into(),
                        ty: Some(Type::Named("ItemSyntax".into(), Vec::new())),
                        convention: Default::default(),
                        default: None,
                    }],
                    body: Block {
                        stmts: vec![witchy_syntax::ast::Stmt::Expr(Expr::Call {
                            name: witchy_syntax::intrinsics::COMPILER_EMIT_ITEM.into(),
                            args: vec![Expr::Var("syntax_value".into())],
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
                        stmts: vec![witchy_syntax::ast::Stmt::Expr(marked_console_print(
                            SOURCE_OUTPUT_MARKER,
                            "line",
                        ))],
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
                value: Expr::List(witchy_syntax::reflect::module_type_info_exprs(module)?),
            },
        );
        if let Some(first) = body.lines.first().copied() {
            body.lines.insert(0, first);
        } else {
            body.lines.push(0);
        }
        // The comptime program runs in an isolated, zero-capability `comptime`
        // link: it resolves bundled std modules and a pruned closure of reachable
        // same-module helper functions/types, but not project sibling modules.
        // Dropping project-local imports lets a module that both `derive`s and
        // imports a sibling (e.g. a rune's test module) still run its comptime.
        // Preserve matching STD `from X import Y` bindings so the block sees the
        // same unqualified std names as its enclosing source. `module_types` is
        // `meta.TypeInfo`s, so meta is always present.
        let mut prog_imports: Vec<String> = module
            .imports
            .iter()
            .filter(|i| witchy_syntax::linker::STD_MODULES.contains(&i.as_str()))
            .cloned()
            .collect();
        let mut prog_from_imports: Vec<(String, Vec<String>)> = module
            .from_imports
            .iter()
            .filter(|(m, _)| witchy_syntax::linker::STD_MODULES.contains(&m.as_str()))
            .cloned()
            .collect();
        ensure_from_imports(&mut prog_from_imports, "meta", &["ItemSyntax", "item"]);
        if !prog_imports.iter().any(|i| i == "meta") {
            prog_imports.push("meta".into());
        }
        let mut prog_items = reachable_local_items(&module.items, &body);
        prog_items.push(Item::Function(Function {
            public: false,
            comptime_only: false,
            attributes: Vec::new(),
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
        }));
        let prog = Module {
            modes: Vec::new(),
            imports: prog_imports,
            from_imports: prog_from_imports,
            items: prog_items,
            import_lines: Vec::new(),
            item_lines: Vec::new(),
            compiler_item_syntax: module.compiler_item_syntax.clone(),
            compiler_expr_syntax: module.compiler_expr_syntax.clone(),
            compiler_type_syntax: module
                .compiler_type_syntax
                .iter()
                .filter(|syntax| !syntax.runtime_identity)
                .cloned()
                .collect(),
            compiler_pattern_syntax: module.compiler_pattern_syntax.clone(),
            compiler_stmt_syntax: module.compiler_stmt_syntax.clone(),
            compiler_block_syntax: module.compiler_block_syntax.clone(),
            linked_entry: None,
        };
        let linked = crate::pipeline::link(vec![("comptime".into(), prog)], "comptime")
            .map_err(|e| format!("module `{name}`: comptime block: {e}"))?;
        witchy_types::typeck::check_comptime(&linked)
            .map_err(|e| format!("module `{name}`: comptime block: {e}"))?;
        let (lines, item_output) = crate::interpreter::run_comptime_module_budgeted_in_scope(
            linked,
            ".",
            crate::interpreter::COMPTIME_STEP_LIMIT,
            Some(format!("comptime:{}:{name}:{expanded}", name.len())),
        )
            .map_err(|e| format!("module `{name}`: comptime block: {e}"))?;
        let emitted = decode_comptime_output(name, block_line, lines, item_output)
            .map_err(|e| format!("module `{name}`: comptime block: {e}"))?;
        let runtime_items_before_merge = match projected_item_count {
            // `comptime` items are expansion work, not runtime items, so
            // consuming the current block does not change this projection.
            Some(previous) => previous,
            None => {
                let projected = validate_and_measure_generated_module(module.clone())
                    .map_err(|e| format!("module `{name}`: comptime generated source: {e}"))?;
                runtime_item_count(&projected)
            }
        };
        merge_emitted_module(module, &mut origins, emitted, block_line);
        // Schedule derives so generated comptime blocks participate in this
        // same expansion pass, but retain generator/async/record source nodes
        // until the linker finishes expansion and applies destructive lowering
        // once. A lowered clone supplies the historical runtime-item accounting
        // without replacing the source-shaped enclosing module.
        witchy_syntax::derive::expand(module)
            .map_err(|e| format!("module `{name}`: comptime generated source: {e}"))?;
        let normalized = validate_and_measure_generated_module(module.clone())
            .map_err(|e| format!("module `{name}`: comptime generated source: {e}"))?;
        // Generator/async lowering adds implicit std imports. Carry those
        // dependency facts forward even though the source items themselves stay
        // unlowered, so a later comptime block sees the same std namespace it
        // saw when normalization replaced the whole module.
        module.imports.clone_from(&normalized.imports);
        module.import_lines.clone_from(&normalized.import_lines);
        let normalized_item_count = runtime_item_count(&normalized);
        let generated_by_block = normalized_item_count
            .checked_sub(runtime_items_before_merge)
            .ok_or_else(|| {
                format!("module `{name}`: comptime generated-item accounting underflow")
            })?;
        projected_item_count = Some(normalized_item_count);
        generated_items = generated_items
            .checked_add(generated_by_block)
            .ok_or_else(|| item_limit_error(name, max_generated_items))?;
        if generated_items > max_generated_items {
            return Err(item_limit_error(name, max_generated_items));
        }
        // Do not increment `i`: removing the block shifted the next original item
        // into this slot, and derive scheduling may also have appended generated
        // comptime blocks for this same pass to consume.
    }
    Ok(origins)
}

fn item_limit_error(name: &str, max_generated_items: usize) -> String {
    format!(
        "module `{name}`: comptime expansion exceeded the generated-item limit of \
         {max_generated_items}"
    )
}

fn marked_console_print(marker: &str, value_name: &str) -> Expr {
    Expr::MethodCall {
        receiver: Box::new(Expr::Var("console".into())),
        method: "print".into(),
        args: vec![Expr::Binary {
            op: BinOp::Add,
            lhs: Box::new(Expr::Str(marker.to_string())),
            rhs: Box::new(Expr::Var(value_name.into())),
        }],
    }
}

fn decode_comptime_output(
    module_name: &str,
    invocation_line: u32,
    lines: Vec<String>,
    item_output: Vec<crate::interpreter::PositionedComptimeItem>,
) -> Result<(Module, OriginTable), String> {
    if !lines.is_empty() && !item_output.is_empty() {
        return Err(
            "mixed legacy source output (`emit`/`console.print`) with typed item output \
             (`emit_item`); use one output channel per `comptime:` block"
                .into(),
        );
    }
    if item_output.is_empty() {
        let src = lines
            .into_iter()
            .map(|line| {
                line.strip_prefix(SOURCE_OUTPUT_MARKER)
                    .unwrap_or(&line)
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let emitted = witchy_syntax::parser::parse_module(&src).map_err(|e| {
            format!(
                "comptime block emitted source that does not parse: {e}\n--- emitted ---\n{src}"
            )
        })?;
        let mut origins = OriginTable::default();
        record_emitted_items(
            &mut origins,
            &emitted,
            module_name,
            invocation_line,
            invocation_line,
            0,
            &[],
        );
        return Ok((emitted, origins));
    }

    let mut emitted = witchy_syntax::parser::parse_module("")
        .expect("the empty module is always valid Witchy");
    let mut origins = OriginTable::default();
    let mut source_batch = Vec::new();
    for positioned in item_output {
        if positioned.output_position != 0 {
            return Err("internal: typed comptime output lost its source ordering".into());
        }
        match positioned.emission {
            crate::interpreter::ComptimeItemEmission::Source(source) => {
                source_batch.push(source);
            }
            crate::interpreter::ComptimeItemEmission::Syntax {
                item,
                definition_line,
                hole_ancestry,
            } => {
                append_item_source_batch(
                    &mut emitted,
                    &mut origins,
                    &mut source_batch,
                    module_name,
                    invocation_line,
                )?;
                let item_index = emitted.items.len();
                emitted.items.push(*item);
                emitted.item_lines.push(0);
                record_emitted_items(
                    &mut origins,
                    &emitted,
                    module_name,
                    definition_line,
                    invocation_line,
                    item_index,
                    &hole_ancestry,
                );
            }
        }
    }
    append_item_source_batch(
        &mut emitted,
        &mut origins,
        &mut source_batch,
        module_name,
        invocation_line,
    )?;
    Ok((emitted, origins))
}

fn append_item_source_batch(
    into: &mut Module,
    into_origins: &mut OriginTable,
    source_batch: &mut Vec<String>,
    module_name: &str,
    invocation_line: u32,
) -> Result<(), String> {
    if source_batch.is_empty() {
        return Ok(());
    }
    let source = std::mem::take(source_batch).join("\n");
    let parsed = witchy_syntax::parser::parse_module(&source).map_err(|e| {
        format!(
            "emit_item received source that does not parse: {e}\n--- emitted item source ---\n{source}"
        )
    })?;
    let mut parsed_origins = OriginTable::default();
    record_emitted_items(
        &mut parsed_origins,
        &parsed,
        module_name,
        invocation_line,
        invocation_line,
        0,
        &[],
    );
    append_unstamped_module(into, into_origins, parsed, parsed_origins);
    Ok(())
}

fn record_emitted_items(
    origins: &mut OriginTable,
    module: &Module,
    module_name: &str,
    definition_line: u32,
    invocation_line: u32,
    start: usize,
    holes: &[crate::interpreter::ComptimeHoleOrigin],
) {
    let origin = ExpansionOrigin {
        definition: SourceSpan::line(module_name, definition_line),
        invocation: SourceSpan::line(module_name, invocation_line),
        hole_ancestry: holes
            .iter()
            .map(|hole| witchy_syntax::origin::HoleOrigin {
                category: hole.category,
                definition: SourceSpan::line(module_name, hole.definition_line),
                invocation: SourceSpan::line(module_name, hole.invocation_line),
            })
            .collect(),
    };
    for (item_index, item) in module.items.iter().enumerate().skip(start) {
        origins.record_item_tree(module_name, item_index, item, origin.clone());
    }
}

fn append_unstamped_module(
    into: &mut Module,
    into_origins: &mut OriginTable,
    mut from: Module,
    from_origins: OriginTable,
) {
    let item_offset = into.items.len();
    for import in from.imports {
        if !into.imports.contains(&import) {
            into.imports.push(import);
        }
    }
    merge_from_imports(&mut into.from_imports, from.from_imports);
    into.items.append(&mut from.items);
    into.item_lines.append(&mut from.item_lines);
    into.compiler_item_syntax.append(&mut from.compiler_item_syntax);
    into.compiler_expr_syntax.append(&mut from.compiler_expr_syntax);
    into.compiler_type_syntax.append(&mut from.compiler_type_syntax);
    into.compiler_pattern_syntax.append(&mut from.compiler_pattern_syntax);
    into.compiler_stmt_syntax.append(&mut from.compiler_stmt_syntax);
    into.compiler_block_syntax.append(&mut from.compiler_block_syntax);
    into_origins.append_shifted(from_origins, item_offset);
}

fn reachable_local_items(items: &[Item], root: &Block) -> Vec<Item> {
    let keep = crate::reachability::reachable_from_block(items, root);
    let mut out = Vec::new();
    for item in items {
        match item {
            Item::Function(f) if f.name != "main" && keep.contains(&f.name) => {
                out.push(Item::Function(f.clone()));
            }
            Item::Type(t) if keep.contains(&t.name) => {
                let mut t = t.clone();
                // The enclosing module has already expanded derives before
                // comptime runs. Do not reintroduce derive-generated comptime
                // blocks inside the helper-only synthetic program.
                t.derives.clear();
                out.push(Item::Type(t));
            }
            Item::TypeAlias { name, params, ty } if keep.contains(name) => {
                out.push(Item::TypeAlias { name: name.clone(), params: params.clone(), ty: ty.clone() });
            }
            _ => {}
        }
    }
    out
}

fn validate_and_measure_generated_module(module: Module) -> Result<Module, String> {
    let module = witchy_syntax::source_check::check(module)?;
    let module = witchy_syntax::generators::lower(module)?;
    let module = witchy_syntax::async_lower::lower(module)?;
    witchy_syntax::records::lower_lenient(module).map(|module| module.into_module())
}

fn runtime_item_count(module: &Module) -> usize {
    module
        .items
        .iter()
        .filter(|item| !matches!(item, Item::Comptime(_)))
        .count()
}

fn merge_emitted_module(
    module: &mut Module,
    origins: &mut OriginTable,
    emitted: (Module, OriginTable),
    block_line: u32,
) {
    let (emitted, emitted_origins) = emitted;
    let item_offset = module.items.len();
    for imp in emitted.imports {
        if !module.imports.contains(&imp) {
            module.imports.push(imp);
            if !module.import_lines.is_empty() {
                module.import_lines.push(u32::MAX);
            }
        }
    }
    merge_from_imports(&mut module.from_imports, emitted.from_imports);
    module.compiler_item_syntax.extend(emitted.compiler_item_syntax);
    module.compiler_expr_syntax.extend(emitted.compiler_expr_syntax);
    module.compiler_type_syntax.extend(emitted.compiler_type_syntax);
    module.compiler_pattern_syntax.extend(emitted.compiler_pattern_syntax);
    module.compiler_stmt_syntax.extend(emitted.compiler_stmt_syntax);
    module.compiler_block_syntax.extend(emitted.compiler_block_syntax);
    let n = emitted.items.len();
    origins.append_shifted(emitted_origins, item_offset);
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

fn ensure_from_imports(target: &mut Vec<(String, Vec<String>)>, module: &str, names: &[&str]) {
    if let Some((_, existing)) = target.iter_mut().find(|(m, _)| m == module) {
        for name in names {
            let name = (*name).to_string();
            if !existing.contains(&name) {
                existing.push(name);
            }
        }
    } else {
        target.push((
            module.to_string(),
            names.iter().map(|name| (*name).to_string()).collect(),
        ));
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
            if line == 0 {
                im.origin = ImplOrigin::CompilerGenerated;
            }
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
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => stamp_expr(expr, line),
        Expr::ExistentialCall { receiver, args, .. } => {
            stamp_expr(receiver, line);
            for arg in args {
                stamp_expr(arg, line);
            }
        }
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
        | Expr::Ctor { args, .. }
        | Expr::AnonCtor { args, .. } => {
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
) -> Result<OriginTable, String> {
    let mut origins = expand_with_origins(name, module)?;
    crate::tagged::expand(name, module, siblings)?;
    if name != "comptime" {
        strip_comptime_only_functions(name, module, &mut origins);
        // Quotation handles have either been emitted or were confined to
        // stripped compile-time helpers. RFC-0082 runtime descriptor requests
        // deliberately survive into typed closed-program preparation, where
        // their stored Type AST is the identity authority.
        module.compiler_item_syntax.clear();
        module.compiler_expr_syntax.clear();
        module
            .compiler_type_syntax
            .retain(|syntax| syntax.runtime_identity);
        module.compiler_pattern_syntax.clear();
        module.compiler_stmt_syntax.clear();
        module.compiler_block_syntax.clear();
    }
    Ok(origins)
}

fn strip_comptime_only_functions(name: &str, module: &mut Module, origins: &mut OriginTable) {
    if !module.items.iter().any(|item| {
        matches!(item, Item::Function(Function { comptime_only: true, .. }))
    }) {
        return;
    }
    let had_lines = !module.item_lines.is_empty();
    let old_lines = std::mem::take(&mut module.item_lines);
    let old_items = std::mem::take(&mut module.items);
    let mut new_items = Vec::with_capacity(old_items.len());
    let mut new_lines = Vec::with_capacity(old_lines.len());
    let retained: Vec<bool> = old_items.iter().map(|item| {
        !matches!(item, Item::Function(Function { comptime_only: true, .. }))
    }).collect();
    for (idx, item) in old_items.into_iter().enumerate() {
        if !retained[idx] {
            continue;
        }
        new_items.push(item);
        if had_lines {
            new_lines.push(old_lines.get(idx).copied().unwrap_or(u32::MAX));
        }
    }
    module.items = new_items;
    if had_lines {
        module.item_lines = new_lines;
    }
    origins.retain_items(name, &retained);
}

#[cfg(test)]
mod tests {
    use witchy_syntax::ast::{Expr, Item, Stmt};

    #[test]
    fn derive_generated_origin_inherits_source_type_line() {
        let src = "type Display derive(Show):\n    value: Int\n\nfn main():\n    0\n";
        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link_with_origins(vec![("main".into(), module)], "main")
            .expect("derived program links with origins");
        let derived = linked
            .origins
            .nodes()
            .iter()
            .find(|entry| entry.origin.invocation.start.line == 1)
            .expect("derived implementation keeps the type declaration invocation");

        assert_eq!(derived.origin.invocation.module, "main");
        assert_ne!(derived.origin.definition.start.line, 0);
    }

    #[test]
    fn typed_item_origin_survives_expansion_and_linking() {
        let src = r#"
import meta

comptime fn build() -> ItemSyntax:
    quote item:
        pub fn generated() -> Int:
            7

comptime:
    emit_item(build())

fn main(console: Console):
    console.print("${generated()}")
"#;

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link_with_origins(vec![("main".into(), module)], "main")
            .expect("link with generated origins");
        let (index, _) = linked.module.items.iter().enumerate().find(|(_, item)| {
            matches!(item, witchy_syntax::ast::Item::Function(function)
                if function.name.ends_with("generated"))
        }).expect("generated function");
        let generated = linked.origins.origin_for_item(index).expect("persistent item origin");

        assert_eq!(generated.id.module, "main");
        assert_eq!(generated.origin.definition.start.line, 5);
        assert_eq!(generated.origin.invocation.start.line, 9);
        assert!(generated.origin.hole_ancestry.is_empty());
        let generated_expr = linked.origins.nodes().iter().find(|entry| {
            entry.node.item as usize == index
                && entry.node.category == witchy_syntax::origin::SyntaxCategory::Expr
                && !entry.node.path.is_empty()
        }).expect("generated function body expression origin");
        assert_eq!(generated_expr.origin.definition.start.line, 5);
        assert_eq!(generated_expr.origin.invocation.start.line, 9);
        assert_eq!(
            linked.origins.origin_for_node(&generated_expr.node).expect("origin lookup by structural path").id,
            generated_expr.id,
        );
        assert!(linked.module.compiler_item_syntax.is_empty());
        let ordinary = crate::pipeline::link(
            vec![(
                "main".into(),
                witchy_syntax::parser::parse_module(src).expect("reparse"),
            )],
            "main",
        ).expect("ordinary runtime link");
        assert_eq!(linked.module, ordinary, "origin collection must not alter backend AST");
        witchy_types::typeck::check(&linked.module).expect("typecheck unchanged runtime AST");
    }

    #[test]
    fn structural_item_hole_ancestry_survives_linking() {
        let src = r#"
import meta

comptime fn build() -> ItemSyntax:
    let leaf = quote expr:
        7
    let value = quote expr:
        ${leaf}
    quote item:
        pub fn generated_with_hole() -> Int:
            ${value}

comptime:
    emit_item(build())

fn main(console: Console):
    console.print("${generated_with_hole()}")
"#;

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link_with_origins(vec![("main".into(), module)], "main")
            .expect("link with structural hole origins");
        let (index, _) = linked.module.items.iter().enumerate().find(|(_, item)| {
            matches!(item, witchy_syntax::ast::Item::Function(function)
                if function.name.ends_with("generated_with_hole"))
        }).expect("generated function");
        let generated = linked.origins.origin_for_item(index).expect("persistent item origin");

        assert_eq!(generated.origin.hole_ancestry.len(), 2);
        assert_eq!(
            generated.origin.hole_ancestry[0].category,
            witchy_syntax::origin::SyntaxCategory::Expr,
        );
        assert_eq!(generated.origin.hole_ancestry[0].definition.start.line, 7);
        assert_eq!(generated.origin.hole_ancestry[0].invocation.start.line, 9);
        assert_eq!(generated.origin.hole_ancestry[1].definition.start.line, 5);
        assert_eq!(generated.origin.hole_ancestry[1].invocation.start.line, 7);
    }

    #[test]
    fn identical_hole_source_keeps_distinct_definition_sites() {
        let src = r#"
import meta

comptime fn build() -> ItemSyntax:
    let first = quote expr:
        7
    let second = quote expr:
        7
    quote item:
        pub fn generated_from_twins() -> Int:
            ${first} + ${second}

comptime:
    emit_item(build())

fn main(console: Console):
    console.print("${generated_from_twins()}")
"#;

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link_with_origins(vec![("main".into(), module)], "main")
            .expect("link with distinct structural hole origins");
        let generated = linked.origins.nodes().iter().find(|entry| {
            linked.module.items.get(entry.node.item as usize).is_some_and(|item| {
                matches!(item, witchy_syntax::ast::Item::Function(function)
                    if function.name.ends_with("generated_from_twins"))
            })
        }).expect("generated function origin");

        let definitions: Vec<u32> = generated
            .origin
            .hole_ancestry
            .iter()
            .map(|hole| hole.definition.start.line)
            .collect();
        assert_eq!(definitions, vec![5, 7]);
        assert!(generated
            .origin
            .hole_ancestry
            .iter()
            .all(|hole| hole.invocation.start.line == 9));
    }

    #[test]
    fn generated_item_budget_allows_the_exact_boundary() {
        let src = r#"
comptime:
    emit("fn first() -> Int:")
    emit("    1")
    emit("fn second() -> Int:")
    emit("    2")

fn main():
    0
"#;

        let mut module = witchy_syntax::parser::parse_module(src).expect("parse");
        super::expand_with_item_limit("main", &mut module, 2)
            .expect("two generated items fit a two-item budget");
    }

    #[test]
    fn generated_item_budget_rejects_one_block_over_the_limit() {
        let src = r#"
comptime:
    emit("fn first() -> Int:")
    emit("    1")
    emit("fn second() -> Int:")
    emit("    2")
    emit("fn third() -> Int:")
    emit("    3")

fn main():
    0
"#;

        let mut module = witchy_syntax::parser::parse_module(src).expect("parse");
        let err = super::expand_with_item_limit("main", &mut module, 2)
            .expect_err("three generated items must exceed a two-item budget");
        assert!(
            err.contains(
                "module `main`: comptime expansion exceeded the generated-item limit of 2"
            ),
            "got: {err}"
        );
    }

    #[test]
    fn generated_item_budget_is_cumulative_across_blocks() {
        let src = r#"
comptime:
    emit("fn first() -> Int:")
    emit("    1")

comptime:
    emit("fn second() -> Int:")
    emit("    2")

fn main():
    0
"#;

        let mut module = witchy_syntax::parser::parse_module(src).expect("parse");
        let err = super::expand_with_item_limit("main", &mut module, 1)
            .expect_err("separate blocks must share one generated-item budget");
        assert!(
            err.contains(
                "module `main`: comptime expansion exceeded the generated-item limit of 1"
            ),
            "got: {err}"
        );
    }

    #[test]
    fn generated_item_budget_counts_lowered_helpers() {
        let src = r#"
comptime:
    emit("gen fn generated() -> Iter(Int):")
    emit("    yield 1")

fn main():
    0
"#;

        let mut module = witchy_syntax::parser::parse_module(src).expect("parse");
        let err = super::expand_with_item_limit("main", &mut module, 1)
            .expect_err("a generated wrapper and helper must consume two item slots");
        assert!(
            err.contains(
                "module `main`: comptime expansion exceeded the generated-item limit of 1"
            ),
            "got: {err}"
        );
    }

    #[test]
    fn generated_item_budget_does_not_recount_retained_generators() {
        let src = r#"
comptime:
    emit("gen fn generated() -> Iter(Int):")
    emit("    yield 1")

comptime:
    emit("fn ordinary() -> Int:")
    emit("    1")

fn main():
    0
"#;

        let mut module = witchy_syntax::parser::parse_module(src).expect("parse");
        super::expand_with_item_limit("main", &mut module, 3)
            .expect("the retained generator consumes its two runtime slots only once");
    }

    #[test]
    fn later_blocks_see_imports_required_by_retained_generators() {
        let src = r#"
comptime:
    emit("gen fn generated() -> Iter(Int):")
    emit("    yield 1")

comptime:
    let xs: List(Int) = iter.collect(iter.range(0, 1))
    for _ in xs:
        emit("fn saw_iter() -> Bool:")
        emit("    true")
"#;

        let mut module = witchy_syntax::parser::parse_module(src).expect("parse");
        super::expand("main", &mut module)
            .expect("the second block inherits the generator's implicit iter import");
        assert!(module.items.iter().any(|item| {
            matches!(item, Item::Function(function) if function.name == "saw_iter")
        }));
    }

    #[test]
    fn generated_item_budget_excludes_temporary_derive_blocks() {
        let src = r#"
comptime:
    emit("type Generated derive(Show):")
    emit("    value: Int")

fn main():
    0
"#;

        let mut module = witchy_syntax::parser::parse_module(src).expect("parse");
        super::expand_with_item_limit("main", &mut module, 2)
            .expect("the generated type and final impl consume two runtime slots");
    }

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
    emit("    " + "${saw_generated}")
    emit("fn saw_handwritten() -> Bool:")
    emit("    " + "${saw_handwritten}")

fn main(console: Console):
    console.print("${saw_generated()}")
    console.print("${saw_handwritten()}")
    let g = Generated(value: 7)
    console.print("${g.value}")
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
    console.print("ok")
"#;

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        witchy_types::typeck::check(&linked).expect("typecheck");
    }

    /// The pulled-std expansion cache (linker.rs) must be TRANSPARENT: linking
    /// the same program twice in one process — the first link populates the
    /// cache (including for `semver`, whose `derive` runs comptime programs),
    /// the second consumes it — must produce identical linked modules. This is
    /// the contract that makes the cache safe: expansion of a bundled std
    /// module is a pure function of the compiled-in std sources.
    #[test]
    fn repeated_links_of_a_derive_importing_program_are_identical() {
        let src = "import semver\nimport json\n\nfn main(console: Console):\n    match semver.parse(\"1.2.3\"):\n        Ok(v) -> console.print(semver.format(v))\n        Err(e) -> console.print(\"err\")\n";
        let link_once = || {
            let module = witchy_syntax::parser::parse_module(src).expect("parse");
            crate::pipeline::link(vec![("main".into(), module)], "main").expect("link")
        };
        let cold = link_once();
        let warm = link_once();
        assert_eq!(cold, warm);
        witchy_types::typeck::check(&warm).expect("typecheck");
        let out = crate::interpreter::run_module(warm, ".", Vec::new()).expect("run");
        assert_eq!(out, ["1.2.3"]);
    }

    #[test]
    fn mixed_source_and_typed_item_output_is_rejected() {
        fn link_error(src: &str) -> String {
            let module = witchy_syntax::parser::parse_module(src).expect("parse");
            crate::pipeline::link(vec![("main".into(), module)], "main")
                .expect_err("mixed comptime channels must be rejected")
                .message
        }

        let via_emit = r#"
comptime:
    emit("fn legacy() -> Int:")
    emit("    1")
    emit_item(item("fn typed() -> Int:\n    2"))

fn main(console: Console):
    console.print("x")
"#;
        let err = link_error(via_emit);
        assert!(err.contains("mixed legacy source output"), "got: {err}");

        let via_console_print = r#"
comptime:
    emit_item(item("fn typed() -> Int:\n    2"))
    console.print("fn legacy() -> Int:")
    console.print("    1")

fn main(console: Console):
    console.print("x")
"#;
        let err = link_error(via_console_print);
        assert!(err.contains("mixed legacy source output"), "got: {err}");
    }

    #[test]
    fn adjacent_compatibility_item_fragments_still_parse_as_one_batch() {
        let src = r#"
comptime:
    let head = "fn split_item() -> Int:"
    let body = "    2"
    emit_item(item(head))
    emit_item(item(body))

fn main(console: Console):
    console.print("${split_item()}")
"#;
        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main")
            .expect("adjacent compatibility item fragments remain supported");
        witchy_types::typeck::check(&linked).expect("typecheck");
        let out = crate::interpreter::run_module(linked, ".", Vec::new()).expect("run");
        assert_eq!(out, ["2"]);
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
    console.print("${p.x}")
    console.print("${p.y}")
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
    console.print(show.render(Generated(value: 7)))
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
    console.print("${xs}")
"#;

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        witchy_types::typeck::check(&linked).expect("typecheck");
        let out = crate::interpreter::run_module(linked, ".", Vec::new()).expect("run");
        assert_eq!(out, ["[1, 2]"]);
    }

    #[test]
    fn expansion_retains_emitted_source_nodes_until_link_lowering() {
        let src = r#"
comptime:
    emit("type Point:")
    emit("    x: Int")
    emit("gen fn generated() -> Iter(Int):")
    emit("    yield 1")
    emit("async fn delayed() -> Int:")
    emit("    1")
    emit("fn point() -> Point:")
    emit("    Point(x: 1)")
"#;

        let mut module = witchy_syntax::parser::parse_module(src).expect("parse");
        super::expand("main", &mut module).expect("expand source-shaped items");
        assert!(module.items.iter().any(|item| {
            matches!(
                item,
                Item::Function(function)
                    if function.name == "generated" && function.is_gen
            )
        }));
        assert!(module.items.iter().any(|item| {
            matches!(
                item,
                Item::Function(function)
                    if function.name == "delayed" && function.is_async
            )
        }));
        assert!(module.items.iter().any(|item| {
            matches!(
                item,
                Item::Function(function)
                    if function.name == "point"
                        && matches!(
                            function.body.stmts.last(),
                            Some(Stmt::Expr(Expr::Record { .. }))
                        )
            )
        }));
    }

    #[test]
    fn lowered_generator_and_async_fanout_keep_valid_item_origins() {
        let src = r#"
comptime:
    emit("gen fn generated() -> Iter(Int):")
    emit("    yield 1")
    emit("async fn delayed() -> Int:")
    emit("    let value = task.done(1).await")
    emit("    value")
"#;

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link_with_origins(vec![("main".into(), module)], "main")
            .expect("link generated fanout with origins");
        let generated_items: Vec<_> = linked
            .module
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                matches!(
                    item,
                    Item::Function(function) if {
                        let name = function
                            .name
                            .rsplit_once('.')
                            .map_or(function.name.as_str(), |(_, name)| name);
                        name == "generated"
                            || name.starts_with("__gen_generated")
                            || name == "delayed"
                            || name.starts_with("__async_delayed")
                    }
                )
            })
            .collect();
        assert!(generated_items.len() >= 4, "both lowerings must fan out items");
        for (index, _) in generated_items {
            let origin = linked
                .origins
                .origin_for_item(index)
                .expect("every lowered output inherits the emitted item trace");
            assert_eq!(origin.origin.invocation.start.line, 2);
        }
    }

    #[test]
    fn emitted_generator_source_is_checked_before_lowering() {
        let generated = r#"
comptime:
    emit("gen fn generated() -> Iter(Int):")
    emit("    region:")
    emit("        yield 1")
    emit("        0")

fn main(console: Console):
    console.print("unreachable")
"#;
        let handwritten = r#"
gen fn generated() -> Iter(Int):
    region:
        yield 1
        0

fn main(console: Console):
    console.print("unreachable")
"#;

        let generated = witchy_syntax::parser::parse_module(generated).expect("parse generated fixture");
        let generated_error = crate::pipeline::link(vec![("main".into(), generated)], "main")
            .expect_err("generated source-only violation must fail before lowering");
        let handwritten = witchy_syntax::parser::parse_module(handwritten).expect("parse handwritten fixture");
        let handwritten_error = crate::pipeline::link(vec![("main".into(), handwritten)], "main")
            .expect_err("handwritten source-only violation must fail before lowering");

        let diagnostic = "cannot `yield` inside `region:`: the generator frame outlives the region";
        assert!(generated_error.message.contains(diagnostic), "{generated_error}");
        assert!(handwritten_error.message.contains(diagnostic), "{handwritten_error}");
        assert!(
            generated_error.message.contains("comptime generated source"),
            "generated diagnostic must retain its expansion boundary: {generated_error}",
        );
    }

    #[test]
    fn emitted_async_functions_are_lowered() {
        let src = r#"
comptime:
    emit("async fn generated() -> Int:")
    emit("    7")

async fn main(console: Console):
    let n = generated().await
    console.print("${n}")
"#;

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        witchy_types::typeck::check(&linked).expect("typecheck");
        let out = crate::interpreter::run_module(linked, ".", Vec::new()).expect("run");
        assert_eq!(out, ["7"]);
    }
}
