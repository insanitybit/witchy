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

const IND: &str = "    ";

/// A cursor over the source's own-line comments, emitted back in order as the
/// printer reaches each anchor line so `witchy fmt` preserves them.
struct Comments<'a> {
    list: &'a [(u32, String)],
    cursor: usize,
}

impl Comments<'_> {
    /// Emit every comment whose source line is before `line`, indented to `depth`.
    fn before(&mut self, s: &mut String, depth: usize, line: u32) {
        while self.cursor < self.list.len() && self.list[self.cursor].0 < line {
            pad(s, depth);
            s.push_str(&self.list[self.cursor].1);
            s.push('\n');
            self.cursor += 1;
        }
    }

    fn remaining(&self) -> bool {
        self.cursor < self.list.len()
    }
}

pub fn module(m: &Module, comments: &[(u32, String)]) -> String {
    let mut s = String::new();
    // Comment placement needs source lines parallel to imports and items; without
    // them (e.g. a linked module) fall back to emitting no comments.
    let have_lines =
        m.import_lines.len() == m.imports.len() && m.item_lines.len() == m.items.len();
    let mut c = Comments {
        list: if have_lines { comments } else { &[] },
        cursor: 0,
    };

    if !m.imports.is_empty() {
        // The comments before the first import are the file header.
        c.before(&mut s, 0, m.import_lines.first().copied().unwrap_or(u32::MAX));
        if !s.is_empty() {
            s.push('\n');
        }
        for imp in &m.imports {
            s.push_str("import ");
            s.push_str(imp);
            s.push('\n');
        }
    }

    for (idx, item) in m.items.iter().enumerate() {
        if !s.is_empty() {
            s.push('\n');
        }
        c.before(&mut s, 0, m.item_lines.get(idx).copied().unwrap_or(u32::MAX));
        item_str(&mut s, item, &mut c);
    }

    // Comments after the last item.
    if c.remaining() && !s.is_empty() {
        s.push('\n');
    }
    c.before(&mut s, 0, u32::MAX);
    s
}

fn pad(s: &mut String, depth: usize) {
    for _ in 0..depth {
        s.push_str(IND);
    }
}

fn item_str(s: &mut String, item: &Item, c: &mut Comments) {
    match item {
        Item::Function(f) => function(s, f, false, c),
        Item::Type(t) => type_def(s, t),
        Item::Trait(t) => trait_def(s, t, c),
        Item::Impl(im) => impl_def(s, im, c),
        Item::Actor(a) => actor_def(s, a, c),
        Item::Const { name, value } => {
            s.push_str("let ");
            s.push_str(name);
            s.push_str(" = ");
            s.push_str(&expr(value));
            s.push('\n');
        }
        Item::TypeAlias { name, ty } => {
            s.push_str("type ");
            s.push_str(name);
            s.push_str(" = ");
            s.push_str(&type_str(ty));
            s.push('\n');
        }
    }
}

fn sig(name: &str, params: &[Param], ret: &Option<Type>, bounds: &[(String, String)]) -> String {
    let mut h = String::new();
    h.push_str(name);
    h.push('(');
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            h.push_str(", ");
        }
        h.push_str(&param(p));
    }
    h.push(')');
    if let Some(r) = ret {
        h.push_str(" -> ");
        h.push_str(&type_str(r));
    }
    if !bounds.is_empty() {
        h.push_str(" where ");
        for (i, (v, t)) in bounds.iter().enumerate() {
            if i > 0 {
                h.push_str(", ");
            }
            h.push_str(v);
            h.push_str(": ");
            h.push_str(t);
        }
    }
    h
}

fn param(p: &Param) -> String {
    let conv = match p.convention {
        Convention::Let => "",
        Convention::Inout => "inout ",
        Convention::Sink => "sink ",
    };
    match &p.ty {
        Some(t) => format!("{conv}{}: {}", p.name, type_str(t)),
        None => format!("{conv}{}", p.name),
    }
}

fn function(s: &mut String, f: &Function, indented: bool, c: &mut Comments) {
    let depth = if indented { 1 } else { 0 };
    pad(s, depth);
    if f.public {
        s.push_str("pub ");
    }
    s.push_str("fn ");
    s.push_str(&sig(&f.name, &f.params, &f.ret, &f.bounds));
    s.push_str(":\n");
    block(s, &f.body, depth + 1, c);
}

fn type_def(s: &mut String, t: &TypeDef) {
    s.push_str("type ");
    s.push_str(&t.name);
    s.push_str(":\n");
    let is_record = t.variants.len() == 1 && !t.variants[0].field_names.is_empty();
    if is_record {
        let v = &t.variants[0];
        for (n, ty) in v.field_names.iter().zip(&v.fields) {
            pad(s, 1);
            s.push_str(n);
            s.push_str(": ");
            s.push_str(&type_str(ty));
            s.push('\n');
        }
    } else {
        for v in &t.variants {
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
    }
}

fn trait_def(s: &mut String, t: &TraitDef, c: &mut Comments) {
    s.push_str("trait ");
    s.push_str(&t.name);
    s.push_str(":\n");
    for m in &t.methods {
        pad(s, 1);
        s.push_str("fn ");
        s.push_str(&sig(&m.name, &m.params, &m.ret, &[]));
        match &m.default {
            Some(b) => {
                s.push_str(":\n");
                block(s, b, 2, c);
            }
            None => s.push('\n'),
        }
    }
}

fn impl_def(s: &mut String, im: &ImplDef, c: &mut Comments) {
    s.push_str("impl ");
    if let Some(t) = &im.trait_name {
        s.push_str(t);
        s.push_str(" for ");
    }
    s.push_str(&im.type_name);
    s.push_str(":\n");
    for (i, m) in im.methods.iter().enumerate() {
        if i > 0 {
            s.push('\n');
        }
        function(s, m, true, c);
    }
    for h in &im.handlers {
        handler(s, h, c);
    }
}

fn actor_def(s: &mut String, a: &ActorDef, c: &mut Comments) {
    s.push_str("actor ");
    s.push_str(&a.name);
    s.push_str(":\n");
    for f in &a.fields {
        pad(s, 1);
        if f.mutable {
            s.push_str("var ");
        }
        s.push_str(&f.name);
        s.push_str(": ");
        s.push_str(&type_str(&f.ty));
        if let Some(init) = &f.init {
            s.push_str(" = ");
            s.push_str(&expr(init));
        }
        s.push('\n');
    }
    // Handlers go in a separate `impl Actor:` block (re-merged on parse).
    if !a.handlers.is_empty() {
        s.push_str("\nimpl ");
        s.push_str(&a.name);
        s.push_str(":\n");
        for h in &a.handlers {
            handler(s, h, c);
        }
    }
}

fn handler(s: &mut String, h: &Handler, c: &mut Comments) {
    pad(s, 1);
    s.push_str("on ");
    s.push_str(&h.message);
    s.push('(');
    for (i, p) in h.params.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&param(p));
    }
    s.push_str("):\n");
    block(s, &h.body, 2, c);
}

fn block(s: &mut String, b: &Block, depth: usize, c: &mut Comments) {
    if b.stmts.is_empty() {
        // An empty block has no off-side representation; emit a no-op expression.
        pad(s, depth);
        s.push_str("0\n");
        return;
    }
    for (i, st) in b.stmts.iter().enumerate() {
        // Own-line comments that preceded this statement in the source.
        if let Some(line) = b.lines.get(i) {
            c.before(s, depth, *line);
        }
        stmt(s, st, depth, c);
    }
}

fn stmt(s: &mut String, st: &Stmt, depth: usize, c: &mut Comments) {
    match st {
        Stmt::Let { name, mutable, value } => {
            pad(s, depth);
            s.push_str(if *mutable { "var " } else { "let " });
            s.push_str(name);
            s.push_str(" = ");
            value_or_block(s, value, depth, c);
        }
        Stmt::Assign { name, value } => {
            pad(s, depth);
            s.push_str(name);
            s.push_str(" = ");
            value_or_block(s, value, depth, c);
        }
        Stmt::LetTuple { names, value } => {
            pad(s, depth);
            s.push_str("let (");
            s.push_str(&names.join(", "));
            s.push_str(") = ");
            value_or_block(s, value, depth, c);
        }
        Stmt::Return(Some(e)) => {
            pad(s, depth);
            s.push_str("return ");
            value_or_block(s, e, depth, c);
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
        Stmt::Expr(e) => block_stmt(s, e, depth, c),
    }
}

/// A statement-position expression: control-flow forms expand multi-line.
fn block_stmt(s: &mut String, e: &Expr, depth: usize, c: &mut Comments) {
    match e {
        Expr::If { .. }
        | Expr::Match { .. }
        | Expr::While { .. }
        | Expr::WhileLet { .. }
        | Expr::For { .. } => {
            pad(s, depth);
            multiline(s, e, depth, c);
        }
        Expr::Block(b) => block(s, b, depth, c),
        Expr::Lambda { params, body } => {
            pad(s, depth);
            lambda_at(s, params, body, depth, c);
        }
        _ => {
            pad(s, depth);
            s.push_str(&expr(e));
            s.push('\n');
        }
    }
}

/// The right-hand side of a `let`/`=`/`return`: use a multi-line form when the
/// value is a `match` (no inline form) or a lambda with a block body, else an
/// inline expr.
fn value_or_block(s: &mut String, e: &Expr, depth: usize, c: &mut Comments) {
    match e {
        Expr::Match { .. } => {
            multiline(s, e, depth, c);
        }
        Expr::Lambda { params, body } => {
            lambda_at(s, params, body, depth, c);
        }
        _ => {
            s.push_str(&expr(e));
            s.push('\n');
        }
    }
}

/// Emit a control-flow expression across multiple lines. `s` is already padded to
/// the header position for `if`/`while`/`for`; for `match` it is positioned after
/// `= ` so we do not pre-pad.
fn multiline(s: &mut String, e: &Expr, depth: usize, c: &mut Comments) {
    match e {
        Expr::If { cond, then_block, else_block } => {
            s.push_str("if ");
            s.push_str(&expr(cond));
            s.push_str(":\n");
            block(s, then_block, depth + 1, c);
            if let Some(eb) = else_block {
                pad(s, depth);
                // `else if` chain: a single nested if statement.
                if eb.stmts.len() == 1 {
                    if let Stmt::Expr(inner @ Expr::If { .. }) = &eb.stmts[0] {
                        s.push_str("else ");
                        multiline(s, inner, depth, c);
                        return;
                    }
                }
                s.push_str("else:\n");
                block(s, eb, depth + 1, c);
            }
        }
        Expr::While { cond, body } => {
            s.push_str("while ");
            s.push_str(&expr(cond));
            s.push_str(":\n");
            block(s, body, depth + 1, c);
        }
        Expr::WhileLet { pattern: pat, scrutinee, body } => {
            s.push_str("while let ");
            s.push_str(&pattern(pat));
            s.push_str(" = ");
            s.push_str(&expr(scrutinee));
            s.push_str(":\n");
            block(s, body, depth + 1, c);
        }
        Expr::For { var, iter, body } => {
            s.push_str("for ");
            s.push_str(var);
            s.push_str(" in ");
            s.push_str(&expr(iter));
            s.push_str(":\n");
            block(s, body, depth + 1, c);
        }
        Expr::Match { scrutinee, arms } => {
            // An empty wildcard arm body can only arise from desugaring an
            // `if let` without `else` (no other construct yields an empty block,
            // which has no off-side surface form). Render it back as `if let`,
            // which re-parses to exactly this match.
            if let [then_arm, else_arm] = arms.as_slice() {
                if then_arm.guard.is_none()
                    && else_arm.guard.is_none()
                    && else_arm.pattern == Pattern::Wildcard
                    && matches!(&else_arm.body, Expr::Block(b) if b.stmts.is_empty())
                {
                    if let Expr::Block(tb) = &then_arm.body {
                        s.push_str("if let ");
                        s.push_str(&pattern(&then_arm.pattern));
                        s.push_str(" = ");
                        s.push_str(&expr(scrutinee));
                        s.push_str(":\n");
                        block(s, tb, depth + 1, c);
                        return;
                    }
                }
            }
            s.push_str("match ");
            s.push_str(&expr(scrutinee));
            s.push_str(":\n");
            for a in arms {
                pad(s, depth + 1);
                s.push_str(&pattern(&a.pattern));
                if let Some(g) = &a.guard {
                    s.push_str(" if ");
                    s.push_str(&expr(g));
                }
                s.push_str(" ->");
                arm_body(s, &a.body, depth + 1, c);
            }
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
fn arm_body(s: &mut String, body: &Expr, depth: usize, c: &mut Comments) {
    match body {
        Expr::Block(b) => {
            s.push('\n');
            block(s, b, depth + 1, c);
        }
        Expr::Match { .. } => {
            s.push(' ');
            multiline(s, body, depth, c);
        }
        Expr::Lambda { params, body } => {
            s.push(' ');
            lambda_at(s, params, body, depth, c);
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
        Expr::Bool(b) => b.to_string(),
        Expr::Var(n) => n.clone(),
        Expr::List(xs) => format!("[{}]", comma(xs)),
        Expr::Tuple(xs) => format!("({})", comma(xs)),
        Expr::Call { name, args } => format!("{name}({})", comma(args)),
        Expr::Ctor { name, args } => {
            if args.is_empty() {
                name.clone()
            } else {
                format!("{name}({})", comma(args))
            }
        }
        Expr::Apply { func, args } => {
            format!("{}({})", operand(func, POSTFIX_PREC, false), comma(args))
        }
        Expr::Field { base, field } => format!("{}.{field}", operand(base, POSTFIX_PREC, false)),
        Expr::Unary { op, expr: inner } => {
            let o = match op {
                UnOp::Neg => "-",
                UnOp::Not => "!",
                UnOp::BitNot => "~",
            };
            format!("{o}{}", operand(inner, UNARY_PREC, false))
        }
        Expr::Binary { op, lhs, rhs } => {
            let p = binop_prec(*op);
            format!("{} {} {}", operand(lhs, p, false), binop(*op), operand(rhs, p, true))
        }
        Expr::Range { lo, hi, inclusive } => {
            let op = if *inclusive { "..=" } else { ".." };
            format!("{}{op}{}", operand(lo, RANGE_PREC, false), operand(hi, RANGE_PREC, true))
        }
        Expr::Index { base, index } => {
            format!("{}[{}]", operand(base, POSTFIX_PREC, false), expr(index))
        }
        Expr::Try(inner) => format!("{}?", operand(inner, POSTFIX_PREC, false)),
        Expr::As { expr, ty } => format!("{} as {}", operand(expr, POSTFIX_PREC, false), type_str(ty)),
        Expr::Lambda { params, body } => {
            let ps: Vec<String> = params.iter().map(param).collect();
            format!("fn({}): {}", ps.join(", "), block_value(body))
        }
        Expr::If { cond, then_block, else_block } => {
            let e = else_block
                .as_ref()
                .map(|b| format!(" else: {}", block_value(b)))
                .unwrap_or_default();
            format!("if {}: {}{}", expr(cond), block_value(then_block), e)
        }
        Expr::RecordUpdate { base, fields } => {
            let fs: Vec<String> = fields
                .iter()
                .map(|(n, v)| format!("{n} = {}", expr(v)))
                .collect();
            format!("update {}: {}", expr(base), fs.join(" "))
        }
        Expr::Spawn { actor, args } => format!("spawn {actor}({})", comma(args)),
        // No inline form — caller should have routed these multi-line. Emit a
        // best-effort block expression so output still parses.
        Expr::Match { .. }
        | Expr::While { .. }
        | Expr::WhileLet { .. }
        | Expr::For { .. }
        | Expr::Block(_) => "0".to_string(),
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
fn lambda_at(s: &mut String, params: &[Param], body: &Block, depth: usize, c: &mut Comments) {
    let ps: Vec<String> = params.iter().map(param).collect();
    s.push_str("fn(");
    s.push_str(&ps.join(", "));
    s.push(')');
    match block_value_opt(body) {
        Some(inline) => {
            s.push_str(": ");
            s.push_str(&inline);
            s.push('\n');
        }
        None => {
            s.push_str(":\n");
            block(s, body, depth + 1, c);
        }
    }
}

fn comma(xs: &[Expr]) -> String {
    xs.iter().map(expr).collect::<Vec<_>>().join(", ")
}

fn binop(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Concat => "<>",
        BinOp::Eq => "==",
        BinOp::NotEq => "!=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
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
        BinOp::Or => 3,
        BinOp::And => 5,
        BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => 7,
        BinOp::BitOr => 9,
        BinOp::BitXor => 11,
        BinOp::BitAnd => 13,
        BinOp::Shl | BinOp::Shr => 15,
        BinOp::Add | BinOp::Sub | BinOp::Concat => 17,
        BinOp::Mul | BinOp::Div | BinOp::Mod => 19,
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
        Expr::Unary { .. } => UNARY_PREC,
        Expr::Field { .. } | Expr::Try(_) | Expr::Apply { .. } | Expr::As { .. } | Expr::Index { .. } => POSTFIX_PREC,
        _ => 100,
    }
}

/// Render `e` as an operand of a binary operator with left binding power
/// `parent`. All binary operators are left-associative, so the right operand is
/// wrapped at equal precedence (`a - (b - c)`), the left operand only when it
/// binds strictly looser (`a - b - c` stays flat).
fn operand(e: &Expr, parent: u8, is_right: bool) -> String {
    let s = expr(e);
    let needs = if is_right { expr_prec(e) <= parent } else { expr_prec(e) < parent };
    if needs {
        format!("({s})")
    } else {
        s
    }
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
    }
}

fn type_str(t: &Type) -> String {
    match t {
        Type::Named(n, args) if args.is_empty() => n.clone(),
        // Capability rights use bracket syntax (`Dir[Read]`, `Net[Connect]`);
        // ordinary generic types use parens (`List(Int)`, `Option(T)`).
        Type::Named(n, args) if n == "Dir" || n == "Net" => {
            format!("{n}[{}]", args.iter().map(type_str).collect::<Vec<_>>().join(", "))
        }
        Type::Named(n, args) => {
            format!("{n}({})", args.iter().map(type_str).collect::<Vec<_>>().join(", "))
        }
        Type::Tuple(ts) => {
            format!("({})", ts.iter().map(type_str).collect::<Vec<_>>().join(", "))
        }
        Type::Fn(ps, r) => {
            format!("fn({}) -> {}", ps.iter().map(type_str).collect::<Vec<_>>().join(", "), type_str(r))
        }
    }
}

/// Reformat witchy source (brace or off-side) as canonical brace-free source,
/// returning `None` unless the result re-parses to the *same* AST. The round-trip
/// guard makes the printer safe to apply in bulk: anything it cannot yet render
/// faithfully is simply left untouched.
pub fn reformat(src: &str) -> Option<String> {
    let mut original = crate::parser::parse_module(src).ok()?;
    let out = module(&original, &crate::lexer::own_line_comments(src));
    let mut reparsed = crate::parser::parse_module(&out).ok()?;
    strip_lines_module(&mut original);
    strip_lines_module(&mut reparsed);
    if original == reparsed {
        Some(out)
    } else {
        None
    }
}

fn strip_lines_module(m: &mut Module) {
    // The source-line arrays are formatting metadata, not part of the program;
    // clear them so the round-trip comparison ignores comment-induced line shifts.
    m.import_lines.clear();
    m.item_lines.clear();
    for it in &mut m.items {
        strip_lines_item(it);
    }
}

fn strip_lines_item(it: &mut Item) {
    match it {
        Item::Function(f) => strip_lines_block(&mut f.body),
        Item::Actor(a) => {
            for fld in &mut a.fields {
                if let Some(e) = &mut fld.init {
                    strip_lines_expr(e);
                }
            }
            for h in &mut a.handlers {
                strip_lines_block(&mut h.body);
            }
        }
        Item::Type(_) | Item::TypeAlias { .. } => {}
        Item::Const { value, .. } => strip_lines_expr(value),
        Item::Trait(t) => {
            for m in &mut t.methods {
                if let Some(b) = &mut m.default {
                    strip_lines_block(b);
                }
            }
        }
        Item::Impl(im) => {
            for f in &mut im.methods {
                strip_lines_block(&mut f.body);
            }
            for h in &mut im.handlers {
                strip_lines_block(&mut h.body);
            }
        }
    }
}

fn strip_lines_block(b: &mut Block) {
    b.lines.clear();
    for s in &mut b.stmts {
        strip_lines_stmt(s);
    }
}

fn strip_lines_stmt(s: &mut Stmt) {
    match s {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetTuple { value, .. } => {
            strip_lines_expr(value)
        }
        Stmt::Return(Some(e)) | Stmt::Expr(e) => strip_lines_expr(e),
        _ => {}
    }
}

fn strip_lines_expr(e: &mut Expr) {
    match e {
        Expr::List(xs) | Expr::Tuple(xs) | Expr::Call { args: xs, .. }
        | Expr::Ctor { args: xs, .. } | Expr::Spawn { args: xs, .. } => {
            for x in xs {
                strip_lines_expr(x);
            }
        }
        Expr::Apply { func, args } => {
            strip_lines_expr(func);
            for x in args {
                strip_lines_expr(x);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            strip_lines_expr(expr)
        }
        Expr::Binary { lhs, rhs, .. } => {
            strip_lines_expr(lhs);
            strip_lines_expr(rhs);
        }
        Expr::Range { lo, hi, .. } => {
            strip_lines_expr(lo);
            strip_lines_expr(hi);
        }
        Expr::Index { base, index } => {
            strip_lines_expr(base);
            strip_lines_expr(index);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            strip_lines_expr(scrutinee);
            strip_lines_block(body);
        }
        Expr::Lambda { body, .. } => strip_lines_block(body),
        Expr::RecordUpdate { base, fields } => {
            strip_lines_expr(base);
            for (_, v) in fields {
                strip_lines_expr(v);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            strip_lines_expr(cond);
            strip_lines_block(then_block);
            if let Some(b) = else_block {
                strip_lines_block(b);
            }
        }
        Expr::Match { scrutinee, arms } => {
            strip_lines_expr(scrutinee);
            for a in arms {
                if let Some(g) = &mut a.guard {
                    strip_lines_expr(g);
                }
                strip_lines_expr(&mut a.body);
            }
        }
        Expr::Block(b) => strip_lines_block(b),
        Expr::While { cond, body } => {
            strip_lines_expr(cond);
            strip_lines_block(body);
        }
        Expr::For { iter, body, .. } => {
            strip_lines_expr(iter);
            strip_lines_block(body);
        }
        _ => {}
    }
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
            _ => s.push(c),
        }
    }
    s.push('"');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrips(src: &str) -> bool {
        reformat(src).is_some()
    }

    #[test]
    fn reformats_every_std_and_example_to_an_equal_ast() {
        // The printer must faithfully round-trip every shipped source file.
        let dirs = ["std", "examples"];
        let mut failures = Vec::new();
        for dir in dirs {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().and_then(|e| e.to_str()) != Some("witchy") {
                    continue;
                }
                let src = std::fs::read_to_string(&path).unwrap();
                if crate::parser::parse_module(&src).is_ok() && !roundtrips(&src) {
                    failures.push(path.display().to_string());
                }
            }
        }
        assert!(failures.is_empty(), "did not round-trip: {failures:?}");
    }

    #[test]
    fn preserves_ranges() {
        // Ranges used to fail to format (they desugared to a synthetic block at
        // parse time); now they round-trip and print back as `lo..hi` / `lo..=hi`,
        // including when used as a value or with operator operands.
        let src = "fn main(console: Console):\n    for i in 0..3:\n        print(console, int_to_string(i))\n    let xs = 1..=n\n    let ys = a + 1..b * 2\n";
        let out = reformat(src).expect("ranges round-trip");
        assert!(out.contains("for i in 0..3:"), "{out}");
        assert!(out.contains("let xs = 1..=n"), "{out}");
        // Operator operands bind tighter than `..`, so they need no parentheses.
        assert!(out.contains("let ys = a + 1..b * 2"), "{out}");
    }

    #[test]
    fn preserves_subscripts() {
        // Subscripts used to de-sugar to `at(xs, i)` on format; now they round-trip
        // and print back as `base[index]`, including nested and computed indices.
        let src = "fn main(console: Console):\n    let xs = [1, 2, 3]\n    let grid = [[1], [2]]\n    print(console, int_to_string(xs[0] + grid[1][0]))\n";
        let out = reformat(src).expect("subscripts round-trip");
        assert!(out.contains("xs[0]"), "{out}");
        assert!(out.contains("grid[1][0]"), "{out}");
        assert!(!out.contains("at("), "subscripts must not de-sugar to at(): {out}");
    }

    #[test]
    fn preserves_while_let() {
        // `while let` used to de-sugar to `while true / match / break` on format;
        // now it round-trips and prints back as `while let PAT = SCRUT:`.
        let src = "fn main(console: Console):\n    var o = Some(1)\n    while let Some(n) = o:\n        print(console, int_to_string(n))\n        o = None\n";
        let out = reformat(src).expect("while let round-trips");
        assert!(out.contains("while let Some(n) = o:"), "{out}");
        assert!(!out.contains("while true"), "while let must not de-sugar: {out}");
        assert!(!out.contains("match o"), "while let must not de-sugar: {out}");
    }

    #[test]
    fn preserves_top_level_comments() {
        // The header (before imports) and a doc comment before an item survive
        // formatting, attached in the right place.
        let src = "// header one\n// header two\n\nimport string\n\n// doc for f\nfn f() -> Int:\n    5\n";
        let out = reformat(src).expect("round-trips");
        assert!(out.contains("// header one\n// header two"), "{out}");
        assert!(out.contains("// doc for f\nfn f"), "{out}");
        // The header stays above the import.
        assert!(out.find("// header one").unwrap() < out.find("import string").unwrap(), "{out}");
    }

    #[test]
    fn preserves_in_body_and_nested_comments() {
        let src = "fn main(console: Console):\n    // before x\n    let x = 5\n    while x > 0:\n        // inside loop\n        x = x - 1\n";
        let out = reformat(src).expect("round-trips");
        assert!(out.contains("    // before x\n    let x = 5"), "{out}");
        // The nested comment keeps the loop body's indentation.
        assert!(out.contains("        // inside loop\n        x = x - 1"), "{out}");
    }

    #[test]
    fn formatting_is_idempotent() {
        // Formatting already-formatted code must be a no-op: `fmt(fmt(x)) == fmt(x)`.
        let dirs = ["std", "examples"];
        let mut failures = Vec::new();
        for dir in dirs {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().and_then(|e| e.to_str()) != Some("witchy") {
                    continue;
                }
                let src = std::fs::read_to_string(&path).unwrap();
                if let Some(once) = reformat(&src) {
                    if reformat(&once).as_deref() != Some(once.as_str()) {
                        failures.push(path.display().to_string());
                    }
                }
            }
        }
        assert!(failures.is_empty(), "formatting is not idempotent: {failures:?}");
    }

    #[test]
    fn block_body_lambdas_round_trip() {
        // A closure with a multi-statement body, and one whose body is a `match`,
        // now format as block-form lambdas and re-parse to the same AST.
        let multi = "fn make(n: Int) -> fn(Int) -> Int:\n    fn(x: Int):\n        let y = (x + n)\n        (y * 2)\n";
        let out = reformat(multi).expect("multi-statement closure round-trips");
        assert!(!out.contains('{'), "braces: {out}");

        let matchy = "type Opt:\n    Some(a)\n    None\n\nfn classify() -> fn(Opt(Int)) -> Int:\n    fn(o: Opt(Int)):\n        match o:\n            Some(n) -> n\n            None -> 0\n";
        assert!(reformat(matchy).is_some(), "match-body closure should round-trip");
    }

    #[test]
    fn reformat_is_idempotent_and_brace_free() {
        let src = "fn classify(n: Int) -> String:\n    if n > 0: \"pos\" else: \"non-pos\"\n\nfn main(console: Console):\n    print(console, classify(5))\n";
        let out = reformat(src).expect("round-trips");
        assert!(!out.contains('{'), "still has braces: {out}");
        assert_eq!(reformat(&out).unwrap(), out, "not idempotent");
    }
}
