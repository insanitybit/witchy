//! `tag"…${expr}…"` — compile-time tagged literals (RFC-0006).
//!
//! A *tag* is an ordinary compile-time function
//! `fn <tag>(parts: List(String), holes: List(String)) -> String` that returns
//! witchy EXPRESSION SOURCE. A tagged literal `tag"a${x}b"` is expanded AT
//! COMPILE TIME — before type-checking.
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
//! This extends the existing `comptime` "source in, items out" model: the tag
//! runs once, in the compiler, on the reference interpreter, and both backends
//! then compile the same expanded AST — so parity is free, exactly like
//! `comptime`/`derive`. `Expr::TaggedLit` is therefore UNREACHABLE after this
//! pass; typeck, the interpreter, and both codegen backends panic on it.

use witchy_syntax::ast::{
    collect_type_names, Block, Expr, Function, Item, MatchArm, Module, Param, Pattern, Stmt, Type,
};
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
    // reaches (`reachable_from_tag`) before linking — which is what keeps the
    // comptime program free of the consumer's own tagged literals (else linking
    // it would re-enter this pass forever; see linker.rs and `expand_one`).
    let by_name: HashMap<&str, &Module> =
        siblings.iter().map(|(n, m)| (n.as_str(), m)).collect();
    let mut items = module.items.clone();
    let mut std_imports: Vec<String> = Vec::new();
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
            items.extend(m.items.iter().cloned());
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
        qualifiers,
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
    /// Module names a tag's generated source may qualify against (the consumer +
    /// its transitive imports). Seeded as `import` lines in the throwaway parse so
    /// `glamour.text(…)` parses as a qualified call, not a method call.
    qualifiers: Vec<String>,
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
        Expr::Call { args, .. } | Expr::Ctor { args, .. } => {
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
        Expr::RecordUpdate { base, fields } => {
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

/// Run one tag: build a synthetic comptime program calling `tag(parts, holes)`,
/// run it on the reference interpreter, and parse the emitted source as an
/// expression. Mirrors `comptime::expand`'s construction (the `emit` print
/// closure + std imports) but the program calls the tag rather than running a
/// user block.
fn expand_one(
    ctx: &Context,
    tag: &str,
    parts: &[String],
    holes: &[String],
    hole_spans: &[(u32, u32)],
) -> Result<Expr, String> {
    let where_ = || format!("module `{}`: tagged literal `{tag}`", ctx.name);
    let str_list = |xs: &[String]| Expr::List(xs.iter().map(|x| Expr::Str(x.clone())).collect());

    // The tag receives an OPAQUE MARKER per hole, NOT the hole's source: it places
    // `__witchy_hole_N` wherever the hole's value belongs, and we substitute the
    // real (already-parsed, position-stamped) hole expression at each marker after
    // the tag returns. This is the hygiene split (RFC-0006): the tag never sees —
    // and so cannot mangle or capture — the author's hole expression.
    let markers: Vec<String> = (0..holes.len()).map(hole_marker).collect();

    // fn main(console: Console):
    //     let emit = fn(line): print(console, line)
    //     emit(<tag>([parts...], [holes...]))
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
                stmts: vec![Stmt::Expr(Expr::Call {
                    name: "print".into(),
                    args: vec![Expr::Var("console".into()), Expr::Var("line".into())],
                })],
                lines: vec![0],
                region: None,
            },
            ret: None,
        },
    };
    let emit_call = Stmt::Expr(Expr::Call {
        name: "emit".into(),
        args: vec![Expr::Call {
            name: tag.to_string(),
            args: vec![str_list(parts), str_list(&markers)],
        }],
    });
    let main = Function {
        public: false,
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
    let keep = reachable_from_tag(&ctx.items, tag);
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

    let prog = Module {
        modes: Vec::new(),
        imports: ctx.imports.clone(),
        from_imports: Vec::new(),
        items,
        import_lines: Vec::new(),
        item_lines: Vec::new(),
    };
    let linked = crate::pipeline::link(vec![("comptime".into(), prog)], "comptime")
        .map_err(|e| format!("{}: {e}", where_()))?;
    witchy_types::typeck::check(&linked).map_err(|e| format!("{}: {e}", where_()))?;
    let lines = crate::interpreter::run_module_budgeted(
        linked,
        ".",
        crate::interpreter::COMPTIME_STEP_LIMIT,
    )
    .map_err(|e| format!("{}: {e}", where_()))?;
    let src = lines.join("\n");

    // Parse the generated source as an expression. The tag emits QUALIFIED
    // constructors (`glamour.text(…)`), so the throwaway parse must know the
    // qualifier module names — else `glamour.text(…)` parses as a UFCS method call.
    let mut e = parse_splice_expr(&src, &ctx.qualifiers)
        .map_err(|e| format!("{}: generated source: {e}\n--- generated ---\n{src}", where_()))?;

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
    wrapped.push_str(&src.replace('\n', "\n    "));
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
        Expr::Call { args, .. } | Expr::Ctor { args, .. } => {
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
        Expr::RecordUpdate { base, fields } => {
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

/// The names of every item (function OR type) REACHABLE from the tag function
/// `root`, transitively. A comptime program built from exactly this set still
/// type-checks (every type a kept function's signature/variants names is kept)
/// AND contains no tagged literals (the consumer's own tag-bearing functions —
/// `view`/`update`/`main` — are unreachable from the tag and so are dropped),
/// which is what breaks the `tagged::expand` → `link` → `tagged::expand`
/// recursion. Names are resolved against three lookups: functions by name, types
/// by name, and constructor → owning-type (so a `Ctor` use / pattern pulls in its
/// type). Unknown names (builtins, std functions resolved by the link's search
/// path, parameter/local binders) are simply ignored.
fn reachable_from_tag(items: &[Item], root: &str) -> HashSet<String> {
    let mut fns: HashMap<&str, &Function> = HashMap::new();
    let mut types: HashMap<&str, &witchy_syntax::ast::TypeDef> = HashMap::new();
    let mut ctor_owner: HashMap<&str, &str> = HashMap::new();
    for item in items {
        match item {
            Item::Function(f) => {
                fns.insert(f.name.as_str(), f);
            }
            Item::Type(t) => {
                types.insert(t.name.as_str(), t);
                for v in &t.variants {
                    ctor_owner.insert(v.name.as_str(), t.name.as_str());
                }
            }
            _ => {}
        }
    }

    let mut keep: HashSet<String> = HashSet::new();
    let mut work: Vec<String> = vec![root.to_string()];
    keep.insert(root.to_string());
    while let Some(name) = work.pop() {
        // A reachable function: pull in the names its body references and the
        // types its signature names.
        if let Some(f) = fns.get(name.as_str()) {
            let mut names: HashSet<String> = HashSet::new();
            collect_refs_block(&f.body, &mut names);
            for p in &f.params {
                if let Some(t) = &p.ty {
                    collect_type_names(t, &mut names);
                }
            }
            if let Some(t) = &f.ret {
                collect_type_names(t, &mut names);
            }
            for r in names {
                push_ref(&r, &fns, &types, &ctor_owner, &mut keep, &mut work);
            }
        }
        // A reachable type: pull in the types its variants' fields name.
        if let Some(t) = types.get(name.as_str()) {
            let mut names: HashSet<String> = HashSet::new();
            for v in &t.variants {
                for field in &v.fields {
                    collect_type_names(field, &mut names);
                }
            }
            for r in names {
                push_ref(&r, &fns, &types, &ctor_owner, &mut keep, &mut work);
            }
        }
    }
    keep
}

/// Resolve a referenced NAME against the item lookups and enqueue whatever it
/// designates: a function, a type, or a constructor (→ its owning type).
fn push_ref(
    name: &str,
    fns: &HashMap<&str, &Function>,
    types: &HashMap<&str, &witchy_syntax::ast::TypeDef>,
    ctor_owner: &HashMap<&str, &str>,
    keep: &mut HashSet<String>,
    work: &mut Vec<String>,
) {
    let mut enqueue = |n: &str| {
        if keep.insert(n.to_string()) {
            work.push(n.to_string());
        }
    };
    if fns.contains_key(name) {
        enqueue(name);
    }
    if types.contains_key(name) {
        enqueue(name);
    }
    if let Some(owner) = ctor_owner.get(name) {
        enqueue(owner);
    }
}

/// Collect every NAME a block references — callees, variable/constructor names,
/// constructor names appearing in patterns, and types named in `as`/closure
/// annotations. A superset of the true call graph is fine: `push_ref` discards
/// any name that designates nothing in the item set.
fn collect_refs_block(b: &Block, out: &mut HashSet<String>) {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { ty, value, .. } => {
                if let Some(t) = ty {
                    collect_type_names(t, out);
                }
                collect_refs_expr(value, out);
            }
            Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } => {
                collect_refs_expr(value, out)
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => collect_refs_expr(e, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_refs_pattern(p: &Pattern, out: &mut HashSet<String>) {
    match p {
        Pattern::Ctor { name, args } => {
            out.insert(name.clone());
            for a in args {
                collect_refs_pattern(a, out);
            }
        }
        Pattern::Tuple(args) | Pattern::List { elems: args, .. } | Pattern::Or(args) => {
            for a in args {
                collect_refs_pattern(a, out);
            }
        }
        Pattern::Wildcard
        | Pattern::Var(_)
        | Pattern::Int(_)
        | Pattern::Str(_)
        | Pattern::Bool(_)
        | Pattern::Duration(_)
        | Pattern::IntRange { .. } => {}
    }
}

fn collect_refs_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Call { name, args } | Expr::Ctor { name, args } => {
            out.insert(name.clone());
            for a in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::LabeledCall { name, args } => {
            out.insert(name.clone());
            for (_, a) in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::Var(name) => {
            out.insert(name.clone());
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_refs_expr(receiver, out);
            for a in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::Apply { func, args } => {
            collect_refs_expr(func, out);
            for a in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                collect_refs_expr(x, out);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::Field { base: expr, .. } => {
            collect_refs_expr(expr, out)
        }
        Expr::As { expr, ty } => {
            collect_refs_expr(expr, out);
            collect_type_names(ty, out);
        }
        Expr::RecordUpdate { base, fields } => {
            collect_refs_expr(base, out);
            for (_, v) in fields {
                collect_refs_expr(v, out);
            }
        }
        Expr::Record { name, fields, spread } => {
            out.insert(name.clone());
            for (_, v) in fields {
                collect_refs_expr(v, out);
            }
            if let Some(s) = spread {
                collect_refs_expr(s, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_refs_expr(lhs, out);
            collect_refs_expr(rhs, out);
        }
        Expr::If { cond, then_block, else_block } => {
            collect_refs_expr(cond, out);
            collect_refs_block(then_block, out);
            if let Some(b) = else_block {
                collect_refs_block(b, out);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_refs_expr(scrutinee, out);
            for arm in arms {
                collect_refs_pattern(&arm.pattern, out);
                if let Some(g) = &arm.guard {
                    collect_refs_expr(g, out);
                }
                collect_refs_expr(&arm.body, out);
            }
        }
        Expr::While { cond, body } => {
            collect_refs_expr(cond, out);
            collect_refs_block(body, out);
        }
        Expr::For { iter, body, .. } => {
            collect_refs_expr(iter, out);
            collect_refs_block(body, out);
        }
        Expr::Range { lo, hi, .. } => {
            collect_refs_expr(lo, out);
            collect_refs_expr(hi, out);
        }
        Expr::Index { base, index } => {
            collect_refs_expr(base, out);
            collect_refs_expr(index, out);
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            collect_refs_pattern(pattern, out);
            collect_refs_expr(scrutinee, out);
            collect_refs_block(body, out);
        }
        Expr::Lambda { params, body, ret } => {
            for p in params {
                if let Some(t) = &p.ty {
                    collect_type_names(t, out);
                }
            }
            if let Some(t) = ret {
                collect_type_names(t, out);
            }
            collect_refs_block(body, out);
        }
        Expr::Block(b) => collect_refs_block(b, out),
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
    }
}
