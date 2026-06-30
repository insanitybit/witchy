//! The uniqueness pass: one ownership analysis behind the compiled tier's
//! in-place fast paths. See rfcs/ownership-analysis.md.
//!
//! The in-place machinery is gated by a runtime ownership token (the shadow
//! `__cap` local: zero = no owned slack, so the next self-assign COPIES into
//! a fresh buffer and thereby re-owns it). The token is **self-healing** —
//! one copy re-establishes ownership — which shapes this analysis: it does
//! not prove uniqueness at every site. It finds
//!
//!  1. **accumulators** — variables with a self-assign accumulation shape
//!     anywhere in the body (each gets a token),
//!  2. **share events** — statements that can create a *live whole-alias* of
//!     an accumulator's buffer (the consumer zeroes the token there), and
//!  3. **dirty sites** — self-assigns whose own right-hand side embeds a
//!     share of the assigned variable (the site runs with a forced zero
//!     token and re-owns through the copy).
//!
//! Everything else is the runtime token's job. A spuriously-zeroed token
//! costs one copy; a token left live across a share would be silent
//! corruption — so every classification below defaults to "share" and earns
//! precision case by case (the builtin effect table, `let`-borrow
//! certification, function summaries).
//!
//! Facts are keyed by statement IDENTITY (`&Stmt as *const _`): the consumer
//! must compile the exact AST instance it analyzed, and asserts afterwards
//! that every kill was consumed — a cloned-subtree bug surfaces as a loud
//! compile error, never a lost kill.

use std::collections::{HashMap, HashSet};

use witchy_syntax::ast::{BinOp, Block, Convention, Expr, Item, Module, Stmt, Type, UnOp};

/// Why an accumulation site reverts to the copying path — surfaced as a
/// check-time note and an LSP hint. Only emitted when the cost repeats (the
/// share or dirty site sits inside a loop that also accumulates the var).
#[derive(Debug, Clone)]
pub struct Cliff {
    pub var: String,
    pub line: u32,
    pub reason: String,
}

#[derive(Default)]
pub struct Facts {
    /// Variables that self-assign-accumulate somewhere in this compile unit
    /// (function, handler, or lambda body — lambda interiors are their own
    /// units): each gets a shadow `__cap` ownership token.
    pub accumulators: HashSet<String>,
    /// Statement identity -> accumulators whose token must be zeroed AFTER
    /// the statement (it can create a live whole-alias of their buffer).
    kills: HashMap<usize, Vec<String>>,
    /// Self-assign sites (statement identity) whose right-hand side embeds a
    /// share of the assigned variable: forced zero token.
    dirty: HashSet<usize>,
    /// Total kill ENTRIES handed out, for the consumption check.
    pub kill_entries: usize,
    /// Total self-assign SITES seen on accumulators, for the same check (a
    /// cloned-subtree site would otherwise silently miss its dirty flag).
    pub site_entries: usize,
    /// Diagnostics: repeated-cost reverts (see `Cliff`).
    pub cliffs: Vec<Cliff>,
}

fn stmt_key(s: &Stmt) -> usize {
    s as *const Stmt as usize
}

impl Facts {
    pub fn kills_after(&self, stmt: &Stmt) -> &[String] {
        self.kills.get(&stmt_key(stmt)).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Whether a self-assign site must run with a forced zero token. A
    /// missed lookup (unknown statement) answers DIRTY — the sound default.
    pub fn is_dirty(&self, stmt: &Stmt) -> bool {
        self.dirty.contains(&stmt_key(stmt))
    }
}

// ---------------------------------------------------------------------------
// The self-assign accumulation shapes. These define WHICH operation runs in
// place; the analysis decides WHETHER the token is live. (Owned here so the
// shape definitions and their soundness analysis live together; codegen and
// the interpreter import them.)
// ---------------------------------------------------------------------------

/// `xs = push(xs, e)`: the appended element.
pub fn self_push_elem<'a>(name: &str, value: &'a Expr) -> Option<&'a Expr> {
    if let Expr::Call { name: f, args } = value {
        if f == "list.push" && args.len() == 2 {
            if matches!(&args[0], Expr::Var(v) if v == name) {
                return Some(&args[1]);
            }
        }
    }
    None
}

/// `d = insert(d, k, v)`: the key and value.
pub fn self_insert_args<'a>(name: &str, value: &'a Expr) -> Option<(&'a Expr, &'a Expr)> {
    if let Expr::Call { name: f, args } = value {
        if f == "dict.insert" && args.len() == 3 {
            if matches!(&args[0], Expr::Var(v) if v == name) {
                return Some((&args[1], &args[2]));
            }
        }
    }
    None
}

/// `d = update(d, k, default, f)`: the key, default, and updater.
pub fn self_update_args<'a>(
    name: &str,
    value: &'a Expr,
) -> Option<(&'a Expr, &'a Expr, &'a Expr)> {
    if let Expr::Call { name: f, args } = value {
        if f == "dict.update" && args.len() == 4 {
            if matches!(&args[0], Expr::Var(v) if v == name) {
                return Some((&args[1], &args[2], &args[3]));
            }
        }
    }
    None
}

/// `xs = set_at(xs, i, v)`: the index and the new value. Unlike `list.push`
/// (a builtin with a stable name), `list.set_at` is an ordinary stdlib function,
/// so by codegen time the call is monomorphized to `list.set_at__<ElemType>`;
/// match that suffixed form as well as the bare name.
pub fn self_set_at<'a>(name: &str, value: &'a Expr) -> Option<(&'a Expr, &'a Expr)> {
    if let Expr::Call { name: f, args } = value {
        if (f == "list.set_at" || f.starts_with("list.set_at__")) && args.len() == 3 {
            if matches!(&args[0], Expr::Var(v) if v == name) {
                return Some((&args[1], &args[2]));
            }
        }
    }
    None
}

/// `xs = update_at(xs, i, f)`: the index and the updater closure. Like
/// [`self_set_at`], `list.update_at` is a monomorphized stdlib function.
pub fn self_update_at<'a>(name: &str, value: &'a Expr) -> Option<(&'a Expr, &'a Expr)> {
    if let Expr::Call { name: f, args } = value {
        if (f == "list.update_at" || f.starts_with("list.update_at__")) && args.len() == 3 {
            if matches!(&args[0], Expr::Var(v) if v == name) {
                return Some((&args[1], &args[2]));
            }
        }
    }
    None
}

/// `s = s + a + b + …` (any left spine whose leftmost leaf is the assigned
/// variable): the appended pieces, in order.
pub fn self_concat_pieces<'a>(name: &str, value: &'a Expr) -> Option<Vec<&'a Expr>> {
    let mut pieces: Vec<&'a Expr> = Vec::new();
    let mut cur = value;
    loop {
        match cur {
            Expr::Binary { op: BinOp::Concat, lhs, rhs } => {
                pieces.push(rhs);
                cur = lhs;
            }
            Expr::Var(v) if v == name && !pieces.is_empty() => {
                pieces.reverse();
                return Some(pieces);
            }
            _ => return None,
        }
    }
}

/// `x = f(move x)` against an own-ABI callee: the call continues `x`'s
/// linear pipeline (the ownership token crosses the call both ways).
pub fn self_own_call<'a>(
    name: &str,
    value: &'a Expr,
    summaries: &Summaries,
) -> Option<(&'a str, usize)> {
    if let Expr::Call { name: f, args } = value {
        let idx = summaries.own_abi(f)?;
        let arg = args.get(idx)?;
        let inner = match arg {
            Expr::Unary { op: UnOp::Move, expr } => expr.as_ref(),
            other => other,
        };
        if matches!(inner, Expr::Var(v) if v == name) {
            return Some((f.as_str(), idx));
        }
    }
    None
}

pub fn is_self_assign_shape(name: &str, value: &Expr, summaries: &Summaries) -> bool {
    self_inplace_op(name, value).is_some() || self_own_call(name, value, summaries).is_some()
}

/// The recognized in-place self-assign accumulation operations (`x = f(x, …)`),
/// unified so codegen consumes ONE shape via a single match instead of a
/// near-identical per-method arm each. (The own-ABI self-call, [`self_own_call`],
/// is intentionally NOT here — it threads the ownership token through a user
/// function rather than a builtin in-place helper, so codegen handles it apart.)
pub enum InPlaceOp<'a> {
    Push(&'a Expr),
    SetAt(&'a Expr, &'a Expr),
    UpdateAt(&'a Expr, &'a Expr),
    Insert(&'a Expr, &'a Expr),
    Update(&'a Expr, &'a Expr, &'a Expr),
    Concat(Vec<&'a Expr>),
}

/// Recover the single in-place accumulation shape of `x = f(x, …)` (or `None`).
/// One entry point replacing the per-method `self_*().is_some()` cascade in codegen.
pub fn self_inplace_op<'a>(name: &str, value: &'a Expr) -> Option<InPlaceOp<'a>> {
    if let Some(e) = self_push_elem(name, value) {
        return Some(InPlaceOp::Push(e));
    }
    if let Some((i, v)) = self_set_at(name, value) {
        return Some(InPlaceOp::SetAt(i, v));
    }
    if let Some((i, f)) = self_update_at(name, value) {
        return Some(InPlaceOp::UpdateAt(i, f));
    }
    if let Some((k, v)) = self_insert_args(name, value) {
        return Some(InPlaceOp::Insert(k, v));
    }
    if let Some((k, d, f)) = self_update_args(name, value) {
        return Some(InPlaceOp::Update(k, d, f));
    }
    if let Some(pieces) = self_concat_pieces(name, value) {
        return Some(InPlaceOp::Concat(pieces));
    }
    None
}

// ---------------------------------------------------------------------------
// Function summaries: can a call leave a live whole-alias of an argument
// observable by the caller (returned, embedded in the return value, or
// written back through an `var` parameter)?
// ---------------------------------------------------------------------------

struct FnInfo {
    convs: Vec<Convention>,
    may_alias_out: Vec<bool>,
    /// `Some(i)`: this function carries the own-ABI — parameter `i` is its
    /// single `own` collection parameter, the function has no `var`
    /// parameters, and the return value may alias that parameter. The
    /// ownership token travels across the call (extra i32 cap param + extra
    /// i32 cap result), so `x = f(move x)` pipelines keep their owned slack.
    own_abi: Option<usize>,
}

pub struct Summaries {
    fns: HashMap<String, FnInfo>,
}

impl Summaries {
    /// No information: every call is assumed to alias every argument out.
    pub fn empty() -> Self {
        Summaries { fns: HashMap::new() }
    }

    /// The full bottom-up pass over the module's call graph: a parameter may
    /// alias out only if some occurrence of it flows somewhere live —
    /// returned (whole or embedded), written back through `var`, captured
    /// by a closure, or passed on to a callee position that itself aliases
    /// out. Optimistic least fixpoint: aliasing needs a syntactic source, so
    /// cycles with no source stay clean (`let`-style read recursion).
    pub fn of_module(module: &Module) -> Self {
        let mut fns: HashMap<String, FnInfo> = HashMap::new();
        let mut bodies: HashMap<String, &witchy_syntax::ast::Function> = HashMap::new();
        for item in &module.items {
            if let Item::Function(f) = item {
                fns.insert(
                    f.name.clone(),
                    FnInfo {
                        convs: f.params.iter().map(|p| p.convention).collect(),
                        // Optimistic start; `let` borrows are certified clean
                        // by typeck (a borrow cannot be returned) and stay so.
                        may_alias_out: vec![false; f.params.len()],
                        own_abi: None,
                    },
                );
                bodies.insert(f.name.clone(), f);
            }
        }
        let mut summaries = Summaries { fns };
        loop {
            let mut changed = false;
            for (name, f) in &bodies {
                for (i, p) in f.params.iter().enumerate() {
                    if p.convention == Convention::Borrow {
                        continue; // typeck: a `let` borrow cannot escape.
                    }
                    if summaries.fns[name.as_str()].may_alias_out[i] {
                        continue;
                    }
                    if param_flows_out(&f.body, &p.name, &summaries) {
                        summaries
                            .fns
                            .get_mut(name.as_str())
                            .unwrap()
                            .may_alias_out[i] = true;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        // The own-ABI: decided AFTER the alias fixpoint, identically wherever
        // this module compiles (callers and the definition must agree
        // on every function's signature).
        for (name, f) in &bodies {
            let owns: Vec<usize> = f
                .params
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    p.convention == Convention::Own
                        && matches!(&p.ty, Some(Type::Named(n, _)) if n == "List" || n == "Dict" || n == "String")
                })
                .map(|(i, _)| i)
                .collect();
            let has_var = f.params.iter().any(|p| p.convention == Convention::Var);
            if let [i] = owns.as_slice() {
                if !has_var && summaries.fns[name.as_str()].may_alias_out[*i] {
                    summaries.fns.get_mut(name.as_str()).unwrap().own_abi = Some(*i);
                }
            }
        }
        summaries
    }

    /// The own-ABI decision for a callee (see `FnInfo::own_abi`).
    pub fn own_abi(&self, name: &str) -> Option<usize> {
        self.fns.get(name).and_then(|f| f.own_abi)
    }

    /// Is the value passed in argument position `idx` of a call to `name`
    /// LIVE after the call (a whole-alias the caller can observe)?
    fn arg_live(&self, name: &str, idx: usize) -> bool {
        match self.fns.get(name) {
            Some(info) => match info.convs.get(idx) {
                // typeck: a `let` borrow cannot escape the call.
                Some(Convention::Borrow) => false,
                // an `own` argument is moved: the caller's binding is
                // dead afterwards (use-after-move is a compile error), so no
                // live DOUBLE alias can form.
                Some(Convention::Own) => false,
                // the var variable itself is rebound at write-back (the
                // consumer's plain-reassign reset covers it); whether OTHER
                // arguments leak into it is `may_alias_out`.
                _ => info.may_alias_out.get(idx).copied().unwrap_or(true),
            },
            // Unknown callee (not in this module): assume the worst.
            None => true,
        }
    }

    /// Unified leak query: does the value passed in argument position `idx` of a
    /// call to `name` (with `argc` arguments) escape the call as a whole alias?
    /// Consults the builtin effect table first (the same precedence the liveness
    /// Walker uses), then per-function summaries. This is the convention/escape
    /// oracle that drives RC-floor reclamation — general over builtins AND user
    /// functions, with no per-method code.
    pub fn arg_leaks(&self, name: &str, idx: usize, argc: usize) -> bool {
        match builtin_arg_liveness(name, argc) {
            Some(effects) => effects.get(idx).copied().unwrap_or(true),
            None => self.arg_live(name, idx),
        }
    }
}

/// Does `param` flow somewhere live inside `body`, given the current
/// summaries? Reuses the same liveness scan as the intraprocedural pass,
/// with one addition: a `Return`/tail value IS live here (it flows to the
/// caller).
fn param_flows_out(body: &Block, param: &str, summaries: &Summaries) -> bool {
    let mut accs = HashSet::new();
    accs.insert(param.to_string());
    let mut w = Walker {
        accs: &accs,
        summaries,
        facts: Facts::default(),
        loop_sites: HashMap::new(),
        loop_stack: Vec::new(),
        cur_line: 0,
        returns_are_live: true,
    };
    w.walk_block(body, true);
    !w.facts.kills.is_empty() || w.facts.kill_entries > 0
}

// ---------------------------------------------------------------------------
// The intraprocedural pass.
// ---------------------------------------------------------------------------

/// Analyze one compile unit (a function/handler/lambda BODY — lambda
/// interiors are separate units and are only mention-scanned here, since a
/// closure's captures alias at creation).
pub fn analyze(body: &Block, summaries: &Summaries) -> Facts {
    // Pass A: accumulators + per-loop self-assign site sets (for cliffs).
    let mut accs = HashSet::new();
    let mut loop_sites: HashMap<usize, HashSet<String>> = HashMap::new();
    collect_accumulators(body, summaries, &mut accs, &mut Vec::new(), &mut loop_sites);
    if accs.is_empty() {
        return Facts::default();
    }
    // Pass B: share events, dirty sites, cliffs.
    let mut w = Walker {
        accs: &accs,
        summaries,
        facts: Facts::default(),
        loop_sites,
        loop_stack: Vec::new(),
        cur_line: 0,
        returns_are_live: false,
    };
    w.walk_block(body, false);
    let mut facts = w.facts;
    facts.accumulators = accs;
    facts
}

fn collect_accumulators(
    b: &Block,
    summaries: &Summaries,
    accs: &mut HashSet<String>,
    loop_ptrs: &mut Vec<usize>,
    loop_sites: &mut HashMap<usize, HashSet<String>>,
) {
    for stmt in &b.stmts {
        if let Stmt::Assign { name, value } = stmt {
            if is_self_assign_shape(name, value, summaries) {
                accs.insert(name.clone());
                for lp in loop_ptrs.iter() {
                    loop_sites.entry(*lp).or_default().insert(name.clone());
                }
            }
        }
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetTuple { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Expr(value)
            | Stmt::Yield(value) => {
                collect_accumulators_expr(value, summaries, accs, loop_ptrs, loop_sites)
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_accumulators_expr(
    e: &Expr,
    summaries: &Summaries,
    accs: &mut HashSet<String>,
    loop_ptrs: &mut Vec<usize>,
    loop_sites: &mut HashMap<usize, HashSet<String>>,
) {
    match e {
        Expr::While { cond, body } => {
            collect_accumulators_expr(cond, summaries, accs, loop_ptrs, loop_sites);
            loop_ptrs.push(body as *const Block as usize);
            collect_accumulators(body, summaries, accs, loop_ptrs, loop_sites);
            loop_ptrs.pop();
        }
        Expr::For { iter, body, .. } => {
            collect_accumulators_expr(iter, summaries, accs, loop_ptrs, loop_sites);
            loop_ptrs.push(body as *const Block as usize);
            collect_accumulators(body, summaries, accs, loop_ptrs, loop_sites);
            loop_ptrs.pop();
        }
        Expr::If { cond, then_block, else_block } => {
            collect_accumulators_expr(cond, summaries, accs, loop_ptrs, loop_sites);
            collect_accumulators(then_block, summaries, accs, loop_ptrs, loop_sites);
            if let Some(b) = else_block {
                collect_accumulators(b, summaries, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_accumulators_expr(scrutinee, summaries, accs, loop_ptrs, loop_sites);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_accumulators_expr(g, summaries, accs, loop_ptrs, loop_sites);
                }
                collect_accumulators_expr(&arm.body, summaries, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::Block(b) => collect_accumulators(b, summaries, accs, loop_ptrs, loop_sites),
        // Lambda interiors are separate compile units with their own facts.
        Expr::Lambda { .. } => {}
        Expr::Binary { lhs, rhs, .. } => {
            collect_accumulators_expr(lhs, summaries, accs, loop_ptrs, loop_sites);
            collect_accumulators_expr(rhs, summaries, accs, loop_ptrs, loop_sites);
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::Field { base: expr, .. } => {
            collect_accumulators_expr(expr, summaries, accs, loop_ptrs, loop_sites)
        }
        Expr::Call { args, .. }
        | Expr::Ctor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => {
            for a in args {
                collect_accumulators_expr(a, summaries, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::Apply { func, args } => {
            collect_accumulators_expr(func, summaries, accs, loop_ptrs, loop_sites);
            for a in args {
                collect_accumulators_expr(a, summaries, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::RecordUpdate { base, fields } => {
            collect_accumulators_expr(base, summaries, accs, loop_ptrs, loop_sites);
            for (_, v) in fields {
                collect_accumulators_expr(v, summaries, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                collect_accumulators_expr(v, summaries, accs, loop_ptrs, loop_sites);
            }
            if let Some(s) = spread {
                collect_accumulators_expr(s, summaries, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::Range { lo, hi, .. } => {
            collect_accumulators_expr(lo, summaries, accs, loop_ptrs, loop_sites);
            collect_accumulators_expr(hi, summaries, accs, loop_ptrs, loop_sites);
        }
        Expr::Index { base, index } => {
            collect_accumulators_expr(base, summaries, accs, loop_ptrs, loop_sites);
            collect_accumulators_expr(index, summaries, accs, loop_ptrs, loop_sites);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            collect_accumulators_expr(scrutinee, summaries, accs, loop_ptrs, loop_sites);
            loop_ptrs.push(body as *const Block as usize);
            collect_accumulators(body, summaries, accs, loop_ptrs, loop_sites);
            loop_ptrs.pop();
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_accumulators_expr(receiver, summaries, accs, loop_ptrs, loop_sites);
            for a in args {
                collect_accumulators_expr(a, summaries, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }
}

struct Walker<'a> {
    accs: &'a HashSet<String>,
    summaries: &'a Summaries,
    facts: Facts,
    /// loop body ptr -> accumulators self-assigned inside it.
    loop_sites: HashMap<usize, HashSet<String>>,
    /// Innermost-last stack of enclosing loop body ptrs.
    loop_stack: Vec<usize>,
    cur_line: u32,
    /// In summary mode, a returned/tail value flows to the caller and counts
    /// as live; intraprocedurally the function is over and nothing kills.
    returns_are_live: bool,
}

impl<'a> Walker<'a> {
    /// `tail_live`: whether this block's tail value flows somewhere live
    /// (e.g. the block is a `let` RHS or a function body in summary mode).
    fn walk_block(&mut self, b: &Block, tail_live: bool) {
        let last = b.stmts.len().saturating_sub(1);
        for (i, stmt) in b.stmts.iter().enumerate() {
            if let Some(line) = b.lines.get(i) {
                self.cur_line = *line;
            }
            let mut shares: Vec<(String, String)> = Vec::new();
            match stmt {
                Stmt::Let { value, .. } | Stmt::LetTuple { value, .. } => {
                    self.scan(value, true, "bound to a new name", &mut shares);
                }
                Stmt::Assign { name, value } => {
                    if let Some((fname, idx)) =
                        self_own_call(name, value, self.summaries).filter(|_| self.accs.contains(name))
                    {
                        // `x = f(move x)`: the ownership token crosses the
                        // call; the other arguments are scanned per the
                        // callee's summary.
                        self.facts.site_entries += 1;
                        let fname = fname.to_string();
                        if let Expr::Call { args, .. } = value {
                            for (j, a) in args.iter().enumerate() {
                                if j == idx {
                                    continue;
                                }
                                let live = self.summaries.arg_live(&fname, j);
                                self.scan(a, live, &format!("passed to `{fname}`"), &mut shares);
                            }
                        }
                        shares.retain(|(v, _)| v != name);
                    } else if let Some(op) =
                        self.accs.contains(name).then(|| self_inplace_op(name, value)).flatten()
                    {
                        self.facts.site_entries += 1;
                        // The shape's own occurrence of `name` is the
                        // operation; everything else in the RHS is scanned,
                        // and a share of `name` itself dirties the site.
                        let mut sub: Vec<(String, String)> = Vec::new();
                        match op {
                            InPlaceOp::Push(elem) => {
                                self.scan(elem, true, "stored back into the list", &mut sub);
                            }
                            InPlaceOp::SetAt(i, v) => {
                                self.scan(i, true, "used as a list index", &mut sub);
                                self.scan(v, true, "stored back into the list", &mut sub);
                            }
                            InPlaceOp::UpdateAt(i, f) => {
                                self.scan(i, true, "used as a list index", &mut sub);
                                self.scan(f, true, "captured by the updater", &mut sub);
                            }
                            InPlaceOp::Insert(k, v) => {
                                self.scan(k, true, "stored as a dict key", &mut sub);
                                self.scan(v, true, "stored as a dict value", &mut sub);
                            }
                            InPlaceOp::Update(k, d, f) => {
                                self.scan(k, true, "stored as a dict key", &mut sub);
                                self.scan(d, true, "stored as a dict value", &mut sub);
                                self.scan(f, true, "captured by the updater", &mut sub);
                            }
                            InPlaceOp::Concat(pieces) => {
                                for (pi, p) in pieces.iter().enumerate() {
                                    self.scan(p, true, "appended to the string", &mut sub);
                                    // Pieces after the first are evaluated AFTER
                                    // the in-place append has already mutated the
                                    // variable, so even a content READ of it sees
                                    // the wrong value: any mention dirties.
                                    if pi > 0 {
                                        let mut seen = HashSet::new();
                                        let one: HashSet<String> =
                                            std::iter::once(name.clone()).collect();
                                        mentions_in_expr(p, &one, &mut seen);
                                        if !seen.is_empty() {
                                            sub.push((
                                                name.clone(),
                                                "read again later in the chain".to_string(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        for (v, reason) in sub {
                            if v == *name {
                                self.facts.dirty.insert(stmt_key(stmt));
                                self.cliff(name, &reason);
                            } else {
                                shares.push((v, reason));
                            }
                        }
                    } else {
                        // A plain reassignment: the consumer's reset covers
                        // `name`'s own token; the RHS may share OTHERS.
                        self.scan(value, true, "bound to a new name", &mut shares);
                        shares.retain(|(v, _)| v != name);
                    }
                }
                Stmt::Expr(e) => {
                    // The statement's value is discarded — only what a call
                    // can smuggle out (per summaries) or a nested statement
                    // does is live. The block-tail case is the exception.
                    let live = tail_live && i == last;
                    self.scan(e, live, "the surrounding expression", &mut shares);
                }
                Stmt::Return(Some(e)) => {
                    if self.returns_are_live {
                        self.scan(e, true, "returned", &mut shares);
                    } else {
                        // The function is over; an alias in the return value
                        // is the CALLER's concern (summaries).
                        self.scan(e, false, "returned", &mut shares);
                        shares.clear();
                    }
                }
                Stmt::Yield(e) => {
                    // A yielded value escapes into the generator frame.
                    self.scan(e, true, "yielded", &mut shares);
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
            if !shares.is_empty() {
                let mut added = 0usize;
                {
                    let entry = self.facts.kills.entry(stmt_key(stmt)).or_default();
                    for (v, _) in &shares {
                        if !entry.contains(v) {
                            entry.push(v.clone());
                            added += 1;
                        }
                    }
                }
                self.facts.kill_entries += added;
                for (v, reason) in &shares {
                    self.cliff(v, reason);
                }
            }
            // A `move <acc>` transfers the accumulator's buffer OUT (into whatever consumes it),
            // so its tracked capacity (`__cap`) is stale afterward — continuing to push to the
            // variable after a later re-bind would otherwise write into the moved-away buffer
            // and corrupt the new owner. The statement therefore KILLS every moved accumulator
            // (codegen resets its `__cap` to 0). This is deliberately NOT a cliff: a move is the
            // intended ownership transfer — the fast path — so `self.cliff` is not called.
            let mut moved = Vec::new();
            match stmt {
                Stmt::Assign { name, value } => {
                    collect_moved_accs(value, self.accs, &mut moved);
                    // `name = f(move name)` is the own-ABI pipeline (ownership round-trips) and
                    // `name = …move name…` re-binds `name` anyway, so the assignment itself
                    // governs `name`'s capacity — killing it here would break the in-place pipe.
                    moved.retain(|v| v != name);
                }
                Stmt::Let { value, .. }
                | Stmt::LetTuple { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Expr(value)
                | Stmt::Yield(value) => collect_moved_accs(value, self.accs, &mut moved),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
            if !moved.is_empty() {
                let mut added = 0usize;
                let entry = self.facts.kills.entry(stmt_key(stmt)).or_default();
                for v in &moved {
                    if !entry.contains(v) {
                        entry.push(v.clone());
                        added += 1;
                    }
                }
                self.facts.kill_entries += added;
            }
        }
    }

    /// Record a cliff if the share/dirty repeats: the variable is also
    /// accumulated inside the innermost enclosing loop.
    fn cliff(&mut self, var: &str, reason: &str) {
        if let Some(lp) = self.loop_stack.last() {
            if self.loop_sites.get(lp).is_some_and(|s| s.contains(var)) {
                self.facts.cliffs.push(Cliff {
                    var: var.to_string(),
                    line: self.cur_line,
                    reason: reason.to_string(),
                });
            }
        }
    }

    /// The effect scan. `live` says whether THIS expression's value flows
    /// somewhere observable after the statement; `reason` describes the sink
    /// for diagnostics. A bare accumulator in a live position is a share.
    /// Every classification below makes a value DEAD only with a reason
    /// written next to it; the default is live (sound).
    fn scan(&mut self, e: &Expr, live: bool, reason: &str, out: &mut Vec<(String, String)>) {
        match e {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::TaggedLit { .. } => {}
            Expr::Var(v) => {
                if live && self.accs.contains(v) {
                    out.push((v.clone(), reason.to_string()));
                }
            }
            // A closure captures its mentioned outer variables BY POINTER at
            // creation: any mention of an accumulator in the body is a share
            // (even read-only — the capture would otherwise observe later
            // in-place mutation; the interpreter's capture is a snapshot).
            Expr::Lambda { body, .. } => {
                let mut mentioned = HashSet::new();
                lambda_mentions(body, self.accs, &mut mentioned);
                for v in mentioned {
                    out.push((v, "captured by a closure".to_string()));
                }
            }
            Expr::Call { name, args } => {
                if let Some(effects) = builtin_arg_liveness(name, args.len()) {
                    for (a, arg_live) in args.iter().zip(effects) {
                        // A builtin's "stored" slot only matters if the
                        // result it is stored INTO is itself live.
                        self.scan(a, live && arg_live, reason, out);
                    }
                } else if self.summaries.fns.contains_key(name) {
                    for (i, a) in args.iter().enumerate() {
                        let arg_live = self.summaries.arg_live(name, i);
                        // NOTE: `may_alias_out` covers BOTH the return value
                        // and var write-backs; an var-channel alias is
                        // live even when the call's result is discarded, so
                        // arg liveness here deliberately ignores `live`.
                        self.scan(a, arg_live, &format!("passed to `{name}`"), out);
                    }
                } else {
                    // Unknown callee (foreign/builtin not in the table):
                    // assume every argument may alias out.
                    for a in args {
                        self.scan(a, true, &format!("passed to `{name}`"), out);
                    }
                }
            }
            // A closure call: no conventions, no summary — every argument may
            // be retained by the closure's result.
            Expr::Apply { func, args } => {
                self.scan(func, false, reason, out);
                for a in args {
                    self.scan(a, true, "passed to a function value", out);
                }
            }
            // Structures store their members by slot (whole-alias) — live iff
            // the structure itself is.
            Expr::List(items) | Expr::Tuple(items) => {
                for it in items {
                    self.scan(it, live, "stored into a collection", out);
                }
            }
            Expr::Ctor { args, .. } => {
                for a in args {
                    self.scan(a, live, "stored into a constructor", out);
                }
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields {
                    self.scan(v, live, "stored into a record", out);
                }
                if let Some(s) = spread {
                    self.scan(s, live, "stored into a record", out);
                }
            }
            Expr::RecordUpdate { base, fields } => {
                self.scan(base, false, reason, out);
                for (_, v) in fields {
                    self.scan(v, live, "stored into a record", out);
                }
            }
            // Binary operators READ their operands into a fresh result
            // (concat copies bytes, comparisons compare content, arithmetic
            // is scalar): operands are dead regardless of the result.
            Expr::Binary { lhs, rhs, .. } => {
                self.scan(lhs, false, reason, out);
                self.scan(rhs, false, reason, out);
            }
            Expr::Unary { expr, .. } => self.scan(expr, false, reason, out),
            // `?` unwraps a payload (a part-alias); `as` is identity.
            Expr::Try(expr) | Expr::As { expr, .. } => self.scan(expr, live, reason, out),
            // Field access reads a slot out of the base (part-alias of the
            // base; the slot value itself flows onward).
            Expr::Field { base, .. } => self.scan(base, false, reason, out),
            Expr::If { cond, then_block, else_block } => {
                self.scan(cond, false, reason, out);
                self.walk_branch_block(then_block, live, out);
                if let Some(b) = else_block {
                    self.walk_branch_block(b, live, out);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.scan(scrutinee, false, reason, out);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.scan(g, false, reason, out);
                    }
                    self.scan(&arm.body, live, "selected by a match", out);
                }
            }
            Expr::Block(b) => self.walk_branch_block(b, live, out),
            Expr::While { cond, body } => {
                self.scan(cond, false, reason, out);
                self.loop_stack.push(body as *const Block as usize);
                self.walk_block(body, false);
                self.loop_stack.pop();
            }
            Expr::For { iter, body, .. } => {
                // Iteration reads elements (part-aliases via the loop var).
                self.scan(iter, false, reason, out);
                self.loop_stack.push(body as *const Block as usize);
                self.walk_block(body, false);
                self.loop_stack.pop();
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                self.scan(scrutinee, false, reason, out);
                self.loop_stack.push(body as *const Block as usize);
                self.walk_block(body, false);
                self.loop_stack.pop();
            }
            Expr::Range { lo, hi, .. } => {
                self.scan(lo, false, reason, out);
                self.scan(hi, false, reason, out);
            }
            Expr::Index { base, index } => {
                // `xs[i]` is `at(xs, i)`: a content read / part-alias.
                self.scan(base, false, reason, out);
                self.scan(index, false, reason, out);
            }
            Expr::MethodCall { receiver, args, .. } => {
                // Pre-lowered before codegen; if seen (diagnostics on the
                // sugared form), be conservative: receiver and args live.
                self.scan(receiver, true, reason, out);
                for a in args {
                    self.scan(a, true, reason, out);
                }
            }
        }
    }

    /// A block in expression position: its inner statements are real
    /// statements (kills attach to them); its tail value flows to `live`.
    fn walk_branch_block(
        &mut self,
        b: &Block,
        live: bool,
        _out: &mut Vec<(String, String)>,
    ) {
        let saved_line = self.cur_line;
        self.walk_block(b, live);
        self.cur_line = saved_line;
    }
}

/// Mentions of any of `accs` anywhere in an expression (reads included).
fn mentions_in_expr(e: &Expr, accs: &HashSet<String>, out: &mut HashSet<String>) {
    expr(e, accs, out)
}

fn expr(e: &Expr, accs: &HashSet<String>, out: &mut HashSet<String>) {
        match e {
            Expr::Var(v) => {
                if accs.contains(v) {
                    out.insert(v.clone());
                }
            }
            Expr::Lambda { body, .. } => lambda_mentions(body, accs, out),
            Expr::If { cond, then_block, else_block } => {
                expr(cond, accs, out);
                lambda_mentions(then_block, accs, out);
                if let Some(b) = else_block {
                    lambda_mentions(b, accs, out);
                }
            }
            Expr::Match { scrutinee, arms } => {
                expr(scrutinee, accs, out);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        expr(g, accs, out);
                    }
                    expr(&arm.body, accs, out);
                }
            }
            Expr::Block(b) => lambda_mentions(b, accs, out),
            Expr::While { cond, body } => {
                expr(cond, accs, out);
                lambda_mentions(body, accs, out);
            }
            Expr::For { iter, body, .. } => {
                expr(iter, accs, out);
                lambda_mentions(body, accs, out);
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                expr(scrutinee, accs, out);
                lambda_mentions(body, accs, out);
            }
            Expr::Binary { lhs, rhs, .. } => {
                expr(lhs, accs, out);
                expr(rhs, accs, out);
            }
            Expr::Unary { expr: e2, .. }
            | Expr::Try(e2)
            | Expr::As { expr: e2, .. }
            | Expr::Field { base: e2, .. } => expr(e2, accs, out),
            Expr::Call { args, .. }
            | Expr::Ctor { args, .. }
            | Expr::List(args)
            | Expr::Tuple(args) => {
                for a in args {
                    expr(a, accs, out);
                }
            }
            Expr::Apply { func, args } => {
                expr(func, accs, out);
                for a in args {
                    expr(a, accs, out);
                }
            }
            Expr::RecordUpdate { base, fields } => {
                expr(base, accs, out);
                for (_, v) in fields {
                    expr(v, accs, out);
                }
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields {
                    expr(v, accs, out);
                }
                if let Some(s) = spread {
                    expr(s, accs, out);
                }
            }
            Expr::Range { lo, hi, .. } => {
                expr(lo, accs, out);
                expr(hi, accs, out);
            }
            Expr::Index { base, index } => {
                expr(base, accs, out);
                expr(index, accs, out);
            }
            Expr::MethodCall { receiver, args, .. } => {
                expr(receiver, accs, out);
                for a in args {
                    expr(a, accs, out);
                }
            }
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::TaggedLit { .. } => {}
    }
}

/// Accumulator variables MOVED (`move x`) within a statement's value — the ones whose
/// capacity the statement must reset (see the kill logic in `walk_block`). Recurses through
/// the same-evaluation expression forms (call args, operators, `if`/`match` branches, …) but
/// NOT into nested loop or lambda bodies: those are separate scopes the walker visits on their
/// own, where the move is killed at its own statement.
fn collect_moved_accs(e: &Expr, accs: &HashSet<String>, out: &mut Vec<String>) {
    match e {
        Expr::Unary { op: UnOp::Move, expr } => match expr.as_ref() {
            Expr::Var(v) if accs.contains(v) => {
                if !out.contains(v) {
                    out.push(v.clone());
                }
            }
            inner => collect_moved_accs(inner, accs, out),
        },
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::Field { base: expr, .. } => collect_moved_accs(expr, accs, out),
        Expr::Binary { lhs, rhs, .. }
        | Expr::Range { lo: lhs, hi: rhs, .. }
        | Expr::Index { base: lhs, index: rhs } => {
            collect_moved_accs(lhs, accs, out);
            collect_moved_accs(rhs, accs, out);
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::List(args) | Expr::Tuple(args) => {
            for a in args {
                collect_moved_accs(a, accs, out);
            }
        }
        Expr::Apply { func, args } => {
            collect_moved_accs(func, accs, out);
            for a in args {
                collect_moved_accs(a, accs, out);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_moved_accs(receiver, accs, out);
            for a in args {
                collect_moved_accs(a, accs, out);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            collect_moved_accs(cond, accs, out);
            collect_moved_in_block(then_block, accs, out);
            if let Some(b) = else_block {
                collect_moved_in_block(b, accs, out);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_moved_accs(scrutinee, accs, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_moved_accs(g, accs, out);
                }
                collect_moved_accs(&arm.body, accs, out);
            }
        }
        Expr::Block(b) => collect_moved_in_block(b, accs, out),
        Expr::RecordUpdate { base, fields } => {
            collect_moved_accs(base, accs, out);
            for (_, v) in fields {
                collect_moved_accs(v, accs, out);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                collect_moved_accs(v, accs, out);
            }
            if let Some(s) = spread {
                collect_moved_accs(s, accs, out);
            }
        }
        // Separate scopes (loops re-bind per iteration, lambdas capture) — visited on their own.
        Expr::While { .. }
        | Expr::For { .. }
        | Expr::WhileLet { .. }
        | Expr::Lambda { .. }
        | Expr::Var(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
    }
}

/// `collect_moved_accs` over the statements of an `if`/`match`/block arm value.
fn collect_moved_in_block(b: &Block, accs: &HashSet<String>, out: &mut Vec<String>) {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::LetTuple { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Expr(value)
            | Stmt::Yield(value) => collect_moved_accs(value, accs, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

/// Mentions of outer accumulators anywhere in a lambda body (its captures).
fn lambda_mentions(b: &Block, accs: &HashSet<String>, out: &mut HashSet<String>) {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetTuple { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Expr(value)
            | Stmt::Yield(value) => expr(value, accs, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
        if let Stmt::Assign { name, .. } = stmt {
            if accs.contains(name) {
                out.insert(name.clone());
            }
        }
    }
}

/// The builtin effect table: per-argument liveness for operations whose
/// behavior is defined by the runtime, not witchy code. `false` = the
/// argument's buffer is only READ (content consumed or part-aliased) — it
/// cannot gain a live whole-alias through this slot. `true` = the value is
/// STORED by slot into the result (live iff the result is, which the caller
/// of this table accounts for). Returns None for names that are not builtins
/// with known effects (witchy functions resolve through summaries; anything
/// else is treated as worst-case by the caller).
fn builtin_arg_liveness(name: &str, argc: usize) -> Option<Vec<bool>> {
    let read_all = |n: usize| Some(vec![false; n]);
    match (name, argc) {
        // Collections: content reads and part-alias reads.
        ("list.length", 1)
        | ("dict.length", 1)
        | ("string.length", 1)
        | ("string.char_count", 1)
        | ("list.at", 2)
        | ("dict.contains_key", 2)
        | ("string.contains", 2)
        | ("string.index_of", 2)
        | ("dict.keys", 1)
        | ("dict.values", 1)
        | ("dict.pairs", 1)
        | ("dict.remove", 2)
        | ("list.concat", 2) => read_all(argc),
        // get_or reads the dict and key; the DEFAULT may be returned.
        ("dict.get_or", 3) => Some(vec![false, false, true]),
        // push/insert/update store their value operands by slot. (The
        // self-assign shape is special-cased before this table is consulted.)
        ("list.push", 2) => Some(vec![false, true]),
        ("dict.insert", 3) => Some(vec![false, true, true]),
        ("dict.update", 4) => Some(vec![false, true, true, true]),
        // Output and messaging copy content out to the host.
        ("print", 2) => read_all(2),
        ("send", _) => read_all(argc),
        ("fail", 1) => read_all(1),
        // Strings: every operation reads content and builds fresh results.
        ("__render", 1)
        | ("string.to_int", 1)
        | ("string.chars", 1)
        | ("string.trim", 1)
        | ("string.to_upper", 1)
        | ("string.to_lower", 1)
        | ("string.split", 2)
        | ("string.starts_with", 2)
        | ("string.ends_with", 2)
        | ("string.substring", 3)
        | ("string.replace", 3) => read_all(argc),
        // Conversions never retain buffers.
        ("math.to_float", 1) | ("math.to_int", 1) | ("math.sqrt", 1) => read_all(argc),
        ("dict.new", 0) => Some(Vec::new()),
        _ => None,
    }
}

/// For RC-floor free-at-overwrite (RFC-0016): if `name`/`argc` is a native builtin
/// whose result is ALWAYS a freshly-allocated heap buffer (never an alias of an
/// input buffer), the byte offset from the result pointer back to the START of its
/// `$rc_alloc` region — so the OLD buffer can be freed when it is overwritten.
/// Dicts carry a hidden index word at `ptr-4` (the region starts there); lists,
/// strings, and records start at the pointer itself.
///
/// This is a BOUNDED, one-time table of the PRIMITIVE allocators — the dict's `-4`
/// layout wrinkle and friends. It does NOT grow per user operation: it is the
/// soundness floor that the result is fresh (builtins never alias-passthrough a
/// buffer arg, unlike a user function that might `return` one). User-type
/// reclamation generalizes through the single `mk` constructor + the escape oracle,
/// not through this table. See [[feedback_no_special_casing_optimizations]].
pub fn fresh_heap_builtin_offset(name: &str, argc: usize) -> Option<i32> {
    match (name, argc) {
        // Dict results: allocated through `$rc_alloc` (so they carry the `[size]`
        // header `$rc_free` needs), with the hidden index word at `ptr-4` — i.e.
        // the rc-region start is `ptr-4`.
        ("dict.insert", 3) | ("dict.update", 4) | ("dict.remove", 2) => Some(4),
        // List / string results: the buffer pointer IS the rc-region start (offset 0).
        // These allocators (`list_push`/`list_concat`/`ascii_case`/`substr`, and
        // `trim` via `substr`) are routed through `$rc_alloc`, so their results carry
        // the `[size]` header and are freeable. NOT `string.replace` (its
        // `replace_helper` keeps the worst-case `ensure` + actual-bump pattern, not
        // routed) and NOT string `+` (a Binary, never a Call — handled in-place by
        // `$str_append_cap`).
        ("list.push", 2) | ("list.concat", 2) => Some(0),
        ("string.to_upper", 1) | ("string.to_lower", 1) | ("string.trim", 1)
        | ("string.substring", 3) => Some(0),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Module-level diagnostics: every function's cliffs, for `witchy check` and
// the LSP. Runs on the sugared AST (the shapes appear identically; the
// method-call/index sugar only makes the scan more conservative, never less
// sound — diagnostics carry no soundness obligation anyway).
// ---------------------------------------------------------------------------

pub fn module_cliffs(module: &Module) -> Vec<(String, Cliff)> {
    let summaries = Summaries::of_module(module);
    let mut out = Vec::new();
    for item in &module.items {
        if let Item::Function(f) = item {
            for c in analyze(&f.body, &summaries).cliffs {
                out.push((f.name.clone(), c));
            }
        }
    }
    out
}
