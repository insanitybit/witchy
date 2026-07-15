//! RFC-0083 phase 1: static lifetime relations and owner loans for borrowed views.
//!
//! A borrowed view is `ast::Type::Qualified(TypeQual::Borrow(lifetime), inner)`
//! (see `witchy-syntax`). It carries NO runtime representation — `to_ty` erases it
//! to its owned inner type, so both backends run identical owned-value semantics
//! (parity by construction). This module adds the COMPILE-TIME contract the RFC
//! calls for, in two passes over the already-lowered whole-program module (so
//! method calls are plain `Call`s and every function is visible):
//!
//! 1. **Signature relations.** Views may appear only in a `mode opt` module. Each
//!    returned view names an input lifetime; that lifetime must be bound by an
//!    input view of the same name. The relation `return borrows params [i, …]` is
//!    read straight off the signature, so it survives direct calls, trait
//!    dispatch, function values, specialization, and module boundaries (they all
//!    resolve to a concrete callee whose signature carries the relation).
//!
//! 2. **Owner loans.** At a call site `let v = f(a0, a1, …)` where `f` returns a
//!    view of parameter `i`, the result creates a LOAN of the owner
//!    `root_local(a_i)`. While that loan is live — until `v`'s last use, or until
//!    `v` is consumed by `.owned()` — the owner may not be moved, mutated,
//!    reassigned, passed to a `var`/`own` parameter, or let escape through a
//!    closure/task/channel. This is the same borrow rule inside `mode opt` and at
//!    every caller mode; a mode boundary cannot erase it.
//!
//! This is NOT a second AST-local type system: it consumes the same signatures
//! the checker builds and never re-infers types. Runtime rooting / host leases /
//! ownership-analysis integration are a later RFC-0083 phase (see the RFC).

use std::collections::HashMap;

use witchy_syntax::ast::{Block, Convention, Expr, Function, Item, Module, Stmt, Type, TypeQual, UnOp};

use crate::typeck::TypeError;

fn terr(message: String) -> TypeError {
    TypeError { message }
}

/// The output-to-input borrow relation of one function, read off its signature.
struct BorrowSig {
    /// `true` when the return type is a borrowed view.
    returns_view: bool,
    /// Parameter indices whose borrow lifetime matches the returned view's
    /// lifetime — the owners a call's result loans. Empty when the return is not
    /// a view (or, after signature validation, never empty when it is).
    owner_params: Vec<usize>,
}

/// The borrow qualifier's lifetime name on a parameter/return type, if any.
fn view_lifetime(ty: &Type) -> Option<&str> {
    match ty {
        Type::Qualified(TypeQual::Borrow(life), _) => Some(life),
        _ => None,
    }
}

fn is_opt_module(module: &Module) -> bool {
    module.modes.iter().any(|m| m == "opt")
}

/// A short callable name for diagnostics: the last `.`-segment of the canonical
/// `module.fn` name.
fn short_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// Whether a function belongs to the bundled standard library (the optimized
/// substrate, exempt from the `mode opt` gate exactly as the linker's import rule
/// exempts it).
fn is_std_fn(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(m, _)| witchy_syntax::linker::STD_MODULES.contains(&m))
}

/// Entry point: validate every function's borrow signature, then check each body
/// for loan violations. Runs on the lowered whole-program module.
pub fn check(module: &Module) -> Result<(), TypeError> {
    let opt = is_opt_module(module);
    let mut sigs: HashMap<String, BorrowSig> = HashMap::new();

    // Pass 1: validate signatures and record each function's borrow relation.
    for item in &module.items {
        let Item::Function(f) = item else { continue };
        let sig = validate_signature(f, opt)?;
        sigs.insert(f.name.clone(), sig);
    }

    // A function that borrows nothing and calls nothing that returns a view can
    // never open a loan — but a `var`/`own` conflict needs the callee conventions,
    // so build that map once and share it.
    let conventions = function_conventions(module);

    // Pass 2: check each body against the collected relations.
    for item in &module.items {
        let Item::Function(f) = item else { continue };
        let mut ctx = LoanCtx { sigs: &sigs, conventions: &conventions, fn_name: &f.name };
        ctx.check_block(&f.body)?;
    }
    Ok(())
}

/// Validate one function's view syntax and compute its borrow relation.
fn validate_signature(f: &Function, opt: bool) -> Result<BorrowSig, TypeError> {
    // Input lifetimes declared by borrowed parameters: name -> param indices.
    let mut input_lifetimes: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut uses_view = false;
    for (i, p) in f.params.iter().enumerate() {
        if let Some(ty) = &p.ty {
            if let Some(life) = view_lifetime(ty) {
                uses_view = true;
                input_lifetimes.entry(life).or_default().push(i);
            }
        }
    }
    let ret_life = f.ret.as_ref().and_then(|t| view_lifetime(t));
    if ret_life.is_some() {
        uses_view = true;
    }

    // Views are a `mode opt`-only surface (RFC-0083). The bundled std is the
    // optimized substrate and is exempt, matching the linker's import rule.
    if uses_view && !opt && !is_std_fn(&f.name) {
        return Err(terr(format!(
            "`{}` uses a borrowed view (`View(T, 'a)` / `let('a) T`), which is only \
             available in a `mode opt` module — add `mode opt` at the top of the file, \
             or return an owned value",
            short_name(&f.name)
        )));
    }

    // A borrowed parameter must not carry a mutable convention: a view is
    // read-only, so `var`/`own` on it is a contradiction.
    for p in &f.params {
        if p.ty.as_ref().is_some_and(|t| view_lifetime(t).is_some()) && p.convention.binds_mutable()
        {
            return Err(terr(format!(
                "parameter `{}` of `{}` is a borrowed view (read-only) but its convention \
                 is mutable (`var`/`own`) — a view cannot be mutated or consumed",
                p.name,
                short_name(&f.name)
            )));
        }
    }

    // Every returned view lifetime must be bound by an input view of the same
    // name: an output borrow cannot come from nowhere (it would dangle).
    let owner_params = if let Some(life) = ret_life {
        match input_lifetimes.get(life) {
            Some(idxs) => idxs.clone(),
            None => {
                return Err(terr(format!(
                    "`{}` returns a view with lifetime `'{life}`, but no parameter borrows \
                     with that lifetime — an output view must borrow from an input. Write \
                     the corresponding parameter as `let('{life}) T`, or return an owned value",
                    short_name(&f.name)
                )));
            }
        }
    } else {
        Vec::new()
    };

    Ok(BorrowSig { returns_view: ret_life.is_some(), owner_params })
}

/// A single open loan: a view binding that borrows an owner local.
#[derive(Clone)]
struct Loan {
    /// The local variable that received the borrowed result (the view).
    view: String,
    /// The owner local whose storage the view borrows.
    owner: String,
    /// Callee whose return type created this loan (for diagnostics).
    origin: String,
}

/// One owner borrowed by a `let` right-hand side, with the borrowing callee.
struct BorrowSource {
    owner: String,
    origin: String,
}

/// The tail expression of a block — the value it evaluates to — if its last
/// statement is a value expression (not a `let`/`return`/loop-control).
fn block_tail(block: &Block) -> Option<&Expr> {
    match block.stmts.last() {
        Some(Stmt::Expr(e)) => Some(e),
        _ => None,
    }
}

/// Extract the root local of a place expression (`x`, `x.f`, `x[i]`). `None` for
/// a non-place (a call result, literal, …).
fn expr_root(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Var(name) => Some(name),
        Expr::Field { base, .. } => expr_root(base),
        Expr::Index { base, .. } => expr_root(base),
        _ => None,
    }
}

struct LoanCtx<'a> {
    sigs: &'a HashMap<String, BorrowSig>,
    conventions: &'a HashMap<String, Vec<Convention>>,
    fn_name: &'a str,
}

impl LoanCtx<'_> {
    /// Check a function body: the top-level block starts with no inherited loans.
    fn check_block(&mut self, block: &Block) -> Result<(), TypeError> {
        self.check_block_with(block, &[])
    }

    /// Check a block's linear statement sequence.
    ///
    /// `inherited` loans come from an enclosing block and are treated as live for
    /// this ENTIRE block (their liveness is governed by the enclosing scope, not
    /// this block's last-use — that is what makes a conflict nested inside an
    /// `if`/`while`/`match` arm still get caught). Loans OPENED in this block are
    /// last-use scoped: dropped once the view is not mentioned again in the
    /// remaining statements of this block — a sound non-lexical window. There is no
    /// `.owned()` special case: `view.owned()` returns an OWNED type (it opens
    /// no loan) and is the view's last use, so last-use ends the loan on its own.
    fn check_block_with(&mut self, block: &Block, inherited: &[Loan]) -> Result<(), TypeError> {
        let mut local: Vec<Loan> = Vec::new();
        for (idx, stmt) in block.stmts.iter().enumerate() {
            // Drop local loans whose view is never mentioned again from here on.
            local.retain(|loan| self.view_used_from(loan, &block.stmts[idx..]));

            // Everything live at this statement: inherited (whole-block) + local.
            let live: Vec<Loan> = inherited.iter().chain(local.iter()).cloned().collect();

            // A conflicting operation on any live loan's owner (in this statement's
            // own expressions, not counting nested blocks) is rejected.
            self.reject_conflicts(stmt, &live)?;

            // Recurse into nested expression blocks, carrying the loans live here so
            // a conflict inside them is caught against the enclosing loans too.
            self.check_nested_blocks(stmt, &live)?;

            // Opening loans: `let v = <expr borrowing one or more owners>`. Any
            // view-producing right-hand side (a direct call, a wrapper call, or an
            // `if`/`match`/block whose branches return views) opens a loan per
            // distinct owner it borrows.
            if let Stmt::Let { name, value, .. } = stmt {
                for owner in self.borrow_sources(value) {
                    local.push(Loan {
                        view: name.clone(),
                        owner: owner.owner,
                        origin: owner.origin,
                    });
                }
            }
        }
        Ok(())
    }

    /// The owners a `let` right-hand side borrows — a RESULT-position analysis: a
    /// loan opens only when the value the binding receives IS a view. So
    /// `wrapper(s)` (returns a view of `s`) borrows `s`, but `borrow(s).owned()`
    /// borrows nothing — the outer `owned` call returns an OWNED value, and the
    /// transient inner view is consumed, not bound. This is exactly why
    /// materialization opens no loan and needs no special case. Traced through
    /// view-returning call results (including nested owner arguments that are
    /// themselves views) and the tails of an `if`/`match`/block.
    fn borrow_sources(&self, value: &Expr) -> Vec<BorrowSource> {
        let mut out: Vec<BorrowSource> = Vec::new();
        let mut seen: Vec<(String, String)> = Vec::new();
        self.collect_view_owners(value, &mut out, &mut seen);
        out
    }

    /// Append the owner roots that `e`'s RESULT value borrows (with the borrowing
    /// callee for diagnostics), if `e` evaluates to a view. `origin` is threaded so
    /// the outermost view-returning callee names the loan.
    fn collect_view_owners(
        &self,
        e: &Expr,
        out: &mut Vec<BorrowSource>,
        seen: &mut Vec<(String, String)>,
    ) {
        match e {
            Expr::Call { name: callee, args } => {
                let Some(sig) = self.sigs.get(callee) else { return };
                if !sig.returns_view {
                    return; // an owned result (e.g. `view.owned()`) borrows nothing
                }
                for &i in &sig.owner_params {
                    let Some(arg) = args.get(i) else { continue };
                    if let Some(root) = expr_root(arg) {
                        let key = (root.to_string(), callee.clone());
                        if !seen.contains(&key) {
                            seen.push(key);
                            out.push(BorrowSource { owner: root.to_string(), origin: callee.clone() });
                        }
                    } else {
                        // The borrowed argument is itself a view expression (e.g.
                        // `outer(borrow(s))`); its own result owners are this
                        // binding's owners, keeping the outer callee as the origin.
                        let mut inner = Vec::new();
                        self.collect_view_owners(arg, &mut inner, &mut Vec::new());
                        for src in inner {
                            let key = (src.owner.clone(), callee.clone());
                            if !seen.contains(&key) {
                                seen.push(key);
                                out.push(BorrowSource { owner: src.owner, origin: callee.clone() });
                            }
                        }
                    }
                }
            }
            Expr::If { then_block, else_block, .. } => {
                if let Some(t) = block_tail(then_block) {
                    self.collect_view_owners(t, out, seen);
                }
                if let Some(b) = else_block.as_ref().and_then(block_tail) {
                    self.collect_view_owners(b, out, seen);
                }
            }
            Expr::Match { arms, .. } => {
                for a in arms {
                    self.collect_view_owners(&a.body, out, seen);
                }
            }
            Expr::Block(b) => {
                if let Some(t) = block_tail(b) {
                    self.collect_view_owners(t, out, seen);
                }
            }
            _ => {}
        }
    }

    /// Reject a statement that moves, mutates, reassigns, or lets escape the owner
    /// of any live loan.
    fn reject_conflicts(&self, stmt: &Stmt, open: &[Loan]) -> Result<(), TypeError> {
        for loan in open {
            // Reassigning the owner place invalidates every view of it.
            if let Stmt::Assign { name, .. } = stmt {
                if name == &loan.owner {
                    return Err(self.conflict(loan, "reassigned"));
                }
            }
            // The view escaping through a closure/task/channel while its loan is
            // live requires materialization (the owner may not outlive the view's
            // new home). Detect the view captured by a lambda or sent/spawned.
            if stmt_lets_view_escape(stmt, &loan.view) {
                return Err(self.escape(loan));
            }
        }
        // Owner moved (`move owner`) or passed to a `var`/`own` parameter anywhere
        // in this statement's expressions.
        self.reject_owner_transfer(stmt, open)?;
        Ok(())
    }

    /// Reject `move owner` and passing `owner` to a `var`/`own` parameter, walked
    /// over every expression in the statement.
    fn reject_owner_transfer(&self, stmt: &Stmt, open: &[Loan]) -> Result<(), TypeError> {
        let mut result = Ok(());
        walk_stmt_exprs(stmt, &mut |e| {
            if result.is_err() {
                return;
            }
            // `move owner`
            if let Expr::Unary { op: UnOp::Move, expr } = e {
                if let Some(root) = expr_root(expr) {
                    if let Some(loan) = open.iter().find(|l| l.owner == root) {
                        result = Err(self.conflict(loan, "moved (`move`)"));
                    }
                }
            }
            // `f(…, owner, …)` where the owner's parameter is `var`/`own`.
            if let Expr::Call { name: callee, args } = e {
                if let Some(convs) = self.owner_conventions(callee) {
                    for (arg, conv) in args.iter().zip(convs) {
                        if !conv.binds_mutable() {
                            continue;
                        }
                        if let Some(root) = expr_root(arg) {
                            if let Some(loan) = open.iter().find(|l| l.owner == root) {
                                let kind = if *conv == Convention::Var { "`var`" } else { "`own`" };
                                result = Err(self.conflict(
                                    loan,
                                    &format!("passed to a {kind} parameter of `{}`", short_name(callee)),
                                ));
                            }
                        }
                    }
                }
            }
        });
        result
    }

    /// Recurse into every block nested in this statement's expressions, carrying
    /// the loans live at this point so an owner conflict inside a nested block is
    /// caught and the nested block may open (and end) its own loans. A `match`
    /// arm body is an expression, not a block; wrap it in a one-statement block so
    /// it goes through the same path.
    fn check_nested_blocks(&mut self, stmt: &Stmt, open: &[Loan]) -> Result<(), TypeError> {
        let mut nested: Vec<Block> = Vec::new();
        collect_nested_blocks_in_stmt(stmt, &mut nested);
        for b in &nested {
            self.check_block_with(b, open)?;
        }
        Ok(())
    }

    /// The parameter conventions of a callee, if known.
    fn owner_conventions(&self, callee: &str) -> Option<&[Convention]> {
        self.conventions.get(callee).map(Vec::as_slice)
    }

    /// Does the loan's view appear anywhere in `stmts` (so its loan is still live)?
    fn view_used_from(&self, loan: &Loan, stmts: &[Stmt]) -> bool {
        stmts.iter().any(|s| stmt_mentions(s, &loan.view))
    }

    fn conflict(&self, loan: &Loan, what: &str) -> TypeError {
        terr(format!(
            "in `{}`: owner `{}` is {what} while the borrowed view `{}` (from `{}`) is still \
             live — a view keeps its owner borrowed until its last use. End the view's use \
             first, or materialize it with `{}.owned()` before touching `{}`",
            short_name(self.fn_name),
            loan.owner,
            loan.view,
            short_name(&loan.origin),
            loan.view,
            loan.owner,
        ))
    }

    fn escape(&self, loan: &Loan) -> TypeError {
        terr(format!(
            "in `{}`: the borrowed view `{}` (from `{}`) escapes through a closure, task, or \
             channel while it still borrows `{}` — a view cannot outlive its owner. \
             Materialize it with `{}.owned()` first to send an owned value",
            short_name(self.fn_name),
            loan.view,
            short_name(&loan.origin),
            loan.owner,
            loan.view,
        ))
    }
}

/// Whether a statement mentions the variable `name` anywhere (read or write).
/// Because `view.owned()` returns an OWNED value (its blanket `Owned` impl returns
/// `Self`, so it opens no loan) and is a mention of `view`, a `let keep =
/// view.owned()` is the view's last use — so last-use ending handles
/// materialization with no name-based special case.
fn stmt_mentions(stmt: &Stmt, name: &str) -> bool {
    let mut found = false;
    walk_stmt_exprs(stmt, &mut |e| {
        if let Expr::Var(v) = e {
            if v == name {
                found = true;
            }
        }
    });
    found
}

/// Whether a statement lets the given view escape via a closure capture, a
/// channel send, or a task spawn while the view is live.
fn stmt_lets_view_escape(stmt: &Stmt, view: &str) -> bool {
    let mut escapes = false;
    walk_stmt_exprs(stmt, &mut |e| {
        match e {
            // Captured by a closure environment.
            Expr::Lambda { body, .. } => {
                if block_mentions_var(body, view) {
                    escapes = true;
                }
            }
            // Sent through a channel or spawned into a task: any call whose name
            // ends in `send`/`spawn` taking the view as an argument.
            Expr::Call { name, args } => {
                let n = short_name(name);
                if (n == "send" || n == "spawn") && args.iter().any(|a| expr_root(a) == Some(view)) {
                    escapes = true;
                }
            }
            _ => {}
        }
    });
    escapes
}

fn block_mentions_var(block: &Block, name: &str) -> bool {
    block.stmts.iter().any(|s| stmt_mentions(s, name))
}

/// Collect the blocks nested directly in a statement's expressions — the bodies
/// of `if`/`while`/`for`/`while let`/bare-block and each `match` arm (an arm body
/// is an expression, wrapped in a one-statement block so it re-uses the block
/// path). Only the FIRST block level is collected; a block found here recurses
/// via `check_block_with`, which collects its own nested blocks in turn.
fn collect_nested_blocks_in_stmt(stmt: &Stmt, out: &mut Vec<Block>) {
    // Walk shallowly: find block-bearing expressions but do NOT descend into the
    // blocks themselves (they recurse separately via `check_block_with`).
    let mut stack: Vec<&Expr> = stmt_top_exprs(stmt);
    while let Some(e) = stack.pop() {
        push_own_blocks(e, out);
        push_shallow_children(e, &mut stack);
    }
}

/// The top-level expressions of a statement (no recursion), as a vec so their
/// lifetime is tied to `stmt` for the shallow walk.
fn stmt_top_exprs(stmt: &Stmt) -> Vec<&Expr> {
    match stmt {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Yield(value)
        | Stmt::Expr(value)
        | Stmt::Return(Some(value)) => vec![value],
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => vec![],
    }
}

/// Push the block bodies a single expression owns directly (its `if`/loop body,
/// its `match` arms). A lambda body is deliberately NOT pushed: it has its own
/// scope, and a view it captures is handled as an escape, not an in-scope use.
fn push_own_blocks(e: &Expr, out: &mut Vec<Block>) {
    match e {
        Expr::If { then_block, else_block, .. } => {
            out.push(then_block.clone());
            if let Some(b) = else_block {
                out.push(b.clone());
            }
        }
        Expr::While { body, .. }
        | Expr::For { body, .. }
        | Expr::Block(body)
        | Expr::WhileLet { body, .. } => out.push(body.clone()),
        Expr::Match { arms, .. } => {
            for a in arms {
                out.push(Block {
                    stmts: vec![Stmt::Expr(a.body.clone())],
                    lines: vec![a.line],
                    region: None,
                });
            }
        }
        _ => {}
    }
}

/// The immediate sub-expressions of `e` that are NOT block bodies (so the shallow
/// walk in `collect_nested_blocks_in_stmt` reaches block-bearing expressions
/// buried in operands without descending into any block it finds).
fn push_shallow_children<'a>(e: &'a Expr, stack: &mut Vec<&'a Expr>) {
    match e {
        Expr::List(xs) | Expr::Tuple(xs) => stack.extend(xs.iter()),
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            stack.extend(args.iter())
        }
        Expr::LabeledCall { args, .. } => stack.extend(args.iter().map(|(_, a)| a)),
        Expr::MethodCall { receiver, args, .. } => {
            stack.push(receiver);
            stack.extend(args.iter());
        }
        Expr::Apply { func, args } => {
            stack.push(func);
            stack.extend(args.iter());
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => stack.push(expr),
        Expr::Field { base, .. } => stack.push(base),
        Expr::RecordUpdate { base, fields, .. } => {
            stack.push(base);
            stack.extend(fields.iter().map(|(_, v)| v));
        }
        Expr::Record { fields, spread, .. } => {
            stack.extend(fields.iter().map(|(_, v)| v));
            if let Some(s) = spread {
                stack.push(s);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            stack.push(lhs);
            stack.push(rhs);
        }
        Expr::Range { lo, hi, .. } => {
            stack.push(lo);
            stack.push(hi);
        }
        Expr::Index { base, index } => {
            stack.push(base);
            stack.push(index);
        }
        // The condition/scrutinee of a block-bearing form still needs scanning for
        // buried blocks; its block body is handled by the `push` closure above.
        Expr::If { cond, .. } => stack.push(cond),
        Expr::While { cond, .. } => stack.push(cond),
        Expr::For { iter, .. } => stack.push(iter),
        Expr::WhileLet { scrutinee, .. } => stack.push(scrutinee),
        Expr::Match { scrutinee, .. } => stack.push(scrutinee),
        _ => {}
    }
}

/// Visit every expression in a statement (pre-order), including nested ones, so a
/// callback can inspect uses without each caller re-implementing the walk.
fn walk_stmt_exprs(stmt: &Stmt, f: &mut impl FnMut(&Expr)) {
    match stmt {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Yield(value)
        | Stmt::Expr(value) => walk_expr(value, f),
        Stmt::Return(Some(value)) => walk_expr(value, f),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
    }
}

fn walk_expr(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    match e {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
        Expr::List(xs) | Expr::Tuple(xs) => xs.iter().for_each(|x| walk_expr(x, f)),
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            args.iter().for_each(|a| walk_expr(a, f))
        }
        Expr::LabeledCall { args, .. } => args.iter().for_each(|(_, a)| walk_expr(a, f)),
        Expr::MethodCall { receiver, args, .. } => {
            walk_expr(receiver, f);
            args.iter().for_each(|a| walk_expr(a, f));
        }
        Expr::Apply { func, args } => {
            walk_expr(func, f);
            args.iter().for_each(|a| walk_expr(a, f));
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => walk_expr(expr, f),
        Expr::Field { base, .. } => walk_expr(base, f),
        Expr::RecordUpdate { base, fields, .. } => {
            walk_expr(base, f);
            fields.iter().for_each(|(_, v)| walk_expr(v, f));
        }
        Expr::Record { fields, spread, .. } => {
            fields.iter().for_each(|(_, v)| walk_expr(v, f));
            if let Some(s) = spread {
                walk_expr(s, f);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        Expr::If { cond, then_block, else_block } => {
            walk_expr(cond, f);
            walk_block(then_block, f);
            if let Some(b) = else_block {
                walk_block(b, f);
            }
        }
        Expr::Match { scrutinee, arms } => {
            walk_expr(scrutinee, f);
            for a in arms {
                if let Some(g) = &a.guard {
                    walk_expr(g, f);
                }
                walk_expr(&a.body, f);
            }
        }
        Expr::Block(b) => walk_block(b, f),
        Expr::While { cond, body } => {
            walk_expr(cond, f);
            walk_block(body, f);
        }
        Expr::For { iter, body, .. } => {
            walk_expr(iter, f);
            walk_block(body, f);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            walk_expr(scrutinee, f);
            walk_block(body, f);
        }
        Expr::Range { lo, hi, .. } => {
            walk_expr(lo, f);
            walk_expr(hi, f);
        }
        Expr::Index { base, index } => {
            walk_expr(base, f);
            walk_expr(index, f);
        }
        Expr::Lambda { body, .. } => walk_block(body, f),
    }
}

fn walk_block(b: &Block, f: &mut impl FnMut(&Expr)) {
    for s in &b.stmts {
        walk_stmt_exprs(s, f);
    }
}

/// Access each function's parameter conventions — used to reject passing a loaned
/// owner to a `var`/`own` parameter.
fn function_conventions(module: &Module) -> HashMap<String, Vec<Convention>> {
    let mut out = HashMap::new();
    for item in &module.items {
        if let Item::Function(f) = item {
            out.insert(f.name.clone(), f.params.iter().map(|p| p.convention).collect());
        }
    }
    out
}

#[cfg(test)]
#[path = "loans_tests.rs"]
mod tests;
