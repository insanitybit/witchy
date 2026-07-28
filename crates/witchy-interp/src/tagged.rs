//! `tag"…${expr}…"` — compile-time tagged literals (RFC-0006).
//!
//! A *tag* is a `comptime` function
//! `comptime fn <tag>(parts: List(String), holes: List(String)) -> meta.ExprSyntax`. A
//! tagged literal `tag"a${x}b"` is expanded AT COMPILE TIME — before type-checking.
//!
//! ## Marker substitution (the hygiene split)
//!
//! Each hole is delivered to the tag NOT as its raw source but as an OPAQUE MARKER
//! — the reserved identifier `__witchy_hole_N` (which lexes as a single primary
//! expression and cannot collide with user code; the `__witchy_` prefix is
//! reserved-synthetic). The tag PLACES these markers wherever a hole's value
//! belongs (an `html` text hole emits `glamour.text(__witchy_hole_0)`). After the
//! tag returns `ExprSyntax`, `expand` walks that AST and
//! REPLACES each `__witchy_hole_N` leaf with a CLONE of the hole's ACTUAL
//! expression — parsed ONCE from the original hole source and STAMPED with the
//! hole's captured `(line, col)` (via a one-statement `Expr::Block` whose `lines`
//! carry the hole line, which typeck reads as `cur_line`). A marker may appear any
//! number of times (a tag may drop a hole or use it more than once, e.g. an
//! `html` body that renders `${v}` twice); each occurrence gets a fresh clone, so
//! re-evaluating the pure hole expression at each site matches the original
//! source-text contract. So:
//!
//!   * tag-emitted nodes keep the tag/generated provenance and resolve in the tag's
//!     scope (the tag emits QUALIFIED names like `glamour.text`, immune to a
//!     call-site local named `text`), and
//!   * hole nodes carry the CALL-SITE position, so a type error in a hole points
//!     INTO the literal at that `${…}` rather than at the tag-emitted constructor.
//!
//! This is RFC-0006's hygiene + hole-precise-diagnostics, delivered by one change.
//!
//! Typed RFC-0080 tags emit an expression event through the same interpreter
//! expansion channel as `comptime` item events. A compiler-owned quotation
//! transfers its AST directly. `meta.expr_raw` is the one explicit bridge for a
//! tag that must construct source dynamically. Both runtime backends then
//! compile the same expanded AST. `Expr::TaggedLit` is therefore UNREACHABLE
//! after this pass; typeck, the interpreter, and both codegen backends panic on it.

use witchy_syntax::ast::{Block, Expr, Function, Item, MatchArm, Module, Stmt, Type};
use std::cell::Cell;
use std::collections::HashMap;

/// A tag may emit a tag (re-expansion); cap the nesting so a self-referential or
/// runaway tag fails loudly rather than looping.
const MAX_TAG_DEPTH: u32 = 64;

/// The opaque hole-marker prefix handed to a tag in place of each hole's source.
/// `__witchy_hole_N` lexes as a single primary expression (`Expr::Var`) and the
/// reserved `__witchy_` prefix cannot collide with user code, so after the tag
/// places it we can find each marker as a leaf `Var` and substitute the real hole.
const HOLE_MARKER_PREFIX: &str = "__witchy_hole_";
const HOLE_ORIGIN_MARKER: &str = "@hole_origin";

/// Build the marker for hole `i` (`__witchy_hole_0`, …).
fn hole_marker(i: usize) -> String {
    format!("{HOLE_MARKER_PREFIX}{i}")
}

/// Parse `"__witchy_hole_N"` back to `N`, or `None` if `name` is not a marker.
fn hole_marker_index(name: &str) -> Option<usize> {
    name.strip_prefix(HOLE_MARKER_PREFIX)?.parse().ok()
}

/// Expand every `Expr::TaggedLit` in `module` in place, splicing each tag's
/// generated expression source over the literal. Runs after `comptime::expand`
/// (per module), before name resolution / type-checking. `name` is the module's
/// name, for error messages.
pub fn expand(name: &str, module: &mut Module, siblings: &[(String, Module)]) -> Result<(), String> {
    // Snapshot the module's own items + std imports once: every tag expansion in
    // this module runs in a comptime program carrying this context, so a tag
    // defined locally (or in std) is callable. Cloning per-module (not per-hole)
    // keeps the cost proportional to the number of literals, not items.
    //
    // A tag may also be IMPORTED: a consumer that `import`s a rune (e.g. glamour)
    // and writes `tag"…"` needs the tag — and its transitive helpers — to
    // resolve. So we also fold in the items of every NON-std module this one
    // imports, transitively, drawn from `siblings` (the rest of the link set).
    // std imports stay declared as `imports` (the bundled std is a search path the
    // comptime link resolves on its own). Note this `items` set is the SEARCH
    // POOL; `expand_one` then prunes it per-tag to only what the tag actually
    // reaches before linking — which is what keeps the
    // comptime program free of the consumer's own tagged literals (else linking
    // it would re-enter this pass forever; see linker.rs and `expand_one`).
    let by_name: HashMap<&str, &Module> =
        siblings.iter().map(|(n, m)| (n.as_str(), m)).collect();
    let mut tag_origins = HashMap::new();
    let mut ambiguous_tags = HashMap::new();
    record_tag_origins(
        &mut tag_origins,
        &mut ambiguous_tags,
        name,
        name,
        &module.items,
        &module.item_lines,
        false,
    );
    for imp in &module.imports {
        if witchy_syntax::linker::STD_MODULES.contains(&imp.as_str()) {
            continue;
        }
        if let Some(imported) = by_name.get(imp.as_str()) {
            record_tag_origins(
                &mut tag_origins,
                &mut ambiguous_tags,
                name,
                imp,
                &imported.items,
                &imported.item_lines,
                true,
            );
        }
    }
    let ctx = Context {
        name: name.to_string(),
        invocation_qualifiers: module.imports.clone(),
        tag_origins,
        ambiguous_tags,
        definition_modules: std::iter::once((name.to_string(), module.clone()))
            .chain(siblings.iter().cloned())
            .collect(),
        fresh_invocation: Cell::new(0),
    };
    // Expand tagged literals in EVERY expression-bearing item position, not just
    // free-function bodies (BUG-181): an inherent/trait `impl` method body, a
    // trait's DEFAULT method body, and a top-level `let` constant value can all
    // contain a `tag"…"`. Missing any of them let an `Expr::TaggedLit` survive to
    // the type checker, which `unreachable!`s on it.
    for item in &mut module.items {
        match item {
            Item::Function(f) => walk_block(&mut f.body, &ctx)?,
            Item::Impl(im) => {
                for m in &mut im.methods {
                    walk_block(&mut m.body, &ctx)?;
                }
            }
            Item::Trait(t) => {
                for m in &mut t.methods {
                    if let Some(body) = &mut m.default {
                        walk_block(body, &ctx)?;
                    }
                }
            }
            Item::Const { value, .. } => walk_expr_depth(value, &ctx, 0, &[])?,
            // `comptime:` blocks are already expanded (and consumed) by
            // `comptime::expand`, which runs before this pass.
            Item::Type(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    Ok(())
}

fn expand_tag_program(
    name: &str,
    module: &mut Module,
    _siblings: &[(String, Module)],
) -> Result<witchy_syntax::origin::OriginTable, String> {
    crate::comptime::expand_with_origins(name, module)
}

/// The per-module context a tag runs against.
struct Context {
    name: String,
    /// Module names a tag's generated source may qualify against (the consumer +
    /// its transitive imports). Seeded as `import` lines in the throwaway parse so
    /// `glamour.text(…)` parses as a qualified call, not a method call.
    invocation_qualifiers: Vec<String>,
    tag_origins: HashMap<String, TagOrigin>,
    ambiguous_tags: HashMap<String, Vec<String>>,
    definition_modules: Vec<(String, Module)>,
    /// Stable traversal ordinal used to give each tagged-literal evaluator an
    /// independent RFC-0080 fresh-name namespace.
    fresh_invocation: Cell<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TagOrigin {
    module: String,
    definition_line: u32,
}

fn record_tag_origins(
    origins: &mut HashMap<String, TagOrigin>,
    ambiguities: &mut HashMap<String, Vec<String>>,
    local_module: &str,
    module: &str,
    items: &[Item],
    item_lines: &[u32],
    public_only: bool,
) {
    for (index, item) in items.iter().enumerate() {
        if let Item::Function(function) = item {
            if public_only && !function.public {
                continue;
            }
            let definition_line = item_lines
                .get(index)
                .copied()
                .filter(|line| *line != u32::MAX)
                .unwrap_or(0);
            if public_only {
                if let Some(modules) = ambiguities.get_mut(&function.name) {
                    if !modules.iter().any(|existing| existing == module) {
                        modules.push(module.to_string());
                        modules.sort();
                    }
                    continue;
                }
                if let Some(existing) = origins.get(&function.name) {
                    if existing.module == local_module {
                        continue;
                    }
                    if existing.module != module {
                        let mut modules = vec![existing.module.clone(), module.to_string()];
                        modules.sort();
                        ambiguities.insert(function.name.clone(), modules);
                        origins.remove(&function.name);
                        continue;
                    }
                }
            }
            origins
                .entry(function.name.clone())
                .or_insert_with(|| TagOrigin {
                    module: module.to_string(),
                    definition_line,
                });
        }
    }
}

fn expansion_site(ctx: &Context, tag: &str, invocation_line: u32) -> String {
    let display_tag = witchy_syntax::linker::definition_site_tag_target(tag)
        .map_or(tag, |(_, name)| name);
    let invocation = if invocation_line == 0 {
        format!("module `{}`: tagged literal `{display_tag}`", ctx.name)
    } else {
        format!(
            "module `{}`: tagged literal `{display_tag}` at invocation line {invocation_line}",
            ctx.name
        )
    };
    let Some((_, origin)) = tag_function(ctx, tag) else {
        return invocation;
    };
    if origin.definition_line == 0 {
        format!("{invocation} (defined in module `{}`)", origin.module)
    } else {
        format!(
            "{invocation} (defined in module `{}` at line {})",
            origin.module, origin.definition_line
        )
    }
}

fn expansion_site_with_trace(
    ctx: &Context,
    tag: &str,
    invocation_line: u32,
    ancestry: &[String],
) -> String {
    let current = expansion_site(ctx, tag, invocation_line);
    if ancestry.is_empty() {
        return current;
    }
    format!(
        "{current}\nexpansion trace:\n{}",
        ancestry
            .iter()
            .map(|frame| format!("  from {frame}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Replace `expr` with its expansion if it is (or contains) a `TaggedLit`,
/// recursing into every child. A spliced expression may itself contain a
/// `TaggedLit` (a tag emitting a tag), so we re-walk to a fixed point under a
/// depth cap.
fn walk_expr_depth(
    expr: &mut Expr,
    ctx: &Context,
    depth: u32,
    ancestry: &[String],
) -> Result<(), String> {
    if let Some((mut hole_expr, hole_index, line, column)) = take_hole_origin(expr) {
        let mut hole_ancestry = ancestry.to_vec();
        hole_ancestry.push(format!(
            "hole {} at hole-local line {}, column {}",
            hole_index + 1,
            line,
            column
        ));
        walk_expr_depth(&mut hole_expr, ctx, depth, &hole_ancestry)?;
        *expr = hole_expr;
        return Ok(());
    }
    if let Expr::TaggedLit { tag, parts, holes, hole_spans, line } = expr {
        if depth >= MAX_TAG_DEPTH {
            return Err(format!(
                "{} expanded past the \
                 depth limit ({MAX_TAG_DEPTH}) — a tag is emitting tags without terminating",
                expansion_site_with_trace(ctx, tag, *line, ancestry)
            ));
        }
        let invocation_line = *line;
        let current_site = expansion_site(ctx, tag, invocation_line);
        let tag = std::mem::take(tag);
        let parts = std::mem::take(parts);
        let holes = std::mem::take(holes);
        let hole_spans = std::mem::take(hole_spans);
        let mut child_ancestry = ancestry.to_vec();
        child_ancestry.push(current_site);
        let mut spliced = expand_one(
            ctx,
            &tag,
            &parts,
            &holes,
            &hole_spans,
            invocation_line,
            ancestry,
        )?;
        // Substitution happens before recursive expansion. This preserves the
        // established generated-tree order: dropped holes are never expanded,
        // and duplicated holes receive independent invocation identities.
        walk_expr_depth(&mut spliced, ctx, depth + 1, &child_ancestry)?;
        *expr = spliced;
        return Ok(());
    }
    walk_children(expr, ctx, depth, ancestry)
}

/// Recurse into an expression's children (it is not itself a `TaggedLit`).
fn walk_children(
    expr: &mut Expr,
    ctx: &Context,
    depth: u32,
    ancestry: &[String],
) -> Result<(), String> {
    let recur = |e: &mut Expr| walk_expr_depth(e, ctx, depth, ancestry);
    match expr {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_) => {}
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                recur(x)?;
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for a in args {
                recur(a)?;
            }
        }
        // (RFC-0056) Tagged-literal expansion runs during linking, BEFORE keyword
        // args are resolved, so a labeled call may still be present here — recurse
        // into its argument values to expand any tagged literal nested inside.
        Expr::LabeledCall { args, .. } => {
            for (_, a) in args {
                recur(a)?;
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            recur(receiver)?;
            for a in args {
                recur(a)?;
            }
        }
        Expr::Apply { func, args } => {
            recur(func)?;
            for a in args {
                recur(a)?;
            }
        }
        Expr::Unary { expr, .. } => recur(expr)?,
        Expr::Field { base, .. } => recur(base)?,
        Expr::Lambda { body, .. } => walk_block_depth(body, ctx, depth, ancestry)?,
        Expr::RecordUpdate { name: _, base, fields } => {
            recur(base)?;
            for (_, v) in fields {
                recur(v)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                recur(v)?;
            }
            if let Some(s) = spread {
                recur(s)?;
            }
        }
        Expr::Try(e) => recur(e)?,
        Expr::As { expr, .. } => recur(expr)?,
        Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => recur(expr)?,
        Expr::ExistentialCall { receiver, args, .. } => {
            recur(receiver)?;
            for arg in args {
                recur(arg)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            recur(lhs)?;
            recur(rhs)?;
        }
        Expr::If { cond, then_block, else_block } => {
            recur(cond)?;
            walk_block_depth(then_block, ctx, depth, ancestry)?;
            if let Some(b) = else_block {
                walk_block_depth(b, ctx, depth, ancestry)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            recur(scrutinee)?;
            for MatchArm { guard, body, .. } in arms {
                if let Some(g) = guard {
                    recur(g)?;
                }
                recur(body)?;
            }
        }
        Expr::Block(b) => walk_block_depth(b, ctx, depth, ancestry)?,
        Expr::While { cond, body } => {
            recur(cond)?;
            walk_block_depth(body, ctx, depth, ancestry)?;
        }
        Expr::For { iter, body, .. } => {
            recur(iter)?;
            walk_block_depth(body, ctx, depth, ancestry)?;
        }
        Expr::Range { lo, hi, .. } => {
            recur(lo)?;
            recur(hi)?;
        }
        Expr::Index { base, index } => {
            recur(base)?;
            recur(index)?;
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            recur(scrutinee)?;
            walk_block_depth(body, ctx, depth, ancestry)?;
        }
        // Replaced by `walk_expr_depth` before reaching here.
        Expr::TaggedLit { .. } => unreachable!("TaggedLit handled by walk_expr_depth"),
    }
    Ok(())
}

fn walk_block(block: &mut Block, ctx: &Context) -> Result<(), String> {
    walk_block_depth(block, ctx, 0, &[])
}

fn walk_block_depth(
    block: &mut Block,
    ctx: &Context,
    depth: u32,
    ancestry: &[String],
) -> Result<(), String> {
    for stmt in &mut block.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => walk_expr_depth(value, ctx, depth, ancestry)?,
            Stmt::Return(opt) => {
                if let Some(e) = opt {
                    walk_expr_depth(e, ctx, depth, ancestry)?;
                }
            }
            Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn validate_tag<'a>(
    ctx: &'a Context,
    tag: &str,
) -> Result<(&'a Function, TagOrigin), String> {
    if let Some(modules) = ctx.ambiguous_tags.get(tag) {
        return Err(format!(
            "is ambiguous across directly imported modules: {}",
            modules.join(", ")
        ));
    }
    let Some((f, origin)) = tag_function(ctx, tag) else {
        return Err("is not defined or is not public in a directly imported module".into());
    };
    if !f.comptime_only {
        return Err("must be declared `comptime`".into());
    }
    if f.ret.as_ref().is_some_and(is_expr_syntax_type) {
        Ok((f, origin))
    } else {
        Err("must return meta.ExprSyntax".into())
    }
}

fn tag_function<'a>(ctx: &'a Context, tag: &str) -> Option<(&'a Function, TagOrigin)> {
    let (module_name, name, recorded_origin) = if let Some((module, name)) =
        witchy_syntax::linker::definition_site_tag_target(tag)
    {
        (module.to_string(), name, None)
    } else {
        let origin = ctx.tag_origins.get(tag)?.clone();
        (origin.module.clone(), tag, Some(origin))
    };
    if witchy_syntax::linker::STD_MODULES.contains(&module_name.as_str()) {
        return None;
    }
    let (_, module) = ctx
        .definition_modules
        .iter()
        .find(|(candidate, _)| candidate == &module_name)?;
    let (index, function) = module.items.iter().enumerate().find_map(|(index, item)| {
        match item {
            Item::Function(function) if function.name == name => Some((index, function)),
            _ => None,
        }
    })?;
    let origin = recorded_origin.unwrap_or_else(|| TagOrigin {
        module: module_name,
        definition_line: module
            .item_lines
            .get(index)
            .copied()
            .filter(|line| *line != u32::MAX)
            .unwrap_or(0),
    });
    Some((function, origin))
}

fn is_expr_syntax_type(ty: &Type) -> bool {
    match ty {
        Type::Qualified(_, inner) => is_expr_syntax_type(inner),
        Type::Named(name, args) => {
            args.is_empty() && (name == "ExprSyntax" || name == "meta.ExprSyntax")
        }
        Type::Dyn(_, _) | Type::Tuple(_) | Type::Fn(_, _, _) | Type::RecordCompose { .. } => false,
    }
}

/// Run one tag: build a synthetic comptime program calling `tag(parts, holes)`,
/// run it on the reference interpreter, and transfer its emitted expression AST.
/// A tag must return `meta.ExprSyntax`.
fn expand_one(
    ctx: &Context,
    tag: &str,
    parts: &[String],
    holes: &[String],
    hole_spans: &[(u32, u32)],
    invocation_line: u32,
    ancestry: &[String],
) -> Result<Expr, String> {
    let where_ = || expansion_site_with_trace(ctx, tag, invocation_line, ancestry);
    let invocation = ctx.fresh_invocation.get();
    let next_invocation = invocation
        .checked_add(1)
        .ok_or_else(|| format!("{}: fresh identifier invocation counter overflowed", where_()))?;
    ctx.fresh_invocation.set(next_invocation);
    let str_list = |xs: &[String]| Expr::List(xs.iter().map(|x| Expr::Str(x.clone())).collect());

    // The tag receives an OPAQUE MARKER per hole, NOT the hole's source: it places
    // `__witchy_hole_N` wherever the hole's value belongs, and we substitute the
    // real (already-parsed, position-stamped) hole expression at each marker after
    // the tag returns. This is the hygiene split (RFC-0006): the tag never sees —
    // and so cannot mangle or capture — the author's hole expression.
    let markers: Vec<String> = (0..holes.len()).map(hole_marker).collect();
    let (selected_tag, definition_origin) = validate_tag(ctx, tag)
        .map_err(|reason| format!("{}: tag `{tag}` {reason}", where_()))?;
    let tag_call = Expr::Call {
        name: selected_tag.name.clone(),
        args: vec![str_list(parts), str_list(&markers)],
    };

    let emit_call = Stmt::Expr(Expr::Call {
        name: witchy_syntax::intrinsics::COMPILER_EMIT_EXPR.into(),
        args: vec![tag_call],
    });
    let bridge_name = "@compiler:tag-bridge";
    let bridge = Function {
        public: true,
        comptime_only: true,
        attributes: Vec::new(),
        name: bridge_name.into(),
        params: Vec::new(),
        ret: None,
        body: Block {
            stmts: vec![emit_call],
            lines: vec![0],
            region: None,
        },
        bounds: Vec::new(),
        is_gen: false,
        is_async: false,
    };
    let main = Function {
        public: false,
        comptime_only: false,
        attributes: Vec::new(),
        name: "main".into(),
        params: Vec::new(),
        ret: None,
        body: Block {
            stmts: vec![Stmt::Expr(Expr::Call {
                name: format!("{}.{bridge_name}", definition_origin.module),
                args: Vec::new(),
            })],
            lines: vec![0],
            region: None,
        },
        bounds: Vec::new(),
        is_gen: false,
        is_async: false,
    };

    // The program carries ONLY the items REACHABLE FROM THE TAG FUNCTION (its
    // transitive function callees + every type those signatures/variants name),
    // plus our synthetic `main`. This is the load-bearing fix for the
    // infinite-recursion bug: linking this comptime program re-runs
    // `tagged::expand` per module (linker.rs), so any `TaggedLit` left in an
    // included function would expand again → rebuild this same program → ∞ loop.
    // A consumer that writes `tag"…"` inside a NON-`main` function (e.g.
    // `fn view(...) -> VNode: html"…"`) folds that tag-bearing function into
    // `ctx.items`; the unreachable-from-the-tag prune drops it (a tag never calls
    // its own consumer's `view`/`update`/`main`), so the comptime program contains
    // no tagged literals and the recursion terminates. The constructors a tag
    // EMITS as source (`element`/`text`/`prop`) live in the GENERATED expression,
    // which is spliced into the consumer and type-checked there — they are not
    // roots of this program, so pruning them out is correct.
    let root_name = selected_tag.name.clone();
    let keep = crate::reachability::reachable_from_module_function(
        &ctx.definition_modules,
        &definition_origin.module,
        &root_name,
    );
    for (module_name, module) in &ctx.definition_modules {
        for (item_index, item) in module.items.iter().enumerate() {
            match item {
                Item::Function(function)
                    if keep.contains(&(module_name.clone(), function.name.clone()))
                        && crate::reachability::block_contains_tagged_literal(&function.body) =>
                {
                    return Err(format!(
                        "{}: tag evaluator function `{}.{}` contains a tagged literal; nested tags must be returned as expression syntax",
                        where_(),
                        module_name,
                        function.name,
                    ));
                }
                Item::Trait(trait_)
                    if keep.contains(&(module_name.clone(), trait_.name.clone())) =>
                {
                    for method in &trait_.methods {
                        if method.default.as_ref().is_some_and(
                            crate::reachability::block_contains_tagged_literal,
                        ) {
                            return Err(format!(
                                "{}: tag evaluator trait default `{}.{}` contains a tagged literal; nested tags must be returned as expression syntax",
                                where_(),
                                trait_.name,
                                method.name,
                            ));
                        }
                    }
                }
                Item::Impl(impl_)
                    if keep.contains(&(
                        module_name.clone(),
                        crate::reachability::impl_item_identity(item_index),
                    )) =>
                {
                    for method in &impl_.methods {
                        if crate::reachability::block_contains_tagged_literal(&method.body) {
                            return Err(format!(
                                "{}: tag evaluator implementation method `{}.{}` contains a tagged literal; nested tags must be returned as expression syntax",
                                where_(),
                                impl_.type_name,
                                method.name,
                            ));
                        }
                    }
                }
                Item::Const { name, value }
                    if keep.contains(&(module_name.clone(), name.clone()))
                        && crate::reachability::expr_contains_tagged_literal(value) =>
                {
                    return Err(format!(
                        "{}: tag evaluator constant `{}.{}` contains a tagged literal; nested tags must be returned as expression syntax",
                        where_(),
                        module_name,
                        name,
                    ));
                }
                _ => {}
            }
        }
    }
    let mut modules: Vec<(String, Module)> = ctx
        .definition_modules
        .iter()
        .filter(|(name, _)| !witchy_syntax::linker::STD_MODULES.contains(&name.as_str()))
        .cloned()
        .collect();
    for (module_name, module) in &mut modules {
        let module_has_reachable_items = keep.iter().any(|(owner, _)| owner == module_name);
        let mut item_index = 0;
        module.items.retain_mut(|item| {
            let original_index = item_index;
            item_index += 1;
            match item {
            Item::Function(function) => {
                keep.contains(&(module_name.clone(), function.name.clone()))
            }
            Item::Type(ty) => keep.contains(&(module_name.clone(), ty.name.clone())),
            Item::TypeAlias { name, .. } => keep.contains(&(module_name.clone(), name.clone())),
            Item::Trait(trait_) => {
                keep.contains(&(module_name.clone(), trait_.name.clone()))
            }
            Item::Impl(_) => keep.contains(&(
                module_name.clone(),
                crate::reachability::impl_item_identity(original_index),
            )),
            Item::Const { name, .. } => {
                keep.contains(&(module_name.clone(), name.clone()))
            }
            Item::Comptime(_) => false,
        }});
        if !module_has_reachable_items {
            module.imports.clear();
            module.from_imports.clear();
            module.compiler_item_syntax.clear();
            module.compiler_expr_syntax.clear();
            module.compiler_type_syntax.clear();
            module.compiler_pattern_syntax.clear();
            module.compiler_stmt_syntax.clear();
            module.compiler_block_syntax.clear();
        } else {
            module.from_imports.retain_mut(|(source, names)| {
                if witchy_syntax::linker::STD_MODULES.contains(&source.as_str()) {
                    return true;
                }
                names.retain(|name| {
                    crate::reachability::module_item_identity(
                        &ctx.definition_modules,
                        source,
                        name,
                    )
                    .is_some_and(|identity| keep.contains(&(source.clone(), identity)))
                });
                !names.is_empty()
            });
            module.compiler_type_syntax.retain(|syntax| !syntax.runtime_identity);
        }
        module.import_lines.clear();
        module.item_lines.clear();
        module.linked_entry = None;
        if module_name == &definition_origin.module {
            module.items.push(Item::Function(bridge.clone()));
        }
    }
    let mut entry_module = "@compiler:tag-entry".to_string();
    while modules.iter().any(|(name, _)| name == &entry_module) {
        entry_module.push(':');
    }
    modules.push((
        entry_module.clone(),
        Module {
            modes: Vec::new(),
            imports: vec![definition_origin.module.clone()],
            from_imports: Vec::new(),
            items: vec![Item::Function(main)],
            import_lines: Vec::new(),
            item_lines: Vec::new(),
            compiler_item_syntax: Vec::new(),
            compiler_expr_syntax: Vec::new(),
            compiler_type_syntax: Vec::new(),
            compiler_pattern_syntax: Vec::new(),
            compiler_stmt_syntax: Vec::new(),
            compiler_block_syntax: Vec::new(),
            linked_entry: None,
        },
    ));
    let linked = witchy_syntax::linker::link(
        modules,
        &entry_module,
        expand_tag_program,
    )
        .map_err(|e| format!("{}: {e}", where_()))?;
    witchy_types::typeck::check_comptime(&linked).map_err(|e| format!("{}: {e}", where_()))?;
    let crate::interpreter::ComptimeOutputs {
        output: lines,
        items: item_output,
        exprs: mut expr_output,
    } = crate::interpreter::run_comptime_module_outputs_budgeted_in_scope_with_qualifiers(
        linked,
        ".",
        crate::interpreter::COMPTIME_STEP_LIMIT,
        Some(format!(
            "tag:{}:{}:{}:{tag}:{invocation}",
            ctx.name.len(),
            ctx.name,
            tag.len()
        )),
        Some({
            let mut qualifiers = ctx
                .definition_modules
                .iter()
                .find(|(name, _)| name == &definition_origin.module)
                .map(|(_, module)| module.imports.clone())
                .unwrap_or_default();
            if !qualifiers.contains(&definition_origin.module) {
                qualifiers.push(definition_origin.module.clone());
            }
            qualifiers
        }),
    )
    .map_err(|e| format!("{}: {e}", where_()))?;
    if !item_output.is_empty() {
        return Err(format!("{}: a tagged literal may emit one expression, not items", where_()));
    }
    let mut e = {
            if !lines.is_empty() {
                return Err(format!(
                    "{}: a typed tag produced unexpected source output",
                    where_()
                ));
            }
            if expr_output.len() != 1 {
                return Err(format!(
                    "{}: a typed tag must emit exactly one expression, emitted {}",
                    where_(),
                    expr_output.len()
                ));
            }
            match expr_output.pop().expect("one expression emission") {
                crate::interpreter::ComptimeExprEmission::Syntax(expr) => {
                    let mut expr = *expr;
                    witchy_syntax::linker::mark_definition_site_expr(
                        &mut expr,
                        &definition_origin.module,
                        &ctx.definition_modules,
                    )
                    .map_err(|error| format!("{}: {error}", where_()))?;
                    expr
                }
            }
    };

    // Parse each hole's ORIGINAL source ONCE, into an expression carrying the
    // author's own AST (resolved at the CALL site), wrapped in a one-statement
    // block stamped with the hole's `(line, col)` so a type error inside the hole
    // reports the hole's position in the literal — not the tag-emitted constructor.
    // Parsing once here (not inside the tag's generated source) is the hygiene win:
    // the hole expression is never re-lexed in the generated context.
    let mut hole_exprs: Vec<Expr> = Vec::with_capacity(holes.len());
    for (i, hole) in holes.iter().enumerate() {
        let span = hole_spans.get(i).copied().unwrap_or((0, 0));
        let hole_expr = parse_hole(hole, span, &ctx.invocation_qualifiers, &where_)?;
        hole_exprs.push(wrap_hole_origin(hole_expr, i, span));
    }

    // Replace every `__witchy_hole_N` marker leaf the tag placed with a CLONE of
    // hole N's parsed-and-stamped expression. A tag may place a hole's marker any
    // number of times (zero, one, or many) — witchy values are data and a hole is a
    // pure call-site expression, so re-evaluating it at each site is correct and
    // matches the original source-text contract (`"${v} ${v}"` duplicates `v`).
    // Each clone keeps the hole's source span, so diagnostics still point into the
    // literal. A tag that drops a hole simply never places its marker (fine).
    substitute_holes(&mut e, &hole_exprs, &where_)?;
    Ok(e)
}

/// Parse `src` as a single expression by wrapping it as the tail expression of a
/// throwaway `fn __tagsplice()` (only `parse_module` exists). `qualifiers` are the
/// module names to seed as `import` lines so `m.f(…)` parses as a qualified call,
/// not a UFCS method call — load-bearing for the tag's hygienic qualified emission.
/// Whether `name` is a valid witchy module identifier — the shape an `import`
/// line accepts, and the only shape a qualified call `name.f(…)` can lex as. A
/// standalone file's module name is its filesystem stem, which may not be one
/// (`tag-hyphen`), so tag-splice import seeding must gate on this.
fn is_module_ident(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Re-indent a spliced tag body so it nests under the throwaway `fn __tagsplice():`
/// wrapper (whose body sits at 4-column indentation): every *structural* newline
/// must carry the same 4-space continuation so a multi-line emitted expression stays
/// inside the function block. A newline that falls INSIDE a `"…"` string literal is
/// NOT structural — it is literal content the author wrote (a multiline tagged
/// literal, BUG-339) and must survive byte-for-byte; injecting spaces there would
/// silently rewrite the string in the shared expanded AST (both backends). We track
/// string state (honoring `\`-escapes) and indent only newlines outside a string.
fn reindent_body(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + src.len() / 8);
    let mut in_string = false;
    let mut escaped = false;
    for c in src.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            // A structural newline: keep the body indented under `__tagsplice`.
            '\n' => out.push_str("\n    "),
            _ => out.push(c),
        }
    }
    out
}

pub(crate) fn parse_generated_splice_expr(
    src: &str,
    qualifiers: &[String],
) -> Result<Expr, String> {
    parse_splice_expr(&escape_generated_string_interpolations(src), qualifiers)
}

/// Tag output is generated expression source, but RFC-0006 hygiene says only the
/// original `${...}` holes resolve at the call site. A tag-emitted string literal
/// containing raw `${...}` is therefore static text, not a fresh interpolation
/// opportunity. Escape those sigils before the throwaway parse; hole source uses
/// `parse_splice_expr` directly and keeps normal interpolation semantics.
fn escape_generated_string_interpolations(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            if escaped {
                out.push(c);
                escaped = false;
                continue;
            }
            match c {
                '\\' => {
                    out.push(c);
                    escaped = true;
                }
                '"' => {
                    out.push(c);
                    in_string = false;
                }
                '$' if chars.peek() == Some(&'{') => {
                    out.push('\\');
                    out.push('$');
                }
                _ => out.push(c),
            }
        } else {
            out.push(c);
            if c == '"' {
                in_string = true;
            }
        }
    }
    out
}

fn parse_splice_expr(src: &str, qualifiers: &[String]) -> Result<Expr, String> {
    let mut wrapped = String::new();
    for q in qualifiers {
        // A qualifier that is not a valid module identifier can never be referenced
        // as `q.f(…)` (it would not lex), so it never needs an `import` line — and
        // emitting one would itself fail to parse. A standalone file's module name
        // is its filesystem stem, which may be a NON-identifier (`tag-hyphen`); such
        // a name reaches `qualifiers` via `seen`, so it must be skipped here rather
        // than break every tag expansion in a hyphenated file (BUG-182).
        if !is_module_ident(q) {
            continue;
        }
        // Only real (non-prelude) module names need an explicit `import`; the
        // prelude modules (`list`/`string`/…) the parser already seeds. Importing a
        // prelude name again is harmless, so we don't filter — the parse is
        // throwaway and never linked.
        wrapped.push_str("import ");
        wrapped.push_str(q);
        wrapped.push('\n');
    }
    wrapped.push_str("fn __tagsplice():\n    ");
    wrapped.push_str(&reindent_body(src));
    wrapped.push('\n');
    let parsed = witchy_syntax::parser::parse_module(&wrapped)
        .map_err(|e| format!("does not parse as an expression: {e}"))?;
    let Some(Item::Function(f)) =
        parsed.items.into_iter().find(|it| matches!(it, Item::Function(f) if f.name == "__tagsplice"))
    else {
        return Err("did not yield an expression".to_string());
    };
    let Some(Stmt::Expr(e)) = f.body.stmts.into_iter().next_back() else {
        return Err("is not a single expression".to_string());
    };
    Ok(e)
}

/// Parse a single hole's source `hole` as an expression and stamp it with the
/// hole's source position `(line, _col)` so type errors point INTO the literal.
/// The stamping is a one-statement `Expr::Block` whose `lines` carry the hole line,
/// which `typeck` reads as `cur_line` while inferring the hole — backends treat the
/// block transparently, so parity is preserved.
fn parse_hole(
    hole: &str,
    (line, _col): (u32, u32),
    qualifiers: &[String],
    where_: &impl Fn() -> String,
) -> Result<Expr, String> {
    let inner = parse_splice_expr(hole, qualifiers)
        .map_err(|e| format!("{}: hole `{hole}` {e}", where_()))?;
    // Stamp the hole line via a one-statement block; line 0 (no span) leaves the
    // statement's own line in effect, exactly as before.
    if line == 0 {
        Ok(inner)
    } else {
        Ok(Expr::Block(Block {
            stmts: vec![Stmt::Expr(inner)],
            lines: vec![line],
            region: None,
        }))
    }
}

fn wrap_hole_origin(
    hole: Expr,
    index: usize,
    (line, column): (u32, u32),
) -> Expr {
    Expr::Block(Block {
        stmts: vec![
            Stmt::Expr(Expr::Call {
                name: HOLE_ORIGIN_MARKER.to_string(),
                args: vec![
                    Expr::Int(index as i64),
                    Expr::Int(i64::from(line)),
                    Expr::Int(i64::from(column)),
                ],
            }),
            Stmt::Expr(hole),
        ],
        lines: vec![line, line],
        region: None,
    })
}

fn take_hole_origin(expr: &mut Expr) -> Option<(Expr, usize, u32, u32)> {
    let Expr::Block(block) = expr else { return None };
    let [Stmt::Expr(Expr::Call { name, args }), Stmt::Expr(_)] = block.stmts.as_slice() else {
        return None;
    };
    if name != HOLE_ORIGIN_MARKER {
        return None;
    }
    let [Expr::Int(index), Expr::Int(line), Expr::Int(column)] = args.as_slice() else {
        return None;
    };
    let index = usize::try_from(*index).ok()?;
    let line = u32::try_from(*line).ok()?;
    let column = u32::try_from(*column).ok()?;
    let Some(Stmt::Expr(hole)) = block.stmts.pop() else {
        unreachable!("hole-origin wrapper shape checked above")
    };
    Some((hole, index, line, column))
}

/// Walk `expr` and replace each `__witchy_hole_N` marker leaf with a CLONE of
/// `holes[N]`. A marker may appear any number of times — a hole is a pure call-site
/// expression and witchy values are data, so re-evaluating it at each site is
/// correct (the original `(parts, holes)` source-text contract let a hole appear
/// any number of times, e.g. `"${v} ${v}"`). Cloning preserves the hole's source
/// span at every site, so a type error still points into the literal.
fn substitute_holes(
    expr: &mut Expr,
    holes: &[Expr],
    where_: &impl Fn() -> String,
) -> Result<(), String> {
    // A marker is a bare `Var` leaf; swap it for a fresh clone of the parsed hole.
    if let Expr::Var(name) = expr {
        if let Some(idx) = hole_marker_index(name) {
            let Some(hole) = holes.get(idx) else {
                return Err(format!(
                    "{}: tag placed marker `{}` but there is no hole {idx}",
                    where_(),
                    hole_marker(idx)
                ));
            };
            *expr = hole.clone();
            return Ok(());
        }
    }
    substitute_holes_children(expr, holes, where_)
}

/// Recurse into an expression's children, substituting markers (it is not itself a
/// marker leaf). Mirrors `walk_children` but threads the hole slots.
fn substitute_holes_children(
    expr: &mut Expr,
    holes: &[Expr],
    where_: &impl Fn() -> String,
) -> Result<(), String> {
    match expr {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_) => {}
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                substitute_holes(x, holes, where_)?;
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for a in args {
                substitute_holes(a, holes, where_)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, a) in args {
                substitute_holes(a, holes, where_)?;
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            substitute_holes(receiver, holes, where_)?;
            for a in args {
                substitute_holes(a, holes, where_)?;
            }
        }
        Expr::Apply { func, args } => {
            substitute_holes(func, holes, where_)?;
            for a in args {
                substitute_holes(a, holes, where_)?;
            }
        }
        Expr::Unary { expr, .. } => substitute_holes(expr, holes, where_)?,
        Expr::Field { base, .. } => substitute_holes(base, holes, where_)?,
        Expr::Lambda { body, .. } => substitute_holes_block(body, holes, where_)?,
        Expr::RecordUpdate { name: _, base, fields } => {
            substitute_holes(base, holes, where_)?;
            for (_, v) in fields {
                substitute_holes(v, holes, where_)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                substitute_holes(v, holes, where_)?;
            }
            if let Some(s) = spread {
                substitute_holes(s, holes, where_)?;
            }
        }
        Expr::Try(e) => substitute_holes(e, holes, where_)?,
        Expr::As { expr, .. } => substitute_holes(expr, holes, where_)?,
        Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => substitute_holes(expr, holes, where_)?,
        Expr::ExistentialCall { receiver, args, .. } => {
            substitute_holes(receiver, holes, where_)?;
            for arg in args {
                substitute_holes(arg, holes, where_)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            substitute_holes(lhs, holes, where_)?;
            substitute_holes(rhs, holes, where_)?;
        }
        Expr::If { cond, then_block, else_block } => {
            substitute_holes(cond, holes, where_)?;
            substitute_holes_block(then_block, holes, where_)?;
            if let Some(b) = else_block {
                substitute_holes_block(b, holes, where_)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            substitute_holes(scrutinee, holes, where_)?;
            for MatchArm { guard, body, .. } in arms {
                if let Some(g) = guard {
                    substitute_holes(g, holes, where_)?;
                }
                substitute_holes(body, holes, where_)?;
            }
        }
        Expr::Block(b) => substitute_holes_block(b, holes, where_)?,
        Expr::While { cond, body } => {
            substitute_holes(cond, holes, where_)?;
            substitute_holes_block(body, holes, where_)?;
        }
        Expr::For { iter, body, .. } => {
            substitute_holes(iter, holes, where_)?;
            substitute_holes_block(body, holes, where_)?;
        }
        Expr::Range { lo, hi, .. } => {
            substitute_holes(lo, holes, where_)?;
            substitute_holes(hi, holes, where_)?;
        }
        Expr::Index { base, index } => {
            substitute_holes(base, holes, where_)?;
            substitute_holes(index, holes, where_)?;
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            substitute_holes(scrutinee, holes, where_)?;
            substitute_holes_block(body, holes, where_)?;
        }
        // A typed tag can still construct a nested tagged literal through
        // `meta.expr_raw`; that nested `tag"…"` is
        // re-expanded by `walk_expr_depth` AFTER substitution, never reached here).
        Expr::TaggedLit { .. } => {}
    }
    Ok(())
}

fn substitute_holes_block(
    block: &mut Block,
    holes: &[Expr],
    where_: &impl Fn() -> String,
) -> Result<(), String> {
    for stmt in &mut block.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => substitute_holes(value, holes, where_)?,
            Stmt::Return(opt) => {
                if let Some(e) = opt {
                    substitute_holes(e, holes, where_)?;
                }
            }
            Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{escape_generated_string_interpolations, reindent_body};

    #[test]
    fn reindents_structural_newlines_only() {
        // A structural newline (outside any string) gets the 4-space continuation
        // so the emitted expression stays nested under `fn __tagsplice()`.
        assert_eq!(reindent_body("f(\na)"), "f(\n    a)");
        // A newline INSIDE a string literal is content — preserved byte-for-byte
        // (BUG-339); it must NOT gain four spaces.
        assert_eq!(reindent_body("\"line1\nline2\""), "\"line1\nline2\"");
        // Mixed: structural newlines reindent, the in-string newline does not.
        assert_eq!(
            reindent_body("f(\n\"a\nb\",\n1)"),
            "f(\n    \"a\nb\",\n    1)"
        );
        // An escaped quote does not close the string, so its trailing newline stays
        // content, and the newline after the real closing quote is structural.
        assert_eq!(
            reindent_body("\"a\\\"\nb\"\nc"),
            "\"a\\\"\nb\"\n    c"
        );
    }

    #[test]
    fn generated_string_interpolation_is_escaped() {
        assert_eq!(
            escape_generated_string_interpolations(r#"glamour.text("${price}")"#),
            r#"glamour.text("\${price}")"#
        );
        assert_eq!(
            escape_generated_string_interpolations(r#""${a}" + show.render(x) + "\${b}""#),
            r#""\${a}" + show.render(x) + "\${b}""#
        );
        assert_eq!(
            escape_generated_string_interpolations("\"line\n${x}\""),
            "\"line\n\\${x}\""
        );
    }
}
