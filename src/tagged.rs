//! `tag"…${expr}…"` — compile-time tagged literals (RFC-0006).
//!
//! A *tag* is an ordinary compile-time function
//! `fn <tag>(parts: List(String), holes: List(String)) -> String` that returns
//! witchy EXPRESSION SOURCE. A tagged literal `tag"a${x}b"` is expanded AT
//! COMPILE TIME — before type-checking — by calling `tag(["a", "b"], ["x"])`
//! (the static fragments and each hole's SOURCE TEXT, as strings), parsing the
//! returned string as an expression, and SPLICING it in place of the literal.
//!
//! This extends the existing `comptime` "source in, items out" model: the tag
//! runs once, in the compiler, on the reference interpreter, and both backends
//! then compile the same expanded AST — so parity is free, exactly like
//! `comptime`/`derive`. `Expr::TaggedLit` is therefore UNREACHABLE after this
//! pass; typeck, the interpreter, and both codegen backends panic on it.

use crate::ast::{
    Block, Expr, Function, Item, MatchArm, Module, Param, Pattern, Stmt, Type,
};
use std::collections::{HashMap, HashSet};

/// A tag may emit a tag (re-expansion); cap the nesting so a self-referential or
/// runaway tag fails loudly rather than looping.
const MAX_TAG_DEPTH: u32 = 64;

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
        if crate::linker::STD_MODULES.contains(&imp.as_str()) {
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
    let ctx = Context {
        name: name.to_string(),
        items,
        imports: std_imports,
    };
    for item in &mut module.items {
        if let Item::Function(f) = item {
            walk_block(&mut f.body, &ctx)?;
        }
    }
    Ok(())
}

/// The per-module context a tag runs against.
struct Context {
    name: String,
    items: Vec<Item>,
    imports: Vec<String>,
}

/// Replace `expr` with its expansion if it is (or contains) a `TaggedLit`,
/// recursing into every child. A spliced expression may itself contain a
/// `TaggedLit` (a tag emitting a tag), so we re-walk to a fixed point under a
/// depth cap.
fn walk_expr_depth(expr: &mut Expr, ctx: &Context, depth: u32) -> Result<(), String> {
    if let Expr::TaggedLit { tag, parts, holes, line } = expr {
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
        let mut spliced = expand_one(ctx, &tag, &parts, &holes)?;
        // The spliced source may itself contain a tagged literal — expand it too.
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
            | Stmt::LetTuple { value, .. }
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
) -> Result<Expr, String> {
    let where_ = || format!("module `{}`: tagged literal `{tag}`", ctx.name);
    let str_list = |xs: &[String]| Expr::List(xs.iter().map(|x| Expr::Str(x.clone())).collect());

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
            }],
            body: Block {
                stmts: vec![Stmt::Expr(Expr::Call {
                    name: "print".into(),
                    args: vec![Expr::Var("console".into()), Expr::Var("line".into())],
                })],
                lines: vec![0],
                restrict: None,
                region: None,
            },
            ret: None,
        },
    };
    let emit_call = Stmt::Expr(Expr::Call {
        name: "emit".into(),
        args: vec![Expr::Call {
            name: tag.to_string(),
            args: vec![str_list(parts), str_list(holes)],
        }],
    });
    let main = Function {
        public: false,
        name: "main".into(),
        params: vec![Param {
            name: "console".into(),
            ty: Some(Type::Named("Console".into(), Vec::new())),
            convention: Default::default(),
        }],
        ret: None,
        body: Block {
            stmts: vec![emit_closure, emit_call],
            lines: vec![0, 0],
            restrict: None,
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
        items,
        import_lines: Vec::new(),
        item_lines: Vec::new(),
    };
    let linked = crate::linker::link(vec![("comptime".into(), prog)], "comptime")
        .map_err(|e| format!("{}: {e}", where_()))?;
    crate::typeck::check(&linked).map_err(|e| format!("{}: {e}", where_()))?;
    let lines = crate::interpreter::run_module_budgeted(
        linked,
        ".",
        crate::interpreter::COMPTIME_STEP_LIMIT,
    )
    .map_err(|e| format!("{}: {e}", where_()))?;
    let src = lines.join("\n");

    // Parse the generated source as an expression by wrapping it as the tail
    // expression of a throwaway function (only `parse_module` exists). The 4-space
    // indent satisfies the off-side layout.
    let wrapped = format!("fn __tagsplice():\n    {}\n", src.replace('\n', "\n    "));
    let parsed = crate::parser::parse_module(&wrapped).map_err(|e| {
        format!(
            "{}: generated source does not parse as an expression: {e}\n--- generated ---\n{src}",
            where_()
        )
    })?;
    let Some(Item::Function(f)) = parsed.items.into_iter().find(|it| {
        matches!(it, Item::Function(f) if f.name == "__tagsplice")
    }) else {
        return Err(format!(
            "{}: generated source did not yield an expression\n--- generated ---\n{src}",
            where_()
        ));
    };
    let Some(Stmt::Expr(e)) = f.body.stmts.into_iter().next_back() else {
        return Err(format!(
            "{}: generated source is not a single expression\n--- generated ---\n{src}",
            where_()
        ));
    };
    Ok(e)
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
    let mut types: HashMap<&str, &crate::ast::TypeDef> = HashMap::new();
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
    types: &HashMap<&str, &crate::ast::TypeDef>,
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

/// Collect every type NAME mentioned in a type expression (heads of `Named`,
/// recursively through its arguments, tuples, and function types). Type
/// *variables* (lowercase, e.g. `msg`) are collected too but harmlessly miss the
/// type lookup.
fn collect_type_names(t: &Type, out: &mut HashSet<String>) {
    match t {
        Type::Named(name, args) => {
            out.insert(name.clone());
            for a in args {
                collect_type_names(a, out);
            }
        }
        Type::Tuple(elems) => {
            for e in elems {
                collect_type_names(e, out);
            }
        }
        Type::Fn(params, ret) => {
            for p in params {
                collect_type_names(p, out);
            }
            collect_type_names(ret, out);
        }
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
            Stmt::Assign { value, .. } | Stmt::LetTuple { value, .. } => {
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
        Pattern::Tuple(args) | Pattern::List { elems: args, .. } => {
            for a in args {
                collect_refs_pattern(a, out);
            }
        }
        Pattern::Wildcard | Pattern::Var(_) | Pattern::Int(_) | Pattern::Str(_) | Pattern::Bool(_) => {}
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
