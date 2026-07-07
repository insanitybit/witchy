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

// foldhash (not SipHash): all keys are compiler-internal names/ids, never
// attacker-chosen collections — see the note in witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};

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
//
// (RFC-0051 I3 — REJECTED-IN-PART, retention justified by measurement 2026-07-03)
// CLAUDE.md's thesis is to DELETE this `self_*` + `*_cap` family via the general
// ownership path. RFC-0051 tested the precondition it claimed had flipped (the RC
// floor is now default-on) and found the deletion still fails, for TWO recorded
// reasons — so the family is retained, and this is the evidence:
//
//   1. The in-place mechanism is LOAD-BEARING, not an over-fit. Compiling the same
//      programs through the general value-semantics rebind (`WITCHY_OPT=-inplace`,
//      the closest thing to "no per-op path") measured (kernel-clock, release):
//        word_count / dict_count / list_sum / knucleotide → OOM TRAP (the O(n²)
//          rebuild-per-iteration the RFC's Alternatives section predicts);
//        list_index 2.70x, binary_trees 1.27x, expr_eval 1.31x SLOWER.
//      The 5%/2% acceptance gate is not close; deletion of the mechanism regresses
//      hard. So the WIR-level in-place emission (append-at-len / store-at-slot /
//      hash-probe-insert / byte-append / closure-at-slot) must survive in SOME form.
//   2. The RFC's ONLY rung that deletes these RECOGNIZERS — rung 2, the runtime
//      `rc == 1` in-place branch — is HARD-GATED on TOTAL dup coverage (a missed dup
//      makes `rc == 1` a lie, and an in-place mutation observed through an alias is a
//      silently-wrong answer, strictly worse than a leak). RFC-0051 I1's own
//      `WITCHY_RC_ASSERT` fire-and-report probe DISPROVES totality: the SEC-037
//      view/slice dup residual still reaches a dup site under the `views` lever
//      (`WITCHY_RC_ASSERT=1 WITCHY_OPT=all` fires on minigrep). Until I1's typed
//      emission closes SEC-037 at its source, rung 2's precondition is unmet.
//   Rung 1 (a signature-table replacing these matchers) deletes only the cheap
//   `self_*` shape-matchers, NOT the six `*_cap` helper BODIES — those are genuinely
//   distinct algorithms (a hash-probe upsert is not "list-append with different
//   constants"), so rung 1 does not achieve CLAUDE.md's "delete the zoo" either.
//   Net: I1 + I2 shipped; I3 is a decided, evidenced RETENTION, not silent drift.
// ---------------------------------------------------------------------------

/// `xs = push(xs, e)`: the appended element.
fn self_push_elem<'a>(name: &str, value: &'a Expr) -> Option<&'a Expr> {
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
fn self_insert_args<'a>(name: &str, value: &'a Expr) -> Option<(&'a Expr, &'a Expr)> {
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
fn self_update_args<'a>(
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
fn self_set_at<'a>(name: &str, value: &'a Expr) -> Option<(&'a Expr, &'a Expr)> {
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
fn self_update_at<'a>(name: &str, value: &'a Expr) -> Option<(&'a Expr, &'a Expr)> {
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
fn self_concat_pieces<'a>(name: &str, value: &'a Expr) -> Option<Vec<&'a Expr>> {
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

fn is_self_assign_shape(name: &str, value: &Expr, summaries: &Summaries) -> bool {
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
    /// `s = {...s, f: v, …}` (a `RecordUpdate` whose base is the assigned record):
    /// the updated `(field-name, value)` pairs. (RFC-0033 R1.)
    RecordUpdate(&'a [(String, Expr)]),
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
    if let Some(fields) = self_record_update(name, value) {
        return Some(InPlaceOp::RecordUpdate(fields));
    }
    None
}

/// `s = {...s, f: v, …}`: a spread-update whose base is the record's OWN binding.
/// The record analog of the list/dict self-assign shapes — when `s` is uniquely
/// owned the update writes the changed fields into `s`'s slots in place rather
/// than reallocating (RFC-0033 R1). Returns the updated `(field, value)` pairs.
fn self_record_update<'a>(name: &str, value: &'a Expr) -> Option<&'a [(String, Expr)]> {
    if let Expr::RecordUpdate { name: _, base, fields } = value {
        if matches!(base.as_ref(), Expr::Var(v) if v == name) {
            return Some(fields.as_slice());
        }
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
        // on every function's signature). (RFC-0033 R3) An `own` param is
        // eligible when its type is HEAP-allocated — the builtin collections plus
        // every user record/enum — so in-place ownership threads through user
        // abstractions, not just the three builtins. Scalars (Int/Bool/Float/
        // Duration are `Type::Named` too) are excluded: they own no buffer, and
        // threading a cap for them would be unsound.
        let heap_types: HashSet<String> = ["List", "Dict", "String", "Bytes"]
            .into_iter()
            .map(String::from)
            .chain(module.items.iter().filter_map(|it| match it {
                Item::Type(td) => Some(td.name.clone()),
                _ => None,
            }))
            .collect();
        for (name, f) in &bodies {
            let owns: Vec<usize> = f
                .params
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    p.convention == Convention::Own
                        && matches!(&p.ty, Some(Type::Named(n, _)) if heap_types.contains(n))
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
            | Stmt::LetPattern { value, .. }
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
        Expr::RecordUpdate { name: _, base, fields } => {
            collect_accumulators_expr(base, summaries, accs, loop_ptrs, loop_sites);
            for (_, v) in fields {
                collect_accumulators_expr(v, summaries, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::LabeledCall { .. } => {
            unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
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
                Stmt::Let { value, .. } | Stmt::LetPattern { value, .. } => {
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
                            InPlaceOp::RecordUpdate(fields) => {
                                // Each updated value is evaluated (into a temp) BEFORE any
                                // in-place store, so a field READ of `name` is fine. But a
                                // value that stores `name` itself into a field (`{...s,
                                // parent: s}`) creates a live self-alias that in-place would
                                // make a self-reference while value semantics points at the
                                // old record — the share detection dirties exactly that.
                                for (_, v) in fields {
                                    self.scan(v, true, "stored into a record field", &mut sub);
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
                | Stmt::LetPattern { value, .. }
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
            Expr::LabeledCall { .. } => {
                unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields {
                    self.scan(v, live, "stored into a record", out);
                }
                if let Some(s) = spread {
                    self.scan(s, live, "stored into a record", out);
                }
            }
            Expr::RecordUpdate { name: _, base, fields } => {
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
            Expr::RecordUpdate { name: _, base, fields } => {
                expr(base, accs, out);
                for (_, v) in fields {
                    expr(v, accs, out);
                }
            }
            Expr::LabeledCall { .. } => {
                unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
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

/// (RFC-0033 R2) `(var, field)` pairs grown by `var.field = list.push(var.field, …)`
/// where every occurrence of `var.field` in `body` is exactly that push receiver, so
/// the field's list buffer is never aliased and may be grown in place. Any other read
/// of `var.field` disables it (conservative + sound). Whole-record aliasing is handled
/// separately by the record ownership token.
pub fn field_push_safe_set(body: &Block) -> HashSet<(String, String)> {
    let mut cands = HashSet::new();
    collect_field_push_candidates(body, &mut cands);
    cands
        .into_iter()
        .filter(|(v, f)| !block_field_escapes(body, v, f))
        .collect()
}

/// The single value expression of a statement (None for the value-less control
/// statements). The walkers below thread through exactly these.
fn stmt_value(s: &Stmt) -> Option<&Expr> {
    match s {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Return(Some(value))
        | Stmt::Expr(value)
        | Stmt::Yield(value) => Some(value),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => None,
    }
}

/// Collect every `(var, field)` updated by `var.field = list.push(var.field, …)` — the
/// shape R2 grows in place — descending into nested blocks so candidates inside loops
/// and branches are found.
fn collect_field_push_candidates(
    body: &Block,
    out: &mut HashSet<(String, String)>,
) {
    for stmt in &body.stmts {
        if let Stmt::Assign { name, value: Expr::RecordUpdate { name: _, base, fields } } = stmt {
            if matches!(base.as_ref(), Expr::Var(v) if v == name) {
                for (f, fv) in fields {
                    if let Expr::Call { name: pn, args } = fv {
                        if pn == "list.push"
                            && args.len() == 2
                            && matches!(&args[0], Expr::Field { base: fb, field }
                                if field == f
                                    && matches!(fb.as_ref(), Expr::Var(b) if b == name))
                        {
                            out.insert((name.clone(), f.clone()));
                        }
                    }
                }
            }
        }
        if let Some(e) = stmt_value(stmt) {
            collect_candidates_in_expr(e, out);
        }
    }
}

/// Find nested blocks reachable from an expression and recurse into them (mutually
/// with `collect_field_push_candidates`). Covers every `Expr` variant — model on the
/// `expr` walker above.
fn collect_candidates_in_expr(e: &Expr, out: &mut HashSet<(String, String)>) {
    match e {
        Expr::If { cond, then_block, else_block } => {
            collect_candidates_in_expr(cond, out);
            collect_field_push_candidates(then_block, out);
            if let Some(b) = else_block {
                collect_field_push_candidates(b, out);
            }
        }
        Expr::While { cond, body } => {
            collect_candidates_in_expr(cond, out);
            collect_field_push_candidates(body, out);
        }
        Expr::For { iter, body, .. } => {
            collect_candidates_in_expr(iter, out);
            collect_field_push_candidates(body, out);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            collect_candidates_in_expr(scrutinee, out);
            collect_field_push_candidates(body, out);
        }
        Expr::Match { scrutinee, arms } => {
            collect_candidates_in_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_candidates_in_expr(g, out);
                }
                collect_candidates_in_expr(&arm.body, out);
            }
        }
        Expr::Block(b) => collect_field_push_candidates(b, out),
        Expr::Lambda { body, .. } => collect_field_push_candidates(body, out),
        Expr::Binary { lhs, rhs, .. } => {
            collect_candidates_in_expr(lhs, out);
            collect_candidates_in_expr(rhs, out);
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::Field { base: expr, .. } => collect_candidates_in_expr(expr, out),
        Expr::Call { args, .. }
        | Expr::Ctor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => {
            for a in args {
                collect_candidates_in_expr(a, out);
            }
        }
        Expr::Apply { func, args } => {
            collect_candidates_in_expr(func, out);
            for a in args {
                collect_candidates_in_expr(a, out);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_candidates_in_expr(receiver, out);
            for a in args {
                collect_candidates_in_expr(a, out);
            }
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            collect_candidates_in_expr(base, out);
            for (_, v) in fields {
                collect_candidates_in_expr(v, out);
            }
        }
        Expr::LabeledCall { .. } => {
            unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                collect_candidates_in_expr(v, out);
            }
            if let Some(s) = spread {
                collect_candidates_in_expr(s, out);
            }
        }
        Expr::Range { lo, hi, .. } => {
            collect_candidates_in_expr(lo, out);
            collect_candidates_in_expr(hi, out);
        }
        Expr::Index { base, index } => {
            collect_candidates_in_expr(base, out);
            collect_candidates_in_expr(index, out);
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

/// True if `var.field` is read anywhere in `body` OTHER than as a `list.push`
/// receiver — i.e. the field's list buffer may be aliased, so R2 must not fire.
fn block_field_escapes(body: &Block, var: &str, field: &str) -> bool {
    body.stmts.iter().any(|s| match stmt_value(s) {
        Some(e) => field_escapes_expr(e, var, field),
        None => false,
    })
}

/// True if `e` reads `var.field` anywhere except as the receiver of a
/// `list.push(var.field, …)` (the one allowed occurrence — only its second
/// argument can carry an escape). Covers every `Expr` variant; a missing variant
/// would be unsound, so this mirrors the `expr` walker exactly.
fn field_escapes_expr(e: &Expr, var: &str, field: &str) -> bool {
    // The one allowed occurrence: `list.push(var.field, elem)`. The receiver is the
    // permitted read; only `elem` can carry an escaping use of `var.field`.
    if let Expr::Call { name, args } = e {
        if name == "list.push" && args.len() == 2 {
            if let Expr::Field { base, field: f } = &args[0] {
                if f == field && matches!(base.as_ref(), Expr::Var(v) if v == var) {
                    return field_escapes_expr(&args[1], var, field);
                }
            }
        }
    }
    match e {
        // Any OTHER read of `var.field` is an escaping read.
        Expr::Field { base, field: f }
            if f == field && matches!(base.as_ref(), Expr::Var(v) if v == var) =>
        {
            true
        }
        Expr::Binary { lhs, rhs, .. } => {
            field_escapes_expr(lhs, var, field) || field_escapes_expr(rhs, var, field)
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => {
            field_escapes_expr(expr, var, field)
        }
        Expr::Field { base, .. } => field_escapes_expr(base, var, field),
        Expr::Call { args, .. }
        | Expr::Ctor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => args.iter().any(|a| field_escapes_expr(a, var, field)),
        Expr::Apply { func, args } => {
            field_escapes_expr(func, var, field)
                || args.iter().any(|a| field_escapes_expr(a, var, field))
        }
        Expr::MethodCall { receiver, args, .. } => {
            field_escapes_expr(receiver, var, field)
                || args.iter().any(|a| field_escapes_expr(a, var, field))
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            field_escapes_expr(base, var, field)
                || fields.iter().any(|(_, v)| field_escapes_expr(v, var, field))
        }
        Expr::LabeledCall { .. } => {
            unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
        }
        Expr::Record { fields, spread, .. } => {
            fields.iter().any(|(_, v)| field_escapes_expr(v, var, field))
                || spread
                    .as_ref()
                    .is_some_and(|s| field_escapes_expr(s, var, field))
        }
        Expr::Range { lo, hi, .. } => {
            field_escapes_expr(lo, var, field) || field_escapes_expr(hi, var, field)
        }
        Expr::Index { base, index } => {
            field_escapes_expr(base, var, field) || field_escapes_expr(index, var, field)
        }
        Expr::If { cond, then_block, else_block } => {
            field_escapes_expr(cond, var, field)
                || block_field_escapes(then_block, var, field)
                || else_block
                    .as_ref()
                    .is_some_and(|b| block_field_escapes(b, var, field))
        }
        Expr::While { cond, body } => {
            field_escapes_expr(cond, var, field) || block_field_escapes(body, var, field)
        }
        Expr::For { iter, body, .. } => {
            field_escapes_expr(iter, var, field) || block_field_escapes(body, var, field)
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            field_escapes_expr(scrutinee, var, field) || block_field_escapes(body, var, field)
        }
        Expr::Match { scrutinee, arms } => {
            field_escapes_expr(scrutinee, var, field)
                || arms.iter().any(|a| {
                    a.guard
                        .as_ref()
                        .is_some_and(|g| field_escapes_expr(g, var, field))
                        || field_escapes_expr(&a.body, var, field)
                })
        }
        Expr::Block(b) => block_field_escapes(b, var, field),
        Expr::Lambda { body, .. } => block_field_escapes(body, var, field),
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => false,
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
        Expr::RecordUpdate { name: _, base, fields } => {
            collect_moved_accs(base, accs, out);
            for (_, v) in fields {
                collect_moved_accs(v, accs, out);
            }
        }
        Expr::LabeledCall { .. } => {
            unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
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
            | Stmt::LetPattern { value, .. }
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
            | Stmt::LetPattern { value, .. }
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
        | ("string.find", 2)
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

// ===========================================================================
// (RFC-0035) last_use / drop placement — the fourth projection of the oracle.
// ===========================================================================
// SOUND by over-approximation: a `$drop` is never placed before a value's true last
// use (never a use-after-free); imprecision costs a late drop (a retained count),
// never a lost one — the ⊥-keeps-the-count floor. ANALYSIS ONLY: nothing in codegen
// consumes this yet; it is verified standalone.
//
// INCREMENT 1 (here): the `DropFacts` substrate + the AIRTIGHT drop case — a `let`
// binding whose name is NEVER read and NEVER reassigned anywhere in the body. Zero
// references ⇒ no alias, no escape, no move-into-a-live-binding to reason about, so
// freeing it right after the binding is unconditionally safe. This reclaims dead
// allocations (`let buf = mk(); <buf never used>`) with no liveness subtlety.
//
// STILL TO COME (the bulk of last_use, deliberately not shipped unverified): the full
// backward-liveness drop-at-last-use, which MUST discharge two soundness obligations
// before it can place a drop on a *used* value — the Perceus dup/move discipline (a
// value moved into a still-live binding is transferred, not dropped) and
// inter-procedural escape via `Summaries::arg_leaks` (a value a callee retains stays
// live). These are exactly what `analyze`/`Walker` already computes for accumulators;
// generalizing that to all heap values is the next increment.

/// Drop points: statement identity → the binding names whose value is dead after that
/// statement, so `$drop name` is emitted immediately after it. Keyed by statement
/// identity (`stmt_key`), like the uniqueness `kills` — the consumer must compile the
/// exact AST instance analyzed.
/// One drop: the local holding the value, and the byte offset from that local's
/// pointer to the START of its `$rc_alloc` region — 0 for list/string/record buffers,
/// 4 for a dict (its hidden index word sits at `ptr-4`). The codegen frees
/// `local - offset`. The offset comes from the value's allocator, so a drop is recorded
/// ONLY for a value produced by a known heap allocator (`fresh_heap_builtin_offset`) —
/// which is also what makes it definitely a freeable heap pointer, never a scalar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drop {
    pub name: String,
    pub offset: i32,
}

#[derive(Default, Debug)]
pub struct DropFacts {
    after: HashMap<usize, Vec<Drop>>,
    read_after: HashMap<usize, Vec<String>>,
}

impl DropFacts {
    /// Values to `$rc_free` immediately after `stmt` (empty if none).
    pub fn drops_after(&self, stmt: &Stmt) -> &[Drop] {
        self.after.get(&stmt_key(stmt)).map(Vec::as_slice).unwrap_or(&[])
    }

    /// (RFC-0035 step 3) Read-owned heap bindings to `$rc_drop` immediately after `stmt` —
    /// a `let x = list.at(...)` whose read was `$rc_dup`'d (so `x` owns a reference) and
    /// whose last use is here. NAMES ONLY: codegen emits the drop only for `x` it recorded
    /// as actually dup'd (`rc_owned_bindings`), always at rc-region offset 0. Unlike
    /// `drops_after` (a fresh unique value → `$rc_free`), these use `$rc_drop` — the value
    /// may be shared, so it frees only at count 0.
    pub fn read_drops_after(&self, stmt: &Stmt) -> &[String] {
        self.read_after.get(&stmt_key(stmt)).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Total drop sites — for the standalone tests.
    pub fn total(&self) -> usize {
        self.after.values().map(Vec::len).sum()
    }

    fn record(&mut self, stmt: &Stmt, name: String, offset: i32) {
        self.after.entry(stmt_key(stmt)).or_default().push(Drop { name, offset });
    }

    fn record_read(&mut self, stmt: &Stmt, name: String) {
        self.read_after.entry(stmt_key(stmt)).or_default().push(name);
    }
}

/// (RFC-0035) Compute drop points for a function body. Two AIRTIGHT cases, neither
/// needing backward-liveness dataflow (which is the next increment):
///  1. **Dead binding** — a `let v` never read and never reassigned → drop right after
///     it (zero references; trivially safe).
///  2. **Single use** — a `let v` read EXACTLY ONCE and never reassigned, whose single
///     read is a direct `Var` argument of a call the summaries say does NOT leak it
///     (`arg_leaks` false ⇒ the callee neither stores the value nor returns it as an
///     alias) → drop after that statement. The single read IS trivially the last use
///     (no dataflow), and `arg_leaks` false guarantees the value's buffer does not
///     escape or alias the call result, so it is dead afterwards — safe to free even per
///     loop iteration when the read sits in a loop body. Anything stored in a
///     constructor/collection, returned, captured, passed to a leaking arg, read more
///     than once, reassigned, or a parameter (caller-owned) falls through to NO drop.
pub fn last_use_drops(body: &Block, summaries: &Summaries) -> DropFacts {
    let mut facts = DropFacts::default();
    // A value bound inside a `region:` block is bulk-reclaimed when the region ends;
    // freeing it here too would double-free. Exclude every region-confined binding.
    let region_confined = region_confined_lets(body);
    place_drops(body, body, summaries, &region_confined, &mut facts);
    facts
}

/// Names `let`-bound anywhere inside a `region:` block (a `Block` with `region.is_some()`,
/// or nested within one). Their allocations are region-born and bulk-freed at the region
/// boundary (RFC-0016 R4), so the RC floor must NOT also free them. The walk is complete
/// (every sub-expression and nested block, lambdas excepted — a separate unit) so no
/// confined binding is missed; a miss would be a double-free once codegen consumes drops.
fn region_confined_lets(body: &Block) -> HashSet<String> {
    let mut out = HashSet::new();
    rc_lets_block(body, false, &mut out);
    out
}

fn rc_lets_block(b: &Block, outer_confined: bool, out: &mut HashSet<String>) {
    let confined = outer_confined || b.region.is_some();
    for s in &b.stmts {
        if confined {
            if let Stmt::Let { name, .. } = s {
                out.insert(name.clone());
            }
        }
        if let Some(v) = stmt_value(s) {
            rc_lets_expr(v, confined, out);
        }
    }
}

fn rc_lets_expr(e: &Expr, confined: bool, out: &mut HashSet<String>) {
    match e {
        Expr::Block(b) => rc_lets_block(b, confined, out),
        Expr::If { cond, then_block, else_block } => {
            rc_lets_expr(cond, confined, out);
            rc_lets_block(then_block, confined, out);
            if let Some(bb) = else_block {
                rc_lets_block(bb, confined, out);
            }
        }
        Expr::While { cond, body } => {
            rc_lets_expr(cond, confined, out);
            rc_lets_block(body, confined, out);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            rc_lets_expr(scrutinee, confined, out);
            rc_lets_block(body, confined, out);
        }
        Expr::For { iter, body, .. } => {
            rc_lets_expr(iter, confined, out);
            rc_lets_block(body, confined, out);
        }
        Expr::Match { scrutinee, arms } => {
            rc_lets_expr(scrutinee, confined, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    rc_lets_expr(g, confined, out);
                }
                rc_lets_expr(&arm.body, confined, out);
            }
        }
        Expr::Lambda { .. } => {}
        Expr::List(xs) | Expr::Tuple(xs) => xs.iter().for_each(|x| rc_lets_expr(x, confined, out)),
        Expr::Call { args, .. } | Expr::Ctor { args, .. } => {
            args.iter().for_each(|a| rc_lets_expr(a, confined, out))
        }
        Expr::MethodCall { receiver, args, .. } => {
            rc_lets_expr(receiver, confined, out);
            args.iter().for_each(|a| rc_lets_expr(a, confined, out));
        }
        Expr::Apply { func, args } => {
            rc_lets_expr(func, confined, out);
            args.iter().for_each(|a| rc_lets_expr(a, confined, out));
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => {
            rc_lets_expr(expr, confined, out)
        }
        Expr::Field { base, .. } => rc_lets_expr(base, confined, out),
        Expr::Binary { lhs, rhs, .. }
        | Expr::Index { base: lhs, index: rhs }
        | Expr::Range { lo: lhs, hi: rhs, .. } => {
            rc_lets_expr(lhs, confined, out);
            rc_lets_expr(rhs, confined, out);
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            rc_lets_expr(base, confined, out);
            fields.iter().for_each(|(_, v)| rc_lets_expr(v, confined, out));
        }
        Expr::LabeledCall { .. } => {
            unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
        }
        Expr::Record { fields, spread, .. } => {
            fields.iter().for_each(|(_, v)| rc_lets_expr(v, confined, out));
            if let Some(sp) = spread {
                rc_lets_expr(sp, confined, out);
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

/// Walk every statement (recursing nested blocks once each) and record the two airtight
/// drop cases. The single-read scan looks at ONLY the current statement's own value
/// expression (never crossing into a nested block — those are separate statements the
/// recursion visits), so a per-iteration read is attributed to its own statement inside
/// the loop body, not to the whole loop.
///
/// SOUNDNESS — the single-use drop requires the value's `let` binding to be in the SAME
/// block as its read (`this_block_lets`). That guarantees the binding and the read run
/// at the same loop-nesting level: a value bound in a loop body and read there is fresh
/// each iteration (safe to free per iteration), whereas a value bound OUTSIDE a loop but
/// read once *inside* it would be read on every iteration — dropping it after the first
/// read would use-after-free on the next. Requiring same-block binding rules that out.
fn place_drops(
    block: &Block,
    fn_body: &Block,
    summaries: &Summaries,
    region_confined: &HashSet<String>,
    facts: &mut DropFacts,
) {
    // The lets in THIS block bound to a known heap allocator, with the free offset.
    // Restricting drops to these gives same-block binding (soundness) AND a definite
    // freeable heap pointer + offset (so codegen never frees a scalar or mis-offsets).
    let block_let_offsets: HashMap<&str, i32> = block
        .stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::Let { name, value: Expr::Call { name: f, args }, .. } => {
                fresh_heap_builtin_offset(f, args.len()).map(|off| (name.as_str(), off))
            }
            _ => None,
        })
        .collect();
    // (RFC-0035 step 3) Lets in THIS block bound to a `list.at` container READ: the read
    // is `$rc_dup`'d (step 1) so the binding owns a reference and must be `$rc_drop`'d at
    // its last use. Same-block binding (soundness, as the fresh case); codegen emits the
    // drop only for bindings it recorded as ACTUALLY dup'd (`rc_owned_bindings`, the same
    // per-type gate), always at rc-region offset 0.
    let block_read_lets: HashSet<&str> = block
        .stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::Let { name, value: Expr::Call { name: f, args }, .. }
                if f == "list.at" && args.len() == 2 =>
            {
                Some(name.as_str())
            }
            _ => None,
        })
        .collect();
    for s in &block.stmts {
        if let Stmt::Let { name, .. } = s {
            if !region_confined.contains(name)
                && name_reassign_count(fn_body, name) == 0
                && name_read_count(fn_body, name) == 0
            {
                if let Some(&off) = block_let_offsets.get(name.as_str()) {
                    facts.record(s, name.clone(), off);
                } else if block_read_lets.contains(name.as_str()) {
                    facts.record_read(s, name.clone());
                }
            }
        }
        if let Some(value) = stmt_value(s) {
            let mut candidates = Vec::new();
            nonleaking_call_arg_vars(value, summaries, &mut candidates);
            for v in candidates {
                let ok = !region_confined.contains(&v)
                    && name_reassign_count(fn_body, &v) == 0
                    && name_read_count(fn_body, &v) == 1;
                if !ok {
                    continue;
                }
                if let Some(&off) = block_let_offsets.get(v.as_str()) {
                    facts.record(s, v, off);
                } else if block_read_lets.contains(v.as_str()) {
                    facts.record_read(s, v);
                }
            }
        }
        each_block_in_stmt(s, &mut |blk| {
            place_drops(blk, fn_body, summaries, region_confined, facts)
        });
    }
}

/// Names appearing as a direct `Var` argument of a `Call` whose parameter does NOT leak
/// (`Summaries::arg_leaks` false: the callee neither retains the value nor returns it as
/// an alias). Recurses through value-position sub-expressions (so `f(g(v))` with a
/// non-leaking `g` is found), but STOPS at block boundaries (if/match/loop bodies,
/// `Block`, lambda) — those are separate statements `place_drops` visits, and crossing
/// them would mis-attribute (or double-count) a per-iteration drop.
fn nonleaking_call_arg_vars(e: &Expr, summaries: &Summaries, out: &mut Vec<String>) {
    if let Expr::Call { name, args } = e {
        for (i, a) in args.iter().enumerate() {
            if let Expr::Var(v) = a {
                if !summaries.arg_leaks(name, i, args.len()) {
                    out.push(v.clone());
                }
            }
        }
    }
    each_value_child(e, &mut |c| nonleaking_call_arg_vars(c, summaries, out));
}

/// Apply `f` to each value-position child EXPRESSION of `e` (operands, call args, match
/// arm bodies, the condition/iterator of a loop or `if`), but NOT into any nested block
/// (loop/branch bodies, `Block`, lambda body) — those are statement scopes handled by
/// `place_drops`'s own recursion.
fn each_value_child(e: &Expr, f: &mut impl FnMut(&Expr)) {
    match e {
        Expr::List(xs) | Expr::Tuple(xs) => xs.iter().for_each(f),
        Expr::Call { args, .. } | Expr::Ctor { args, .. } => args.iter().for_each(f),
        Expr::MethodCall { receiver, args, .. } => {
            f(receiver);
            args.iter().for_each(f);
        }
        Expr::Apply { func, args } => {
            f(func);
            args.iter().for_each(f);
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => f(expr),
        Expr::Field { base, .. } => f(base),
        Expr::Binary { lhs, rhs, .. }
        | Expr::Index { base: lhs, index: rhs }
        | Expr::Range { lo: lhs, hi: rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            f(base);
            fields.iter().for_each(|(_, v)| f(v));
        }
        Expr::LabeledCall { .. } => {
            unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
        }
        Expr::Record { fields, spread, .. } => {
            fields.iter().for_each(|(_, v)| f(v));
            if let Some(sp) = spread {
                f(sp);
            }
        }
        // `if`/loops: their condition/iterator is a value child; their bodies are blocks.
        Expr::If { cond, .. } => f(cond),
        Expr::While { cond, .. } => f(cond),
        Expr::WhileLet { scrutinee, .. } => f(scrutinee),
        Expr::For { iter, .. } => f(iter),
        // `match`: scrutinee + guards + arm bodies are value children (an arm body that is
        // itself a `Block` STOPS the recursion at that `Block` arm — handled by the block walk).
        Expr::Match { scrutinee, arms } => {
            f(scrutinee);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    f(g);
                }
                if !matches!(arm.body, Expr::Block(_)) {
                    f(&arm.body);
                }
            }
        }
        // Boundaries (a nested statement scope) and leaves: no value children to descend.
        Expr::Block(_)
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

/// Number of READ occurrences of `name` (an `Expr::Var(name)` in value position)
/// anywhere in `b`. A binding's own `let`/assign target is NOT a read.
fn name_read_count(b: &Block, name: &str) -> usize {
    let mut n = 0;
    for s in &b.stmts {
        match s {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Yield(value)
            | Stmt::Expr(value) => n += expr_read_count(value, name),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    n
}

fn expr_read_count(e: &Expr, name: &str) -> usize {
    let mut n = 0;
    match e {
        Expr::Var(v) => {
            if v == name {
                n += 1;
            }
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                n += expr_read_count(x, name);
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } => {
            for a in args {
                n += expr_read_count(a, name);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            n += expr_read_count(receiver, name);
            for a in args {
                n += expr_read_count(a, name);
            }
        }
        Expr::Apply { func, args } => {
            n += expr_read_count(func, name);
            for a in args {
                n += expr_read_count(a, name);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => {
            n += expr_read_count(expr, name);
        }
        Expr::Field { base, .. } => n += expr_read_count(base, name),
        Expr::Binary { lhs, rhs, .. }
        | Expr::Index { base: lhs, index: rhs }
        | Expr::Range { lo: lhs, hi: rhs, .. } => {
            n += expr_read_count(lhs, name);
            n += expr_read_count(rhs, name);
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            n += expr_read_count(base, name);
            for (_, v) in fields {
                n += expr_read_count(v, name);
            }
        }
        Expr::LabeledCall { .. } => {
            unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                n += expr_read_count(v, name);
            }
            if let Some(sp) = spread {
                n += expr_read_count(sp, name);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            n += expr_read_count(cond, name);
            n += name_read_count(then_block, name);
            if let Some(b) = else_block {
                n += name_read_count(b, name);
            }
        }
        Expr::Match { scrutinee, arms } => {
            n += expr_read_count(scrutinee, name);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    n += expr_read_count(g, name);
                }
                n += expr_read_count(&arm.body, name);
            }
        }
        Expr::Block(b) => n += name_read_count(b, name),
        Expr::While { cond, body } => {
            n += expr_read_count(cond, name);
            n += name_read_count(body, name);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            n += expr_read_count(scrutinee, name);
            n += name_read_count(body, name);
        }
        Expr::For { iter, body, .. } => {
            n += expr_read_count(iter, name);
            n += name_read_count(body, name);
        }
        Expr::Lambda { body, .. } => {
            // A read inside a lambda body captures `name` — counts as a use (the closure
            // keeps it alive), so it correctly disqualifies the dead-binding drop.
            n += name_read_count(body, name);
        }
    }
    n
}

/// Number of `name = …` reassignments anywhere in `b` — a write to `name`'s slot is
/// not a read, but it means the binding's value flows on (or its buffer is reused), so
/// the dead-binding drop does not apply.
fn name_reassign_count(b: &Block, name: &str) -> usize {
    let mut n = 0;
    for s in &b.stmts {
        if let Stmt::Assign { name: t, .. } = s {
            if t == name {
                n += 1;
            }
        }
        each_block_in_stmt(s, &mut |blk| n += name_reassign_count(blk, name));
    }
    n
}

/// Apply `f` to every `Block` nested directly inside statement `s` (loop/branch/match
/// bodies, scope blocks, lambda bodies). Mirrors the existing analysis walkers; used by
/// the drop passes to recurse without re-deriving control-flow structure.
fn each_block_in_stmt(s: &Stmt, f: &mut impl FnMut(&Block)) {
    match s {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Return(Some(value))
        | Stmt::Yield(value)
        | Stmt::Expr(value) => each_block_in_expr(value, f),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
    }
}

fn each_block_in_expr(e: &Expr, f: &mut impl FnMut(&Block)) {
    match e {
        Expr::If { cond, then_block, else_block } => {
            each_block_in_expr(cond, f);
            f(then_block);
            if let Some(b) = else_block {
                f(b);
            }
        }
        Expr::Match { scrutinee, arms } => {
            each_block_in_expr(scrutinee, f);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    each_block_in_expr(g, f);
                }
                each_block_in_expr(&arm.body, f);
            }
        }
        Expr::Block(b) => f(b),
        Expr::While { cond, body } => {
            each_block_in_expr(cond, f);
            f(body);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            each_block_in_expr(scrutinee, f);
            f(body);
        }
        Expr::For { iter, body, .. } => {
            each_block_in_expr(iter, f);
            f(body);
        }
        // A lambda body is a SEPARATE compile unit (its own params + captured-by-value
        // environment). The drop/let passes must not cross into it — a captured value
        // belongs to the closure, and dropping it inside the body would double-free on
        // repeated calls. (Read-counting DOES descend into lambdas — a capture is a use.)
        Expr::Lambda { .. } => {}
        Expr::List(xs) | Expr::Tuple(xs) => xs.iter().for_each(|x| each_block_in_expr(x, f)),
        Expr::Call { args, .. } | Expr::Ctor { args, .. } => {
            args.iter().for_each(|a| each_block_in_expr(a, f))
        }
        Expr::MethodCall { receiver, args, .. } => {
            each_block_in_expr(receiver, f);
            args.iter().for_each(|a| each_block_in_expr(a, f));
        }
        Expr::Apply { func, args } => {
            each_block_in_expr(func, f);
            args.iter().for_each(|a| each_block_in_expr(a, f));
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => {
            each_block_in_expr(expr, f)
        }
        Expr::Field { base, .. } => each_block_in_expr(base, f),
        Expr::Binary { lhs, rhs, .. }
        | Expr::Index { base: lhs, index: rhs }
        | Expr::Range { lo: lhs, hi: rhs, .. } => {
            each_block_in_expr(lhs, f);
            each_block_in_expr(rhs, f);
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            each_block_in_expr(base, f);
            fields.iter().for_each(|(_, v)| each_block_in_expr(v, f));
        }
        Expr::LabeledCall { .. } => {
            unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
        }
        Expr::Record { fields, spread, .. } => {
            fields.iter().for_each(|(_, v)| each_block_in_expr(v, f));
            if let Some(sp) = spread {
                each_block_in_expr(sp, f);
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

#[cfg(test)]
mod last_use_tests {
    use super::*;
    use witchy_syntax::parser;

    fn drops(src: &str) -> DropFacts {
        let m = parser::parse_module(src).expect("parse");
        let summaries = Summaries::of_module(&m);
        let f = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("a function");
        last_use_drops(&f.body, &summaries)
    }

    // ---- case 1: dead binding (never read) ----

    #[test]
    fn unread_binding_is_dropped() {
        let d = drops("import list\nfn f():\n    let buf = list.push([], 1)\n    0\n");
        assert_eq!(d.total(), 1, "the never-read `buf` is a dead-binding drop");
    }

    #[test]
    fn multiple_dead_bindings() {
        let d = drops("import list\nfn f():\n    let a = list.push([], 1)\n    let b = list.push([], 2)\n    0\n");
        assert_eq!(d.total(), 2, "both `a` and `b` are never read");
    }

    // ---- case 2: single use in a non-leaking call argument ----

    /// Read exactly once, as the arg of a non-leaking call (`list.length` reads its
    /// arg and returns an Int) → dropped after that statement.
    #[test]
    fn single_use_nonleaking_call_is_dropped() {
        let d = drops("import list\nfn f() -> Int:\n    let buf = list.push([], 1)\n    list.length(buf)\n");
        assert_eq!(d.total(), 1, "`buf`'s single non-leaking read is its last use");
    }

    /// The soak/residual shape: per-iteration scratch bound in a loop body, read once in
    /// a non-leaking call → dropped INSIDE the body (per iteration), bounding the loop.
    #[test]
    fn single_use_scratch_in_loop_drops_per_iteration() {
        let d = drops("import list\nfn f() -> Int:\n    var sum = 0\n    var i = 0\n    while i < 5:\n        let tmp = list.push([], i)\n        sum = sum + list.length(tmp)\n        i = i + 1\n    sum\n");
        assert_eq!(d.total(), 1, "`tmp` (per-iteration scratch) drops; `sum`/`i` are reassigned scalars");
    }

    /// SOUNDNESS: a value bound OUTSIDE a loop but read once INSIDE it is read on every
    /// iteration — it must NOT be dropped after the read (that would use-after-free on
    /// the next iteration). The same-block-binding rule rules this out.
    #[test]
    fn outer_binding_read_in_loop_is_not_dropped() {
        let d = drops("import list\nfn f() -> Int:\n    let v = list.push([], 1)\n    var acc = 0\n    var i = 0\n    while i < 3:\n        acc = acc + list.length(v)\n        i = i + 1\n    acc\n");
        assert_eq!(d.total(), 0, "`v` is bound outside the loop; a per-iteration drop would UAF");
    }

    // ---- adversarial: each of these must produce NO drop (soundness) ----

    /// A value stored into a constructor/collection is retained by it — never dropped.
    #[test]
    fn stored_in_constructor_is_not_dropped() {
        let d = drops("import list\nfn f() -> List(List(Int)):\n    let v = list.push([], 1)\n    let xs = [v]\n    xs\n");
        assert_eq!(d.total(), 0, "`v` is stored in `xs`; `xs` is returned — no drop");
    }

    /// A value passed to a LEAKING arg (`list.push` STORES its element operand) is kept
    /// live — never dropped. The list operand is a parameter so it can't confound the
    /// count; only `v` (arg 1, which leaks) is a local candidate, and it must NOT drop.
    #[test]
    fn leaking_call_arg_is_not_dropped() {
        let d = drops("import list\nfn f(other: List(Int)) -> List(Int):\n    let v = list.push([], 1)\n    list.push(other, v)\n");
        assert_eq!(d.total(), 0, "`v` leaks into the list (arg 1 is stored); `other` is a param");
    }

    /// Read more than once ⇒ the single-use case does not apply (needs real liveness).
    #[test]
    fn read_twice_is_not_dropped() {
        let d = drops("import list\nfn f() -> Int:\n    let v = list.push([], 1)\n    let n = list.length(v) + list.length(v)\n    n\n");
        assert_eq!(d.total(), 0, "`v` is read twice; `n` is returned");
    }

    /// A parameter is caller-owned — the callee must never drop it.
    #[test]
    fn parameter_is_not_dropped() {
        let d = drops("import list\nfn f(xs: List(Int)) -> Int:\n    list.length(xs)\n");
        assert_eq!(d.total(), 0, "`xs` is a parameter, not a local");
    }

    /// A value captured by a closure belongs to the closure — never dropped by the outer
    /// pass (dropping it inside the lambda body would double-free on repeated calls).
    #[test]
    fn captured_binding_is_not_dropped() {
        let d = drops("import list\nfn f() -> fn() -> Int:\n    let buf = list.push([], 1)\n    fn(): list.length(buf)\n");
        assert_eq!(d.total(), 0, "`buf` is captured by the returned closure");
    }

    /// A value bound inside a `region:` block is reclaimed by the region — the RC floor
    /// must not also free it (double-free). Otherwise `tmp` would be a single-use drop.
    #[test]
    fn region_confined_value_is_not_dropped() {
        let d = drops("import list\nfn f() -> Int:\n    let n = region -> Int:\n        let tmp = list.push([], 1)\n        list.length(tmp)\n    n\n");
        assert_eq!(d.total(), 0, "`tmp` is region-confined; the region frees it, not the RC floor");
    }

    /// A reassigned binding's churn is rc-floor's free-at-overwrite, not this pass.
    #[test]
    fn reassigned_binding_is_not_dropped() {
        let d = drops("import list\nfn f():\n    var buf = []\n    buf = list.push(buf, 1)\n    0\n");
        assert_eq!(d.total(), 0, "`buf` is reassigned");
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
