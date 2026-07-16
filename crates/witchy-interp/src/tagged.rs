//! `tag"…${expr}…"` — compile-time tagged literals (RFC-0006).
//!
//! A *tag* is an ordinary compile-time function
//! `fn <tag>(parts: List(String), holes: List(String)) -> String` that returns
//! witchy EXPRESSION SOURCE, or the RFC-0080 typed form
//! `fn <tag>(parts: List(String), holes: List(String)) -> meta.ExprSyntax`. A
//! tagged literal `tag"a${x}b"` is expanded AT COMPILE TIME — before type-checking.
//!
//! ## Marker substitution (the hygiene split)
//!
//! Each hole is delivered to the tag NOT as its raw source but as an OPAQUE MARKER
//! — the reserved identifier `__witchy_hole_N` (which lexes as a single primary
//! expression and cannot collide with user code; the `__witchy_` prefix is
//! reserved-synthetic). The tag PLACES these markers wherever a hole's value
//! belongs (an `html` text hole emits `glamour.text(__witchy_hole_0)`). After the
//! tag returns source and we parse it to an AST, `expand` walks that AST and
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
//! transfers its AST directly; compatibility `ExprSyntax` and legacy `String`
//! results retain the explicit source-parse fallback. Both runtime backends then
//! compile the same expanded AST. `Expr::TaggedLit` is therefore UNREACHABLE
//! after this pass; typeck, the interpreter, and both codegen backends panic on it.

use witchy_syntax::ast::{
    Block, CompilerBlockSyntax, CompilerExprSyntax, CompilerItemSyntax, CompilerPatternSyntax,
    CompilerStmtSyntax, CompilerTypeSyntax, Expr, Function, Item, MatchArm, Module, Param, Stmt,
    Type,
};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};

/// A tag may emit a tag (re-expansion); cap the nesting so a self-referential or
/// runaway tag fails loudly rather than looping.
const MAX_TAG_DEPTH: u32 = 64;

/// The opaque hole-marker prefix handed to a tag in place of each hole's source.
/// `__witchy_hole_N` lexes as a single primary expression (`Expr::Var`) and the
/// reserved `__witchy_` prefix cannot collide with user code, so after the tag
/// places it we can find each marker as a leaf `Var` and substitute the real hole.
const HOLE_MARKER_PREFIX: &str = "__witchy_hole_";

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
    let mut items = module.items.clone();
    let mut compiler_item_syntax = module.compiler_item_syntax.clone();
    let mut compiler_expr_syntax = module.compiler_expr_syntax.clone();
    let mut tag_origins = HashMap::new();
    record_tag_origins(&mut tag_origins, name, &module.items);
    let mut compiler_type_syntax = module.compiler_type_syntax.clone();
    let mut compiler_pattern_syntax = module.compiler_pattern_syntax.clone();
    let mut compiler_stmt_syntax = module.compiler_stmt_syntax.clone();
    let mut compiler_block_syntax = module.compiler_block_syntax.clone();
    let mut std_imports: Vec<String> = Vec::new();
    let mut std_from_imports: Vec<(String, Vec<String>)> = Vec::new();
    merge_std_from_imports(&mut std_from_imports, &module.from_imports);
    let mut seen = HashSet::new();
    seen.insert(name.to_string());
    let mut frontier: Vec<String> = module.imports.clone();
    while let Some(imp) = frontier.pop() {
        if witchy_syntax::linker::STD_MODULES.contains(&imp.as_str()) {
            if !std_imports.contains(&imp) {
                std_imports.push(imp);
            }
            continue;
        }
        if !seen.insert(imp.clone()) {
            continue;
        }
        if let Some(m) = by_name.get(imp.as_str()) {
            merge_std_from_imports(&mut std_from_imports, &m.from_imports);
            items.extend(m.items.iter().cloned());
            compiler_item_syntax.extend(m.compiler_item_syntax.iter().cloned());
            record_tag_origins(&mut tag_origins, &imp, &m.items);
            compiler_expr_syntax.extend(m.compiler_expr_syntax.iter().cloned());
            compiler_type_syntax.extend(m.compiler_type_syntax.iter().cloned());
            compiler_pattern_syntax.extend(m.compiler_pattern_syntax.iter().cloned());
            compiler_stmt_syntax.extend(m.compiler_stmt_syntax.iter().cloned());
            compiler_block_syntax.extend(m.compiler_block_syntax.iter().cloned());
            frontier.extend(m.imports.iter().cloned());
        }
    }
    // Module names that may appear as a QUALIFIER in a tag's generated source: the
    // tag emits hygienic, qualified constructors (`glamour.text(…)`), and that
    // source is parsed in a throwaway wrapper whose `imports` decide whether `m.f`
    // is a qualified call vs. a UFCS method call. Seed the consumer itself plus
    // every module it imports (transitively) so any qualifier the tag could emit
    // parses as a call. (`seen` already accumulated the consumer + visited
    // non-std imports; add the std imports too.)
    let mut qualifiers: Vec<String> = seen.into_iter().collect();
    for s in &std_imports {
        if !qualifiers.contains(s) {
            qualifiers.push(s.clone());
        }
    }
    let ctx = Context {
        name: name.to_string(),
        items,
        imports: std_imports,
        from_imports: std_from_imports,
        qualifiers,
        compiler_item_syntax,
        compiler_expr_syntax,
        tag_origins,
        compiler_type_syntax,
        compiler_pattern_syntax,
        compiler_stmt_syntax,
        compiler_block_syntax,
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
            Item::Const { value, .. } => walk_expr_depth(value, &ctx, 0)?,
            // `comptime:` blocks are already expanded (and consumed) by
            // `comptime::expand`, which runs before this pass.
            Item::Type(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    Ok(())
}

/// The per-module context a tag runs against.
struct Context {
    name: String,
    items: Vec<Item>,
    imports: Vec<String>,
    from_imports: Vec<(String, Vec<String>)>,
    /// Module names a tag's generated source may qualify against (the consumer +
    /// its transitive imports). Seeded as `import` lines in the throwaway parse so
    /// `glamour.text(…)` parses as a qualified call, not a method call.
    qualifiers: Vec<String>,
    compiler_item_syntax: Vec<CompilerItemSyntax>,
    compiler_expr_syntax: Vec<CompilerExprSyntax>,
    tag_origins: HashMap<String, String>,
    compiler_type_syntax: Vec<CompilerTypeSyntax>,
    compiler_pattern_syntax: Vec<CompilerPatternSyntax>,
    compiler_stmt_syntax: Vec<CompilerStmtSyntax>,
    compiler_block_syntax: Vec<CompilerBlockSyntax>,
    definition_modules: Vec<(String, Module)>,
    /// Stable traversal ordinal used to give each tagged-literal evaluator an
    /// independent RFC-0080 fresh-name namespace.
    fresh_invocation: Cell<u64>,
}

fn record_tag_origins(
    origins: &mut HashMap<String, String>,
    module: &str,
    items: &[Item],
) {
    for item in items {
        if let Item::Function(function) = item {
            origins
                .entry(function.name.clone())
                .or_insert_with(|| module.to_string());
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TagOutput {
    SourceString,
    ExprSyntax,
}

/// Replace `expr` with its expansion if it is (or contains) a `TaggedLit`,
/// recursing into every child. A spliced expression may itself contain a
/// `TaggedLit` (a tag emitting a tag), so we re-walk to a fixed point under a
/// depth cap.
fn walk_expr_depth(expr: &mut Expr, ctx: &Context, depth: u32) -> Result<(), String> {
    if let Expr::TaggedLit { tag, parts, holes, hole_spans, line } = expr {
        if depth >= MAX_TAG_DEPTH {
            return Err(format!(
                "module `{}`: tagged literal `{tag}` (line {line}) expanded past the \
                 depth limit ({MAX_TAG_DEPTH}) — a tag is emitting tags without terminating",
                ctx.name
            ));
        }
        let tag = std::mem::take(tag);
        let parts = std::mem::take(parts);
        let holes = std::mem::take(holes);
        let hole_spans = std::mem::take(hole_spans);
        let mut spliced = expand_one(ctx, &tag, &parts, &holes, &hole_spans)?;
        // The spliced source may itself contain a tagged literal — expand it too.
        // (Substitution has already replaced the markers with the real holes, so a
        // nested tag inside a hole is a normal `TaggedLit` this recursion handles.)
        walk_expr_depth(&mut spliced, ctx, depth + 1)?;
        *expr = spliced;
        return Ok(());
    }
    walk_children(expr, ctx, depth)
}

/// Recurse into an expression's children (it is not itself a `TaggedLit`).
fn walk_children(expr: &mut Expr, ctx: &Context, depth: u32) -> Result<(), String> {
    let recur = |e: &mut Expr| walk_expr_depth(e, ctx, depth);
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
        Expr::Lambda { body, .. } => walk_block_depth(body, ctx, depth)?,
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
        Expr::Binary { lhs, rhs, .. } => {
            recur(lhs)?;
            recur(rhs)?;
        }
        Expr::If { cond, then_block, else_block } => {
            recur(cond)?;
            walk_block_depth(then_block, ctx, depth)?;
            if let Some(b) = else_block {
                walk_block_depth(b, ctx, depth)?;
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
        Expr::Block(b) => walk_block_depth(b, ctx, depth)?,
        Expr::While { cond, body } => {
            recur(cond)?;
            walk_block_depth(body, ctx, depth)?;
        }
        Expr::For { iter, body, .. } => {
            recur(iter)?;
            walk_block_depth(body, ctx, depth)?;
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
            walk_block_depth(body, ctx, depth)?;
        }
        // Replaced by `walk_expr_depth` before reaching here.
        Expr::TaggedLit { .. } => unreachable!("TaggedLit handled by walk_expr_depth"),
    }
    Ok(())
}

fn walk_block(block: &mut Block, ctx: &Context) -> Result<(), String> {
    walk_block_depth(block, ctx, 0)
}

fn walk_block_depth(block: &mut Block, ctx: &Context, depth: u32) -> Result<(), String> {
    for stmt in &mut block.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => walk_expr_depth(value, ctx, depth)?,
            Stmt::Return(opt) => {
                if let Some(e) = opt {
                    walk_expr_depth(e, ctx, depth)?;
                }
            }
            Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn tag_output(ctx: &Context, tag: &str) -> TagOutput {
    for item in &ctx.items {
        let Item::Function(f) = item else {
            continue;
        };
        if f.name != tag {
            continue;
        }
        if f.ret.as_ref().is_some_and(is_expr_syntax_type) {
            return TagOutput::ExprSyntax;
        }
        return TagOutput::SourceString;
    }
    TagOutput::SourceString
}

fn is_expr_syntax_type(ty: &Type) -> bool {
    match ty {
        Type::Qualified(_, inner) => is_expr_syntax_type(inner),
        Type::Named(name, args) => {
            args.is_empty() && (name == "ExprSyntax" || name == "meta.ExprSyntax")
        }
        Type::Dyn(_, _) | Type::Tuple(_) | Type::Fn(_, _, _) => false,
    }
}

fn merge_std_from_imports(target: &mut Vec<(String, Vec<String>)>, imports: &[(String, Vec<String>)]) {
    for (module, names) in imports {
        if witchy_syntax::linker::STD_MODULES.contains(&module.as_str()) {
            merge_from_import(target, module, names);
        }
    }
}

fn merge_from_import(target: &mut Vec<(String, Vec<String>)>, module: &str, names: &[String]) {
    if let Some((_, existing)) = target.iter_mut().find(|(m, _)| m == module) {
        for name in names {
            if !existing.contains(name) {
                existing.push(name.clone());
            }
        }
    } else {
        target.push((module.to_string(), names.to_vec()));
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

/// Run one tag: build a synthetic comptime program calling `tag(parts, holes)`,
/// run it on the reference interpreter, and parse the emitted source as an
/// expression. Mirrors `comptime::expand`'s construction (the `emit` print
/// closure + std imports) but the program calls the tag rather than running a
/// user block. Legacy tags return `String`; RFC-0080 tags may return
/// `meta.ExprSyntax`, which the harness destructures internally before printing
/// the source payload.
fn expand_one(
    ctx: &Context,
    tag: &str,
    parts: &[String],
    holes: &[String],
    hole_spans: &[(u32, u32)],
) -> Result<Expr, String> {
    let where_ = || format!("module `{}`: tagged literal `{tag}`", ctx.name);
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
    let output = tag_output(ctx, tag);
    let tag_call = Expr::Call {
        name: tag.to_string(),
        args: vec![str_list(parts), str_list(&markers)],
    };

    // fn main(console: Console):
    //     let emit = fn(line): console.print(line)
    //     emit(<tag>([parts...], [markers...]))
    //
    // Typed RFC-0080 tags send their sealed value through a compiler-only
    // expression event. Compiler-owned syntax transfers its AST directly;
    // source-backed compatibility values are classified by the interpreter.
    let emit_closure = Stmt::Let {
        ty: None,
        name: "emit".into(),
        mutable: false,
        value: Expr::Lambda {
            params: vec![Param {
                name: "line".into(),
                ty: Some(Type::Named("String".into(), Vec::new())),
                convention: Default::default(),
                default: None,
            }],
            body: Block {
                stmts: vec![Stmt::Expr(Expr::MethodCall {
                    receiver: Box::new(Expr::Var("console".into())),
                    method: "print".into(),
                    args: vec![Expr::Var("line".into())],
                })],
                lines: vec![0],
                region: None,
            },
            ret: None,
        },
    };
    let emit_call = match output {
        TagOutput::SourceString => Stmt::Expr(Expr::Call {
            name: "emit".into(),
            args: vec![tag_call],
        }),
        TagOutput::ExprSyntax => Stmt::Expr(Expr::Call {
            name: witchy_syntax::intrinsics::COMPILER_EMIT_EXPR.into(),
            args: vec![tag_call],
        }),
    };
    let main = Function {
        public: false,
        comptime_only: false,
        name: "main".into(),
        params: vec![Param {
            name: "console".into(),
            ty: Some(Type::Named("Console".into(), Vec::new())),
            convention: Default::default(),
            default: None,
        }],
        ret: None,
        body: Block {
            stmts: vec![emit_closure, emit_call],
            lines: vec![0, 0],
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
    let keep = crate::reachability::reachable_from_function(&ctx.items, tag);
    let mut items: Vec<Item> = ctx
        .items
        .iter()
        .filter(|it| match it {
            Item::Function(f) => f.name != "main" && keep.contains(f.name.as_str()),
            Item::Type(t) => keep.contains(t.name.as_str()),
            // A `trait` declaration is small and a reachable bounded tag (`where a:
            // T`) needs it in scope; keeping all of them is harmless (no body to
            // reference a pruned type).
            Item::Trait(_) => true,
            // Everything else is DROPPED. An `impl` in particular must NOT be kept
            // blindly: `derive(Reflect)` on a CONSUMER type (e.g. `type Msg
            // derive(Reflect)`) lowers to an `impl Reflect for Msg` whose body
            // names `Msg`. The synthetic main only ever calls the tag, so that
            // impl is never used — and `Msg` is unreachable from the tag, so
            // keeping the impl would leave it dangling ("unknown type `Msg`").
            // Consts/aliases/comptime are likewise already inlined/expanded for the
            // real module and irrelevant to running the tag.
            _ => false,
        })
        .cloned()
        .collect();
    items.push(Item::Function(main));

    let mut from_imports = ctx.from_imports.clone();
    if output == TagOutput::ExprSyntax {
        ensure_from_imports(&mut from_imports, "meta", &["ExprSyntax"]);
    }
    let prog = Module {
        modes: Vec::new(),
        imports: ctx.imports.clone(),
        from_imports,
        items,
        import_lines: Vec::new(),
        item_lines: Vec::new(),
        compiler_item_syntax: ctx.compiler_item_syntax.clone(),
        compiler_expr_syntax: ctx.compiler_expr_syntax.clone(),
        compiler_type_syntax: ctx.compiler_type_syntax.clone(),
        compiler_pattern_syntax: ctx.compiler_pattern_syntax.clone(),
        compiler_stmt_syntax: ctx.compiler_stmt_syntax.clone(),
        compiler_block_syntax: ctx.compiler_block_syntax.clone(),
    };
    let linked = crate::pipeline::link(vec![("comptime".into(), prog)], "comptime")
        .map_err(|e| format!("{}: {e}", where_()))?;
    witchy_types::typeck::check_comptime(&linked).map_err(|e| format!("{}: {e}", where_()))?;
    let crate::interpreter::ComptimeOutputs {
        output: lines,
        items: item_output,
        exprs: mut expr_output,
    } = crate::interpreter::run_comptime_module_outputs_budgeted_in_scope(
        linked,
        ".",
        crate::interpreter::COMPTIME_STEP_LIMIT,
        Some(format!(
            "tag:{}:{}:{}:{tag}:{invocation}",
            ctx.name.len(),
            ctx.name,
            tag.len()
        )),
    )
    .map_err(|e| format!("{}: {e}", where_()))?;
    if !item_output.is_empty() {
        return Err(format!("{}: a tagged literal may emit one expression, not items", where_()));
    }
    let parse_source = |src: String| {
        // Source compatibility still parses with the tag module qualifiers so
        // `glamour.text(...)` remains a qualified call rather than UFCS.
        parse_generated_splice_expr(&src, &ctx.qualifiers).map_err(|error| {
            format!("{}: generated source: {error}\n--- generated ---\n{src}", where_())
        })
    };
    let mut e = match output {
        TagOutput::SourceString => {
            if !expr_output.is_empty() {
                return Err(format!(
                    "{}: a source-returning tag produced a typed expression event",
                    where_()
                ));
            }
            parse_source(lines.join("\n"))?
        }
        TagOutput::ExprSyntax => {
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
                    let definition_module = ctx
                        .tag_origins
                        .get(tag)
                        .ok_or_else(|| {
                            format!(
                                "{}: typed tag `{tag}` lost its definition module",
                                where_()
                            )
                        })?;
                    let mut expr = *expr;
                    witchy_syntax::linker::mark_definition_site_expr(
                        &mut expr,
                        definition_module,
                        &ctx.definition_modules,
                    )
                    .map_err(|error| format!("{}: {error}", where_()))?;
                    expr
                }
                crate::interpreter::ComptimeExprEmission::Source(source) => parse_source(source)?,
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
        hole_exprs.push(parse_hole(hole, span, &ctx.qualifiers, &where_)?);
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

fn parse_generated_splice_expr(src: &str, qualifiers: &[String]) -> Result<Expr, String> {
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
        // The generated source is plain witchy: a tag cannot emit a tagged literal
        // here (it returns STRING source; a nested `tag"…"` in that source is
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
