//! Off-side (indentation) pretty-printer for witchy. Renders a parsed `Module`
//! as canonical brace-free source — the backend of `witchy fmt` and of the
//! one-time migration that took the test corpus off braces.
//!
//! Correctness over beauty: any expression whose re-parse could regroup is fully
//! parenthesized, and constructs that have no inline brace-free form (e.g.
//! `match`) are always emitted multi-line. The migration driver only keeps a
//! reformatting when it round-trips to the same AST, so this printer never has to
//! be perfect — only sound where it fires.

use crate::ast::*;
// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};

const IND: &str = "    ";

/// A cursor over the source's own-line comments, emitted back in order as the
/// printer reaches each anchor line so `witchy fmt` preserves them.
struct Comments<'a> {
    list: &'a [(u32, u32, String)],
    cursor: usize,
    trailing: HashMap<u32, Vec<String>>,
}

impl Comments<'_> {
    /// Emit every comment whose source line is before `line`, indented to `depth`.
    /// A blank line the author left between two comments is preserved (so comment
    /// paragraphs stay separated).
    fn before(&mut self, s: &mut String, depth: usize, line: u32) {
        let mut prev: Option<u32> = None;
        while self.cursor < self.list.len() && self.list[self.cursor].0 < line {
            let cl = self.list[self.cursor].0;
            if prev.is_some_and(|p| cl - p > 1) {
                s.push('\n');
            }
            pad(s, depth);
            s.push_str(&self.list[self.cursor].2);
            s.push('\n');
            prev = Some(cl);
            self.cursor += 1;
        }
    }

    /// Flush the comments that belong INSIDE a construct's body (an enum's
    /// variant list, a `match`'s arms): those before `line` that are indented
    /// deeper than the construct's own column `header_col`. A next-item comment
    /// sitting at the header column is left for the enclosing flush, so it is not
    /// pulled into the body (BUG-332). Emitted at `depth`.
    fn before_body(&mut self, s: &mut String, depth: usize, header_col: u32, line: u32) {
        let mut prev: Option<u32> = None;
        while self.cursor < self.list.len() {
            let (cl, cc, _) = &self.list[self.cursor];
            if *cl >= line || *cc <= header_col {
                break;
            }
            if prev.is_some_and(|p| cl - p > 1) {
                s.push('\n');
            }
            pad(s, depth);
            s.push_str(&self.list[self.cursor].2);
            s.push('\n');
            prev = Some(*cl);
            self.cursor += 1;
        }
    }

    fn remaining(&self) -> bool {
        self.cursor < self.list.len()
    }

    fn trailing_remaining(&self) -> bool {
        !self.trailing.is_empty()
    }

    fn append_trailing(&mut self, s: &mut String, line: u32) {
        let Some(comments) = self.trailing.remove(&line) else {
            return;
        };
        if s.ends_with('\n') {
            s.pop();
        }
        for comment in comments {
            s.push(' ');
            s.push_str(&comment);
        }
        s.push('\n');
    }

    /// How many own-line comments fall strictly between source lines `lo` and
    /// `hi`. Used to tell an author's blank line apart from a comment in the gap
    /// between two statements.
    fn count_between(&self, lo: u32, hi: u32) -> usize {
        self.list.iter().filter(|(l, _, _)| *l > lo && *l < hi).count()
    }
}

/// The greatest source line touched by a statement, accounting for the nested
/// blocks of control-flow forms — so a blank *after* a multi-line statement is
/// told apart from blank-looking gaps *within* it. `default` is the statement's
/// own line (used when it spans none of its own nested blocks).
fn stmt_max_line(st: &Stmt, default: u32) -> u32 {
    match st {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Return(Some(value))
        | Stmt::Expr(value) => expr_max_line(value, default),
        _ => default,
    }
}

fn expr_max_line(e: &Expr, default: u32) -> u32 {
    match e {
        Expr::If { cond, then_block, else_block } => {
            let mut m = expr_max_line(cond, default).max(block_max_line(then_block, default));
            if let Some(b) = else_block {
                m = m.max(block_max_line(b, default));
            }
            m
        }
        Expr::While { cond, body } => {
            expr_max_line(cond, default).max(block_max_line(body, default))
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            expr_max_line(scrutinee, default).max(block_max_line(body, default))
        }
        Expr::For { iter, body, .. } => {
            expr_max_line(iter, default).max(block_max_line(body, default))
        }
        Expr::Match { scrutinee, arms } => {
            let mut m = expr_max_line(scrutinee, default);
            for a in arms {
                m = m.max(expr_max_line(&a.body, default));
            }
            m
        }
        Expr::Block(b) => block_max_line(b, default),
        Expr::Lambda { body, .. } => block_max_line(body, default),
        // A fluent chain that `chain_wrap` breaks occupies one extra line per call
        // below the head, so the following statement is not given a phantom blank.
        Expr::MethodCall { .. } if chain_should_wrap(e) => default + method_chain_len(e) as u32,
        _ => default,
    }
}

fn block_max_line(b: &Block, default: u32) -> u32 {
    let mut m = default;
    for (i, st) in b.stmts.iter().enumerate() {
        let line = b.lines.get(i).copied().unwrap_or(default);
        m = m.max(line).max(stmt_max_line(st, line));
    }
    m
}

pub fn module(m: &Module, comments: &[(u32, u32, String)]) -> String {
    render_module(m, comments, &[]).0
}

fn module_with_trailing(
    m: &Module,
    comments: &[(u32, u32, String)],
    trailing_comments: &[(u32, u32, String)],
) -> Option<String> {
    let (out, dropped_trailing) = render_module(m, comments, trailing_comments);
    (!dropped_trailing).then_some(out)
}

fn render_module(
    m: &Module,
    comments: &[(u32, u32, String)],
    trailing_comments: &[(u32, u32, String)],
) -> (String, bool) {
    let mut s = String::new();
    // Comment placement needs source lines parallel to imports and items; without
    // them (e.g. a linked module) fall back to emitting no comments.
    let have_lines =
        m.import_lines.len() == m.imports.len() && m.item_lines.len() == m.items.len();
    let mut trailing = HashMap::new();
    if have_lines {
        for (line, _, text) in trailing_comments {
            trailing.entry(*line).or_insert_with(Vec::new).push(text.clone());
        }
    }
    let mut c = Comments {
        list: if have_lines { comments } else { &[] },
        cursor: 0,
        trailing,
    };

    // The performance mode `mode opt` leads the file.
    // The following block (imports or the first item) supplies the blank-line
    // separator, so we emit no trailing blank here.
    for mode in &m.modes {
        s.push_str("mode ");
        s.push_str(mode);
        s.push('\n');
    }

    if !m.imports.is_empty() || !m.from_imports.is_empty() {
        // The comments before the first import are the file header.
        c.before(&mut s, 0, m.import_lines.first().copied().unwrap_or(u32::MAX));
        if !s.is_empty() {
            s.push('\n');
        }
        // A module that appears in a `from X import …` is rendered as that line,
        // not a bare `import X` (the `from` form implies the plain import).
        let from_mods: HashSet<&str> =
            m.from_imports.iter().map(|(x, _)| x.as_str()).collect();
        let mut first = true;
        for (i, imp) in m.imports.iter().enumerate() {
            if from_mods.contains(imp.as_str()) {
                continue;
            }
            // A comment sitting between two imports stays with the import below
            // it (rather than being relocated past the whole import block).
            if !first {
                c.before(&mut s, 0, m.import_lines.get(i).copied().unwrap_or(u32::MAX));
            }
            first = false;
            s.push_str("import ");
            s.push_str(imp);
            s.push('\n');
        }
        // (RFC-0042) `from X import Y, Z` — rendered after the plain imports.
        for (module, names) in &m.from_imports {
            s.push_str("from ");
            s.push_str(module);
            s.push_str(" import ");
            s.push_str(&names.join(", "));
            s.push('\n');
        }
    }

    for (idx, item) in m.items.iter().enumerate() {
        // The synthetic record behind each `.{…}` is regenerated from the literal on
        // re-parse, so it must not be printed (else fmt would duplicate it).
        if matches!(item, Item::Type(t) if t.name.starts_with("__anon")) {
            continue;
        }
        if !s.is_empty() {
            s.push('\n');
        }
        c.before(&mut s, 0, m.item_lines.get(idx).copied().unwrap_or(u32::MAX));
        // The next item's line bounds this item's body, so a comment inside an
        // enum's variant list is flushed within the type (not relocated to the
        // next item) while a next-item comment stays put (BUG-332).
        let next = m.item_lines.get(idx + 1).copied().unwrap_or(u32::MAX);
        item_str(&mut s, item, &mut c, next);
    }

    // Comments after the last item.
    if c.remaining() && !s.is_empty() {
        s.push('\n');
    }
    c.before(&mut s, 0, u32::MAX);
    let dropped_trailing = c.trailing_remaining();
    (s, dropped_trailing)
}

fn pad(s: &mut String, depth: usize) {
    for _ in 0..depth {
        s.push_str(IND);
    }
}

fn item_str(s: &mut String, item: &Item, c: &mut Comments, next_item_line: u32) {
    match item {
        Item::Function(f) => function(s, f, false, c, next_item_line),
        Item::Type(t) => type_def(s, t, c, next_item_line),
        Item::Trait(t) => trait_def(s, t, c, next_item_line),
        Item::Impl(im) => impl_def(s, im, c, next_item_line),
        Item::Const { name, value } => {
            s.push_str("let ");
            s.push_str(name);
            s.push_str(" = ");
            s.push_str(&expr(value));
            s.push('\n');
        }
        Item::TypeAlias { name, params, ty } => {
            s.push_str("type ");
            s.push_str(name);
            if !params.is_empty() {
                s.push('(');
                s.push_str(&params.join(", "));
                s.push(')');
            }
            s.push_str(" = ");
            s.push_str(&type_str(ty));
            s.push('\n');
        }
        Item::Comptime(b) => {
            s.push_str("comptime:\n");
            block(s, b, 1, c, next_item_line);
        }
    }
}

fn sig(name: &str, params: &[Param], ret: &Option<Type>, bounds: &[(String, String, Vec<Type>)]) -> String {
    // `impl Trait` params are stored desugared (a fresh `impltrait_N` type var
    // plus a bound). Render them back to `impl Trait` and omit those bounds from
    // the `where` clause, so the surface syntax round-trips through `witchy fmt`.
    let is_impl_var = |v: &str| v.starts_with("impltrait_");
    let mut h = String::new();
    h.push_str(name);
    h.push('(');
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            h.push_str(", ");
        }
        match &p.ty {
            Some(Type::Named(v, args)) if args.is_empty() && is_impl_var(v) => {
                let trait_name = bounds
                    .iter()
                    .find(|(bv, _, _)| bv == v)
                    .map(|(_, t, _)| t.as_str())
                    .unwrap_or("?");
                h.push_str(&format!("{}: impl {trait_name}", p.name));
            }
            _ => h.push_str(&param(p)),
        }
    }
    h.push(')');
    if let Some(r) = ret {
        h.push_str(" -> ");
        h.push_str(&type_str(r));
    }
    let visible: Vec<&(String, String, Vec<Type>)> =
        bounds.iter().filter(|(v, _, _)| !is_impl_var(v)).collect();
    if !visible.is_empty() {
        h.push_str(" where ");
        for (i, (v, t, ta)) in visible.iter().enumerate() {
            if i > 0 {
                h.push_str(", ");
            }
            h.push_str(v);
            h.push_str(": ");
            h.push_str(t);
            if !ta.is_empty() {
                let rendered: Vec<String> = ta.iter().map(type_str).collect();
                h.push_str(&format!("({})", rendered.join(", ")));
            }
        }
    }
    h
}

fn param(p: &Param) -> String {
    // `var` (mutate + writeback) / `own` (consume) are the parameter
    // conventions; an explicit borrow prints `let`.
    let conv = match p.convention {
        Convention::Let => "",
        Convention::Borrow => "let ",
        Convention::Var => "var ",
        Convention::Own => "own ",
    };
    let base = match &p.ty {
        Some(t) => format!("{conv}{}: {}", p.name, type_str(t)),
        None => format!("{conv}{}", p.name),
    };
    // (RFC-0056) A closed-constant default renders back as `= <const>` so a
    // defaulted parameter round-trips through `witchy fmt` (BUG-206).
    match &p.default {
        Some(d) => format!("{base} = {}", expr(d)),
        None => base,
    }
}

fn function(s: &mut String, f: &Function, indented: bool, c: &mut Comments, upper: u32) {
    let depth = if indented { 1 } else { 0 };
    pad(s, depth);
    if f.public {
        s.push_str("pub ");
    }
    if f.is_async {
        s.push_str("async ");
    }
    if f.is_gen {
        s.push_str("gen ");
    }
    s.push_str("fn ");
    s.push_str(&sig(&f.name, &f.params, &f.ret, &f.bounds));
    s.push_str(":\n");
    block(s, &f.body, depth + 1, c, upper);
}

fn type_def(s: &mut String, t: &TypeDef, c: &mut Comments, upper: u32) {
    // A top-level `type`/`capability` header sits at column 1; a comment in its
    // variant/field body is indented past that (BUG-332). Flushed after the
    // header, below.
    const HEADER_COL: u32 = 1;
    if t.sealed && t.is_capability {
        if t.grantable {
            s.push_str("grantable ");
        }
        s.push_str("capability ");
        s.push_str(&t.name);
        let v = &t.variants[0];
        // RFC-0011 record form (`capability X:` with named fields, carried state).
        if !v.field_names.is_empty() {
            s.push_str(":\n");
            for (idx, (n, ty)) in v.field_names.iter().zip(&v.fields).enumerate() {
                c.before_body(s, 1, HEADER_COL, v.field_lines.get(idx).copied().unwrap_or(v.line));
                pad(s, 1);
                s.push_str(n);
                s.push_str(": ");
                s.push_str(&type_str(ty));
                s.push('\n');
            }
            c.before_body(s, 1, HEADER_COL, upper);
            return;
        }
        // RFC-0002 `capability X from U` — a sealed brand. Its single variant's
        // field types ARE the underlying capabilities it refines.
        s.push_str(" from ");
        let fields = &v.fields;
        if fields.len() == 1 {
            s.push_str(&type_str(&fields[0]));
        } else {
            s.push('(');
            for (i, ty) in fields.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&type_str(ty));
            }
            s.push(')');
        }
        s.push('\n');
        return;
    }
    // `sealed type X …` (RFC-0065): a sealed general type renders through the
    // ordinary type path with a leading `sealed ` (a capability took the branch above).
    if t.sealed {
        s.push_str("sealed ");
    }
    s.push_str("type ");
    s.push_str(&t.name);
    if !t.params.is_empty() {
        s.push('(');
        s.push_str(&t.params.join(", "));
        s.push(')');
    }
    // (RFC-0027) the `packed` modifier, before `derive` (matching the parser order).
    if t.packed {
        s.push_str(" packed");
    }
    if !t.derives.is_empty() {
        s.push_str(" derive(");
        s.push_str(&t.derives.join(", "));
        s.push(')');
    }
    s.push_str(":\n");
    let is_record = t.variants.len() == 1 && !t.variants[0].field_names.is_empty();
    if is_record {
        let v = &t.variants[0];
        for (idx, (n, ty)) in v.field_names.iter().zip(&v.fields).enumerate() {
            c.before_body(s, 1, HEADER_COL, v.field_lines.get(idx).copied().unwrap_or(v.line));
            pad(s, 1);
            s.push_str(n);
            s.push_str(": ");
            s.push_str(&type_str(ty));
            s.push('\n');
        }
        c.before_body(s, 1, HEADER_COL, upper);
    } else {
        for v in &t.variants {
            c.before_body(s, 1, HEADER_COL, v.line);
            pad(s, 1);
            s.push_str(&v.name);
            if !v.fields.is_empty() {
                s.push('(');
                for (i, ty) in v.fields.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&type_str(ty));
                }
                s.push(')');
            }
            s.push('\n');
        }
        c.before_body(s, 1, HEADER_COL, upper);
    }
}

fn trait_def(s: &mut String, t: &TraitDef, c: &mut Comments, upper: u32) {
    s.push_str("trait ");
    s.push_str(&t.name);
    if !t.typarams.is_empty() {
        s.push_str(&format!("({})", t.typarams.join(", ")));
    }
    if !t.supertraits.is_empty() {
        s.push_str(": ");
        s.push_str(&t.supertraits.join(" + "));
    }
    // A marker trait (no methods) opens no block.
    if t.methods.is_empty() {
        s.push('\n');
        return;
    }
    s.push_str(":\n");
    for m in &t.methods {
        pad(s, 1);
        s.push_str("fn ");
        s.push_str(&sig(&m.name, &m.params, &m.ret, &[]));
        match &m.default {
            Some(b) => {
                s.push_str(":\n");
                block(s, b, 2, c, upper);
            }
            None => s.push('\n'),
        }
    }
}

fn impl_def(s: &mut String, im: &ImplDef, c: &mut Comments, upper: u32) {
    s.push_str("impl ");
    if let Some(t) = &im.trait_name {
        s.push_str(t);
        if !im.trait_args.is_empty() {
            let rendered: Vec<String> = im.trait_args.iter().map(type_str).collect();
            s.push_str(&format!("({})", rendered.join(", ")));
        }
        s.push_str(" for ");
    }
    // The target, with its type arguments: `List(a)`, `Box(a, b)`, or a tuple
    // `(a, b)` (whose head is the synthetic `Tuple{N}`, printed back as a tuple).
    if let Some(arity) = im.type_name.strip_prefix("Tuple").and_then(|n| n.parse::<usize>().ok()) {
        let _ = arity;
        let rendered: Vec<String> = im.target_args.iter().map(type_str).collect();
        s.push_str(&format!("({})", rendered.join(", ")));
    } else {
        s.push_str(&im.type_name);
        if !im.target_args.is_empty() {
            let rendered: Vec<String> = im.target_args.iter().map(type_str).collect();
            s.push_str(&format!("({})", rendered.join(", ")));
        }
    }
    // A conditional impl's `where` clause (`impl FromIterator(a) for Set(a) where a: Eq`).
    if !im.bounds.is_empty() {
        s.push_str(" where ");
        for (i, (v, t, ta)) in im.bounds.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(v);
            s.push_str(": ");
            s.push_str(t);
            if !ta.is_empty() {
                let rendered: Vec<String> = ta.iter().map(type_str).collect();
                s.push_str(&format!("({})", rendered.join(", ")));
            }
        }
    }
    // A marker-trait impl (`impl Eq for Int`) has no method block.
    if im.methods.is_empty() {
        s.push('\n');
        return;
    }
    s.push_str(":\n");
    for (i, m) in im.methods.iter().enumerate() {
        if i > 0 {
            s.push('\n');
        }
        function(s, m, true, c, upper);
    }
}

/// Write a `region:` / `region -> T:` header, no leading pad or newline.
fn region_header(s: &mut String, r: &crate::ast::RegionAnn) {
    s.push_str("region");
    if let Some(t) = &r.ty {
        s.push_str(" -> ");
        s.push_str(&type_str(t));
    }
    s.push(':');
}

fn block(s: &mut String, b: &Block, depth: usize, c: &mut Comments, upper: u32) {
    // A `region:` block renders its header at this depth and indents its body one
    // level deeper; an ordinary block renders its statements here.
    if let Some(r) = &b.region {
        pad(s, depth);
        region_header(s, r);
        s.push('\n');
        block_stmts(s, b, depth + 1, c, upper);
    } else {
        block_stmts(s, b, depth, c, upper);
    }
}

/// Render just a block's statements at `depth`, ignoring any `region` header
/// (the caller emits that). Factored out so a value-position block can place
/// the header inline after `= ` and then reuse this for the body. `upper` is the
/// source line at which this block's scope ends — the bound a nested `match`
/// uses to flush its arm comments (BUG-332).
fn block_stmts(s: &mut String, b: &Block, depth: usize, c: &mut Comments, upper: u32) {
    if b.stmts.is_empty() {
        // An empty block has no off-side representation; emit a no-op expression.
        pad(s, depth);
        s.push_str("0\n");
        return;
    }
    for (i, st) in b.stmts.iter().enumerate() {
        // Preserve a single author blank line between statements: a gap of source
        // lines larger than the comments occupying it means a blank was there.
        if i > 0 {
            if let (Some(&line), Some(&prev)) = (b.lines.get(i), b.lines.get(i - 1)) {
                let prev_last = stmt_max_line(&b.stmts[i - 1], prev);
                let gap = line.saturating_sub(prev_last).saturating_sub(1);
                if gap as usize > c.count_between(prev_last, line) {
                    s.push('\n');
                }
            }
        }
        // Own-line comments that preceded this statement in the source.
        if let Some(line) = b.lines.get(i) {
            c.before(s, depth, *line);
        }
        // This statement's scope ends at the next statement's line, or at the
        // block's `upper` for the last one.
        let st_upper = b.lines.get(i + 1).copied().unwrap_or(upper);
        stmt(s, st, depth, c, st_upper);
        if let Some(line) = b.lines.get(i) {
            c.append_trailing(s, *line);
        }
    }
}

/// The surface compound-assignment operator (`+=`, …) for a binary op, or `None`
/// for one that has no compound form.
fn compound_op(op: BinOp) -> Option<&'static str> {
    match op {
        BinOp::Add => Some("+"),
        BinOp::Sub => Some("-"),
        BinOp::Mul => Some("*"),
        BinOp::Div => Some("/"),
        BinOp::Mod => Some("%"),
        _ => None,
    }
}

/// Re-sugar an RFC-0022 place-assignment. `desugar_place_assign` lowers `v[i] = e`
/// to `v = v.set_at(i, e)` and `v.f = e` to `v = update v: f = e` (a
/// `RecordUpdate`); this recovers the canonical surface form, and the compound
/// `v[i] += e` / `v.f += e` when the RHS reads the same place. Returns `None` when
/// `value` is not such a self-assignment, so a plain assignment prints normally.
/// (BUG-333/BUG-330 — mirrors the while-let/UFCS/comprehension re-sugaring.)
fn place_assign_sugar(name: &str, value: &Expr) -> Option<String> {
    let same_var = |e: &Expr| matches!(e, Expr::Var(v) if v == name);
    match value {
        Expr::MethodCall { receiver, method, args }
            if method == "set_at" && args.len() == 2 && same_var(receiver) =>
        {
            let idx = &args[0];
            let val = &args[1];
            // Compound `v[i] += e`: RHS is `v[i] <op> e` reading the same place.
            if let Expr::Binary { op, lhs, rhs } = val {
                if let (Expr::Index { base, index }, Some(sym)) = (lhs.as_ref(), compound_op(*op)) {
                    if same_var(base) && index.as_ref() == idx {
                        return Some(format!("{name}[{}] {sym}= {}", expr(idx), expr(rhs)));
                    }
                }
            }
            Some(format!("{name}[{}] = {}", expr(idx), expr(val)))
        }
        Expr::RecordUpdate { name: _, base, fields } if fields.len() == 1 && same_var(base) => {
            let (f, val) = &fields[0];
            // Compound `v.f += e`: RHS is `v.f <op> e` reading the same field.
            if let Expr::Binary { op, lhs, rhs } = val {
                if let (Expr::Field { base: fb, field }, Some(sym)) =
                    (lhs.as_ref(), compound_op(*op))
                {
                    if same_var(fb) && field == f {
                        return Some(format!("{name}.{f} {sym}= {}", expr(rhs)));
                    }
                }
            }
            Some(format!("{name}.{f} = {}", expr(val)))
        }
        _ => None,
    }
}

/// Recognize the `for var x in xs:` desugar (an indexed `__fvN` loop that binds
/// `var x = xs[__fvN]` first and writes `xs[__fvN] = x` back last). Returns the
/// element name, the list variable, and the body with the bind/write-back
/// stripped, or `None` when `body` is not that exact shape. (BUG-334.)
fn for_var_sugar<'a>(idx: &str, iter: &'a Expr, body: &'a Block) -> Option<(&'a str, &'a str, Block)> {
    // iter must be `0..<list>.length()`.
    let Expr::Range { lo, hi, inclusive: false } = iter else { return None };
    if !matches!(lo.as_ref(), Expr::Int(0)) {
        return None;
    }
    let Expr::MethodCall { receiver, method, args } = hi.as_ref() else { return None };
    if method != "length" || !args.is_empty() {
        return None;
    }
    let Expr::Var(list_var) = receiver.as_ref() else { return None };
    if body.stmts.len() < 2 {
        return None;
    }
    // First stmt: `var x = list[idx]`.
    let Stmt::Let { name: elem, ty: None, mutable: true, value: bind } = &body.stmts[0] else {
        return None;
    };
    let Expr::Index { base, index } = bind else { return None };
    if !matches!(base.as_ref(), Expr::Var(v) if v == list_var)
        || !matches!(index.as_ref(), Expr::Var(v) if v == idx)
    {
        return None;
    }
    // Last stmt: `list[idx] = x`  (i.e. `list = list.set_at(idx, x)`).
    let Stmt::Assign { name: wb_list, value: wb } = body.stmts.last()? else { return None };
    if wb_list != list_var {
        return None;
    }
    let Expr::MethodCall { receiver: wr, method: wm, args: wa } = wb else { return None };
    if wm != "set_at" || wa.len() != 2 || !matches!(wr.as_ref(), Expr::Var(v) if v == list_var) {
        return None;
    }
    if !matches!(&wa[0], Expr::Var(v) if v == idx) || !matches!(&wa[1], Expr::Var(v) if v == elem) {
        return None;
    }
    let n = body.stmts.len();
    let inner = Block {
        stmts: body.stmts[1..n - 1].to_vec(),
        lines: body.lines.get(1..n - 1).map(<[u32]>::to_vec).unwrap_or_default(),
        region: body.region.clone(),
    };
    Some((elem, list_var, inner))
}

fn stmt(s: &mut String, st: &Stmt, depth: usize, c: &mut Comments, upper: u32) {
    match st {
        Stmt::Let { name, ty, mutable, value } => {
            pad(s, depth);
            s.push_str(if *mutable { "var " } else { "let " });
            s.push_str(name);
            if let Some(t) = ty {
                s.push_str(": ");
                s.push_str(&type_str(t));
            }
            s.push_str(" = ");
            value_or_block(s, value, depth, c, upper);
        }
        Stmt::Assign { name, value } => {
            pad(s, depth);
            // (RFC-0022) Re-sugar a place-assignment that the parser lowered to a
            // self-`set_at`/`RecordUpdate` back to `v[i] = e` / `v.f = e`
            // (BUG-333/BUG-330); an ordinary assignment prints normally.
            if let Some(line) = place_assign_sugar(name, value) {
                s.push_str(&line);
                s.push('\n');
                return;
            }
            s.push_str(name);
            s.push_str(" = ");
            value_or_block(s, value, depth, c, upper);
        }
        Stmt::LetPattern { pattern: pat, value } => {
            pad(s, depth);
            s.push_str("let ");
            s.push_str(&pattern(pat));
            s.push_str(" = ");
            value_or_block(s, value, depth, c, upper);
        }
        Stmt::Return(Some(e)) => {
            pad(s, depth);
            s.push_str("return ");
            value_or_block(s, e, depth, c, upper);
        }
        Stmt::Return(None) => {
            pad(s, depth);
            s.push_str("return\n");
        }
        Stmt::Break => {
            pad(s, depth);
            s.push_str("break\n");
        }
        Stmt::Continue => {
            pad(s, depth);
            s.push_str("continue\n");
        }
        Stmt::Yield(e) => {
            pad(s, depth);
            s.push_str("yield ");
            value_or_block(s, e, depth, c, upper);
        }
        Stmt::Expr(e) => block_stmt(s, e, depth, c, upper),
    }
}

/// An expression's single-line rendering, or None when it only renders multi-line
/// (match/lambda/block) or wrapped onto several lines.
fn inline_form(e: &Expr) -> Option<String> {
    if matches!(e, Expr::Match { .. } | Expr::Lambda { .. } | Expr::Block(_)) {
        return None;
    }
    let rendered = expr(e);
    (!rendered.contains('\n')).then_some(rendered)
}

/// A statement-position expression: control-flow forms expand multi-line.
fn block_stmt(s: &mut String, e: &Expr, depth: usize, c: &mut Comments, upper: u32) {
    // Postfix-guard return: the parser desugars `return X if cond` to an `if`
    // whose then-block is the lone return, tagged with the `u32::MAX` synthetic
    // line marker. Re-collapse exactly that shape — and only it, so an explicitly
    // written multi-line `if cond: return X` (real line numbers) is left as is.
    if let Expr::If { cond, then_block, else_block: None } = e {
        if then_block.region.is_none() && then_block.lines.as_slice() == [u32::MAX] {
            if let [Stmt::Return(val)] = then_block.stmts.as_slice() {
                let val_inline = match val {
                    None => Some(None),
                    Some(v) => inline_form(v).map(Some),
                };
                if let (Some(cond_str), Some(val_str)) = (inline_form(cond), val_inline) {
                    pad(s, depth);
                    s.push_str("return");
                    if let Some(v) = val_str {
                        s.push(' ');
                        s.push_str(&v);
                    }
                    s.push_str(" if ");
                    s.push_str(&cond_str);
                    s.push('\n');
                    return;
                }
            }
        }
    }
    // A trailing-block-lambda call, possibly behind `await`/`move`, renders
    // multi-line with the prefix on the head line.
    if let Some((prefix, call, suffix)) = unwrap_block_lambda_call(e) {
        pad(s, depth);
        s.push_str(&prefix);
        call_block_lambda(s, &call_head(call).unwrap(), call_args(call), &suffix, depth, c, upper);
        return;
    }
    match e {
        Expr::If { .. }
        | Expr::Match { .. }
        | Expr::While { .. }
        | Expr::WhileLet { .. }
        | Expr::For { .. } => {
            pad(s, depth);
            multiline(s, e, depth, c, upper);
        }
        Expr::Block(b) => {
            if let Some(sugar) = comprehension_sugar(b) {
                pad(s, depth);
                s.push_str(&sugar);
                s.push('\n');
            } else {
                block(s, b, depth, c, upper);
            }
        }
        Expr::Lambda { params, body, ret } => {
            pad(s, depth);
            lambda_at(s, params, body, ret, depth, c, upper);
        }
        _ => {
            pad(s, depth);
            if !chain_wrap(s, e, depth) {
                s.push_str(&expr(e));
                s.push('\n');
            }
        }
    }
}

/// The inline length past which a fluent method chain is broken one call per line.
const MAX_WIDTH: usize = 100;

/// The number of chained `.method(..)` calls in `e` (0 if it is not a method call).
fn method_chain_len(e: &Expr) -> usize {
    let mut n = 0;
    let mut cur = e;
    while let Expr::MethodCall { receiver, .. } = cur {
        n += 1;
        cur = receiver;
    }
    n
}

/// Whether a fluent chain is long enough to break onto one call per line. The test
/// is column-INDEPENDENT (the chain's own inline length, not its indented position)
/// so `chain_wrap` and `expr_max_line` always agree — which is what keeps fmt
/// idempotent across the wrap.
fn chain_should_wrap(e: &Expr) -> bool {
    method_chain_len(e) >= 2 && expr(e).len() > MAX_WIDTH
}

/// Wrap a long fluent method chain — `head.a(..).b(..).c(..)` — onto one call per
/// line, each `.method(..)` indented a level below the statement (witchy's layout
/// joins these leading-`.` continuation lines back into the chain). Returns false,
/// emitting nothing, for a short chain or non-chain, which the caller renders inline.
fn chain_wrap(s: &mut String, e: &Expr, depth: usize) -> bool {
    if !chain_should_wrap(e) {
        return false;
    }
    let mut segments: Vec<(&str, &[Expr])> = Vec::new();
    let mut cur = e;
    while let Expr::MethodCall { receiver, method, args } = cur {
        segments.push((method.as_str(), args.as_slice()));
        cur = receiver;
    }
    segments.reverse();
    s.push_str(&expr(cur));
    for (method, args) in segments {
        s.push('\n');
        pad(s, depth + 1);
        s.push('.');
        s.push_str(method);
        s.push('(');
        s.push_str(&comma(args));
        s.push(')');
    }
    s.push('\n');
    true
}

/// The right-hand side of a `let`/`=`/`return`: use a multi-line form when the
/// value is a `match` (no inline form) or a lambda with a block body, else an
/// inline expr.
fn value_or_block(s: &mut String, e: &Expr, depth: usize, c: &mut Comments, upper: u32) {
    if let Some((prefix, call, suffix)) = unwrap_block_lambda_call(e) {
        s.push_str(&prefix);
        call_block_lambda(s, &call_head(call).unwrap(), call_args(call), &suffix, depth, c, upper);
        return;
    }
    match e {
        Expr::Match { .. } => {
            multiline(s, e, depth, c, upper);
        }
        Expr::Lambda { params, body, ret } => {
            lambda_at(s, params, body, ret, depth, c, upper);
        }
        // A `region:` block used as a value: header after `= `, body below.
        Expr::Block(b) if b.region.is_some() => {
            region_header(s, b.region.as_ref().unwrap());
            s.push('\n');
            block_stmts(s, b, depth + 1, c, upper);
        }
        _ => {
            if !chain_wrap(s, e, depth) {
                s.push_str(&expr(e));
                s.push('\n');
            }
        }
    }
}

/// Emit a control-flow expression across multiple lines. `s` is already padded to
/// the header position for `if`/`while`/`for`; for `match` it is positioned after
/// `= ` so we do not pre-pad.
fn multiline(s: &mut String, e: &Expr, depth: usize, c: &mut Comments, upper: u32) {
    match e {
        Expr::If { cond, then_block, else_block } => {
            s.push_str("if ");
            s.push_str(&expr(cond));
            s.push_str(":\n");
            block(s, then_block, depth + 1, c, upper);
            if let Some(eb) = else_block {
                pad(s, depth);
                // `else if` chain: a single nested if statement.
                if eb.stmts.len() == 1 {
                    if let Stmt::Expr(inner @ Expr::If { .. }) = &eb.stmts[0] {
                        s.push_str("else ");
                        multiline(s, inner, depth, c, upper);
                        return;
                    }
                }
                s.push_str("else:\n");
                block(s, eb, depth + 1, c, upper);
            }
        }
        Expr::While { cond, body } => {
            s.push_str("while ");
            s.push_str(&expr(cond));
            s.push_str(":\n");
            block(s, body, depth + 1, c, upper);
        }
        Expr::WhileLet { pattern: pat, scrutinee, body } => {
            s.push_str("while let ");
            s.push_str(&pattern(pat));
            s.push_str(" = ");
            s.push_str(&expr(scrutinee));
            s.push_str(":\n");
            block(s, body, depth + 1, c, upper);
        }
        Expr::For { var, iter, body } => {
            // `for var x in xs:` (RFC-0028) desugars to an indexed loop with a
            // synthetic `__fvN` counter, a leading `var x = xs[__fvN]` bind, and a
            // trailing `xs[__fvN] = x` write-back. Recognize exactly that shape and
            // print the surface form back (BUG-334) — leaking the internal counter
            // into formatted source is a de-sugar defect, like while-let.
            if var.starts_with("__fv") {
                if let Some((elem, list_var, inner)) = for_var_sugar(var, iter, body) {
                    s.push_str("for var ");
                    s.push_str(elem);
                    s.push_str(" in ");
                    s.push_str(list_var);
                    s.push_str(":\n");
                    block(s, &inner, depth + 1, c, upper);
                    return;
                }
            }
            // `for a, b in e:` desugars at parse to a synthetic element variable
            // plus a leading destructure; print the sugar back (unparenthesized —
            // the canonical Python-style form; `for (a, b) in e:` also parses).
            if var.starts_with("__fortuple") {
                if let Some(Stmt::LetPattern { pattern: pat, value: Expr::Var(v) }) = body.stmts.first() {
                    if v == var {
                        let inner = Block {
                            stmts: body.stmts[1..].to_vec(),
                            lines: body.lines.get(1..).map(<[u32]>::to_vec).unwrap_or_default(),
                            region: body.region.clone(),
                        };
                        s.push_str("for ");
                        // A tuple header prints in the canonical unparenthesized
                        // comma form (`for a, b in e:`); any other pattern prints
                        // as itself.
                        match pat {
                            Pattern::Tuple(ps) => {
                                s.push_str(
                                    &ps.iter().map(pattern).collect::<Vec<_>>().join(", "),
                                );
                            }
                            other => s.push_str(&pattern(other)),
                        }
                        s.push_str(" in ");
                        s.push_str(&expr(iter));
                        s.push_str(":\n");
                        block(s, &inner, depth + 1, c, upper);
                        return;
                    }
                }
            }
            // `for await x in rx:` — the parser marks the receiver as
            // `chan.__recv_stream(rx)`; print the surface form back.
            if let Expr::Call { name, args } = &**iter {
                if name == "chan.__recv_stream" && args.len() == 1 {
                    s.push_str("for await ");
                    s.push_str(var);
                    s.push_str(" in ");
                    s.push_str(&expr(&args[0]));
                    s.push_str(":\n");
                    block(s, body, depth + 1, c, upper);
                    return;
                }
            }
            s.push_str("for ");
            s.push_str(var);
            s.push_str(" in ");
            s.push_str(&expr(iter));
            s.push_str(":\n");
            block(s, body, depth + 1, c, upper);
        }
        Expr::Match { scrutinee, arms } => {
            // A two-arm match of `pattern -> block` plus an unguarded wildcard
            // block IS an `if let` (the desugar produces exactly this shape, and
            // re-parsing the sugar reproduces it), so that is how it prints. An
            // empty wildcard body is the elseless form — an empty block has no
            // off-side surface form, so it can only come from the desugar.
            if let [then_arm, else_arm] = arms.as_slice() {
                if then_arm.guard.is_none()
                    && else_arm.guard.is_none()
                    && then_arm.pattern != Pattern::Wildcard
                    && else_arm.pattern == Pattern::Wildcard
                {
                    if let (Expr::Block(tb), Expr::Block(eb)) = (&then_arm.body, &else_arm.body)
                    {
                        if tb.region.is_none() && eb.region.is_none() && !tb.stmts.is_empty() {
                            s.push_str("if let ");
                            s.push_str(&pattern(&then_arm.pattern));
                            s.push_str(" = ");
                            s.push_str(&expr(scrutinee));
                            s.push_str(":\n");
                            block(s, tb, depth + 1, c, upper);
                            if !eb.stmts.is_empty() {
                                pad(s, depth);
                                s.push_str("else:\n");
                                block(s, eb, depth + 1, c, upper);
                            }
                            return;
                        }
                    }
                }
            }
            s.push_str("match ");
            s.push_str(&expr(scrutinee));
            s.push_str(":\n");
            let header_col = depth as u32 * 4 + 1;
            for a in arms {
                c.before_body(s, depth + 1, header_col, a.line);
                pad(s, depth + 1);
                s.push_str(&pattern(&a.pattern));
                if let Some(g) = &a.guard {
                    s.push_str(" if ");
                    s.push_str(&expr(g));
                }
                s.push_str(" ->");
                arm_body(s, &a.body, depth + 1, c, upper);
            }
            c.before_body(s, depth + 1, header_col, upper);
        }
        _ => {
            s.push_str(&expr(e));
            s.push('\n');
        }
    }
}

/// A match-arm body. A genuine block (multiple statements) opens an indented
/// block after the `->`; otherwise the body stays a bare expression — a `match`
/// continues multi-line on the same line as `->`, everything else is inline (so
/// the re-parsed arm body has the same shape, not a `Block` wrapper).
fn arm_body(s: &mut String, body: &Expr, depth: usize, c: &mut Comments, upper: u32) {
    // A one-statement block whose statement fits on a line prints inline
    // (`-> return e`, `-> x = e`, `-> break`) — the same source shape it
    // parses from. `return` with no value must stay in block form: inline it
    // would swallow the next arm's pattern as its value.
    fn inline_value(e: &Expr) -> Option<String> {
        if matches!(e, Expr::Match { .. } | Expr::Lambda { .. } | Expr::Block(_)) {
            return None;
        }
        let rendered = expr(e);
        (!rendered.contains('\n')).then_some(rendered)
    }
    match body {
        Expr::Block(b) if b.stmts.len() == 1 && b.region.is_none() => {
            let inline = match &b.stmts[0] {
                Stmt::Return(Some(e)) => inline_value(e).map(|v| format!("return {v}")),
                Stmt::Assign { name, value } => place_assign_sugar(name, value)
                    .filter(|l| !l.contains('\n'))
                    .or_else(|| inline_value(value).map(|v| format!("{name} = {v}"))),
                Stmt::Break => Some("break".into()),
                Stmt::Continue => Some("continue".into()),
                _ => None,
            };
            match inline {
                Some(line) => {
                    s.push(' ');
                    s.push_str(&line);
                    s.push('\n');
                }
                None => {
                    s.push('\n');
                    block(s, b, depth + 1, c, upper);
                }
            }
        }
        Expr::Block(b) => {
            s.push('\n');
            block(s, b, depth + 1, c, upper);
        }
        Expr::Match { .. } => {
            s.push(' ');
            multiline(s, body, depth, c, upper);
        }
        Expr::Lambda { params, body, ret } => {
            s.push(' ');
            lambda_at(s, params, body, ret, depth, c, upper);
        }
        _ => {
            s.push(' ');
            s.push_str(&expr(body));
            s.push('\n');
        }
    }
}

/// Render a duration literal (stored as whole milliseconds) using the largest
/// unit that divides it exactly, so `30000` prints as `30s` and re-parses to the
/// same value. A zero or non-dividing value falls back to `ms`.
fn duration_literal(ms: i64) -> String {
    for (unit_ms, suffix) in [
        (604_800_000_i64, "w"),
        (86_400_000, "d"),
        (3_600_000, "h"),
        (60_000, "m"),
        (1_000, "s"),
    ] {
        if ms != 0 && ms % unit_ms == 0 {
            return format!("{}{}", ms / unit_ms, suffix);
        }
    }
    format!("{ms}ms")
}

/// Inline expression. Anything that could regroup on re-parse is parenthesized.
fn expr(e: &Expr) -> String {
    match e {
        Expr::Int(n) => n.to_string(),
        Expr::Duration(ms) => duration_literal(*ms),
        Expr::Float(x) => {
            let t = x.to_string();
            if t.contains('.') || t.contains('e') || t.contains("inf") || t.contains("NaN") {
                t
            } else {
                format!("{t}.0")
            }
        }
        Expr::Str(v) => string_lit(v),
        // `tag"a${x}b"` — reconstruct from the static parts (escaped) and holes
        // (raw source). `parts` has one more element than `holes`.
        Expr::TaggedLit { tag, parts, holes, .. } => {
            let mut s = format!("{tag}\"");
            for (i, part) in parts.iter().enumerate() {
                s.push_str(&tagged_part(part));
                if let Some(hole) = holes.get(i) {
                    s.push_str("${");
                    s.push_str(hole);
                    s.push('}');
                }
            }
            s.push('"');
            s
        }
        Expr::Bool(b) => b.to_string(),
        Expr::Var(n) => n.clone(),
        Expr::List(xs) => format!("[{}]", comma(xs)),
        Expr::Tuple(xs) => format!("({})", comma(xs)),
        Expr::Call { name, args } => {
            // `to_string`/`int_to_string` were retired in favor of string
            // interpolation, whose only surface spelling is `"${x}"`; rewrite a
            // single-argument render call to that form (unless the module defines
            // its own function by that name). This is render-EQUIVALENT — the
            // three spellings desugar to the same internal render tree — and is the
            // printer's only tree-changing rewrite.
            //
            // The formatter does NOT rewrite a call to a module-qualified stdlib
            // path (BUG-014): a bare `foo(...)` prints verbatim, so a parameter
            // or `let` that shadows a stdlib function name (e.g. an `update`
            // callback) can never be silently re-pointed at `dict.update`. The
            // formatter must never change which function a call resolves to.
            if !local_fn(name)
                && matches!(name.as_str(), "to_string" | "int_to_string")
                && args.len() == 1
            {
                // Inline arguments only; anything multiline falls through and the
                // round-trip guard skips the file rather than mangle it.
                let inner = expr(&args[0]);
                if !inner.contains('\n') {
                    return format!("\"${{{inner}}}\"");
                }
            }
            format!("{name}({})", comma(args))
        }
        // (RFC-0056) A labeled direct call — print each argument as written,
        // `label: value` for the labeled ones (unlowered; the formatter never
        // links, so it keeps the source shape like it does for `Expr::Record`).
        Expr::LabeledCall { name, args } => {
            let parts: Vec<String> = args
                .iter()
                .map(|(label, v)| match label {
                    Some(l) => format!("{l}: {}", expr(v)),
                    None => expr(v),
                })
                .collect();
            format!("{name}({})", parts.join(", "))
        }
        Expr::MethodCall { receiver, method, args } => {
            format!("{}.{method}({})", operand(receiver, POSTFIX_PREC, false), comma(args))
        }
        Expr::Ctor { name, args } => {
            if args.is_empty() {
                name.clone()
            } else {
                format!("{name}({})", comma(args))
            }
        }
        Expr::AnonCtor { tag, args } => {
            if args.is_empty() {
                format!(".{tag}")
            } else {
                format!(".{tag}({})", comma(args))
            }
        }
        Expr::Record { name, fields, spread } => {
            let mut parts: Vec<String> =
                fields.iter().map(|(f, v)| format!("{f}: {}", expr(v))).collect();
            if let Some(s) = spread {
                parts.push(format!("..{}", expr(s)));
            }
            // An anonymous struct prints back as `.{…}`, not its synthetic name.
            if name.starts_with("__anon") {
                format!(".{{{}}}", parts.join(", "))
            } else {
                format!("{name}({})", parts.join(", "))
            }
        }
        Expr::Apply { func, args } => {
            format!("{}({})", operand(func, POSTFIX_PREC, false), comma(args))
        }
        Expr::Field { base, field } => format!("{}.{field}", operand(base, POSTFIX_PREC, false)),
        Expr::Unary { op: UnOp::Await, expr: inner } => {
            // `await` is POSTFIX: `e.await` (binds at postfix precedence).
            format!("{}.await", operand(inner, POSTFIX_PREC, false))
        }
        Expr::Unary { op, expr: inner } => {
            format!("{}{}", unary_prefix(*op), operand(inner, UNARY_PREC, false))
        }
        Expr::Binary { op, lhs, rhs } => {
            // A string-`+` chain in the canonical shape interpolation
            // desugars to prints back as the interpolation itself (see
            // `interpolation_sugar`).
            if matches!(op, BinOp::Concat | BinOp::Add) {
                if let Some(sugar) = interpolation_sugar(e) {
                    return sugar;
                }
            }
            let p = binop_prec(*op);
            // `??` is RIGHT-associative: the natural chain `a ?? b ?? c` nests to
            // the right, so the RIGHT child at equal precedence needs no parens
            // and the LEFT one does — the mirror image of every other binary op.
            // Swapping the `is_right` flags encodes exactly that.
            if matches!(op, BinOp::Coalesce) {
                return format!("{} ?? {}", operand(lhs, p, true), operand(rhs, p, false));
            }
            format!("{} {} {}", operand(lhs, p, false), binop(*op), operand(rhs, p, true))
        }
        Expr::Range { lo, hi, inclusive } => {
            let op = if *inclusive { "..=" } else { ".." };
            format!("{}{op}{}", operand(lo, RANGE_PREC, false), operand(hi, RANGE_PREC, true))
        }
        Expr::Index { base, index } => {
            format!("{}[{}]", operand(base, POSTFIX_PREC, false), expr(index))
        }
        // `(__try_ctx(e, msg))?` is the desugar of `e ? "msg"` — render it back to
        // the surface form rather than exposing the intrinsic.
        Expr::Try(inner) => match inner.as_ref() {
            Expr::Call { name, args } if name == "__try_ctx" && args.len() == 2 => {
                format!("{} ? {}", operand(&args[0], POSTFIX_PREC, false), expr(&args[1]))
            }
            _ => format!("{}?", operand(inner, POSTFIX_PREC, false)),
        },
        Expr::As { expr, ty } => format!("{} as {}", operand(expr, POSTFIX_PREC, false), type_str(ty)),
        Expr::Lambda { params, body, ret } => {
            let ps: Vec<String> = params.iter().map(param).collect();
            let r = match ret {
                Some(t) => format!(" -> {}", type_str(t)),
                None => String::new(),
            };
            format!("fn({}){}: {}", ps.join(", "), r, block_value(body))
        }
        Expr::If { cond, then_block, else_block } => {
            let e = else_block
                .as_ref()
                .map(|b| format!(" else: {}", block_value(b)))
                .unwrap_or_default();
            format!("if {}: {}{}", expr(cond), block_value(then_block), e)
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            let fs: Vec<String> = fields
                .iter()
                .map(|(n, v)| format!("{n} = {}", expr(v)))
                .collect();
            format!("update {}: {}", expr(base), fs.join(" "))
        }
        // A block in expression position is a comprehension's desugar (the
        // only block with an inline surface form) — print the literal back.
        Expr::Block(b) => {
            if let Some(sugar) = comprehension_sugar(b) {
                return sugar;
            }
            "0".to_string()
        }
        // No inline form — caller should have routed these multi-line. Emit a
        // best-effort placeholder; the reformat round-trip guard rejects the
        // output if one of these ever leaks into it.
        Expr::Match { .. }
        | Expr::While { .. }
        | Expr::WhileLet { .. }
        | Expr::For { .. } => "0".to_string(),
    }
}

/// The single-expression value of a block (for inline lambda / if bodies), or
/// `None` if the block has no faithful inline form (multiple statements, or a
/// single expression that itself needs multiple lines).
fn block_value_opt(b: &Block) -> Option<String> {
    if b.stmts.len() == 1 {
        if let Stmt::Expr(e) = &b.stmts[0] {
            if inline_ok(e) {
                return Some(expr(e));
            }
        }
    }
    None
}

/// Whether `expr(e)` faithfully renders `e` inline (no `0` placeholder). `match`
/// and loops have no inline form; an `if`/lambda is inline only if its sub-blocks
/// are.
fn inline_ok(e: &Expr) -> bool {
    match e {
        Expr::Match { .. } | Expr::While { .. } | Expr::WhileLet { .. } | Expr::For { .. } | Expr::Block(_) => false,
        Expr::If { then_block, else_block, .. } => {
            block_value_opt(then_block).is_some()
                && else_block.as_ref().is_none_or(|b| block_value_opt(b).is_some())
        }
        Expr::Lambda { body, .. } => block_value_opt(body).is_some(),
        _ => true,
    }
}

fn block_value(b: &Block) -> String {
    block_value_opt(b).unwrap_or_else(|| "0".to_string())
}

/// Emit a lambda in a layout-friendly position (statement / `let` value / arm):
/// inline `fn(p): expr` when the body is a single inline expression, otherwise
/// `fn(p):` followed by an indented block. `s` is positioned where the `fn`
/// begins.
fn lambda_at(s: &mut String, params: &[Param], body: &Block, ret: &Option<Type>, depth: usize, c: &mut Comments, upper: u32) {
    let ps: Vec<String> = params.iter().map(param).collect();
    s.push_str("fn(");
    s.push_str(&ps.join(", "));
    s.push(')');
    if let Some(t) = ret {
        s.push_str(" -> ");
        s.push_str(&type_str(t));
    }
    match block_value_opt(body) {
        Some(inline) => {
            s.push_str(": ");
            s.push_str(&inline);
            s.push('\n');
        }
        None => {
            s.push_str(":\n");
            block(s, body, depth + 1, c, upper);
        }
    }
}

fn comma(xs: &[Expr]) -> String {
    xs.iter().map(expr).collect::<Vec<_>>().join(", ")
}

/// Whether a call's LAST argument is a block-bodied lambda (no inline form) — the
/// trailing-lambda shape that has to render across multiple lines.
fn has_trailing_block_lambda(args: &[Expr]) -> bool {
    matches!(args.last(), Some(Expr::Lambda { body, .. }) if block_value_opt(body).is_none())
}

/// The printed prefix for a unary operator (`move`/`await` are word prefixes with
/// a trailing space; the rest are sigils).
fn unary_prefix(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Not => "!",
        UnOp::BitNot => "~",
        UnOp::Move => "move ",
        UnOp::Await => "await ",
    }
}

/// A call whose last argument is a block-bodied lambda, possibly behind a postfix `.await`
/// (`f(x, fn(p): <block>).await`). Returns the printed prefix and the
/// underlying call to render with `call_block_lambda` — this is what keeps the
/// trailing-lambda multi-line form intact when an `await`/`move` sits in front.
fn unwrap_block_lambda_call(e: &Expr) -> Option<(String, &Expr, String)> {
    match e {
        Expr::Call { args, .. } | Expr::MethodCall { args, .. }
            if has_trailing_block_lambda(args) =>
        {
            Some((String::new(), e, String::new()))
        }
        // `e.await` is postfix: the `.await` rides as a SUFFIX, after the call's
        // closing `)`. Other unaries (`move`) stay prefixes.
        Expr::Unary { op: UnOp::Await, expr: inner } => unwrap_block_lambda_call(inner)
            .map(|(p, call, suf)| (p, call, format!("{suf}.await"))),
        Expr::Unary { op, expr: inner } => unwrap_block_lambda_call(inner)
            .map(|(p, call, suf)| (format!("{}{p}", unary_prefix(*op)), call, suf)),
        _ => None,
    }
}

fn call_args(e: &Expr) -> &[Expr] {
    match e {
        Expr::Call { args, .. } | Expr::MethodCall { args, .. } => args,
        _ => &[],
    }
}

/// Render a call whose last argument is a block-bodied lambda multi-line:
/// `head(lead.., fn(p):` then the indented body, then a dedented `)`. `s` is
/// positioned where the call head begins.
fn call_block_lambda(s: &mut String, head: &str, args: &[Expr], suffix: &str, depth: usize, c: &mut Comments, upper: u32) {
    s.push_str(head);
    s.push('(');
    let (lead, last) = args.split_at(args.len() - 1);
    for a in lead {
        s.push_str(&expr(a));
        s.push_str(", ");
    }
    if let Expr::Lambda { params, body, ret } = &last[0] {
        lambda_at(s, params, body, ret, depth, c, upper);
    }
    pad(s, depth);
    s.push(')');
    s.push_str(suffix);
    s.push('\n');
}

/// The printed head of a call/method-call (everything before the `(`), for the
/// multi-line trailing-lambda form.
fn call_head(e: &Expr) -> Option<String> {
    match e {
        Expr::Call { name, .. } => Some(if local_fn(name) {
            name.clone()
        } else {
            crate::aliases::moved_builtin(name).unwrap_or(name).to_string()
        }),
        Expr::MethodCall { receiver, method, .. } => {
            Some(format!("{}.{method}", operand(receiver, POSTFIX_PREC, false)))
        }
        _ => None,
    }
}

fn binop(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Concat => "+",
        BinOp::Eq => "==",
        BinOp::NotEq => "!=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::Coalesce => "??",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
    }
}

/// The left binding power of a binary operator, mirroring the parser's
/// `infix_bp` so the formatter can omit parentheses the precedence makes
/// redundant. Higher binds tighter.
fn binop_prec(op: BinOp) -> u8 {
    match op {
        // `??` is RIGHT-associative; the `Expr::Binary` printer swaps the
        // `operand` flags so the natural right-nested chain prints as
        // `a ?? b ?? c` and a left-nested one keeps its parens.
        BinOp::Coalesce => 4,
        BinOp::Or => 6,
        BinOp::And => 8,
        BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => 10,
        BinOp::BitOr => 12,
        BinOp::BitXor => 14,
        BinOp::BitAnd => 16,
        BinOp::Shl | BinOp::Shr => 18,
        BinOp::Add | BinOp::Sub | BinOp::Concat => 20,
        BinOp::Mul | BinOp::Div | BinOp::Mod => 22,
    }
}

/// Precedence of prefix and postfix operators, both tighter than every binary
/// operator (max 19) so they never need wrapping as a binary operand.
const UNARY_PREC: u8 = 50;
const POSTFIX_PREC: u8 = 90;

/// Binding power of `..`/`..=` (the parser's left bp of 2), looser than every
/// binary operator so a range operand that is itself an operator never needs
/// parentheses, and a range used as another operator's operand always does.
const RANGE_PREC: u8 = 2;

/// Precedence of an expression for deciding whether it needs parentheses as the
/// operand of a tighter-binding operator. Atoms and forms that print their own
/// delimiters never need wrapping (100).
fn expr_prec(e: &Expr) -> u8 {
    match e {
        Expr::Binary { op, .. } => binop_prec(*op),
        Expr::Range { .. } => RANGE_PREC,
        Expr::Unary { op: UnOp::Await, .. } => POSTFIX_PREC,
        Expr::Unary { .. } => UNARY_PREC,
        Expr::Field { .. } | Expr::Try(_) | Expr::Apply { .. } | Expr::As { .. } | Expr::Index { .. } | Expr::MethodCall { .. } => POSTFIX_PREC,
        _ => 100,
    }
}

/// Render `e` as an operand of a binary operator with left binding power
/// `parent`. All binary operators are left-associative, so the right operand is
/// wrapped at equal precedence (`a - (b - c)`), the left operand only when it
/// binds strictly looser (`a - b - c` stays flat).
fn operand(e: &Expr, parent: u8, is_right: bool) -> String {
    // A concat chain that prints as an interpolated string literal is a
    // PRIMARY — never parenthesized, whatever its AST precedence says.
    if let Expr::Binary { op: BinOp::Concat | BinOp::Add, .. } = e {
        if let Some(sugar) = interpolation_sugar(e) {
            return sugar;
        }
    }
    let s = expr(e);
    let needs = if is_right { expr_prec(e) <= parent } else { expr_prec(e) < parent };
    if needs {
        format!("({s})")
    } else {
        s
    }
}

/// Print a comprehension's desugar back as the literal it came from.
///
/// `[elem for x in xs if c ...]` parses to a block of the exact shape
/// `{ var __comprN = []; <for/if nest ending in __comprN = list.push(__comprN,
/// elem)>; __comprN }`; this is its inverse. The shape is strict (single-
/// statement nesting, the accumulator name, the push call), and both
/// spellings parse to the same AST modulo the fresh accumulator counter, so a
/// hand-written block of this shape prints as a comprehension too.
fn comprehension_sugar(b: &Block) -> Option<String> {
    if b.region.is_some() || b.stmts.len() != 3 {
        return None;
    }
    let Stmt::Let { name: acc, ty: None, mutable: true, value: Expr::List(init) } = &b.stmts[0] else {
        return None;
    };
    if !acc.starts_with("__compr") || !init.is_empty() {
        return None;
    }
    if !matches!(&b.stmts[2], Stmt::Expr(Expr::Var(v)) if v == acc) {
        return None;
    }
    let mut clauses = String::new();
    let mut cur = &b.stmts[1];
    loop {
        match cur {
            Stmt::Expr(Expr::For { var, iter, body })
                if body.stmts.len() == 1 && body.region.is_none() =>
            {
                clauses.push_str(&format!(" for {var} in {}", expr(iter)));
                cur = &body.stmts[0];
            }
            Stmt::Expr(Expr::If { cond, then_block, else_block: None })
                if then_block.stmts.len() == 1 && then_block.region.is_none() =>
            {
                clauses.push_str(&format!(" if {}", expr(cond)));
                cur = &then_block.stmts[0];
            }
            Stmt::Assign { name, value: Expr::Call { name: push, args } } => {
                if name != acc || push != "list.push" || args.len() != 2 {
                    return None;
                }
                if !matches!(&args[0], Expr::Var(v) if v == acc) {
                    return None;
                }
                // The nest must contain at least one `for` clause.
                if !clauses.starts_with(" for") {
                    return None;
                }
                return Some(format!("[{}{clauses}]", expr(&args[1])));
            }
            _ => return None,
        }
    }
}

/// Print a `<>` chain back as the string interpolation it desugared from.
///
/// The lexer expands `"a ${x} b"` to `("a " + @render(x) + " b")` at the
/// TOKEN level, so the AST has no interpolation node; this is its inverse.
/// The shape is strict — literal segments alternating with render intrinsic
/// pieces, starting and ending with a literal (the lexer always emits the
/// trailing literal, even when empty) — and the two spellings parse to the
/// same AST, so re-sugaring is pure canonicalization: a hand-written chain of
/// this exact shape prints as the interpolation idiom too.
fn interpolation_sugar(e: &Expr) -> Option<String> {
    let mut pieces: Vec<&Expr> = Vec::new();
    let mut cur = e;
    loop {
        match cur {
            Expr::Binary { op: BinOp::Concat | BinOp::Add, lhs, rhs } => {
                pieces.push(rhs);
                cur = lhs;
            }
            other => {
                pieces.push(other);
                break;
            }
        }
    }
    pieces.reverse();
    if pieces.len() < 3 || pieces.len().is_multiple_of(2) {
        return None;
    }
    let mut out = String::from("\"");
    for (i, p) in pieces.iter().enumerate() {
        if i % 2 == 0 {
            let Expr::Str(text) = p else { return None };
            out.push_str(&interp_segment(text));
        } else {
            let Expr::Call { name, args } = p else { return None };
            if !is_render_intrinsic(name) || args.len() != 1 {
                return None;
            }
            let inner = expr(&args[0]);
            // A multi-line rendering can't live inside a string literal.
            if inner.contains('\n') {
                return None;
            }
            out.push_str("${");
            out.push_str(&inner);
            out.push('}');
        }
    }
    out.push('"');
    Some(out)
}

/// `string_lit` escaping for a segment inside an interpolated literal — also
/// escapes `$` so a literal dollar survives the round trip.
fn interp_segment(v: &str) -> String {
    let mut s = String::new();
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\t' => s.push_str("\\t"),
            '\r' => s.push_str("\\r"),
            '\0' => s.push_str("\\0"),
            '$' => s.push_str("\\$"),
            _ => s.push(c),
        }
    }
    s
}

fn pattern(p: &Pattern) -> String {
    match p {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Var(n) => n.clone(),
        Pattern::Int(n) => n.to_string(),
        Pattern::Str(v) => string_lit(v),
        Pattern::Bool(b) => b.to_string(),
        Pattern::Ctor { name, args } => {
            if args.is_empty() {
                name.clone()
            } else {
                format!("{name}({})", args.iter().map(pattern).collect::<Vec<_>>().join(", "))
            }
        }
        Pattern::AnonCtor { tag, args } => {
            if args.is_empty() {
                format!(".{tag}")
            } else {
                format!(".{tag}({})", args.iter().map(pattern).collect::<Vec<_>>().join(", "))
            }
        }
        Pattern::Tuple(ps) => {
            format!("({})", ps.iter().map(pattern).collect::<Vec<_>>().join(", "))
        }
        Pattern::List { elems, rest } => {
            let mut parts: Vec<String> = elems.iter().map(pattern).collect();
            match rest {
                Some(Some(n)) => parts.push(format!("..{n}")),
                Some(None) => parts.push("..".to_string()),
                None => {}
            }
            format!("[{}]", parts.join(", "))
        }
        Pattern::Duration(ms) => duration_literal(*ms),
        Pattern::IntRange { lo, hi, inclusive } => {
            format!("{lo}{}{hi}", if *inclusive { "..=" } else { ".." })
        }
        Pattern::Or(alts) => alts.iter().map(pattern).collect::<Vec<_>>().join(" | "),
    }
}

pub fn expr_str(e: &Expr) -> String {
    expr(e)
}

fn parse_fixed_width_usize(s: &str, pos: &mut usize, width: usize) -> Option<usize> {
    let end = pos.checked_add(width)?;
    let part = s.get(*pos..end)?;
    if !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    *pos = end;
    part.parse().ok()
}

fn decode_anon_record_type_name(name: &str) -> Option<Vec<String>> {
    let mut pos = "__anon".len();
    let rest = name.strip_prefix("__anon")?;
    if rest.len() < 10 {
        return None;
    }
    let count = parse_fixed_width_usize(name, &mut pos, 10)?;
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let len = parse_fixed_width_usize(name, &mut pos, 10)?;
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            let byte = parse_fixed_width_usize(name, &mut pos, 3)?;
            if byte > u8::MAX as usize {
                return None;
            }
            bytes.push(byte as u8);
        }
        fields.push(String::from_utf8(bytes).ok()?);
    }
    if pos == name.len() { Some(fields) } else { None }
}

fn decode_anon_union_type_name(name: &str) -> Option<Vec<(String, usize)>> {
    let mut pos = "__union".len();
    let rest = name.strip_prefix("__union")?;
    if rest.len() < 10 {
        return None;
    }
    let count = parse_fixed_width_usize(name, &mut pos, 10)?;
    let mut variants = Vec::with_capacity(count);
    for _ in 0..count {
        let len = parse_fixed_width_usize(name, &mut pos, 10)?;
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            let byte = parse_fixed_width_usize(name, &mut pos, 3)?;
            if byte > u8::MAX as usize {
                return None;
            }
            bytes.push(byte as u8);
        }
        let tag = String::from_utf8(bytes).ok()?;
        let arity = parse_fixed_width_usize(name, &mut pos, 10)?;
        variants.push((tag, arity));
    }
    if pos == name.len() { Some(variants) } else { None }
}

pub fn type_str(t: &Type) -> String {
    match t {
        Type::Qualified(q, inner) => format!("{} {}", q.as_str(), type_str(inner)),
        Type::Named(n, args) => {
            if let Some(variants) = decode_anon_union_type_name(n) {
                let total: usize = variants.iter().map(|(_, arity)| *arity).sum();
                if total == args.len() {
                    let mut idx = 0;
                    let rendered = variants
                        .iter()
                        .map(|(tag, arity)| {
                            if *arity == 0 {
                                tag.clone()
                            } else {
                                let payloads = args[idx..idx + *arity]
                                    .iter()
                                    .map(type_str)
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                idx += *arity;
                                format!("{tag}({payloads})")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" | ");
                    return format!(".[{rendered}]");
                }
            }
            if let Some(fields) = decode_anon_record_type_name(n) {
                if fields.len() == args.len() {
                    let rendered = fields
                        .iter()
                        .zip(args)
                        .map(|(field, ty)| format!("{field}: {}", type_str(ty)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    return format!(".{{{rendered}}}");
                }
            }
            if args.is_empty() {
                return n.clone();
            }
            // Capability rights use bracket syntax (`Dir[Read]`, `Net[Connect]`);
            // ordinary generic types use parens (`List(Int)`, `Option(T)`).
            if n == "Dir" || n == "File" || n == "Net" {
                format!("{n}[{}]", args.iter().map(type_str).collect::<Vec<_>>().join(", "))
            } else {
                format!("{n}({})", args.iter().map(type_str).collect::<Vec<_>>().join(", "))
            }
        }
        Type::Tuple(ts) => {
            format!("({})", ts.iter().map(type_str).collect::<Vec<_>>().join(", "))
        }
        Type::Fn(ps, r) => {
            format!("fn({}) -> {}", ps.iter().map(type_str).collect::<Vec<_>>().join(", "), type_str(r))
        }
    }
}

thread_local! {
    /// Function names defined by the module currently being formatted —
    /// exempt from the moved-builtin rewrite.
    static LOCAL_FNS: std::cell::RefCell<HashSet<String>> =
        std::cell::RefCell::new(HashSet::new());
}

fn local_fn(name: &str) -> bool {
    LOCAL_FNS.with(|s| s.borrow().contains(name))
}

fn seed_local_fns(module: &Module) {
    LOCAL_FNS.with(|s| {
        let mut s = s.borrow_mut();
        s.clear();
        for it in &module.items {
            match it {
                Item::Function(f) => {
                    s.insert(f.name.clone());
                }
                Item::Impl(i) => {
                    for m in &i.methods {
                        s.insert(m.name.clone());
                    }
                }
                Item::Trait(t) => {
                    for m in &t.methods {
                        s.insert(m.name.clone());
                    }
                }
                _ => {}
            }
        }
    });
}

// --- Round-trip canonicalization -----------------------------------------
//
// The semantic guard in `reformat` compares the input AST to the output's,
// after canonicalizing BOTH: source-line metadata is cleared (layout shifts
// freely), and the formatter's one tree-changing rewrite — the render-call
// (`to_string`/`int_to_string`) → interpolation desugar — is applied, so it
// doesn't read as a difference. Everything else
// the printer does (re-sugaring comprehensions/interpolation/if-let, inline
// arms, bare nullary constructors) parses back to an identical tree and needs
// no allowance here.

fn canon_module(m: &mut Module) {
    m.import_lines.clear();
    m.item_lines.clear();
    // Import ORDER is semantically irrelevant, and the printer emits `from X
    // import Y` lines after the plain `import X` block (RFC-0042) — so a source
    // that interleaves them reparses with a different `imports` order. Normalize
    // both lists for the round-trip comparison; the emitted text is unaffected.
    m.imports.sort();
    m.imports.dedup();
    m.from_imports.sort();
    for it in &mut m.items {
        canon_item(it);
    }
}

fn canon_item(it: &mut Item) {
    match it {
        Item::Function(f) => canon_block(&mut f.body),
        Item::Type(_) | Item::TypeAlias { .. } => {}
        Item::Const { value, .. } => canon_expr(value),
        Item::Trait(t) => {
            for m in &mut t.methods {
                if let Some(b) = &mut m.default {
                    canon_block(b);
                }
            }
        }
        Item::Impl(im) => {
            for f in &mut im.methods {
                canon_block(&mut f.body);
            }
        }
        Item::Comptime(b) => canon_block(b),
    }
}

fn canon_block(b: &mut Block) {
    b.lines.clear();
    for s in &mut b.stmts {
        canon_stmt(s);
    }
}

fn canon_stmt(s: &mut Stmt) {
    match s {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } => {
            canon_expr(value)
        }
        Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => canon_expr(e),
        _ => {}
    }
}

fn canon_expr(e: &mut Expr) {
    if let Expr::Call { name, args } = e {
        // The printer re-sugars interpolation to `"${x}"`, which reparses through
        // the generated render intrinsic. Normalize the legacy oracle spelling for
        // this comparison pass only; standalone `__render(x)` still prints as the
        // source wrote it.
        if is_render_intrinsic(name) {
            *name = GENERATED_RENDER_INTRINSIC.into();
        }
        // A retired rendering call canonicalizes to the interpolation DESUGAR
        // (`"" + @render(e) + ""`), the exact tree `"${e}"` parses to — so the
        // printer's interpolation rewrite reads as equality, not a change.
        if !local_fn(name)
            && matches!(name.as_str(), "to_string" | "int_to_string")
            && args.len() == 1
        {
            let mut arg = args.remove(0);
            canon_expr(&mut arg);
            *e = Expr::Binary {
                op: BinOp::Add,
                lhs: Box::new(Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Str(String::new())),
                    rhs: Box::new(Expr::Call {
                        name: GENERATED_RENDER_INTRINSIC.into(),
                        args: vec![arg],
                    }),
                }),
                rhs: Box::new(Expr::Str(String::new())),
            };
            return;
        }
    }
    match e {
        Expr::Call { name: _, args } => {
            // The printer prints a call target verbatim (BUG-014), so there is
            // no target rewrite to mirror here — just recurse into arguments.
            for x in args {
                canon_expr(x);
            }
        }
        Expr::List(xs) | Expr::Tuple(xs) | Expr::Ctor { args: xs, .. }
        | Expr::AnonCtor { args: xs, .. } => {
            for x in xs {
                canon_expr(x);
            }
        }
        Expr::Apply { func, args } => {
            canon_expr(func);
            for x in args {
                canon_expr(x);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::Field { base: expr, .. } => canon_expr(expr),
        Expr::Binary { op, lhs, rhs } => {
            // Legacy `<>` reads as the `+` it prints to.
            if *op == BinOp::Concat {
                *op = BinOp::Add;
            }
            canon_expr(lhs);
            canon_expr(rhs);
        }
        Expr::Range { lo, hi, .. } => {
            canon_expr(lo);
            canon_expr(hi);
        }
        Expr::Index { base, index } => {
            canon_expr(base);
            canon_expr(index);
        }
        Expr::MethodCall { receiver, args, .. } => {
            canon_expr(receiver);
            for a in args {
                canon_expr(a);
            }
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            canon_expr(scrutinee);
            canon_block(body);
        }
        Expr::Lambda { body, .. } => canon_block(body),
        Expr::RecordUpdate { name: _, base, fields } => {
            canon_expr(base);
            for (_, v) in fields {
                canon_expr(v);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                canon_expr(v);
            }
            if let Some(s) = spread {
                canon_expr(s);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            canon_expr(cond);
            canon_block(then_block);
            if let Some(b) = else_block {
                canon_block(b);
            }
        }
        Expr::Match { scrutinee, arms } => {
            canon_expr(scrutinee);
            for a in arms {
                if let Some(g) = &mut a.guard {
                    canon_expr(g);
                }
                canon_expr(&mut a.body);
            }
        }
        Expr::Block(b) => canon_block(b),
        Expr::While { cond, body } => {
            canon_expr(cond);
            canon_block(body);
        }
        Expr::For { iter, body, .. } => {
            canon_expr(iter);
            canon_block(body);
        }
        Expr::TaggedLit { hole_spans, line, .. } => {
            hole_spans.clear();
            *line = 0;
        }
        _ => {}
    }
}

fn rewrite_cap_method_module(m: &mut Module) {
    seed_local_fns(m);
    for it in &mut m.items {
        match it {
            Item::Function(f) => rewrite_cap_method_block(&mut f.body),
            Item::Const { value, .. } => rewrite_cap_method_expr(value),
            Item::Trait(t) => {
                for method in &mut t.methods {
                    if let Some(body) = &mut method.default {
                        rewrite_cap_method_block(body);
                    }
                }
            }
            Item::Impl(im) => {
                for method in &mut im.methods {
                    rewrite_cap_method_block(&mut method.body);
                }
            }
            Item::Comptime(body) => rewrite_cap_method_block(body),
            Item::Type(_) | Item::TypeAlias { .. } => {}
        }
    }
}

fn rewrite_cap_method_block(b: &mut Block) {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } => {
                rewrite_cap_method_expr(value)
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => rewrite_cap_method_expr(e),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn rewrite_cap_method_expr(e: &mut Expr) {
    match e {
        Expr::Call { name, args } => {
            for arg in args.iter_mut() {
                rewrite_cap_method_expr(arg);
            }
            if !name.contains('.')
                && !local_fn(name)
                && crate::cap_ops::is_op_name(name)
                && !args.is_empty()
            {
                let receiver = Box::new(args.remove(0));
                let method = name.clone();
                let rest = std::mem::take(args);
                *e = Expr::MethodCall {
                    receiver,
                    method,
                    args: rest,
                };
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, arg) in args {
                rewrite_cap_method_expr(arg);
            }
        }
        Expr::List(xs) | Expr::Tuple(xs) | Expr::Ctor { args: xs, .. }
        | Expr::AnonCtor { args: xs, .. } => {
            for x in xs {
                rewrite_cap_method_expr(x);
            }
        }
        Expr::Apply { func, args } => {
            rewrite_cap_method_expr(func);
            for x in args {
                rewrite_cap_method_expr(x);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::Field { base: expr, .. } => rewrite_cap_method_expr(expr),
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_cap_method_expr(lhs);
            rewrite_cap_method_expr(rhs);
        }
        Expr::Range { lo, hi, .. } => {
            rewrite_cap_method_expr(lo);
            rewrite_cap_method_expr(hi);
        }
        Expr::Index { base, index } => {
            rewrite_cap_method_expr(base);
            rewrite_cap_method_expr(index);
        }
        Expr::MethodCall { receiver, args, .. } => {
            rewrite_cap_method_expr(receiver);
            for a in args {
                rewrite_cap_method_expr(a);
            }
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            rewrite_cap_method_expr(scrutinee);
            rewrite_cap_method_block(body);
        }
        Expr::Lambda { body, .. } => rewrite_cap_method_block(body),
        Expr::RecordUpdate { name: _, base, fields } => {
            rewrite_cap_method_expr(base);
            for (_, v) in fields {
                rewrite_cap_method_expr(v);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                rewrite_cap_method_expr(v);
            }
            if let Some(s) = spread {
                rewrite_cap_method_expr(s);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            rewrite_cap_method_expr(cond);
            rewrite_cap_method_block(then_block);
            if let Some(b) = else_block {
                rewrite_cap_method_block(b);
            }
        }
        Expr::Match { scrutinee, arms } => {
            rewrite_cap_method_expr(scrutinee);
            for a in arms {
                if let Some(g) = &mut a.guard {
                    rewrite_cap_method_expr(g);
                }
                rewrite_cap_method_expr(&mut a.body);
            }
        }
        Expr::Block(b) => rewrite_cap_method_block(b),
        Expr::While { cond, body } => {
            rewrite_cap_method_expr(cond);
            rewrite_cap_method_block(body);
        }
        Expr::For { iter, body, .. } => {
            rewrite_cap_method_expr(iter);
            rewrite_cap_method_block(body);
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. }
        | Expr::Var(_) => {}
    }
}

/// Reformat witchy source (brace or off-side) as canonical brace-free source,
/// returning `None` unless the output re-parses and formats to itself
/// (idempotence). That guard makes the printer safe to apply in bulk: anything
/// it cannot yet render faithfully is simply left untouched.
pub fn reformat(src: &str) -> Option<String> {
    let original = crate::parser::parse_module(src).ok()?;
    seed_local_fns(&original);
    let out = module_with_trailing(
        &original,
        &crate::lexer::own_line_comments(src),
        &crate::lexer::trailing_comments(src),
    )?;
    // Two guards, both required:
    //  1. SEMANTICS — the output must parse back to the same program as the
    //     input (modulo the canonicalization `canon_module` applies to both
    //     sides). Idempotence alone is NOT enough: a printer bug that mangles
    //     a construct stably (e.g. printing a placeholder for a shape it
    //     doesn't know) is idempotent and would silently ship wrong code.
    //  2. STABILITY — the output formats to itself, so fmt converges in one
    //     pass and `--check` is meaningful.
    let reparsed = crate::parser::parse_module(&out).ok()?;
    let mut want = original;
    let mut got = reparsed.clone();
    canon_module(&mut want);
    canon_module(&mut got);
    if want != got {
        return None;
    }
    let again = module_with_trailing(
        &reparsed,
        &crate::lexer::own_line_comments(&out),
        &crate::lexer::trailing_comments(&out),
    )?;
    if out == again {
        Some(out)
    } else {
        None
    }
}

/// One-time RFC-0076 migration helper: reformat source and rewrite legacy bare
/// capability ops (`console.print(x)`) to method form (`console.print(x)`).
/// A module-local function/method/trait method with the same name suppresses the
/// rewrite, so ordinary user functions can reclaim names like `read`.
pub fn reformat_cap_methods(src: &str) -> Option<String> {
    let mut target = crate::parser::parse_module(src).ok()?;
    rewrite_cap_method_module(&mut target);
    let out = module_with_trailing(
        &target,
        &crate::lexer::own_line_comments(src),
        &crate::lexer::trailing_comments(src),
    )?;
    let reparsed = crate::parser::parse_module(&out).ok()?;
    let mut want = target;
    let mut got = reparsed.clone();
    canon_module(&mut want);
    canon_module(&mut got);
    if want != got {
        return None;
    }
    let mut again_module = reparsed;
    rewrite_cap_method_module(&mut again_module);
    let again = module_with_trailing(
        &again_module,
        &crate::lexer::own_line_comments(&out),
        &crate::lexer::trailing_comments(&out),
    )?;
    if out == again {
        Some(out)
    } else {
        None
    }
}






/// Escape one static fragment of a tagged literal for re-emission. Like the
/// body of `string_lit` but a bare `$` is escaped only when it would open an
/// interpolation (`${`), so ordinary `$` in markup/SQL survives unchanged.
fn tagged_part(v: &str) -> String {
    let mut s = String::new();
    let chars: Vec<char> = v.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\t' => s.push_str("\\t"),
            '\r' => s.push_str("\\r"),
            '\0' => s.push_str("\\0"),
            '$' if chars.get(i + 1) == Some(&'{') => s.push_str("\\$"),
            _ => s.push(c),
        }
    }
    s
}

fn string_lit(v: &str) -> String {
    let mut s = String::from("\"");
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\t' => s.push_str("\\t"),
            '\r' => s.push_str("\\r"),
            '\0' => s.push_str("\\0"),
            // `$` prints escaped: a bare `${` in the output would re-parse
            // as interpolation — a different program.
            '$' => s.push_str("\\$"),
            _ => s.push(c),
        }
    }
    s.push('"');
    s
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
