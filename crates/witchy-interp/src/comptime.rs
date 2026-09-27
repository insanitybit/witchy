//! `comptime:` — compile-time, capability-free, ADDITIVE item generation
//! (rfcs/language-evolution.md Phase 5).
//!
//! A comptime block is an ordinary witchy block executed AT COMPILE TIME. It
//! cannot reach a capability — there is no parameter list to receive one — so
//! its execution is deterministic by construction. Typed `ItemSyntax` values
//! are APPENDED to the enclosing module before type checking and footprint
//! analysis run: generated code is analyzed exactly like handwritten code, and
//! nothing existing can be rewritten or removed, so no comptime block can
//! launder authority out of a signature. Source-string output is retired.
//!
//! v1 executes the block on the reference interpreter (the same zero-grant
//! determinism contract the WASM build sandbox enforces structurally); typed
//! compiler-owned syntax remains AST all the way back to this expander.

use witchy_syntax::ast::{
    Block, CallableQualifiers, Expr, Function, ImplOrigin, Item, Module, Param, Stmt, Type,
};
use witchy_syntax::origin::{ExpansionOrigin, OriginTable, SourceSpan};
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

// foldhash (not SipHash): the expansion cache is keyed by an already-computed
// u64 digest of compiler-internal syntax (never attacker-controlled), and sits
// on the comptime re-expansion hot path — matching the interpreter's own
// FxHashMap convention (see interpreter.rs).
use foldhash::{HashMap, HashMapExt as _};

mod batch;

use batch::{BatchNext, BatchStart, ComptimeBatch, MAX_BATCH_RESTARTS};

const MAX_COMPTIME_BLOCKS: usize = 256;
// Per module, after generated `gen`/`async` helpers are lowered into real items.
const MAX_COMPTIME_GENERATED_ITEMS: usize = 4096;

/// Expand every `comptime:` block in `module` (consuming the items), running
/// each and appending its typed `ItemSyntax` output. `name` is the module's
/// name, for error messages.
pub fn expand(name: &str, module: &mut Module) -> Result<(), String> {
    expand_with_origins(name, module).map(|_| ())
}

/// Expand while retaining the typed source provenance that runtime linking and
/// backend compilation intentionally ignore. Tooling uses this entry point.
pub(crate) fn expand_with_origins(name: &str, module: &mut Module) -> Result<OriginTable, String> {
    expand_with_item_limit_and_origins(name, module, MAX_COMPTIME_GENERATED_ITEMS)
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
    let mut batch: Option<ComptimeBatch> = None;
    let mut batch_restarts = if batch::enabled() { 0 } else { MAX_BATCH_RESTARTS };
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
        let Item::Comptime(body) = module.items.remove(i) else {
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

        let prog = build_comptime_program(module, body)?;
        let fresh_ident_scope = Some(format!("comptime:{}:{name}:{expanded}", name.len()));
        let mut batched = None;
        if let Some(mut pending) = batch.take() {
            match pending.take_next(&prog, &fresh_ident_scope) {
                BatchNext::Ran(output) => {
                    batched = Some(output);
                    if !pending.is_empty() {
                        batch = Some(pending);
                    }
                }
                // Rerun alone so the diagnostic is exactly the unbatched one.
                BatchNext::Failed => batch_restarts = MAX_BATCH_RESTARTS,
                BatchNext::Diverged => batch_restarts += 1,
            }
        }
        if batched.is_none() && batch_restarts < MAX_BATCH_RESTARTS {
            match ComptimeBatch::start(module, i, &prog, name, expanded) {
                BatchStart::Started(output, pending) => {
                    batched = Some(output);
                    batch = pending;
                }
                BatchStart::Nothing => {}
                BatchStart::Failed => batch_restarts = MAX_BATCH_RESTARTS,
            }
        }
        let (lines, item_output) = match batched {
            Some(output) => output,
            None => {
                let linked =
                    crate::pipeline::link_internal(vec![("comptime".into(), prog)], "comptime")
                        .map_err(|e| format!("module `{name}`: comptime block: {e}"))?;
                witchy_types::typeck::check_comptime(&linked)
                    .map_err(|e| format!("module `{name}`: comptime block: {e}"))?;
                crate::interpreter::run_comptime_module_budgeted_in_scope(
                    linked,
                    ".",
                    crate::interpreter::COMPTIME_STEP_LIMIT,
                    fresh_ident_scope,
                )
                .map_err(|e| format!("module `{name}`: comptime block: {e}"))?
            }
        };
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

/// The synthetic, zero-capability program that runs one `comptime:` block
/// against the enclosing module's current state.
fn build_comptime_program(module: &Module, mut body: Block) -> Result<Module, String> {
    // The block becomes a zero-argument `fn main()` in a synthetic,
    // zero-capability program carrying the enclosing module's imports.
    // `emit_item(meta.ItemSyntax)` is the typed RFC-0080 migration
    // boundary. `emit_item` calls an unspellable compiler operation so a
    // compiler-owned item can remain AST all the way back to this
    // expander. Linked recursively
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
                qualifiers: CallableQualifiers::ORDINARY,
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
    ensure_from_imports(&mut prog_from_imports, "meta", &["ItemSyntax"]);
    if !prog_imports.iter().any(|i| i == "meta") {
        prog_imports.push("meta".into());
    }
    let mut prog_items = reachable_local_items(&module.items, &body);
    prog_items.push(Item::Function(Function {
        line: 0,
        public: false,
        pure: false,
        comptime_only: false,
        attributes: Vec::new(),
        name: "main".into(),
        type_params: Vec::new(),
        params: Vec::new(),
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
    Ok(prog)
}

fn item_limit_error(name: &str, max_generated_items: usize) -> String {
    format!(
        "module `{name}`: comptime expansion exceeded the generated-item limit of \
         {max_generated_items}"
    )
}

fn decode_comptime_output(
    module_name: &str,
    invocation_line: u32,
    lines: Vec<String>,
    item_output: Vec<crate::interpreter::PositionedComptimeItem>,
) -> Result<(Module, OriginTable), String> {
    if !lines.is_empty() {
        return Err(
            "compile-time source output is retired; use typed `emit_item` with `quote item`"
                .into(),
        );
    }
    if item_output.is_empty() {
        let emitted = witchy_syntax::parser::parse_module("")
            .expect("the empty module is always valid Witchy");
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
    for positioned in item_output {
        if positioned.output_position != 0 {
            return Err("internal: typed comptime output lost its source ordering".into());
        }
        match positioned.emission {
            crate::interpreter::ComptimeItemEmission::ModuleSyntax {
                mut module,
                definition_line,
                hole_ancestry,
            } => {
                mark_typed_item_impls_compiler_generated(&mut module);
                let mut module_origins = OriginTable::default();
                record_emitted_items(
                    &mut module_origins,
                    &module,
                    module_name,
                    definition_line,
                    invocation_line,
                    0,
                    &hole_ancestry,
                );
                append_unstamped_module(&mut emitted, &mut origins, *module, module_origins);
            }
            crate::interpreter::ComptimeItemEmission::Syntax {
                mut item,
                definition_line,
                hole_ancestry,
            } => {
                mark_typed_item_impl_compiler_generated(item.as_mut());
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
    Ok((emitted, origins))
}

fn mark_typed_item_impls_compiler_generated(module: &mut Module) {
    for item in &mut module.items {
        mark_typed_item_impl_compiler_generated(item);
    }
}

fn mark_typed_item_impl_compiler_generated(item: &mut Item) {
    match item {
        Item::Function(function) => {
            if !function
                .attributes
                .iter()
                .any(|attribute| {
                    attribute == witchy_syntax::source_check::GENERATED_ITEM_ATTRIBUTE
                })
            {
                function
                    .attributes
                    .push(witchy_syntax::source_check::GENERATED_ITEM_ATTRIBUTE.into());
            }
        }
        Item::Impl(implementation) => {
            // Typed quotation arrives here only through the unspellable
            // `emit_item` intrinsic. Preserve that
            // compiler-owned boundary so source validation can distinguish
            // generated private methods from user declarations.
            implementation.origin = ImplOrigin::CompilerGenerated;
        }
        _ => {}
    }
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
    let module = witchy_syntax::source_check::check(module)
        .map_err(|error| error.to_string())?;
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
        // Typed emitted items carry compiler-owned fragment lines. Re-stamp
        // them to the `comptime:` block's own source line so later diagnostics
        // remain anchored to the real file (BUG-341).
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
        Expr::LabeledMethodCall { receiver, args, .. } => {
            stamp_expr(receiver, line);
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
    // Ordinary modules have no expensive expansion to memoize. Keep running
    // both passes and finalization below, but avoid hashing sibling ASTs and
    // retaining a cloned result just to cache an expansion-free module.
    let key = (module.items.iter().any(|item| matches!(item, Item::Comptime(_)))
        || crate::tagged::needs_expansion(module))
        .then(|| expansion_cache_key(name, module, siblings));
    if let Some(key) = key
        && let Ok(cache) = expansion_cache().lock()
        && let Some(cached) = cache.get(&key)
    {
        *module = cached.module.clone();
        return Ok(cached.origins.clone());
    }
    let mut origins = expand_with_origins(name, module)?;
    origins.append_shifted(crate::tagged::expand(name, module, siblings)?, 0);
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
    if let Some(key) = key
        && let Ok(mut cache) = expansion_cache().lock()
    {
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(
            key,
            CachedExpansion {
                module: module.clone(),
                origins: origins.clone(),
            },
        );
    }
    Ok(origins)
}

#[derive(Clone)]
struct CachedExpansion {
    module: Module,
    origins: OriginTable,
}

fn expansion_cache() -> &'static Mutex<HashMap<u64, CachedExpansion>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, CachedExpansion>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn expansion_cache_key(name: &str, module: &Module, siblings: &[(String, Module)]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    format!("{module:?}").hash(&mut hasher);
    format!("{siblings:?}").hash(&mut hasher);
    hasher.finish()
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
