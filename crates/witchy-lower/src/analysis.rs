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

use witchy_syntax::ast::{
    BinOp, Block, Convention, Expr, Item, Module, Stmt, Type, TypeQual, UnOp,
};
use witchy_syntax::intrinsics;

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
    /// The first soundness reason for each kill. Codegen only needs `kills`,
    /// while `mode opt` uses this provenance to turn a missed no-copy proof
    /// into an actionable diagnostic.
    kill_reasons: HashMap<usize, HashMap<String, String>>,
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

    pub fn kill_reason_after<'a>(&'a self, stmt: &Stmt, var: &str) -> Option<&'a str> {
        self.kill_reasons
            .get(&stmt_key(stmt))
            .and_then(|reasons| reasons.get(var))
            .map(String::as_str)
    }

    /// Whether a self-assign site must run with a forced zero token. A
    /// missed lookup (unknown statement) answers DIRTY — the sound default.
    pub fn is_dirty(&self, stmt: &Stmt) -> bool {
        self.dirty.contains(&stmt_key(stmt))
    }

    /// RFC-0083: opening a view creates a live whole alias of its owner. Merge
    /// that checked event into the same conservative cap-kill product used for
    /// ordinary shares, so RFC-0088 extraction cannot mutate through the alias.
    pub fn merge_loan_kills(
        &mut self,
        body: &Block,
        loans: &witchy_types::loans::LoanFacts,
    ) {
        fn walk(
            facts: &mut Facts,
            block: &Block,
            loans: &witchy_types::loans::LoanFacts,
        ) {
            for stmt in &block.stmts {
                let mut added = 0;
                {
                    let entry = facts.kills.entry(stmt_key(stmt)).or_default();
                    for loan in loans.opens_after(stmt) {
                        if facts.accumulators.contains(&loan.owner) {
                            if !entry.contains(&loan.owner) {
                                entry.push(loan.owner.clone());
                                added += 1;
                            }
                            facts
                                .kill_reasons
                                .entry(stmt_key(stmt))
                                .or_default()
                                .insert(
                                    loan.owner.clone(),
                                    format!("loaned to view `{}` by `{}`", loan.view, loan.origin),
                                );
                        }
                    }
                }
                facts.kill_entries += added;
                each_block_in_stmt(stmt, &mut |nested| walk(facts, nested, loans));
            }
        }

        walk(self, body, loans);
    }
}

/// Logical ownership state and uniform physical channels for one finalized
/// callable signature.
///
/// Checked call expressions, no-copy enforcement, accumulator discovery, and
/// code generation all derive these axes here. This keeps an indirect call's
/// hidden capacity arguments tied to the same checked access identity that
/// authorized the source call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallOwnershipFact {
    consuming_state_param: Option<usize>,
    own_capacity_param: Option<usize>,
    var_capacity_params: Vec<usize>,
    no_copy_var_params: Vec<usize>,
    // (RFC-0110 criterion 2) `own unique` (Consuming + Unique/LocalUnique)
    // parameter indices. Kept SEPARATE from `no_copy_var_params` (which codegen
    // consumes for var write-back capacity-slot decisions) so widening the
    // uniqueness-miss detector to consuming params cannot change owned-argument
    // lowering. Only the no-copy miss detector reads this.
    own_unique_params: Vec<usize>,
    unique_capacity_result: bool,
}

impl CallOwnershipFact {
    /// The consuming input that carries backend-neutral physical ownership.
    /// Layout-dependent inputs are included even when their exact direct ABI
    /// needs no legacy trailing capacity slot.
    pub fn consuming_state_param(&self) -> Option<usize> {
        self.consuming_state_param
    }

    pub fn own_capacity_param(&self) -> Option<usize> {
        self.own_capacity_param
    }

    pub fn var_capacity_params(&self) -> &[usize] {
        &self.var_capacity_params
    }

    pub fn no_copy_var_params(&self) -> &[usize] {
        &self.no_copy_var_params
    }

    /// (RFC-0110) `own unique` parameter indices — the consuming counterpart of
    /// `no_copy_var_params`. The no-copy miss detector unions the two so every
    /// source-facing `unique` parameter (var and own) is checked at every call
    /// shape; codegen never reads this (owned-arg lowering is unchanged).
    pub fn own_unique_params(&self) -> &[usize] {
        &self.own_unique_params
    }

    /// (RFC-0110 criterion 2) Every source-facing `unique` parameter to check
    /// for a no-copy miss — the union of `var unique` write-back params and
    /// `own unique` consuming params, deduplicated and ordered. This is the ONLY
    /// axis the miss detector consumes; codegen keeps reading `no_copy_var_params`
    /// alone so owned-argument lowering is byte-identical.
    pub fn unique_params_to_check(&self) -> Vec<usize> {
        let mut indices = self.no_copy_var_params.clone();
        for &index in &self.own_unique_params {
            if !indices.contains(&index) {
                indices.push(index);
            }
        }
        indices.sort_unstable();
        indices
    }

    pub fn unique_capacity_result(&self) -> bool {
        self.unique_capacity_result
    }

    fn argument_may_alias_out(&self, index: usize) -> bool {
        self.consuming_state_param != Some(index)
            && !self.var_capacity_params.contains(&index)
    }
}

fn type_has_capacity_token(ty: &Type) -> bool {
    match ty {
        Type::Named(name, _) => matches!(name.as_str(), "List" | "Dict" | "String" | "Bytes"),
        Type::Qualified(_, inner) => type_has_capacity_token(inner),
        _ => false,
    }
}

fn type_is_unique_capacity_shape(ty: &Type) -> bool {
    matches!(ty.unqualified(), Type::Named(name, _)
        if matches!(name.as_str(), "List" | "Dict"))
}

pub fn call_ownership_fact(
    signature: &witchy_types::access::AccessSignature,
) -> CallOwnershipFact {
    use witchy_types::access::{AccessKind, AccessQualifier, OwnershipStateClass};

    let owns_physical_state = |state: &OwnershipStateClass| {
        matches!(
            state,
            OwnershipStateClass::LinearMemoryObject
                | OwnershipStateClass::LayoutDependent { .. }
        )
    };
    let consuming_state_param =
        signature.params().iter().enumerate().find_map(|(index, param)| {
            (param.kind() == AccessKind::Consuming
                && param.ownership().input().is_some_and(owns_physical_state))
            .then_some(index)
        });
    // The backend-neutral fact above remains true for exact LayoutId values.
    // This narrower axis records only the uniform container capacity channel;
    // codegen decides whether a non-container consuming state needs its legacy
    // compatibility slot after consulting the named callable layout.
    let is_legacy_capacity_state = |state: &OwnershipStateClass| {
        matches!(state, OwnershipStateClass::LinearMemoryObject)
    };
    let own_capacity_param = signature.params().iter().enumerate().find_map(|(index, param)| {
        (param.kind() == AccessKind::Consuming
            && type_has_capacity_token(param.ty())
            && param
                .ownership()
                .input()
                .is_some_and(is_legacy_capacity_state))
        .then_some(index)
    });
    let var_capacity_params = signature
        .params()
        .iter()
        .enumerate()
        .filter_map(|(index, param)| {
            (param.kind() == AccessKind::ExclusiveWriteback
                && param.ownership().writeback().is_some()
                && type_has_capacity_token(param.ty()))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    let no_copy_var_params = signature
        .params()
        .iter()
        .enumerate()
        .filter_map(|(index, param)| {
            (param.kind() == AccessKind::ExclusiveWriteback
                && param.qualifiers().iter().any(|qualifier| {
                    matches!(qualifier, AccessQualifier::Unique | AccessQualifier::LocalUnique)
                }))
            .then_some(index)
        })
        .collect();
    // (RFC-0110 criterion 2) The consuming (`own unique`) counterpart. Separate
    // vector — codegen must not see it; only the miss detector unions it with
    // `no_copy_var_params`. `let`-borrowed (`SharedBorrow`) uniques are excluded:
    // a shared borrow cannot mutate, so a re-own repair is meaningless there.
    let own_unique_params = signature
        .params()
        .iter()
        .enumerate()
        .filter_map(|(index, param)| {
            (param.kind() == AccessKind::Consuming
                && param.qualifiers().iter().any(|qualifier| {
                    matches!(qualifier, AccessQualifier::Unique | AccessQualifier::LocalUnique)
                }))
            .then_some(index)
        })
        .collect();
    // Result uniqueness remains a logical proof for both legacy containers and
    // layout-dependent values. Codegen refines an exact layout result through
    // `signature_has_unique_layout_result`; erasing this fact here would break
    // the next no-copy proof even when no trailing capacity result is emitted.
    let unique_capacity_result = signature
        .result()
        .ownership_output()
        .is_some_and(|state| {
            matches!(
                state,
                OwnershipStateClass::LinearMemoryObject
                    | OwnershipStateClass::LayoutDependent { .. }
            )
        })
        && type_is_unique_capacity_shape(signature.result().ty());
    CallOwnershipFact {
        consuming_state_param,
        own_capacity_param,
        var_capacity_params,
        no_copy_var_params,
        own_unique_params,
        unique_capacity_result,
    }
}

fn checked_call_ownership_fact(
    module: &Module,
    access: &witchy_types::access::CheckedAccessFacts<'_>,
    expression: &Expr,
) -> Option<CallOwnershipFact> {
    let signature = access.call_at(module, expression)?;
    let mut fact = call_ownership_fact(signature);
    // Place assignment lowers to a compiler-private, value-returning Dict
    // helper. Its checked signature carries the exact `unique` physical input,
    // while the source place supplies the write-back edge. Preserve that edge
    // in this same per-call fact so accumulator, kill, and no-copy consumers do
    // not need a second structural-call authority.
    if matches!(expression, Expr::Call { name, .. } if private_structural_helper(name))
        && signature.params().first().is_some_and(|param| {
            type_has_capacity_token(param.ty())
                && param.ownership().input().is_some()
                && param.qualifiers().iter().any(|qualifier| {
                    matches!(
                        qualifier,
                        witchy_types::access::AccessQualifier::Unique
                            | witchy_types::access::AccessQualifier::LocalUnique
                    )
                })
        })
    {
        if !fact.var_capacity_params.contains(&0) {
            fact.var_capacity_params.push(0);
        }
        if !fact.no_copy_var_params.contains(&0) {
            fact.no_copy_var_params.push(0);
        }
    }
    Some(fact)
}

#[derive(Clone, Copy)]
struct CheckedCallContext<'facts, 'module> {
    module: &'module Module,
    access: &'facts witchy_types::access::CheckedAccessFacts<'module>,
}

impl CheckedCallContext<'_, '_> {
    fn fact(&self, expression: &Expr) -> Option<CallOwnershipFact> {
        checked_call_ownership_fact(self.module, self.access, expression)
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
        if (matches!(
            f.as_str(),
            "list.push" | intrinsics::LIST_PUSH | intrinsics::GENERATED_LIST_PUSH
        ) || f.starts_with("list.push__"))
            && args.len() == 2
        {
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
        if (matches!(f.as_str(), "dict.insert" | intrinsics::DICT_INSERT)
            || f.starts_with("dict.insert__")
            || f
                .strip_prefix(intrinsics::DICT_INSERT)
                .is_some_and(|suffix| suffix.starts_with("__")))
            && args.len() == 3
        {
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
        if (matches!(f.as_str(), "dict.update" | intrinsics::DICT_UPDATE)
            || f.starts_with("dict.update__"))
            && args.len() == 4
        {
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
        if (matches!(f.as_str(), "list.set_at" | intrinsics::LIST_SET_AT)
            || f.starts_with("list.set_at__"))
            && args.len() == 3
        {
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
        if (matches!(f.as_str(), "list.update_at" | "list.__update_at")
            || f.starts_with("list.update_at__"))
            && args.len() == 3
        {
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
    self_inplace_op(name, value).is_some()
        || self_own_call(name, value, summaries).is_some()
        || self_private_structural_call(name, value)
}

fn private_structural_helper(name: &str) -> bool {
    matches!(name, intrinsics::DICT_INSERT | intrinsics::DICT_REMOVE)
        || [intrinsics::DICT_INSERT, intrinsics::DICT_REMOVE]
            .into_iter()
            .any(|base| {
                name.strip_prefix(base)
                    .is_some_and(|suffix| suffix.starts_with("__"))
            })
}

fn self_private_structural_call(name: &str, value: &Expr) -> bool {
    matches!(value, Expr::Call { name: callee, args }
        if private_structural_helper(callee)
            && matches!(args.first(), Some(Expr::Var(root)) if root == name))
}

/// The RFC-0087 statement form of an operation backed by the existing
/// RFC-0051 in-place paths. Trait lowering mangles generic public functions, so
/// recognize both their bare and specialized names. Arbitrary `var` calls are
/// deliberately excluded: only operations with a proven fast path establish an
/// accumulator fact.
pub(crate) fn direct_inplace_root(value: &Expr) -> Option<&str> {
    let Expr::Call { name, args } = value else { return None };
    let recognized = matches!(name.as_str(), "list.push" | "list.set_at" | "list.update_at"
        | "dict.insert" | "dict.update")
        || ["list.push__", "list.set_at__", "list.update_at__", "dict.insert__", "dict.update__"]
            .iter()
            .any(|prefix| name.starts_with(prefix));
    if !recognized {
        return None;
    }
    match args.first() {
        Some(Expr::Var(root)) => Some(root),
        _ => None,
    }
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
    /// True when some path may allocate linear-memory storage or perform
    /// reclamation whose state a loop watermark must preserve.
    may_allocate: bool,
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

fn view_lifetime(ty: Option<&Type>) -> Option<&str> {
    match ty {
        Some(Type::Qualified(TypeQual::Borrow(lifetime) | TypeQual::LegacyBorrow(lifetime), _)) => Some(lifetime),
        _ => None,
    }
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
        // Free functions summarize under their own names; inherent impl methods
        // under the generated implementation symbol (`{Type}__{method}`) that the
        // linker's module-function aliases target (RFC-0099: `list.contains(xs, t)`
        // rewrites to `List__contains`), so a pre-lowering caller resolves a
        // method callee's summary exactly as it would a free function's. Trait/
        // impl lowering erases `Item::Impl`, so the method arm is a no-op on the
        // lowered path.
        let mut sources: Vec<(String, &witchy_syntax::ast::Function)> = Vec::new();
        for item in &module.items {
            match item {
                Item::Function(f) => sources.push((f.name.clone(), f)),
                Item::Impl(im) if im.trait_name.is_none() => {
                    for f in &im.methods {
                        sources.push((format!("{}__{}", im.type_name, f.name), f));
                    }
                }
                _ => {}
            }
        }
        for (name, f) in sources {
            let returned_lifetime = view_lifetime(f.ret.as_ref());
            let may_alias_out = f
                .params
                .iter()
                .map(|param| {
                    returned_lifetime.is_some_and(|lifetime| {
                        view_lifetime(param.ty.as_ref()) == Some(lifetime)
                    })
                })
                .collect();
            fns.insert(
                name.clone(),
                FnInfo {
                    convs: f.params.iter().map(|p| p.convention).collect(),
                    // The declared output-to-input lifetime relation is an
                    // immediate alias source. Other positions start at the
                    // optimistic bottom and rise through the body fixpoint.
                    may_alias_out,
                    may_allocate: false,
                    own_abi: None,
                },
            );
            bodies.insert(name, f);
        }
        let mut summaries = Summaries { fns };
        // Allocation/reclamation effects use the dual fixed point from alias
        // escape. A function has a local source (constructor, collection/string
        // operation, unknown/indirect/host call, region) or inherits the effect
        // from a direct user callee. Starting clean makes effect-free recursive
        // components clean; any concrete source propagates through the component.
        let known: HashSet<String> = bodies.keys().cloned().collect();
        let allocation_effects: HashMap<String, (bool, HashSet<String>)> = bodies
            .iter()
            .map(|(name, function)| {
                let mut scan = AllocationScan::new(&known);
                scan.block(&function.body);
                (name.clone(), (scan.local_effect, scan.callees))
            })
            .collect();
        loop {
            let mut changed = false;
            for (name, (local_effect, callees)) in &allocation_effects {
                let inherited = callees.iter().any(|callee| {
                    summaries
                        .fns
                        .get(callee)
                        .is_none_or(|info| info.may_allocate)
                });
                if (*local_effect || inherited)
                    && !summaries.fns[name.as_str()].may_allocate
                {
                    summaries.fns.get_mut(name.as_str()).unwrap().may_allocate = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        loop {
            let mut changed = false;
            for (name, f) in &bodies {
                for (i, p) in f.params.iter().enumerate() {
                    if p.convention == Convention::Borrow {
                        // An ordinary `let` borrow cannot escape. A returnable
                        // view is the explicit exception and was seeded from
                        // the signature relation above.
                        continue;
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
                        && matches!(
                            p.ty.as_ref().map(Type::unqualified),
                            Some(Type::Named(n, _)) if heap_types.contains(n)
                        )
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

    /// Conservative whole-block allocation/reclamation effect. Direct user
    /// calls consult the transitive summary; every unresolved boundary is an
    /// effect source by construction.
    pub fn block_may_allocate(&self, body: &Block) -> bool {
        let known: HashSet<String> = self.fns.keys().cloned().collect();
        let mut scan = AllocationScan::new(&known);
        scan.block(body);
        scan.local_effect
            || scan.callees.iter().any(|callee| {
                self.fns
                    .get(callee)
                    .is_none_or(|info| info.may_allocate)
            })
    }

    pub fn call_may_allocate(&self, name: &str) -> bool {
        self.fns.get(name).is_none_or(|info| info.may_allocate)
    }

    /// Can the storage passed at `idx` flow out through the call's result or
    /// another caller-observable alias? This is deliberately separate from
    /// [`Self::arg_leaks`]: an `own` argument kills the caller's input binding,
    /// but the callee may still return that exact storage.
    pub fn arg_may_alias_out(&self, name: &str, idx: usize) -> bool {
        self.fns
            .get(name)
            .and_then(|info| info.may_alias_out.get(idx))
            .copied()
            .unwrap_or(true)
    }

    /// Parameter positions written back through the uniform `var` ABI. This is
    /// the operation-independent ownership hook: callers can attach tokens from
    /// conventions without recognizing a source method name.
    pub fn var_arg_indices(&self, name: &str) -> impl Iterator<Item = usize> + '_ {
        self.fns
            .get(name)
            .into_iter()
            .flat_map(|info| info.convs.iter().enumerate())
            .filter_map(|(index, convention)| {
                (*convention == Convention::Var).then_some(index)
            })
    }

    /// Is the value passed in argument position `idx` of a call to `name`
    /// LIVE after the call (a whole-alias the caller can observe)?
    fn arg_live(&self, name: &str, idx: usize) -> bool {
        match self.fns.get(name) {
            Some(info) => match info.convs.get(idx) {
                // Explicit `let` normally borrows only for the call. RFC-0083
                // permits the signature to tie a returned view to this input;
                // that relation is a caller-observable storage alias.
                Some(Convention::Borrow) => {
                    info.may_alias_out.get(idx).copied().unwrap_or(true)
                }
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

/// Syntactic allocation/reclamation sources plus direct-user-call edges. This
/// is intentionally default-deny: recognizing another allocation-free host or
/// intrinsic requires an explicit proof here rather than an optimistic guess.
struct AllocationScan<'a> {
    known: &'a HashSet<String>,
    local_effect: bool,
    callees: HashSet<String>,
}

impl<'a> AllocationScan<'a> {
    fn new(known: &'a HashSet<String>) -> Self {
        Self {
            known,
            local_effect: false,
            callees: HashSet::new(),
        }
    }

    fn block(&mut self, block: &Block) {
        if block.region.is_some() {
            self.local_effect = true;
        }
        for statement in &block.stmts {
            match statement {
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Expr(value) => self.expr(value),
                Stmt::Yield(value) => {
                    self.local_effect = true;
                    self.expr(value);
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Var(_) => {}
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::ExistentialUpcast { expr, .. }
            | Expr::Field { base: expr, .. } => self.expr(expr),
            Expr::Binary { op, lhs, rhs } => {
                if matches!(op, BinOp::Concat) {
                    self.local_effect = true;
                }
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                self.expr(cond);
                self.block(then_block);
                if let Some(block) = else_block {
                    self.block(block);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.expr(&arm.body);
                }
            }
            Expr::Block(block) => self.block(block),
            Expr::While { cond, body } => {
                self.expr(cond);
                self.block(body);
            }
            Expr::For { iter, body, .. } => {
                self.expr(iter);
                self.block(body);
            }
            Expr::WhileLet {
                scrutinee, body, ..
            } => {
                self.expr(scrutinee);
                self.block(body);
            }
            Expr::Range { lo, hi, .. } => {
                self.expr(lo);
                self.expr(hi);
            }
            Expr::Index { base, index } => {
                // Index lowering may cross a list/dict boundary. Keep the
                // watermark until a typed, operation-specific proof exists.
                self.local_effect = true;
                self.expr(base);
                self.expr(index);
            }
            Expr::Call { name, args } => {
                for argument in args {
                    self.expr(argument);
                }
                if self.known.contains(name) {
                    self.callees.insert(name.clone());
                } else {
                    self.local_effect = true;
                }
            }
            Expr::List(items)
            | Expr::Tuple(items)
            | Expr::Ctor { args: items, .. }
            | Expr::AnonCtor { args: items, .. } => {
                self.local_effect = true;
                for item in items {
                    self.expr(item);
                }
            }
            Expr::RecordUpdate { base, fields, .. } => {
                self.local_effect = true;
                self.expr(base);
                for (_, value) in fields {
                    self.expr(value);
                }
            }
            Expr::Record { fields, spread, .. } => {
                self.local_effect = true;
                for (_, value) in fields {
                    self.expr(value);
                }
                if let Some(value) = spread {
                    self.expr(value);
                }
            }
            Expr::Lambda { body, .. } => {
                self.local_effect = true;
                self.block(body);
            }
            Expr::ExistentialPack { expr, .. } => {
                self.local_effect = true;
                self.expr(expr);
            }
            Expr::Apply { func, args } => {
                self.local_effect = true;
                self.expr(func);
                for argument in args {
                    self.expr(argument);
                }
            }
            Expr::MethodCall { receiver, args, .. }
            | Expr::ExistentialCall { receiver, args, .. } => {
                self.local_effect = true;
                self.expr(receiver);
                for argument in args {
                    self.expr(argument);
                }
            }
            Expr::LabeledCall { args, .. } => {
                self.local_effect = true;
                for (_, argument) in args {
                    self.expr(argument);
                }
            }
            Expr::LabeledMethodCall { receiver, args, .. } => {
                self.local_effect = true;
                self.expr(receiver);
                for (_, argument) in args {
                    self.expr(argument);
                }
            }
            Expr::TaggedLit { .. } => self.local_effect = true,
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
        calls: None,
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
    analyze_with_calls(body, summaries, None)
}

pub fn analyze_with_access(
    body: &Block,
    summaries: &Summaries,
    module: &Module,
    access: &witchy_types::access::CheckedAccessFacts<'_>,
) -> Facts {
    analyze_with_calls(body, summaries, Some(CheckedCallContext { module, access }))
}

fn analyze_with_calls(
    body: &Block,
    summaries: &Summaries,
    calls: Option<CheckedCallContext<'_, '_>>,
) -> Facts {
    // Pass A: accumulators + per-loop self-assign site sets (for cliffs).
    let mut accs = HashSet::new();
    let mut loop_sites: HashMap<usize, HashSet<String>> = HashMap::new();
    collect_accumulators(
        body,
        summaries,
        calls,
        &mut accs,
        &mut Vec::new(),
        &mut loop_sites,
    );
    if accs.is_empty() {
        return Facts::default();
    }
    // Pass B: share events, dirty sites, cliffs.
    let mut w = Walker {
        accs: &accs,
        summaries,
        calls,
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
    calls: Option<CheckedCallContext<'_, '_>>,
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
        } else if let Stmt::Expr(value) = stmt
            && let Some(name) = direct_inplace_root(value)
        {
            accs.insert(name.to_string());
            for lp in loop_ptrs.iter() {
                loop_sites.entry(*lp).or_default().insert(name.to_string());
            }
        }
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Expr(value)
            | Stmt::Yield(value) => {
                collect_accumulators_expr(value, summaries, calls, accs, loop_ptrs, loop_sites)
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_accumulators_expr(
    e: &Expr,
    summaries: &Summaries,
    calls: Option<CheckedCallContext<'_, '_>>,
    accs: &mut HashSet<String>,
    loop_ptrs: &mut Vec<usize>,
    loop_sites: &mut HashMap<usize, HashSet<String>>,
) {
    match e {
        Expr::While { cond, body } => {
            collect_accumulators_expr(cond, summaries, calls, accs, loop_ptrs, loop_sites);
            loop_ptrs.push(body as *const Block as usize);
            collect_accumulators(body, summaries, calls, accs, loop_ptrs, loop_sites);
            loop_ptrs.pop();
        }
        Expr::For { iter, body, .. } => {
            collect_accumulators_expr(iter, summaries, calls, accs, loop_ptrs, loop_sites);
            loop_ptrs.push(body as *const Block as usize);
            collect_accumulators(body, summaries, calls, accs, loop_ptrs, loop_sites);
            loop_ptrs.pop();
        }
        Expr::If { cond, then_block, else_block } => {
            collect_accumulators_expr(cond, summaries, calls, accs, loop_ptrs, loop_sites);
            collect_accumulators(then_block, summaries, calls, accs, loop_ptrs, loop_sites);
            if let Some(b) = else_block {
                collect_accumulators(b, summaries, calls, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_accumulators_expr(scrutinee, summaries, calls, accs, loop_ptrs, loop_sites);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_accumulators_expr(g, summaries, calls, accs, loop_ptrs, loop_sites);
                }
                collect_accumulators_expr(&arm.body, summaries, calls, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::Block(b) => collect_accumulators(b, summaries, calls, accs, loop_ptrs, loop_sites),
        // Lambda interiors are separate compile units with their own facts.
        Expr::Lambda { .. } => {}
        Expr::Binary { lhs, rhs, .. } => {
            collect_accumulators_expr(lhs, summaries, calls, accs, loop_ptrs, loop_sites);
            collect_accumulators_expr(rhs, summaries, calls, accs, loop_ptrs, loop_sites);
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => {
            collect_accumulators_expr(expr, summaries, calls, accs, loop_ptrs, loop_sites)
        }
        Expr::Call { name, args } => {
            if let Some(calls) = calls {
                if let Some(fact) = calls.fact(e) {
                    let operands = args.iter().collect::<Vec<_>>();
                    collect_call_accumulator_roots(
                        &operands,
                        &fact,
                        accs,
                        loop_ptrs,
                        loop_sites,
                    );
                }
            } else {
                let mut indices = summaries.var_arg_indices(name).collect::<Vec<_>>();
                if private_structural_helper(name) && !indices.contains(&0) {
                    indices.push(0);
                }
                for index in indices {
                    if let Some(Expr::Var(root)) = args.get(index) {
                        accs.insert(root.clone());
                        for loop_ptr in loop_ptrs.iter() {
                            loop_sites.entry(*loop_ptr).or_default().insert(root.clone());
                        }
                    }
                }
            }
            for arg in args {
                collect_accumulators_expr(arg, summaries, calls, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::Ctor { args, .. }
        | Expr::AnonCtor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => {
            for a in args {
                collect_accumulators_expr(a, summaries, calls, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::Apply { func, args } => {
            if let Some(fact) = calls.and_then(|calls| calls.fact(e)) {
                let operands = args.iter().collect::<Vec<_>>();
                collect_call_accumulator_roots(
                    &operands,
                    &fact,
                    accs,
                    loop_ptrs,
                    loop_sites,
                );
            }
            collect_accumulators_expr(func, summaries, calls, accs, loop_ptrs, loop_sites);
            for a in args {
                collect_accumulators_expr(a, summaries, calls, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            collect_accumulators_expr(base, summaries, calls, accs, loop_ptrs, loop_sites);
            for (_, v) in fields {
                collect_accumulators_expr(v, summaries, calls, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::LabeledCall { .. } | Expr::LabeledMethodCall { .. } => {
            unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                collect_accumulators_expr(v, summaries, calls, accs, loop_ptrs, loop_sites);
            }
            if let Some(s) = spread {
                collect_accumulators_expr(s, summaries, calls, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::Range { lo, hi, .. } => {
            collect_accumulators_expr(lo, summaries, calls, accs, loop_ptrs, loop_sites);
            collect_accumulators_expr(hi, summaries, calls, accs, loop_ptrs, loop_sites);
        }
        Expr::Index { base, index } => {
            collect_accumulators_expr(base, summaries, calls, accs, loop_ptrs, loop_sites);
            collect_accumulators_expr(index, summaries, calls, accs, loop_ptrs, loop_sites);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            collect_accumulators_expr(scrutinee, summaries, calls, accs, loop_ptrs, loop_sites);
            loop_ptrs.push(body as *const Block as usize);
            collect_accumulators(body, summaries, calls, accs, loop_ptrs, loop_sites);
            loop_ptrs.pop();
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_accumulators_expr(receiver, summaries, calls, accs, loop_ptrs, loop_sites);
            for a in args {
                collect_accumulators_expr(a, summaries, calls, accs, loop_ptrs, loop_sites);
            }
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            if let Some(fact) = calls.and_then(|calls| calls.fact(e)) {
                let operands = std::iter::once(receiver.as_ref()).chain(args).collect::<Vec<_>>();
                collect_call_accumulator_roots(
                    &operands,
                    &fact,
                    accs,
                    loop_ptrs,
                    loop_sites,
                );
            }
            collect_accumulators_expr(receiver, summaries, calls, accs, loop_ptrs, loop_sites);
            for a in args {
                collect_accumulators_expr(a, summaries, calls, accs, loop_ptrs, loop_sites);
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

fn collect_call_accumulator_roots(
    operands: &[&Expr],
    fact: &CallOwnershipFact,
    accs: &mut HashSet<String>,
    loop_ptrs: &[usize],
    loop_sites: &mut HashMap<usize, HashSet<String>>,
) {
    for index in fact.var_capacity_params() {
        if let Some(Expr::Var(root)) = operands.get(*index).copied() {
            accs.insert(root.clone());
            for loop_ptr in loop_ptrs {
                loop_sites.entry(*loop_ptr).or_default().insert(root.clone());
            }
        }
    }
}

struct Walker<'a> {
    accs: &'a HashSet<String>,
    summaries: &'a Summaries,
    calls: Option<CheckedCallContext<'a, 'a>>,
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
                    if direct_inplace_root(e).is_some_and(|name| self.accs.contains(name))
                        && let Expr::Call { args, .. } = e
                    {
                        // The receiver is consumed and written back, just as in
                        // the former self-assignment shape. Only the remaining
                        // arguments can create a live alias of the accumulator.
                        self.facts.site_entries += 1;
                        for arg in args.iter().skip(1) {
                            self.scan(arg, true, "stored by an in-place operation", &mut shares);
                        }
                    } else {
                        // The statement's value is discarded — only what a call
                        // can smuggle out (per summaries) or a nested statement
                        // does is live. The block-tail case is the exception.
                        let live = tail_live && i == last;
                        self.scan(e, live, "the surrounding expression", &mut shares);
                    }
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
                for (v, reason) in &shares {
                    self.facts
                        .kill_reasons
                        .entry(stmt_key(stmt))
                        .or_default()
                        .entry(v.clone())
                        .or_insert_with(|| reason.clone());
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
                    self.facts
                        .kill_reasons
                        .entry(stmt_key(stmt))
                        .or_default()
                        .entry(v.clone())
                        .or_insert_with(|| "moved out of this binding".to_string());
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
                } else if let Some(fact) = self.calls.and_then(|calls| calls.fact(e)) {
                    for (index, argument) in args.iter().enumerate() {
                        // The checked access fact owns the physical channels
                        // (consuming and write-back arguments), while the
                        // bottom-up summary owns the semantic escape question
                        // for an ordinary direct argument.  A plain immutable
                        // parameter has no physical channel, but that alone
                        // does not mean a read-only callee retains it.
                        self.scan(
                            argument,
                            fact.argument_may_alias_out(index)
                                && self.summaries.arg_live(name, index),
                            &format!("passed to `{name}`"),
                            out,
                        );
                    }
                } else if self.calls.is_none() && self.summaries.fns.contains_key(name) {
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
            Expr::Apply { func, args } => {
                self.scan(func, false, reason, out);
                let fact = self.calls.and_then(|calls| calls.fact(e));
                for (index, argument) in args.iter().enumerate() {
                    self.scan(
                        argument,
                        fact.as_ref()
                            .is_none_or(|fact| fact.argument_may_alias_out(index)),
                        "passed to a function value",
                        out,
                    );
                }
            }
            // Structures store their members by slot (whole-alias) — live iff
            // the structure itself is.
            Expr::List(items) | Expr::Tuple(items) => {
                for it in items {
                    self.scan(it, live, "stored into a collection", out);
                }
            }
            Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
                for a in args {
                    self.scan(a, live, "stored into a constructor", out);
                }
            }
            Expr::LabeledCall { .. } | Expr::LabeledMethodCall { .. } => {
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
            Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::ExistentialUpcast { expr, .. } => self.scan(expr, live, reason, out),
            Expr::ExistentialPack { expr, .. } => {
                self.scan(expr, live, "stored in an existential value", out)
            }
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
            Expr::ExistentialCall { receiver, args, .. } => {
                let fact = self.calls.and_then(|calls| calls.fact(e));
                self.scan(
                    receiver,
                    fact.as_ref().is_none_or(|fact| fact.argument_may_alias_out(0)),
                    "passed to an existential function",
                    out,
                );
                for (index, argument) in args.iter().enumerate() {
                    self.scan(
                        argument,
                        fact.as_ref()
                            .is_none_or(|fact| fact.argument_may_alias_out(index + 1)),
                        "passed to an existential function",
                        out,
                    );
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
            | Expr::ExistentialPack { expr: e2, .. }
            | Expr::ExistentialUpcast { expr: e2, .. }
            | Expr::Field { base: e2, .. } => expr(e2, accs, out),
            Expr::Call { args, .. }
            | Expr::Ctor { args, .. }
            | Expr::AnonCtor { args, .. }
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
            Expr::LabeledCall { .. } | Expr::LabeledMethodCall { .. } => {
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
            Expr::MethodCall { receiver, args, .. }
            | Expr::ExistentialCall { receiver, args, .. } => {
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
                        if matches!(pn.as_str(), "list.push" | intrinsics::LIST_PUSH)
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
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => collect_candidates_in_expr(expr, out),
        Expr::Call { args, .. }
        | Expr::Ctor { args, .. }
        | Expr::AnonCtor { args, .. }
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
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
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
        Expr::LabeledCall { .. } | Expr::LabeledMethodCall { .. } => {
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
        if matches!(name.as_str(), "list.push" | intrinsics::LIST_PUSH) && args.len() == 2 {
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
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => {
            field_escapes_expr(expr, var, field)
        }
        Expr::Field { base, .. } => field_escapes_expr(base, var, field),
        Expr::Call { args, .. }
        | Expr::Ctor { args, .. }
        | Expr::AnonCtor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => args.iter().any(|a| field_escapes_expr(a, var, field)),
        Expr::Apply { func, args } => {
            field_escapes_expr(func, var, field)
                || args.iter().any(|a| field_escapes_expr(a, var, field))
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            field_escapes_expr(receiver, var, field)
                || args.iter().any(|a| field_escapes_expr(a, var, field))
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            field_escapes_expr(base, var, field)
                || fields.iter().any(|(_, v)| field_escapes_expr(v, var, field))
        }
        Expr::LabeledCall { .. } | Expr::LabeledMethodCall { .. } => {
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
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => collect_moved_accs(expr, accs, out),
        Expr::Binary { lhs, rhs, .. }
        | Expr::Range { lo: lhs, hi: rhs, .. }
        | Expr::Index { base: lhs, index: rhs } => {
            collect_moved_accs(lhs, accs, out);
            collect_moved_accs(rhs, accs, out);
        }
        Expr::Call { args, .. }
        | Expr::Ctor { args, .. }
        | Expr::AnonCtor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => {
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
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
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
        Expr::LabeledCall { .. } | Expr::LabeledMethodCall { .. } => {
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
    if argc == 1 && witchy_syntax::ast::is_render_intrinsic(name) {
        return read_all(1);
    }
    if argc == 1 && intrinsics::is_list_pop_extract(name) {
        return read_all(1);
    }
    if argc == 2 && intrinsics::is_dict_remove_extract(name) {
        return read_all(2);
    }
    if argc == 3 && intrinsics::is_dict_insert_extract(name) {
        return Some(vec![false, true, true]);
    }
    match (name, argc) {
        // Collections: content reads and part-alias reads.
        (intrinsics::LIST_LENGTH, 1)
        | (intrinsics::DICT_LENGTH, 1)
        | (intrinsics::STRING_LENGTH, 1)
        | (intrinsics::STRING_LEN, 1)
        | (intrinsics::STRING_CHAR_COUNT, 1)
        | (intrinsics::LIST_AT, 2)
        | (intrinsics::DICT_AT, 2)
        | (intrinsics::DICT_CONTAINS_KEY, 2)
        | (intrinsics::STRING_CONTAINS, 2)
        | (intrinsics::STRING_FIND, 2)
        | (intrinsics::DICT_KEYS, 1)
        | (intrinsics::DICT_VALUES, 1)
        | (intrinsics::DICT_PAIRS, 1)
        | ("dict.remove" | intrinsics::DICT_REMOVE, 2)
        | (intrinsics::LIST_CONCAT, 2) => read_all(argc),
        // get_or reads the dict and key; the DEFAULT may be returned.
        (intrinsics::DICT_GET_OR, 3) => Some(vec![false, false, true]),
        // push/insert/update store their value operands by slot. (The
        // self-assign shape is special-cased before this table is consulted.)
        ("list.push" | intrinsics::LIST_PUSH, 2) => Some(vec![false, true]),
        ("dict.insert" | intrinsics::DICT_INSERT, 3) => Some(vec![false, true, true]),
        ("dict.update" | intrinsics::DICT_UPDATE, 4) => Some(vec![false, true, true, true]),
        // Output and messaging copy content out to the host.
        ("print", 2) => read_all(2),
        ("send", _) => read_all(argc),
        ("fail", 1) => read_all(1),
        // Strings: every operation reads content and builds fresh results.
        (intrinsics::STRING_TO_INT, 1)
        | (intrinsics::STRING_CHARS, 1)
        | (intrinsics::STRING_TRIM, 1)
        | (intrinsics::STRING_TO_UPPER, 1)
        | (intrinsics::STRING_TO_LOWER, 1)
        | (intrinsics::STRING_SPLIT, 2)
        | (intrinsics::STRING_STARTS_WITH, 2)
        | (intrinsics::STRING_ENDS_WITH, 2)
        | (intrinsics::STRING_AS_STR, 1)
        | (intrinsics::STRING_TO_STRING, 1)
        | (intrinsics::STRING_SLICE, 3)
        | (intrinsics::STRING_SUBSTRING, 3)
        | (intrinsics::STRING_REPLACE, 3) => read_all(argc),
        // Conversions never retain buffers.
        (intrinsics::MATH_TO_FLOAT, 1)
        | (intrinsics::MATH_TO_INT, 1)
        | (intrinsics::MATH_SQRT, 1) => read_all(argc),
        (intrinsics::LIST_WITH_CAPACITY, 1) | (intrinsics::DICT_NEW, 0) => Some(Vec::new()),
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
        ("dict.insert" | intrinsics::DICT_INSERT, 3)
        | ("dict.update" | intrinsics::DICT_UPDATE, 4)
        | ("dict.remove" | intrinsics::DICT_REMOVE, 2) => Some(4),
        // List / string results: the buffer pointer IS the rc-region start (offset 0).
        // These allocators (`list_push`/`list_concat`/`ascii_case`/`substr`, and
        // `trim` via `substr`) are routed through `$rc_alloc`, so their results carry
        // the `[size]` header and are freeable. NOT `string.replace` (its
        // `replace_helper` keeps the worst-case `ensure` + actual-bump pattern, not
        // routed) and NOT string `+` (a Binary, never a Call — handled in-place by
        // `$str_append_cap`).
        ("list.push" | intrinsics::LIST_PUSH, 2)
        | (intrinsics::LIST_CONCAT, 2)
        | (intrinsics::LIST_WITH_CAPACITY, 1) => Some(0),
        (intrinsics::STRING_TO_UPPER, 1)
        | (intrinsics::STRING_TO_LOWER, 1)
        | (intrinsics::STRING_TRIM, 1)
        | (intrinsics::STRING_SUBSTRING, 3) => Some(0),
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
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            args.iter().for_each(|a| rc_lets_expr(a, confined, out))
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            rc_lets_expr(receiver, confined, out);
            args.iter().for_each(|a| rc_lets_expr(a, confined, out));
        }
        Expr::Apply { func, args } => {
            rc_lets_expr(func, confined, out);
            args.iter().for_each(|a| rc_lets_expr(a, confined, out));
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => {
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
        Expr::LabeledCall { .. } | Expr::LabeledMethodCall { .. } => {
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
                if f == intrinsics::LIST_AT && args.len() == 2 =>
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
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            args.iter().for_each(f)
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            f(receiver);
            args.iter().for_each(f);
        }
        Expr::Apply { func, args } => {
            f(func);
            args.iter().for_each(f);
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => f(expr),
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
        Expr::LabeledCall { .. } | Expr::LabeledMethodCall { .. } => {
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
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for a in args {
                n += expr_read_count(a, name);
            }
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
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
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => {
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
        Expr::LabeledCall { .. } | Expr::LabeledMethodCall { .. } => {
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
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            args.iter().for_each(|a| each_block_in_expr(a, f))
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            each_block_in_expr(receiver, f);
            args.iter().for_each(|a| each_block_in_expr(a, f));
        }
        Expr::Apply { func, args } => {
            each_block_in_expr(func, f);
            args.iter().for_each(|a| each_block_in_expr(a, f));
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => {
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
        Expr::LabeledCall { .. } | Expr::LabeledMethodCall { .. } => {
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

    #[test]
    fn returned_view_makes_explicit_borrow_argument_leak() {
        let module = parser::parse_module(
            "mode opt\n\n\
             fn view(let xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\n\
             fn read(let xs: List(Int)) -> Int:\n    list.length(xs)\n",
        )
        .expect("parse");
        let summaries = Summaries::of_module(&module);

        assert!(
            summaries.arg_leaks("view", 0, 1),
            "the declared returned-view relation keeps the owner shared"
        );
        assert!(
            !summaries.arg_leaks("read", 0, 1),
            "an ordinary explicit borrow remains call-scoped"
        );
    }

    #[test]
    fn owned_result_alias_is_distinct_from_a_live_input_alias() {
        let module = parser::parse_module(
            "mode opt\n\n\
             type Token packed:\n    Skip\n    Value(Int)\n\n\
             fn pass(own token: Token) -> Token:\n    token\n",
        )
        .expect("parse");
        let summaries = Summaries::of_module(&module);

        assert!(
            !summaries.arg_leaks("pass", 0, 1),
            "the caller's moved input binding is dead after an own call"
        );
        assert!(
            summaries.arg_may_alias_out("pass", 0),
            "the moved storage still aliases the returned value"
        );
    }

    #[test]
    fn returned_view_relation_invalidates_the_uniqueness_token() {
        let module = parser::parse_module(
            "mode opt\n\n\
             fn view(let xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\n\
             fn use() -> Nil:\n    var xs = [1]\n    let w = view(xs)\n    list.push(xs, 2)\n    let _ = w\n    return\n",
        )
        .expect("parse");
        let summaries = Summaries::of_module(&module);
        let function = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "use" => Some(function),
                _ => None,
            })
            .expect("use function");
        let facts = analyze(&function.body, &summaries);
        let view_binding = &function.body.stmts[1];

        assert!(
            facts.kills_after(view_binding).iter().any(|name| name == "xs"),
            "a returned view must invalidate the owner's uniqueness token"
        );
    }

    /// A reassigned binding's churn is rc-floor's free-at-overwrite, not this pass.
    #[test]
    fn reassigned_binding_is_not_dropped() {
        let d = drops("import list\nfn f():\n    var buf = []\n    buf = list.__push(buf, 1)\n    0\n");
        assert_eq!(d.total(), 0, "`buf` is reassigned");
    }
}

// ---------------------------------------------------------------------------
// Module-level diagnostics: copy cliffs and opt-mode no-copy contracts, for
// `witchy check` and the LSP.
// ---------------------------------------------------------------------------

/// A `unique` / `local unique` `var` argument whose ownership token is not
/// statically available at a promised no-copy call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoCopyMiss {
    pub function: String,
    pub callee: String,
    pub var: String,
    pub line: u32,
    // (RFC-0110) The callee parameter index this argument targets. Part of the
    // repair key `(function, line, callee, arg_index)` so two unproven-unique
    // calls on one source line are disambiguated — the codegen repair-set
    // consumer cannot use `&Stmt` pointer identity (the checked module is a
    // clone of the codegen module; `module_boundary_repairs`).
    pub arg_index: usize,
    // (RFC-0110 step 5) The `*const Expr as usize` of the CALL node this miss was
    // recorded at. Valid only for pointers into the exact module the walker ran
    // on — codegen runs the walker on its own `checked_module` (via
    // `module_boundary_repair_ptrs`) so the counter can match the call node it is
    // lowering by identity, with no source-line or name plumbing. Zero when the
    // walker had no call node in scope (never, for real misses).
    pub call_ptr: usize,
    pub reason: String,
}

/// A canonical functional-in-place kernel that cannot satisfy its static
/// allocation-free, constant-stack contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FipMiss {
    pub function: String,
    pub line: u32,
    pub reason: String,
}

struct FipChecker<'a> {
    function: &'a str,
    owner: &'a str,
    owner_index: usize,
    line: u32,
    misses: Vec<FipMiss>,
}

impl<'a> FipChecker<'a> {
    fn miss(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        if !self
            .misses
            .iter()
            .any(|miss| miss.line == self.line && miss.reason == reason)
        {
            self.misses.push(FipMiss {
                function: self.function.to_string(),
                line: self.line,
                reason,
            });
        }
    }

    fn block(&mut self, block: &Block, tail_value: bool) {
        if block.region.is_some() {
            self.miss("a `region` block performs reclamation inside the kernel");
        }
        for (index, stmt) in block.stmts.iter().enumerate() {
            self.line = block.lines.get(index).copied().unwrap_or(self.line);
            let is_tail = tail_value && index + 1 == block.stmts.len();
            match stmt {
                Stmt::Let { value, .. } | Stmt::LetPattern { value, .. } => {
                    self.value(value);
                    if is_tail {
                        self.miss("the fallthrough path does not return the owned value");
                    }
                }
                Stmt::Assign { name, value } if name == self.owner => {
                    match value {
                        Expr::RecordUpdate { base, fields, .. }
                            if self.is_owner_value(base) =>
                        {
                            for (_, field) in fields {
                                self.value(field);
                            }
                        }
                        _ => self.miss(
                            "the owned value may only be changed by an in-place field update",
                        ),
                    }
                    if is_tail {
                        self.miss("the fallthrough path does not return the owned value");
                    }
                }
                Stmt::Assign { value, .. } => {
                    self.value(value);
                    if is_tail {
                        self.miss("the fallthrough path does not return the owned value");
                    }
                }
                Stmt::Return(Some(value)) => self.tail(value),
                Stmt::Return(None) => self.miss("a return path does not return the owned value"),
                Stmt::Expr(value) if is_tail => self.tail(value),
                Stmt::Expr(value) => self.value(value),
                Stmt::Yield(_) => self.miss("generators cannot be FIP kernels"),
                Stmt::Break | Stmt::Continue => {
                    self.miss("loop control cannot appear in an FIP kernel")
                }
            }
            if matches!(stmt, Stmt::Return(_)) {
                break;
            }
        }
        if tail_value && block.stmts.is_empty() {
            self.miss("the fallthrough path does not return the owned value");
        }
    }

    fn tail(&mut self, expr: &Expr) {
        match expr {
            Expr::Var(name) if name == self.owner => {}
            Expr::Unary { op: UnOp::Move, expr } if self.is_owner_value(expr) => {}
            Expr::Call { name, args } if name == self.function => {
                if args.len() <= self.owner_index || !self.is_owner_value(&args[self.owner_index]) {
                    self.miss("the recursive edge does not forward the owned value directly");
                }
                for (index, arg) in args.iter().enumerate() {
                    if index != self.owner_index {
                        self.value(arg);
                    }
                }
            }
            Expr::If { cond, then_block, else_block } => {
                self.value(cond);
                self.block(then_block, true);
                if let Some(block) = else_block {
                    self.block(block, true);
                } else {
                    self.miss("a tail `if` without `else` does not return the owned value");
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.value(scrutinee);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.value(guard);
                    }
                    self.line = arm.line;
                    self.tail(&arm.body);
                }
            }
            Expr::Block(block) => self.block(block, true),
            _ => {
                self.value(expr);
                self.miss("a return path does not return the owned value directly");
            }
        }
    }

    fn value(&mut self, expr: &Expr) {
        match expr {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_) => {}
            Expr::Var(name) if name == self.owner => {
                self.miss("the owned value escapes outside a field read or tail return")
            }
            Expr::Var(_) => {}
            Expr::Unary { op: UnOp::Await, .. } => {
                self.miss("suspension cannot occur inside an FIP kernel")
            }
            Expr::Unary { expr, .. } => self.value(expr),
            Expr::Field { base, .. } if self.is_owner_projection(base) => {}
            Expr::Field { base, .. } => self.value(base),
            Expr::Binary { op, lhs, rhs } => {
                if matches!(op, BinOp::Concat | BinOp::Coalesce) {
                    self.miss("this operator may allocate or alter control flow");
                }
                self.value(lhs);
                self.value(rhs);
            }
            Expr::If { cond, then_block, else_block } => {
                self.value(cond);
                self.block(then_block, false);
                if let Some(block) = else_block {
                    self.block(block, false);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.value(scrutinee);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.value(guard);
                    }
                    self.line = arm.line;
                    self.value(&arm.body);
                }
            }
            Expr::Block(block) => self.block(block, false),
            Expr::Call { name, args } if name == self.function => {
                for arg in args {
                    if !self.is_owner_value(arg) {
                        self.value(arg);
                    }
                }
                self.miss("the recursive call is not in tail position");
            }
            Expr::Call { name, args } => {
                for arg in args {
                    self.value(arg);
                }
                self.miss(format!("call to `{name}` may allocate or perform an effect"));
            }
            Expr::RecordUpdate { .. } => self.miss(
                "a record update must be assigned back to the owned parameter before returning",
            ),
            Expr::List(items) | Expr::Tuple(items) => {
                for item in items {
                    self.value(item);
                }
                self.miss("aggregate construction allocates inside the FIP kernel")
            }
            Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
                for arg in args {
                    self.value(arg);
                }
                self.miss("aggregate construction allocates inside the FIP kernel")
            }
            Expr::Record { fields, spread, .. } => {
                for (_, field) in fields {
                    self.value(field);
                }
                if let Some(spread) = spread {
                    self.value(spread);
                }
                self.miss("aggregate construction allocates inside the FIP kernel")
            }
            Expr::LabeledCall { args, .. } | Expr::LabeledMethodCall { args, .. } => {
                for (_, arg) in args {
                    self.value(arg);
                }
                self.miss("an unresolved or first-class call cannot satisfy the FIP contract")
            }
            Expr::MethodCall { receiver, args, .. }
            | Expr::ExistentialCall { receiver, args, .. } => {
                self.value(receiver);
                for arg in args {
                    self.value(arg);
                }
                self.miss("an unresolved or first-class call cannot satisfy the FIP contract")
            }
            Expr::Apply { func, args } => {
                self.value(func);
                for arg in args {
                    self.value(arg);
                }
                self.miss("an unresolved or first-class call cannot satisfy the FIP contract")
            }
            Expr::Lambda { .. } => self.miss("closure construction allocates inside the FIP kernel"),
            Expr::ExistentialPack { expr, .. } => {
                self.value(expr);
                self.miss("existential construction allocates inside the FIP kernel");
            }
            Expr::ExistentialUpcast { expr, .. } => self.value(expr),
            Expr::Try(_) => self.miss("early propagation is not part of the initial FIP contract"),
            Expr::As { expr, .. } => self.value(expr),
            Expr::While { .. }
            | Expr::For { .. }
            | Expr::Range { .. }
            | Expr::WhileLet { .. } => {
                self.miss("loops and ranges are outside the recursive FIP kernel shape")
            }
            Expr::Index { .. } => self.miss("indexed access is outside the initial FIP contract"),
            Expr::TaggedLit { .. } => {
                self.miss("an unexpanded tagged literal cannot satisfy the FIP contract")
            }
        }
    }

    fn is_owner_value(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Var(name) if name == self.owner)
            || matches!(expr, Expr::Unary { op: UnOp::Move, expr }
                if matches!(expr.as_ref(), Expr::Var(name) if name == self.owner))
    }

    fn is_owner_projection(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Var(name) => name == self.owner,
            Expr::Field { base, .. } => self.is_owner_projection(base),
            _ => false,
        }
    }
}

fn unique_inner(ty: &Option<Type>) -> Option<&Type> {
    match ty.as_ref()? {
        Type::Qualified(TypeQual::Unique, inner) => Some(inner.unqualified()),
        _ => None,
    }
}

fn fip_scalar_type(ty: &Type) -> bool {
    matches!(
        ty.unqualified(),
        Type::Named(name, args)
            if args.is_empty()
                && matches!(name.as_str(), "Int" | "Float" | "Bool" | "Duration")
    )
}

fn fip_scalar_record(module: &Module, ty: &Type) -> bool {
    let Type::Named(name, args) = ty.unqualified() else { return false };
    if !args.is_empty() {
        return false;
    }
    module.items.iter().any(|item| {
        matches!(item, Item::Type(def)
            if def.name == *name
                && matches!(def.variants.as_slice(), [variant]
                    if !variant.field_names.is_empty()
                        && variant.fields.iter().all(fip_scalar_type)))
    })
}

fn fip_block_calls(block: &Block, function: &str) -> bool {
    block.stmts.iter().any(|stmt| match stmt {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Yield(value)
        | Stmt::Expr(value)
        | Stmt::Return(Some(value)) => fip_expr_calls(value, function),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => false,
    })
}

fn fip_expr_calls(expr: &Expr, function: &str) -> bool {
    match expr {
        Expr::Call { name, args } => {
            name == function || args.iter().any(|arg| fip_expr_calls(arg, function))
        }
        Expr::LabeledCall { name, args } => {
            name == function || args.iter().any(|(_, arg)| fip_expr_calls(arg, function))
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            fip_expr_calls(receiver, function) || args.iter().any(|(_, arg)| fip_expr_calls(arg, function))
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            fip_expr_calls(receiver, function)
                || args.iter().any(|arg| fip_expr_calls(arg, function))
        }
        Expr::Apply { func, args } => {
            fip_expr_calls(func, function)
                || args.iter().any(|arg| fip_expr_calls(arg, function))
        }
        Expr::List(items) | Expr::Tuple(items) => {
            items.iter().any(|item| fip_expr_calls(item, function))
        }
        Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            args.iter().any(|arg| fip_expr_calls(arg, function))
        }
        Expr::Unary { expr, .. }
        | Expr::Field { base: expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => fip_expr_calls(expr, function),
        Expr::Lambda { body, .. } | Expr::Block(body) => fip_block_calls(body, function),
        Expr::RecordUpdate { base, fields, .. } => {
            fip_expr_calls(base, function)
                || fields.iter().any(|(_, value)| fip_expr_calls(value, function))
        }
        Expr::Record { fields, spread, .. } => {
            fields.iter().any(|(_, value)| fip_expr_calls(value, function))
                || spread
                    .as_deref()
                    .is_some_and(|value| fip_expr_calls(value, function))
        }
        Expr::Binary { lhs, rhs, .. } => {
            fip_expr_calls(lhs, function) || fip_expr_calls(rhs, function)
        }
        Expr::If { cond, then_block, else_block } => {
            fip_expr_calls(cond, function)
                || fip_block_calls(then_block, function)
                || else_block
                    .as_ref()
                    .is_some_and(|block| fip_block_calls(block, function))
        }
        Expr::Match { scrutinee, arms } => {
            fip_expr_calls(scrutinee, function)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|guard| fip_expr_calls(guard, function))
                        || fip_expr_calls(&arm.body, function)
                })
        }
        Expr::While { cond, body } => {
            fip_expr_calls(cond, function) || fip_block_calls(body, function)
        }
        Expr::For { iter, body, .. } => {
            fip_expr_calls(iter, function) || fip_block_calls(body, function)
        }
        Expr::Range { lo, hi, .. } => {
            fip_expr_calls(lo, function) || fip_expr_calls(hi, function)
        }
        Expr::Index { base, index } => {
            fip_expr_calls(base, function) || fip_expr_calls(index, function)
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            fip_expr_calls(scrutinee, function) || fip_block_calls(body, function)
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => false,
    }
}

/// Check the initial RFC-0089 FIP surface. A function opts in structurally: one
/// `own unique T` parameter, a `unique T` result, and a recursive self-edge.
/// Non-recursive consume-and-return helpers retain their ordinary meaning.
pub fn module_fip_misses(module: &Module) -> Vec<FipMiss> {
    let mut out = Vec::new();
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        let Some(result) = unique_inner(&function.ret) else { continue };
        let owners: Vec<_> = function
            .params
            .iter()
            .enumerate()
            .filter_map(|(index, param)| {
                (param.convention == Convention::Own)
                    .then(|| unique_inner(&param.ty).map(|inner| (index, param, inner)))
                    .flatten()
            })
            .collect();
        let [(owner_index, owner, owner_ty)] = owners.as_slice() else { continue };
        if *owner_ty != result {
            continue;
        }
        if !fip_block_calls(&function.body, &function.name) {
            continue;
        }
        let mut checker = FipChecker {
            function: &function.name,
            owner: &owner.name,
            owner_index: *owner_index,
            line: function.body.lines.first().copied().unwrap_or(0),
            misses: Vec::new(),
        };
        if !fip_scalar_record(module, owner_ty) {
            checker.miss(
                "the initial FIP contract requires a record whose stored fields are all scalar",
            );
        }
        for (index, param) in function.params.iter().enumerate() {
            if index != *owner_index && !param.ty.as_ref().is_some_and(fip_scalar_type) {
                checker.miss(format!(
                    "auxiliary parameter `{}` is not scalar in the initial FIP contract",
                    param.name
                ));
            }
        }
        checker.block(&function.body, true);
        let canonical_tail = matches!(
            function.body.stmts.last(),
            Some(Stmt::Expr(Expr::Call { name, .. })) if name == &function.name
        );
        if !canonical_tail {
            checker.line = function.body.lines.last().copied().unwrap_or(checker.line);
            checker.miss(
                "the initial FIP contract requires the recursive edge as the function's final expression",
            );
        }
        out.extend(checker.misses);
    }
    out
}

#[derive(Debug, Clone)]
enum NoCopyProof {
    Available,
    Unavailable(String),
    /// A first-class callable whose listed parameters promise no-copy var
    /// update-and-extract, plus whether its result returns ownership state.
    Callable { required: Vec<usize>, unique_result: bool },
}

impl NoCopyProof {
    fn reason(&self) -> Option<&str> {
        match self {
            NoCopyProof::Available => None,
            NoCopyProof::Unavailable(reason) => Some(reason),
            NoCopyProof::Callable { .. } => None,
        }
    }
}

fn no_copy_qualified(ty: &Option<Type>) -> bool {
    ty.as_ref().is_some_and(no_copy_qualified_type)
}

fn no_copy_qualified_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Qualified(
            witchy_syntax::ast::TypeQual::Unique | witchy_syntax::ast::TypeQual::LocalUnique,
            _
        )
    )
}

fn callable_no_copy_contract_type(ty: &Type) -> Option<(Vec<usize>, bool)> {
    let signature = witchy_types::access::AccessSignature::from_function_type(ty).ok()?;
    Some(no_copy_contract(&signature)).filter(|(required, unique_result)| {
        !required.is_empty() || *unique_result
    })
}

fn no_copy_contract(
    signature: &witchy_types::access::AccessSignature,
) -> (Vec<usize>, bool) {
    let fact = call_ownership_fact(signature);
    (fact.unique_params_to_check(), fact.unique_capacity_result())
}

fn no_copy_requirements(
    module: &Module,
    access: Option<&witchy_types::access::CheckedAccessFacts<'_>>,
) -> HashMap<String, Vec<usize>> {
    use witchy_types::access::AccessQualifier;

    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) => {
                let private_structural = private_structural_helper(&function.name);
                let signature = access
                    .and_then(|facts| facts.declaration(&function.name).cloned())
                    .or_else(|| {
                        witchy_types::access::AccessSignature::from_function(function).ok()
                    })?;
                let mut required = no_copy_contract(&signature).0;
                if private_structural
                    && signature.params().first().is_some_and(|param| {
                        param.qualifiers().iter().any(|qualifier| {
                            matches!(
                                qualifier,
                                AccessQualifier::Unique | AccessQualifier::LocalUnique
                            )
                        })
                    })
                    && !required.contains(&0)
                {
                    required.push(0);
                }
                required.sort_unstable();
                (!required.is_empty()).then(|| (function.name.clone(), required))
            }
            _ => None,
        })
        .collect()
}

fn unique_capacity_results(
    module: &Module,
    access: Option<&witchy_types::access::CheckedAccessFacts<'_>>,
) -> HashSet<String> {
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) => access
                .and_then(|facts| facts.declaration(&function.name).cloned())
                .or_else(|| {
                    witchy_types::access::AccessSignature::from_function(function).ok()
                })
                .as_ref()
                .map(no_copy_contract)
                .and_then(|(_, unique_result)| unique_result.then(|| function.name.clone())),
            _ => None,
        })
        .collect()
}

fn no_copy_display_name(name: &str) -> String {
    if name == intrinsics::DICT_INSERT
        || name
            .strip_prefix(intrinsics::DICT_INSERT)
            .is_some_and(|suffix| suffix.starts_with("__"))
    {
        return "dict.insert".to_string();
    }
    if name == intrinsics::DICT_REMOVE
        || name
            .strip_prefix(intrinsics::DICT_REMOVE)
            .is_some_and(|suffix| suffix.starts_with("__"))
    {
        return "dict.remove".to_string();
    }
    name.split_once("__").map_or(name, |(source, _)| source).to_string()
}

/// (RFC-0110 criterion 2) A freshly-constructed value with no other reference —
/// so it trivially satisfies a consuming `own unique` parameter (nothing can
/// alias a value produced inline at the call). A record/anon constructor and a
/// tuple literal are the fresh-value shapes beyond the list/dict cases below;
/// this is the consuming-side analog of the place-based uniqueness proof for
/// `var` parameters.
fn no_copy_fresh_owned(expr: &Expr) -> bool {
    matches!(expr, Expr::Ctor { .. } | Expr::AnonCtor { .. } | Expr::Tuple(_)) || no_copy_fresh(expr)
}

fn no_copy_fresh(expr: &Expr) -> bool {
    match expr {
        // A literal owns its allocation. The compiled tier initializes its
        // capacity token from the literal length.
        Expr::List(_) => true,
        // The empty dictionary has no old spine to copy. Its first structural
        // update establishes the geometric-capacity token returned by the var ABI.
        Expr::Call { name, args }
            if args.is_empty()
                && (name == intrinsics::DICT_NEW
                    || name
                        .strip_prefix(intrinsics::DICT_NEW)
                        .is_some_and(|suffix| suffix.starts_with("__"))
                    || name.ends_with(".dict.new")) =>
        {
            true
        }
        _ => false,
    }
}

fn merge_no_copy_proof(left: &NoCopyProof, right: &NoCopyProof) -> NoCopyProof {
    match (left, right) {
        (NoCopyProof::Available, NoCopyProof::Available) => NoCopyProof::Available,
        (
            NoCopyProof::Callable { required: left, unique_result: left_result },
            NoCopyProof::Callable { required: right, unique_result: right_result },
        ) if left == right && left_result == right_result => {
            NoCopyProof::Callable {
                required: left.clone(),
                unique_result: *left_result,
            }
        }
        (NoCopyProof::Unavailable(reason), _) | (_, NoCopyProof::Unavailable(reason)) => {
            NoCopyProof::Unavailable(reason.clone())
        }
        _ => NoCopyProof::Unavailable(
            "control flow does not preserve one ownership-capacity contract".to_string(),
        ),
    }
}

fn merge_no_copy_env(
    before: &HashMap<String, NoCopyProof>,
    branches: &[HashMap<String, NoCopyProof>],
) -> HashMap<String, NoCopyProof> {
    let Some((first, rest)) = branches.split_first() else {
        return before.clone();
    };
    let mut merged = before.clone();
    for name in before.keys() {
        let mut proof = first.get(name).unwrap_or(&before[name]).clone();
        for branch in rest {
            let candidate = branch.get(name).unwrap_or(&before[name]);
            proof = merge_no_copy_proof(&proof, candidate);
        }
        merged.insert(name.clone(), proof);
    }
    merged
}

#[derive(Clone, Copy)]
struct NoCopyInputs<'facts, 'module> {
    module: &'module Module,
    access: Option<&'facts witchy_types::access::CheckedAccessFacts<'module>>,
    places: &'facts witchy_types::access::CheckedPlaceFacts<'module>,
    required: &'facts HashMap<String, Vec<usize>>,
    unique_results: &'facts HashSet<String>,
    summaries: &'facts Summaries,
    loans: &'facts witchy_types::loans::LoanFacts,
}

struct NoCopyWalker<'facts, 'module> {
    function: String,
    module: &'module Module,
    access: Option<&'facts witchy_types::access::CheckedAccessFacts<'module>>,
    places: &'facts witchy_types::access::CheckedPlaceFacts<'module>,
    required: &'facts HashMap<String, Vec<usize>>,
    unique_results: &'facts HashSet<String>,
    summaries: &'facts Summaries,
    facts: Facts,
    loans: &'facts witchy_types::loans::LoanFacts,
    misses: Vec<NoCopyMiss>,
    line: u32,
}

impl<'facts, 'module> NoCopyWalker<'facts, 'module> {
    fn new(
        function: String,
        body: &Block,
        inputs: NoCopyInputs<'facts, 'module>,
    ) -> Self {
        let NoCopyInputs {
            module,
            access,
            places,
            required,
            unique_results,
            summaries,
            loans,
        } = inputs;
        let mut facts = access.map_or_else(
            || analyze(body, summaries),
            |access| analyze_with_access(body, summaries, module, access),
        );
        facts.merge_loan_kills(body, loans);
        Self {
            function,
            module,
            access,
            places,
            required,
            unique_results,
            summaries,
            facts,
            loans,
            misses: Vec::new(),
            line: 0,
        }
    }

    fn walk(mut self, function: &witchy_syntax::ast::Function) -> Vec<NoCopyMiss> {
        let own_cap_param = self.summaries.own_abi(&function.name);
        let signature = self.access.and_then(|facts| facts.declaration(&function.name)).cloned();
        self.walk_body(&function.params, &function.body, own_cap_param, signature.as_ref());
        self.misses
    }

    fn walk_lambda(
        mut self,
        params: &[witchy_syntax::ast::Param],
        body: &Block,
        signature: Option<&witchy_types::access::AccessSignature>,
    ) -> Vec<NoCopyMiss> {
        let own_cap_param = signature.and_then(|signature| {
            signature.params().iter().enumerate().find_map(|(index, param)| {
                (param.kind() == witchy_types::access::AccessKind::Consuming
                    && param.ownership().input().is_some()
                    && no_copy_qualified_type(param.ty()))
                .then_some(index)
            })
        });
        self.walk_body(params, body, own_cap_param, signature);
        self.misses
    }

    fn walk_body(
        &mut self,
        params: &[witchy_syntax::ast::Param],
        body: &Block,
        own_cap_param: Option<usize>,
        signature: Option<&witchy_types::access::AccessSignature>,
    ) {
        let mut env = HashMap::new();
        for (index, param) in params.iter().enumerate() {
            let resolved = signature.and_then(|signature| signature.params().get(index));
            let param_type = resolved
                .map(witchy_types::access::AccessParam::ty)
                .or(param.ty.as_ref());
            if let Some((required, unique_result)) =
                param_type.and_then(callable_no_copy_contract_type)
            {
                env.insert(
                    param.name.clone(),
                    NoCopyProof::Callable { required, unique_result },
                );
                continue;
            }
            let access_kind = resolved
                .map(witchy_types::access::AccessParam::kind)
                .unwrap_or_else(|| witchy_types::access::AccessKind::from(param.convention));
            let convention = match access_kind {
                witchy_types::access::AccessKind::OwnedImmutable => Convention::Let,
                witchy_types::access::AccessKind::SharedBorrow => Convention::Borrow,
                witchy_types::access::AccessKind::ExclusiveWriteback => Convention::Var,
                witchy_types::access::AccessKind::Consuming => Convention::Own,
            };
            let qualified = param_type.is_some_and(no_copy_qualified_type);
            let carries_cap = convention == Convention::Var
                || (convention == Convention::Own && own_cap_param == Some(index));
            let proof = if qualified && carries_cap {
                NoCopyProof::Available
            } else {
                let reason = if qualified {
                    format!(
                        "parameter `{}` is unique, but its `{}` convention does not carry a capacity token into this function",
                        param.name,
                        match convention {
                            Convention::Let => "default let",
                            Convention::Borrow => "let",
                            Convention::Own => "own",
                            Convention::Var => "var",
                        }
                    )
                } else {
                    format!(
                        "parameter `{}` is not declared `unique` or `local unique`",
                        param.name
                    )
                };
                NoCopyProof::Unavailable(reason)
            };
            env.insert(param.name.clone(), proof);
        }
        self.block(body, &mut env);
    }

    fn block(&mut self, block: &Block, env: &mut HashMap<String, NoCopyProof>) {
        // Assignments to outer bindings flow out of a block, but bindings
        // declared here do not. Remember the first shadowed value so a nested
        // `let`/`var` cannot replace the outer binding's proof after scope exit.
        let mut shadowed: HashMap<String, Option<NoCopyProof>> = HashMap::new();
        for (index, stmt) in block.stmts.iter().enumerate() {
            self.line = block.lines.get(index).copied().unwrap_or(self.line);
            let exits_block = matches!(stmt, Stmt::Return(_) | Stmt::Break | Stmt::Continue);
            match stmt {
                Stmt::Let { name, value, .. } => {
                    let proof = self.expr(value, stmt, env);
                    // The general uniqueness facts only track roots that the
                    // syntax exposes as direct accumulator call operands. A
                    // first-class no-copy call hides that shape behind a local
                    // callable, so invalidate an available proof here as well
                    // when a plain binding creates a live whole-value alias.
                    if let Expr::Var(root) = value
                        && matches!(env.get(root), Some(NoCopyProof::Available))
                    {
                        env.insert(
                            root.clone(),
                            NoCopyProof::Unavailable("bound to a new name".to_string()),
                        );
                    }
                    shadowed.entry(name.clone()).or_insert_with(|| env.get(name).cloned());
                    env.insert(name.clone(), proof);
                }
                Stmt::Assign { name, value } => {
                    let mut proof = self.expr(value, stmt, env);
                    if self_inplace_op(name, value).is_some()
                        || self_own_call(name, value, self.summaries).is_some()
                        || self_private_structural_call(name, value)
                    {
                        proof = NoCopyProof::Available;
                    }
                    env.insert(name.clone(), proof);
                }
                Stmt::Expr(value) => {
                    let _ = self.expr(value, stmt, env);
                    if let Some(root) = direct_inplace_root(value) {
                        env.insert(root.to_string(), NoCopyProof::Available);
                    }
                }
                Stmt::LetPattern { pattern, value } => {
                    let _ = self.expr(value, stmt, env);
                    let mut names = Vec::new();
                    witchy_syntax::ast::pattern_binds(pattern, &mut names);
                    for name in names {
                        shadowed.entry(name.clone()).or_insert_with(|| env.get(&name).cloned());
                        env.insert(
                            name,
                            NoCopyProof::Unavailable(
                                "a destructured binding has no independent ownership-capacity token"
                                    .to_string(),
                            ),
                        );
                    }
                }
                Stmt::Return(Some(value)) | Stmt::Yield(value) => {
                    let _ = self.expr(value, stmt, env);
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
            for name in self.facts.kills_after(stmt) {
                let reason = self
                    .facts
                    .kill_reason_after(stmt, name)
                    .unwrap_or("ownership may be shared after this statement")
                    .to_string();
                env.insert(name.clone(), NoCopyProof::Unavailable(reason));
            }
            if exits_block {
                break;
            }
        }
        for (name, previous) in shadowed {
            match previous {
                Some(proof) => {
                    env.insert(name, proof);
                }
                None => {
                    env.remove(&name);
                }
            }
        }
    }

    fn expr(
        &mut self,
        expr: &Expr,
        stmt: &Stmt,
        env: &mut HashMap<String, NoCopyProof>,
    ) -> NoCopyProof {
        match expr {
            Expr::Var(name) => {
                let required = self.required.get(name).cloned().unwrap_or_default();
                let unique_result = self.unique_results.contains(name);
                if !required.is_empty() || unique_result {
                    NoCopyProof::Callable { required, unique_result }
                } else if let Some(NoCopyProof::Callable { required, unique_result }) = env.get(name)
                {
                    NoCopyProof::Callable {
                        required: required.clone(),
                        unique_result: *unique_result,
                    }
                } else {
                    NoCopyProof::Unavailable(format!("it aliases `{name}`"))
                }
            }
            Expr::List(items) => {
                for item in items {
                    let _ = self.expr(item, stmt, env);
                }
                NoCopyProof::Available
            }
            Expr::Call { name, args } => {
                for arg in args {
                    let _ = self.expr(arg, stmt, env);
                }
                let checked = self.checked_call_fact(expr);
                let legacy = self.access.is_none().then(|| {
                    env.get(name)
                        .and_then(|proof| match proof {
                            NoCopyProof::Callable { required, unique_result } => {
                                Some((required.clone(), *unique_result))
                            }
                            _ => None,
                        })
                        .or_else(|| {
                            self.required.get(name).map(|required| {
                                (required.clone(), self.unique_results.contains(name))
                            })
                        })
                }).flatten();
                let (required, unique_result) = checked
                    .as_ref()
                    .map(|fact| {
                        (fact.unique_params_to_check(), fact.unique_capacity_result())
                    })
                    .or(legacy)
                    .unwrap_or_default();
                let operands = args.iter().collect::<Vec<_>>();
                let callee = if matches!(env.get(name), Some(NoCopyProof::Callable { .. })) {
                    "indirect function".to_string()
                } else {
                    no_copy_display_name(name)
                };
                self.record_call_misses(
                    &operands,
                    &required,
                    stmt,
                    env,
                    &callee,
                    expr as *const Expr as usize,
                );
                if unique_result || no_copy_fresh(expr) {
                    NoCopyProof::Available
                } else {
                    NoCopyProof::Unavailable(format!(
                        "the result of `{name}` has no declared uniqueness proof"
                    ))
                }
            }
            Expr::Unary { op: UnOp::Move, expr } => {
                if let Expr::Var(name) = expr.as_ref() {
                    env.insert(
                        name.clone(),
                        NoCopyProof::Unavailable("it was moved out of this binding".to_string()),
                    );
                    NoCopyProof::Unavailable(format!(
                        "`move {name}` transfers the value but not its hidden capacity token to a new binding"
                    ))
                } else {
                    self.expr(expr, stmt, env)
                }
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. }
            | Expr::Field { base: expr, .. } => self.expr(expr, stmt, env),
            Expr::Tuple(items) | Expr::Ctor { args: items, .. } | Expr::AnonCtor { args: items, .. } => {
                for item in items {
                    let _ = self.expr(item, stmt, env);
                }
                NoCopyProof::Unavailable("the aggregate does not carry an ownership-capacity token".to_string())
            }
            Expr::Apply { func, args } => {
                let callable = self.expr(func, stmt, env);
                for arg in args {
                    let _ = self.expr(arg, stmt, env);
                }
                let checked = self.checked_call_fact(expr);
                let legacy = self.access.is_none().then_some(match callable {
                    NoCopyProof::Callable { required, unique_result } => {
                        Some((required, unique_result))
                    }
                    _ => None,
                }).flatten();
                let (required, unique_result) = checked
                    .as_ref()
                    .map(|fact| {
                        (fact.unique_params_to_check(), fact.unique_capacity_result())
                    })
                    .or(legacy)
                    .unwrap_or_default();
                let operands = args.iter().collect::<Vec<_>>();
                self.record_call_misses(
                    &operands,
                    &required,
                    stmt,
                    env,
                    "indirect function",
                    expr as *const Expr as usize,
                );
                if unique_result {
                    return NoCopyProof::Available;
                }
                NoCopyProof::Unavailable("an indirect call has no declared no-copy proof".to_string())
            }
            Expr::MethodCall { receiver, args, .. } => {
                let _ = self.expr(receiver, stmt, env);
                for arg in args {
                    let _ = self.expr(arg, stmt, env);
                }
                NoCopyProof::Unavailable("unresolved method call".to_string())
            }
            Expr::ExistentialCall { receiver, args, .. } => {
                let _ = self.expr(receiver, stmt, env);
                for arg in args {
                    let _ = self.expr(arg, stmt, env);
                }
                let fact = self.checked_call_fact(expr);
                let operands = std::iter::once(receiver.as_ref()).chain(args).collect::<Vec<_>>();
                let required = fact
                    .as_ref()
                    .map(|fact| fact.unique_params_to_check())
                    .unwrap_or_default();
                self.record_call_misses(
                    &operands,
                    &required,
                    stmt,
                    env,
                    "existential dispatch",
                    expr as *const Expr as usize,
                );
                if fact.is_some_and(|fact| fact.unique_capacity_result()) {
                    NoCopyProof::Available
                } else {
                    NoCopyProof::Unavailable(
                        "an existential call has no declared no-copy result".to_string(),
                    )
                }
            }
            Expr::Lambda { params, body, ret } => {
                let name = format!("{}::<lambda>", self.function);
                let signature = self
                    .access
                    .and_then(|facts| facts.callable_at(self.module, expr))
                    .cloned();
                let nested = NoCopyWalker::new(
                    name,
                    body,
                    NoCopyInputs {
                        module: self.module,
                        access: self.access,
                        places: self.places,
                        required: self.required,
                        unique_results: self.unique_results,
                        summaries: self.summaries,
                        loans: self.loans,
                    },
                )
                .walk_lambda(params, body, signature.as_ref());
                self.misses.extend(nested);
                let (required, unique_result) = signature
                    .as_ref()
                    .map(no_copy_contract)
                    .unwrap_or_else(|| {
                        let required = params
                            .iter()
                            .enumerate()
                            .filter_map(|(index, param)| {
                                (param.convention == Convention::Var
                                    && no_copy_qualified(&param.ty))
                                .then_some(index)
                            })
                            .collect::<Vec<_>>();
                        let unique_result = ret.as_ref().is_some_and(|ty| {
                            matches!(
                                ty,
                                Type::Qualified(witchy_syntax::ast::TypeQual::Unique, inner)
                                    if matches!(inner.unqualified(), Type::Named(name, _)
                                        if matches!(name.as_str(), "List" | "Dict"))
                            )
                        });
                        (required, unique_result)
                    });
                if required.is_empty() && !unique_result {
                    NoCopyProof::Unavailable("a closure value is shared".to_string())
                } else {
                    NoCopyProof::Callable { required, unique_result }
                }
            }
            Expr::If { cond, then_block, else_block } => {
                let _ = self.expr(cond, stmt, env);
                let before = env.clone();
                let mut then_env = before.clone();
                self.block(then_block, &mut then_env);
                let mut branches = vec![then_env];
                let mut else_env = before.clone();
                if let Some(else_block) = else_block {
                    self.block(else_block, &mut else_env);
                }
                branches.push(else_env);
                *env = merge_no_copy_env(&before, &branches);
                NoCopyProof::Unavailable("control-flow result has no unique token".to_string())
            }
            Expr::Match { scrutinee, arms } => {
                let _ = self.expr(scrutinee, stmt, env);
                let before = env.clone();
                let mut branches = Vec::new();
                for arm in arms {
                    let mut branch = before.clone();
                    if let Some(guard) = &arm.guard {
                        let _ = self.expr(guard, stmt, &mut branch);
                    }
                    let _ = self.expr(&arm.body, stmt, &mut branch);
                    branches.push(branch);
                }
                *env = merge_no_copy_env(&before, &branches);
                NoCopyProof::Unavailable("match result has no unique token".to_string())
            }
            Expr::Block(block) => {
                self.block(block, env);
                NoCopyProof::Unavailable("block result has no unique token".to_string())
            }
            Expr::While { cond, body } => {
                let _ = self.expr(cond, stmt, env);
                self.loop_body(body, env);
                NoCopyProof::Unavailable("loop result is not an owned container".to_string())
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                let _ = self.expr(scrutinee, stmt, env);
                self.loop_body(body, env);
                NoCopyProof::Unavailable("loop result is not an owned container".to_string())
            }
            Expr::For { iter, body, .. } => {
                let _ = self.expr(iter, stmt, env);
                self.loop_body(body, env);
                NoCopyProof::Unavailable("loop result is not an owned container".to_string())
            }
            Expr::Binary { lhs, rhs, .. }
            | Expr::Index { base: lhs, index: rhs }
            | Expr::Range { lo: lhs, hi: rhs, .. } => {
                let _ = self.expr(lhs, stmt, env);
                let _ = self.expr(rhs, stmt, env);
                NoCopyProof::Unavailable("the expression has no ownership-capacity token".to_string())
            }
            Expr::RecordUpdate { base, fields, .. } => {
                let _ = self.expr(base, stmt, env);
                for (_, value) in fields {
                    let _ = self.expr(value, stmt, env);
                }
                NoCopyProof::Unavailable("record updates do not carry collection capacity".to_string())
            }
            Expr::Record { fields, spread, .. } => {
                for (_, value) in fields {
                    let _ = self.expr(value, stmt, env);
                }
                if let Some(spread) = spread {
                    let _ = self.expr(spread, stmt, env);
                }
                NoCopyProof::Unavailable("record values do not carry collection capacity".to_string())
            }
            Expr::LabeledCall { .. } | Expr::LabeledMethodCall { .. } => {
                unreachable!("labeled calls are resolved before performance-mode analysis")
            }
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::TaggedLit { .. } => {
                NoCopyProof::Unavailable("the value is not an owned collection".to_string())
            }
        }
    }

    fn loop_body(&mut self, body: &Block, env: &mut HashMap<String, NoCopyProof>) {
        let before = env.clone();
        let mut first = before.clone();
        self.block(body, &mut first);
        let entry = merge_no_copy_env(&before, &[before.clone(), first]);
        // The second pass checks the back-edge state: a share after a promised
        // call in iteration one must reject that call in iteration two.
        let mut second = entry.clone();
        self.block(body, &mut second);
        *env = merge_no_copy_env(&before, &[before.clone(), second]);
    }

    fn checked_call_fact(&self, expression: &Expr) -> Option<CallOwnershipFact> {
        self.access
            .and_then(|access| checked_call_ownership_fact(self.module, access, expression))
    }

    fn record_call_misses(
        &mut self,
        args: &[&Expr],
        indices: &[usize],
        stmt: &Stmt,
        env: &mut HashMap<String, NoCopyProof>,
        callee: &str,
        call_ptr: usize,
    ) {
        for &index in indices {
            let Some(arg) = args.get(index).copied() else { continue };
            // (RFC-0110) A freshly-constructed argument (record/tuple/list/dict
            // literal) is provably unique — nothing else references a value made
            // inline at the call — so it satisfies a consuming `own unique`
            // parameter with no mutable-place fact required. This is the fresh
            // counterpart of the place-based `var` proof; without it, widening
            // the detector to `own unique` (criterion 2) would wrongly reject an
            // owner-threaded call like `scan(ScanState(0, 0), n)`.
            if no_copy_fresh_owned(arg) {
                continue;
            }
            let Some(place) = self.places.place_at(self.module, arg) else {
                self.misses.push(NoCopyMiss {
                    function: self.function.clone(),
                    callee: callee.to_string(),
                    var: "<computed value>".to_string(),
                    line: self.line,
                    arg_index: index,
                    call_ptr,
                    reason: "the argument has no checked mutable-place fact".to_string(),
                });
                continue;
            };
            let root = place.root();
            if !place.steps().is_empty() {
                let reason = if place.has_dynamic_index() {
                    "a dynamically indexed place has no fixed ownership-capacity token"
                } else {
                    "a nested place has no independent ownership-capacity token"
                };
                self.misses.push(NoCopyMiss {
                    function: self.function.clone(),
                    callee: callee.to_string(),
                    var: root.to_string(),
                    line: self.line,
                    arg_index: index,
                    call_ptr,
                    reason: reason.to_string(),
                });
                continue;
            }
            let reason = self
                .loans
                .active_at(stmt)
                .iter()
                .find(|loan| loan.owner == root)
                .map(|loan| {
                    format!(
                        "it is actively loaned to view `{}` by `{}`",
                        loan.view, loan.origin
                    )
                })
                .or_else(|| env.get(root).and_then(NoCopyProof::reason).map(ToOwned::to_owned))
                .or_else(|| {
                    self.facts.kill_reason_after(stmt, root).map(|reason| {
                        format!(
                            "this statement also shares the binding ({reason}); split the operations into separate statements so ownership at the call is explicit"
                        )
                    })
                })
                .or_else(|| {
                    (!env.contains_key(root))
                        .then(|| "the binding has no tracked ownership-capacity token".to_string())
                });
            if let Some(reason) = reason {
                self.misses.push(NoCopyMiss {
                    function: self.function.clone(),
                    callee: callee.to_string(),
                    var: root.to_string(),
                    line: self.line,
                    arg_index: index,
                    call_ptr,
                    reason,
                });
            }
            env.insert(root.to_string(), NoCopyProof::Available);
        }
    }
}

/// Check every declared no-copy `var` contract against the same ownership and
/// loan facts consumed by codegen. The caller decides whether the module's mode
/// promotes these misses to errors.
pub fn module_no_copy_misses(module: &Module) -> Vec<NoCopyMiss> {
    try_module_no_copy_misses(module).unwrap_or_else(|_| {
        let lowered = witchy_types::traits::lower(module.clone());
        module_no_copy_misses_with_access(&lowered, None)
    })
}

/// Build no-copy diagnostics from a fully checked access graph. Performance
/// enforcement uses this result-bearing boundary so an invalid callable
/// contract cannot silently fall back to structural name/convention guesses.
pub fn try_module_no_copy_misses(module: &Module) -> Result<Vec<NoCopyMiss>, String> {
    // Method syntax is resolved only by typed trait lowering. Analyze that same
    // ordinary-call AST so `d.insert(...)` and `dict.insert(d, ...)` consult one
    // signature contract instead of maintaining a method-name census here.
    let lowered = witchy_types::traits::lower(module.clone());
    let typed = witchy_types::typeck::annotate_checked(lowered)
        .map_err(|error| format!("checked ownership/access analysis failed: {error}"))?;
    let access = witchy_types::access::checked_facts(typed.module(), typed.table())
        .map_err(|error| format!("checked ownership/access analysis failed: {error}"))?;
    Ok(module_no_copy_misses_with_access(typed.module(), Some(&access)))
}

/// (RFC-0110 criterion 2/9) A normal-mode one-copy repair site, keyed by source
/// coordinates so a codegen consumer can match it WITHOUT `&Stmt` pointer
/// identity (the checked module is a `traits::lower(module.clone())` — its
/// statement pointers differ from the codegen module's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundaryRepair {
    pub function: String,
    pub line: u32,
    pub callee: String,
    pub arg_index: usize,
}

/// The compiler-owned entry selected for a conventional call.  This is not a
/// source-level overload: both arms target the one declared function identity.
/// `Repair` means the normal-mode copy-correct adapter must establish the
/// access proof before entering that body; `Proven` may enter it directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundaryEntry {
    Proven,
    Repair,
}

/// The compiler-owned ABI selection for one conventional call.  The access
/// identity is cloned from the checked call table, so later lowering cannot
/// accidentally choose an entry by surface spelling or lose the effects that
/// its repair implementation must preserve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundaryAdapter {
    entry: BoundaryEntry,
    access_identity: witchy_types::access::AccessIdentityKey,
}

impl BoundaryAdapter {
    pub fn entry(&self) -> BoundaryEntry {
        self.entry
    }

    pub fn access_identity(&self) -> &witchy_types::access::AccessIdentityKey {
        &self.access_identity
    }
}

/// Checked, address-keyed conventional-call entry selection for one exact AST.
/// Every entry retains the canonical checked callable identity. A pointer not
/// present in `repair_sites` is deliberately `Proven`: the no-copy walker
/// records every checked unique-access miss, while conventional signatures
/// with no such requirement have no adapter work to perform.
#[derive(Clone, Debug, Default)]
pub struct BoundaryEntrySelection {
    adapters: HashMap<usize, BoundaryAdapter>,
}

impl BoundaryEntrySelection {
    pub fn adapter_for(&self, call: &Expr) -> Option<&BoundaryAdapter> {
        self.adapters.get(&(call as *const Expr as usize))
    }

    pub fn entry_for(&self, call: &Expr) -> BoundaryEntry {
        self.adapter_for(call)
            .map(BoundaryAdapter::entry)
            .unwrap_or(BoundaryEntry::Proven)
    }
}

/// The set of normal-mode one-copy repair sites: exactly the no-copy misses —
/// a site that `mode opt` rejects for a missing uniqueness proof is the same
/// site normal mode repairs by re-owning the argument (the zero-token
/// copy-on-write boundary). Returns an empty set if the checked access graph is
/// unavailable (a repair counter is best-effort observability, never a
/// correctness gate). Keyed by `(function, line, callee, arg_index)`.
pub fn module_boundary_repairs(module: &Module) -> Vec<BoundaryRepair> {
    try_module_no_copy_misses(module)
        .map(|misses| {
            misses
                .into_iter()
                .filter(|miss| !function_is_opt(module, &miss.function))
                .map(|miss| BoundaryRepair {
                    function: miss.function,
                    line: miss.line,
                    callee: miss.callee,
                    arg_index: miss.arg_index,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// (RFC-0110 step 5) The set of call-node pointers (`*const Expr as usize`) that
/// are normal-mode one-copy repair sites, computed on `module` DIRECTLY (no
/// clone) so the pointers are live for a caller that walks the same module —
/// codegen passes its `checked_module` + the access facts derived from it. Each
/// pointer is a call whose `unique` argument lacks a uniqueness proof; normal
/// mode re-owns it (the zero-token copy-on-write boundary), and this is where the
/// boundary-reown counter fires. A repaired call may target more than one
/// unproven `unique` parameter; the set is by call node, matching one counter
/// increment per repaired boundary call.
pub fn module_boundary_repair_ptrs(
    module: &Module,
    access: Option<&witchy_types::access::CheckedAccessFacts<'_>>,
) -> foldhash::HashSet<usize> {
    use foldhash::HashSetExt as _;
    let mut ptrs = foldhash::HashSet::new();
    for miss in module_no_copy_misses_with_access(module, access) {
        if miss.call_ptr != 0 && !function_is_opt(module, &miss.function) {
            ptrs.insert(miss.call_ptr);
        }
    }
    ptrs
}

/// Linked modules retain one `@opt:<module>` mode marker per source module;
/// an unlinked opt file retains the plain `opt` marker.  A missing no-copy
/// proof in either form is an opt contract failure, never a normal-mode repair
/// entry.
fn function_is_opt(module: &Module, function: &str) -> bool {
    function
        .rsplit_once('.')
        .map(|(owner, _)| {
            module
                .modes
                .iter()
                .any(|mode| mode == &format!("@opt:{owner}"))
        })
        .unwrap_or_else(|| module.modes.iter().any(|mode| mode == "opt"))
}

/// Derive the one conventional-call entry selection consumed by lowering.  The
/// same checked access graph drives opt diagnostics, normal repair selection,
/// and the repair telemetry; no backend is allowed to reconstruct this choice
/// from spelling or a separate ownership heuristic.
pub fn boundary_entry_selection(
    module: &Module,
    access: Option<&witchy_types::access::CheckedAccessFacts<'_>>,
) -> BoundaryEntrySelection {
    let repair_sites = module_boundary_repair_ptrs(module, access);
    let Some(access) = access else {
        return BoundaryEntrySelection::default();
    };
    BoundaryEntrySelection {
        adapters: access
            .call_contracts()
            .map(|(site, signature)| {
                let entry = if repair_sites.contains(&site) {
                    BoundaryEntry::Repair
                } else {
                    BoundaryEntry::Proven
                };
                (
                    site,
                    BoundaryAdapter {
                        entry,
                        access_identity: signature.identity_key(),
                    },
                )
            })
            .collect(),
    }
}

fn module_no_copy_misses_with_access(
    module: &Module,
    access: Option<&witchy_types::access::CheckedAccessFacts<'_>>,
) -> Vec<NoCopyMiss> {
    let places = witchy_types::access::checked_place_facts(module);
    let required = no_copy_requirements(module, access);
    if required.is_empty() {
        return Vec::new();
    }
    let summaries = Summaries::of_module(module);
    let unique_results = unique_capacity_results(module, access);
    let loans = match witchy_types::loans::facts(module) {
        Ok(loans) => loans,
        // Type checking reports the authoritative loan error first.
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for item in &module.items {
        if let Item::Function(function) = item {
            out.extend(
                NoCopyWalker::new(
                    function.name.clone(),
                    &function.body,
                    NoCopyInputs {
                        module,
                        access,
                        places: &places,
                        required: &required,
                        unique_results: &unique_results,
                        summaries: &summaries,
                        loans: &loans,
                    },
                )
                .walk(function),
            );
        }
    }
    out.sort_by(|left, right| {
        (&left.function, left.line, &left.callee, &left.var, &left.reason).cmp(&(
            &right.function,
            right.line,
            &right.callee,
            &right.var,
            &right.reason,
        ))
    });
    out.dedup();
    out
}

#[cfg(test)]
mod fip_tests {
    use super::*;
    use witchy_syntax::parser;

    fn misses(source: &str) -> Vec<FipMiss> {
        let module = parser::parse_module(source).expect("parse");
        witchy_types::typeck::check(&module).expect("type check");
        module_fip_misses(&module)
    }

    const STATE: &str = "type State:\n    count: Int\n    limit: Int\n\n";

    #[test]
    fn canonical_owner_threaded_tail_kernel_satisfies_contract() {
        let source = format!(
            "{STATE}fn run(own state: unique State, n: Int) -> unique State:\n\
             \x20   if n == 0:\n\
             \x20       return state\n\
             \x20   state.count = state.count + 1\n\
             \x20   run(state, n - 1)\n"
        );
        assert!(misses(&source).is_empty());
    }

    #[test]
    fn non_tail_recursion_and_replacement_owner_are_rejected() {
        let non_tail = format!(
            "{STATE}fn run(own state: unique State, n: Int) -> unique State:\n\
             \x20   if n == 0:\n\
             \x20       return state\n\
             \x20   let next = run(state, n - 1)\n\
             \x20   next\n"
        );
        let found = misses(&non_tail);
        assert!(found.iter().any(|miss| miss.reason.contains("not in tail position")));

        let replacement = format!(
            "{STATE}fn run(own state: unique State, n: Int) -> unique State:\n\
             \x20   if n == 0:\n\
             \x20       return State(0, 0)\n\
             \x20   run(state, n - 1)\n"
        );
        let found = misses(&replacement);
        assert!(found.iter().any(|miss| miss.reason.contains("owned value directly")));

        let nested_tail = format!(
            "{STATE}fn run(own state: unique State, n: Int) -> unique State:\n\
             \x20   if n == 0:\n\
             \x20       state\n\
             \x20   else:\n\
             \x20       run(state, n - 1)\n"
        );
        let found = misses(&nested_tail);
        assert!(found.iter().any(|miss| miss.reason.contains("final expression")));
    }

    #[test]
    fn non_recursive_consume_and_return_helper_does_not_opt_in() {
        let source = format!(
            "{STATE}fn normalize(own state: unique State) -> unique State:\n\
             \x20   console.print(\"not actually linked\")\n\
             \x20   state\n"
        );
        let module = parser::parse_module(&source).expect("parse");
        assert!(module_fip_misses(&module).is_empty());
    }

    #[test]
    fn heap_fields_and_auxiliary_parameters_are_rejected() {
        let source = "type State:\n    text: String\n\n\
                      fn run(own state: unique State, suffix: String, n: Int) -> unique State:\n\
                      \x20   if n == 0:\n        return state\n\
                      \x20   state.text = suffix\n\
                      \x20   run(state, suffix, n - 1)\n";
        let module = parser::parse_module(source).expect("parse");
        let found = module_fip_misses(&module);
        assert!(found.iter().any(|miss| miss.reason.contains("stored fields")));
        assert!(found.iter().any(|miss| miss.reason.contains("`suffix` is not scalar")));
    }

    #[test]
    fn recursion_hidden_inside_another_expression_still_activates_contract() {
        let source = format!(
            "{STATE}fn pass(own state: unique State) -> unique State:\n    state\n\n\
             fn run(own state: unique State, n: Int) -> unique State:\n\
             \x20   if n == 0:\n        return state\n\
             \x20   pass(run(state, n - 1))\n"
        );
        let found = misses(&source);
        assert!(found.iter().any(|miss| miss.reason.contains("not in tail position")));
        assert!(found.iter().any(|miss| miss.reason.contains("final expression")));
        assert!(found.iter().any(|miss| miss.reason.contains("call to `pass`")));
    }
}

#[cfg(test)]
mod no_copy_tests {
    use super::*;
    use witchy_syntax::parser;

    fn misses(source: &str) -> Vec<NoCopyMiss> {
        let module = parser::parse_module(source).expect("parse");
        witchy_types::typeck::check(&module).expect("type check");
        module_no_copy_misses(&module)
    }

    fn misses_unchecked(source: &str) -> Vec<NoCopyMiss> {
        let module = parser::parse_module(source).expect("parse");
        module_no_copy_misses(&module)
    }

    /// (RFC-0110 Step 4) The boundary-repair set is keyed by source coordinates,
    /// and two unproven-unique calls on ONE line are disambiguated by callee +
    /// arg_index — the codegen consumer cannot use `&Stmt` pointers because the
    /// checked module is a clone. This pins that keying.
    #[test]
    fn boundary_repairs_disambiguate_same_line_calls_by_callee_and_arg_index() {
        let module = parser::parse_module(
            "fn one(own a: unique List(Int)) -> Int:\n    list.length(a)\n\
             \nfn two(own b: unique List(Int)) -> Int:\n    list.length(b)\n\
             \nfn caller() -> Int:\n    var xs = [1]\n    let alias = xs\n    var ys = [2]\n    let alias2 = ys\n    one(xs) + two(ys)\n",
        )
        .expect("parse");
        witchy_types::typeck::check(&module).expect("type check");
        let repairs = module_boundary_repairs(&module);
        // Both aliased owners are repair sites, on the same source line, told
        // apart by their callee (arg_index is 0 for each single-owner call).
        let one = repairs.iter().find(|r| r.callee.ends_with("one"));
        let two = repairs.iter().find(|r| r.callee.ends_with("two"));
        assert!(one.is_some() && two.is_some(), "both same-line repairs present: {repairs:?}");
        assert_eq!(one.unwrap().line, two.unwrap().line, "both on the one source line");
        assert_eq!(one.unwrap().arg_index, 0);
        assert_eq!(two.unwrap().arg_index, 0);
        // A fresh owner is NOT a repair site.
        let fresh = parser::parse_module(
            "fn one(own a: unique List(Int)) -> Int:\n    list.length(a)\n\
             \nfn caller() -> Int:\n    one([1, 2, 3])\n",
        )
        .expect("parse fresh");
        witchy_types::typeck::check(&fresh).expect("type check fresh");
        assert!(module_boundary_repairs(&fresh).is_empty(), "a fresh owner needs no repair");
    }

    #[test]
    fn checked_boundary_entry_selection_distinguishes_proven_and_repair_calls() {
        fn adapter(source: &str) -> BoundaryAdapter {
            let parsed = parser::parse_module(source).expect("parse");
            let lowered = witchy_types::traits::lower(parsed);
            let typed = witchy_types::typeck::annotate_checked(lowered).expect("type check");
            let access = witchy_types::access::checked_facts(typed.module(), typed.table())
                .expect("access facts");
            let selection = boundary_entry_selection(typed.module(), Some(&access));
            let caller = typed
                .module()
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Function(function) if function.name == "caller" => Some(function),
                    _ => None,
                })
                .expect("caller");
            let call = caller
                .body
                .stmts
                .last()
                .and_then(|statement| match statement {
                    Stmt::Expr(call @ Expr::Call { .. }) => Some(call),
                    _ => None,
                })
                .expect("terminal direct call");
            selection.adapter_for(call).cloned().expect("checked call adapter")
        }

        let repaired = adapter(
            "fn take(own xs: unique List(Int)) -> Nil:\n    return\n\n\
             fn caller() -> Nil:\n    var xs = [1]\n    let alias = xs\n    take(xs)\n",
        );
        assert_eq!(
            repaired.entry(),
            BoundaryEntry::Repair,
            "an aliased unique value selects the normal repair adapter"
        );
        let proven = adapter(
            "fn take(own xs: unique List(Int)) -> Nil:\n    return\n\n\
             fn caller() -> Nil:\n    take([1])\n",
        );
        assert_eq!(
            proven.entry(),
            BoundaryEntry::Proven,
            "a fresh value enters the same opt body through its proven access path"
        );
        assert_eq!(
            repaired.access_identity(),
            proven.access_identity(),
            "proven and repair entries retain one checked callable ABI identity"
        );

        let opt = adapter(
            "mode opt\n\nfn take(own xs: unique List(Int)) -> Nil:\n    return\n\n\
             fn caller() -> Nil:\n    var xs = [1]\n    let alias = xs\n    take(xs)\n",
        );
        assert_eq!(
            opt.entry(),
            BoundaryEntry::Proven,
            "an opt caller never selects the normal repair entry"
        );
    }

    #[test]
    fn fresh_local_and_unique_parameter_satisfy_contract() {
        let found = misses(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn forward(var xs: unique List(Int)) -> Nil:\n    take(xs)\n    return\n\
             \nfn fresh() -> Nil:\n    var xs = [1]\n    take(xs)\n    return\n",
        );
        assert!(found.is_empty(), "available proofs should pass: {found:?}");
    }

    #[test]
    fn checked_fixed_field_place_reports_its_owner_root() {
        let found = misses(
            "type State:\n    items: unique List(Int)\n\n\
             fn take(var xs: unique List(Int)) -> Nil:\n    return\n\n\
             fn caller() -> Nil:\n    var state = State([1])\n    take(state.items)\n    return\n",
        );
        assert_eq!(found.len(), 1, "the field needs its own capacity token: {found:?}");
        assert_eq!(found[0].var, "state");
        assert!(
            found[0]
                .reason
                .contains("nested place has no independent ownership-capacity token"),
            "{found:?}"
        );
    }

    #[test]
    fn checked_dynamic_index_place_still_fails_closed() {
        let found = misses(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\n\
             fn caller(i: Int) -> Nil:\n    var grid: List(unique List(Int)) = [[1]]\n    take(grid[i])\n    return\n",
        );
        assert_eq!(found.len(), 1, "the dynamic coordinate cannot name a fixed token: {found:?}");
        assert_eq!(found[0].var, "grid");
        assert!(
            found[0]
                .reason
                .contains("dynamically indexed place has no fixed ownership-capacity token"),
            "{found:?}"
        );
    }

    #[test]
    fn unique_value_without_a_threaded_capacity_token_is_rejected() {
        let found = misses(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn direct(own xs: unique List(Int)) -> Nil:\n    take(xs)\n    return\n\
             \nfn rebound(own xs: unique List(Int)) -> Nil:\n    var ys = move xs\n    take(ys)\n    return\n",
        );
        assert_eq!(found.len(), 2, "both missing-token paths must reject: {found:?}");
        assert!(found.iter().any(|miss| miss.reason.contains("`own` convention")), "{found:?}");
        assert!(found.iter().any(|miss| miss.reason.contains("transfers the value")), "{found:?}");
    }

    #[test]
    fn ordinary_inplace_mutator_can_reestablish_the_token() {
        let found = misses_unchecked(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn repaired() -> Nil:\n    var xs = [1]\n    let snapshot = xs\n    list.push(xs, 2)\n    take(xs)\n    let _ = snapshot\n    return\n",
        );
        assert!(found.is_empty(), "the copying push re-owns before the promise: {found:?}");
    }

    #[test]
    fn alias_reports_the_statement_that_invalidated_uniqueness() {
        let found = misses(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn bad() -> Nil:\n    var xs = [1]\n    let snapshot = xs\n    take(xs)\n    return\n",
        );
        assert_eq!(found.len(), 1, "one aliased call: {found:?}");
        assert_eq!(found[0].var, "xs");
        assert_eq!(found[0].line, 7);
        assert!(found[0].reason.contains("bound to a new name"), "{found:?}");
    }

    #[test]
    fn loop_back_edge_catches_alias_after_first_call() {
        let found = misses(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn bad() -> Nil:\n    var xs = [1]\n    var i = 0\n    while i < 2:\n        take(xs)\n        let snapshot = xs\n        i = i + 1\n    return\n",
        );
        assert_eq!(found.len(), 1, "second-iteration miss is deduplicated: {found:?}");
        assert_eq!(found[0].line, 8);
        assert!(found[0].reason.contains("bound to a new name"), "{found:?}");
    }

    #[test]
    fn completed_loan_explains_why_the_capacity_proof_was_lost() {
        let found = misses(
            "mode opt\n\nfn view(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\
             \nfn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn bad() -> Nil:\n    var xs = [1]\n    let w = view(xs)\n    let _ = list.length(w)\n    take(xs)\n    return\n",
        );
        assert_eq!(found.len(), 1, "the ended loan still invalidated the cap: {found:?}");
        assert!(found[0].reason.contains("loaned to view `w` by `view`"), "{found:?}");
    }

    #[test]
    fn nested_shadow_does_not_poison_the_outer_proof() {
        let found = misses(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn scoped() -> Nil:\n    var xs = [1]\n    if true:\n        var xs = [2]\n        let snapshot = xs\n        take(xs)\n        let _ = snapshot\n    take(xs)\n    return\n",
        );
        assert_eq!(found.len(), 1, "only the aliased inner binding must reject: {found:?}");
        assert_eq!(found[0].line, 9);
    }

    #[test]
    fn untracked_binding_fails_closed() {
        let found = misses_unchecked(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn destructured() -> Nil:\n    let (xs,) = ([1],)\n    take(xs)\n    return\n",
        );
        assert_eq!(found.len(), 1, "an untracked binding must not imply uniqueness: {found:?}");
        assert!(found[0].reason.contains("destructured binding"), "{found:?}");
    }

    #[test]
    fn indirect_no_copy_call_preserves_capacity_proof() {
        let found = misses(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn indirect() -> Nil:\n    var xs = [1]\n    let f = take\n    f(xs)\n    return\n",
        );
        assert!(found.is_empty(), "the indirect ABI carries the proof: {found:?}");
    }

    #[test]
    fn indirect_no_copy_call_still_rejects_an_alias() {
        let found = misses(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn indirect() -> Nil:\n    var xs = [1]\n    let f = take\n    let alias = xs\n    f(xs)\n    let _ = alias\n    return\n",
        );
        assert_eq!(found.len(), 1, "the alias must invalidate the proof: {found:?}");
        assert!(found[0].reason.contains("bound to a new name"), "{found:?}");
    }

    #[test]
    fn checked_no_copy_boundary_does_not_suppress_access_errors() {
        let module = parser::parse_module(
            "mode opt\n\nfn plain(xs: List(Int)) -> Int:\n    0\n\n\
             fn require(callback: fn(unique List(Int)) -> Int) -> Nil:\n    return\n\n\
             fn main() -> Nil:\n    require(plain)\n    return\n",
        )
        .expect("parse invalid callable contract");
        let error = try_module_no_copy_misses(&module)
            .expect_err("checked performance analysis must retain the access error");
        assert!(error.contains("ownership/access contract"), "{error}");
    }

    #[test]
    fn lambda_body_misses_keep_the_enclosing_function_name() {
        let found = misses(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn main() -> Int:\n    let work = fn() -> Nil:\n        var xs = [1]\n        let snapshot = xs\n        take(xs)\n        let _ = snapshot\n        return\n    work()\n    0\n",
        );
        assert_eq!(found.len(), 1, "the lambda miss must remain visible: {found:?}");
        assert!(found[0].function.starts_with("main::<lambda>"), "{found:?}");
    }

    #[test]
    fn inferred_lambda_var_uses_its_checked_unique_contract() {
        let found = misses(
            "mode opt\n\nfn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn invoke(f: fn(var unique List(Int)) -> Nil) -> Nil:\n    var xs = [1]\n    f(xs)\n    return\n\
             \nfn main() -> Nil:\n    invoke(fn(var xs): take(xs))\n    return\n",
        );
        assert!(
            found.is_empty(),
            "the checked lambda signature carries its inferred unique var capacity: {found:?}"
        );
    }

    #[test]
    fn unreachable_reown_after_continue_does_not_repair_the_backedge() {
        let found = misses_unchecked(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn bad() -> Nil:\n    var xs = [1]\n    let snapshot = xs\n    var i = 0\n    while i < 2:\n        take(xs)\n        i = i + 1\n        continue\n        xs = [2]\n    let _ = snapshot\n    return\n",
        );
        assert_eq!(found.len(), 1, "unreachable code must not repair the next iteration: {found:?}");
        assert!(found[0].reason.contains("bound to a new name"), "{found:?}");
    }

    #[test]
    fn exhaustive_reown_branches_restore_the_proof() {
        let found = misses_unchecked(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn repaired(flag: Bool) -> Nil:\n    var xs = [1]\n    let snapshot = xs\n    if flag:\n        list.push(xs, 2)\n    else:\n        list.push(xs, 3)\n    take(xs)\n    let _ = snapshot\n    return\n",
        );
        assert!(found.is_empty(), "every runtime branch re-owns the list: {found:?}");
    }

    #[test]
    fn declared_unique_result_supplies_the_next_no_copy_proof() {
        let found = misses(
            "fn build() -> unique List(Int):\n    [1, 2]\n\
             \nfn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn composed() -> Nil:\n    var xs = build()\n    take(xs)\n    return\n",
        );
        assert!(found.is_empty(), "a unique result is a reusable ownership proof: {found:?}");
    }

    #[test]
    fn indirect_unique_result_supplies_the_next_no_copy_proof() {
        let found = misses(
            "fn build() -> unique List(Int):\n    [1]\n\
             \nfn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn indirect() -> Nil:\n    let f = build\n    var xs = f()\n    take(xs)\n    return\n",
        );
        assert!(found.is_empty(), "the indirect result carries ownership state: {found:?}");
    }

    #[test]
    fn same_statement_alias_cannot_precede_a_promised_call() {
        let found = misses_unchecked(
            "fn take(var xs: unique List(Int)) -> Nil:\n    return\n\
             \nfn bad() -> Nil:\n    var xs = [1]\n    let pair = (xs, take(xs))\n    let _ = pair\n    return\n",
        );
        assert_eq!(found.len(), 1, "the tuple alias exists before the call: {found:?}");
        assert!(found[0].reason.contains("same statement") || found[0].reason.contains("this statement"), "{found:?}");
    }
}

pub fn module_cliffs(module: &Module) -> Vec<(String, Cliff)> {
    let summaries = Summaries::of_module(module);
    let mut out = Vec::new();
    for item in &module.items {
        match item {
            Item::Function(f) => {
                for c in analyze(&f.body, &summaries).cliffs {
                    out.push((f.name.clone(), c));
                }
            }
            // Inherent impl methods own their implementations under RFC-0099's
            // methods-first stdlib, so a cliff inside a method body must be
            // caught exactly like one in a free function. Reported under the
            // generated implementation symbol the aliases target.
            Item::Impl(im) if im.trait_name.is_none() => {
                for f in &im.methods {
                    for c in analyze(&f.body, &summaries).cliffs {
                        out.push((format!("{}__{}", im.type_name, f.name), c));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// (RFC-0073) Shape-matcher contract tests. The `self_*` recognizers gate every
// in-place emission (RFC-0051's retained family), so each one gets an
// isolated accepting case plus rejecting near-misses — previously they were
// exercised only end-to-end through codegen + the parity suite. A regression
// here is a UAF-class risk (a wrong match mutates a value someone else still
// holds), which is exactly why the contract deserves direct tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod shape_matcher_tests {
    use super::*;

    fn var(name: &str) -> Expr {
        Expr::Var(name.to_string())
    }

    fn int(n: i64) -> Expr {
        Expr::Int(n)
    }

    fn call(name: &str, args: Vec<Expr>) -> Expr {
        Expr::Call { name: name.to_string(), args }
    }

    // ---- self_push_elem: `xs = list.__push(xs, e)` ----

    #[test]
    fn push_matches_self_append() {
        for f in ["list.push", "list.push__task.Handle"] {
            let v = call(f, vec![var("xs"), int(1)]);
            assert!(self_push_elem("xs", &v).is_some(), "{f} should match");
        }
    }

    #[test]
    fn push_rejects_wrong_receiver_var() {
        // `xs = list.push(ys, e)` copies from ANOTHER list — mutating in place
        // would corrupt `ys`.
        let v = call("list.push", vec![var("ys"), int(1)]);
        assert!(self_push_elem("xs", &v).is_none());
    }

    #[test]
    fn push_rejects_wrong_callee_and_arity() {
        // A same-shaped call to a different function must not be treated as an
        // append; nor a push with a computed receiver.
        assert!(self_push_elem("xs", &call("list.concat", vec![var("xs"), int(1)])).is_none());
        assert!(self_push_elem("xs", &call("list.push", vec![int(0), int(1)])).is_none());
        assert!(self_push_elem("xs", &call("list.push", vec![var("xs")])).is_none());
    }

    // ---- self_insert_args / self_update_args: dict upserts ----

    #[test]
    fn insert_matches_self_upsert_and_rejects_alias() {
        for f in ["dict.insert", "dict.insert__String__Int"] {
            let ok = call(f, vec![var("d"), int(1), int(2)]);
            assert!(self_insert_args("d", &ok).is_some(), "{f} should match");
        }
        let alias = call("dict.insert", vec![var("e"), int(1), int(2)]);
        assert!(self_insert_args("d", &alias).is_none());
    }

    #[test]
    fn update_matches_arity_four_only() {
        let ok = call("dict.update", vec![var("d"), int(1), int(0), var("f")]);
        assert!(self_update_args("d", &ok).is_some());
        let short = call("dict.update", vec![var("d"), int(1), int(0)]);
        assert!(self_update_args("d", &short).is_none());
    }

    // ---- self_set_at / self_update_at: monomorphized stdlib names ----

    #[test]
    fn set_at_matches_bare_and_monomorphized_names() {
        for f in ["list.set_at", "list.__set_at", "list.set_at__Int"] {
            let v = call(f, vec![var("xs"), int(0), int(9)]);
            assert!(self_set_at("xs", &v).is_some(), "{f} should match");
        }
        // A user function that merely CONTAINS the name must not match: the
        // recognizer requires the exact monomorphization prefix `list.set_at__`.
        let imposter = call("mylib.list.set_at_extra", vec![var("xs"), int(0), int(9)]);
        assert!(self_set_at("xs", &imposter).is_none());
    }

    #[test]
    fn update_at_rejects_wrong_receiver() {
        let v = call("list.update_at", vec![var("ys"), int(0), var("f")]);
        assert!(self_update_at("xs", &v).is_none());
    }

    // ---- self_concat_pieces: `s = s + a + b` left spines ----

    fn concat(lhs: Expr, rhs: Expr) -> Expr {
        Expr::Binary { op: BinOp::Concat, lhs: Box::new(lhs), rhs: Box::new(rhs) }
    }

    #[test]
    fn concat_spine_collects_pieces_in_order() {
        // s + "a" + "b" — leftmost leaf is the assigned var.
        let v = concat(concat(var("s"), int(1)), int(2));
        let pieces = self_concat_pieces("s", &v).expect("spine matches");
        assert_eq!(pieces.len(), 2);
    }

    #[test]
    fn concat_rejects_var_not_at_spine_head() {
        // "a" + s — the assigned var is a RHS piece, not the spine head: this
        // PREPENDS, and appending in place would produce the wrong string.
        let v = concat(int(1), var("s"));
        assert!(self_concat_pieces("s", &v).is_none());
        // Bare `s` with no appended pieces is not a concat either.
        assert!(self_concat_pieces("s", &var("s")).is_none());
    }
}
