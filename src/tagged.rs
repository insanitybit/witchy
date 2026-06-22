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
    Block, Expr, Function, Item, MatchArm, Module, Param, Stmt, Type,
};

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
    // and writes `tag"…"` needs the tag — and the constructors the tag emits
    // (`element`/`text`/`prop`) — to resolve. So we also fold in the items of
    // every NON-std module this one imports, transitively, drawn from `siblings`
    // (the rest of the link set). std imports stay declared as `imports` (the
    // bundled std is a search path the comptime link resolves on its own).
    let by_name: std::collections::HashMap<&str, &Module> =
        siblings.iter().map(|(n, m)| (n.as_str(), m)).collect();
    let mut items = module.items.clone();
    let mut std_imports: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
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

    // The program carries the enclosing module's own items (so a locally defined
    // tag resolves) plus its std imports — but NOT any existing `main` (we supply
    // our own).
    let mut items: Vec<Item> = ctx
        .items
        .iter()
        .filter(|it| !matches!(it, Item::Function(f) if f.name == "main"))
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
