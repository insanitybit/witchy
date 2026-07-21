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
//! the checker builds and never re-infers types. A successful pass also publishes
//! statement-identity loan events for lowering, so ownership and rooting consume
//! this exact checker rather than approximating it.

use std::collections::HashMap;

use witchy_syntax::ast::{Block, Convention, Expr, Function, Item, Module, Param, Stmt, Type, TypeQual, UnOp};

use crate::typeck::TypeError;

fn terr(message: String) -> TypeError {
    TypeError { message }
}

/// The output-to-input borrow relation of one function, read off its signature.
#[derive(Clone)]
struct BorrowSig {
    /// `true` when the return type is a borrowed view.
    returns_view: bool,
    /// Parameter indices whose borrow lifetime matches the returned view's
    /// lifetime — the owners a call's result loans. Empty when the return is not
    /// a view (or, after signature validation, never empty when it is).
    owner_params: Vec<(usize, Type)>,
    conventions: Vec<Convention>,
    /// The runtime storage type of the returned view. This can differ from the
    /// owner parameter type (`Record -> View(Bytes, 'a)`) and therefore governs
    /// the compiled root's refcount-header layout.
    view_type: Option<Type>,
    callable_params: Vec<Option<Box<BorrowSig>>>,
    callable_return: Option<Box<BorrowSig>>,
}

/// The borrow qualifier's lifetime name on a parameter/return type, if any.
fn view_lifetime(ty: &Type) -> Option<&str> {
    match ty {
        Type::Qualified(TypeQual::Borrow(life), _) => Some(life),
        _ => None,
    }
}

fn view_storage_type(ty: &Type) -> Option<Type> {
    match ty {
        Type::Qualified(TypeQual::Borrow(_), inner) => Some((**inner).clone()),
        _ => None,
    }
}

fn is_opt_function(name: &str, modes: &[String]) -> bool {
    if let Some((module, _)) = name.rsplit_once('.') {
        return modes.iter().any(|mode| mode == &format!("@opt:{module}"));
    }
    modes.iter().any(|mode| mode == "opt")
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

/// One checked owner loan. Lowering uses these names to invalidate ownership
/// tokens and retain the owner while the view is live.
#[derive(Clone, Debug, PartialEq)]
pub struct LoanEvent {
    pub view: String,
    pub owner: String,
    pub origin: String,
    pub owner_type: Type,
}

/// Authoritative events keyed by statement identity in the checked module.
#[derive(Default)]
pub struct LoanFacts {
    active: HashMap<usize, Vec<LoanEvent>>,
    opens_after: HashMap<usize, Vec<LoanEvent>>,
    closes_after: HashMap<usize, Vec<LoanEvent>>,
}

fn stmt_key(stmt: &Stmt) -> usize {
    stmt as *const Stmt as usize
}

fn block_key(block: &Block) -> usize {
    block as *const Block as usize
}

impl LoanFacts {
    pub fn active_at(&self, stmt: &Stmt) -> &[LoanEvent] {
        self.active.get(&stmt_key(stmt)).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn opens_after(&self, stmt: &Stmt) -> &[LoanEvent] {
        self.opens_after.get(&stmt_key(stmt)).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn closes_after(&self, stmt: &Stmt) -> &[LoanEvent] {
        self.closes_after.get(&stmt_key(stmt)).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Identity key for a statement that carries any lowering-relevant loan
    /// fact. Unknown/cloned statements return `None`; lowering compares the set
    /// consumed by each compile unit with the set collected from the checked AST.
    pub fn event_key(&self, stmt: &Stmt) -> Option<usize> {
        let key = stmt_key(stmt);
        (self.active.contains_key(&key)
            || self.opens_after.contains_key(&key)
            || self.closes_after.contains_key(&key))
            .then_some(key)
    }
}

/// Validate loan semantics when the caller does not need lowering facts.
pub(crate) fn check(module: &Module) -> Result<(), TypeError> {
    facts(module).map(|_| ())
}

/// Validate every function and return the exact events consumed by lowering.
pub fn facts(module: &Module) -> Result<LoanFacts, TypeError> {
    let mut sigs: HashMap<String, BorrowSig> = HashMap::new();

    // Pass 1: validate signatures and record each function's borrow relation.
    for item in &module.items {
        let Item::Function(f) = item else { continue };
        let sig = validate_signature(f, is_opt_function(&f.name, &module.modes))?;
        sigs.insert(f.name.clone(), sig);
    }

    let mut facts = LoanFacts::default();
    // Pass 2: check each body and record statement-identity events.
    for item in &module.items {
        let Item::Function(f) = item else { continue };
        let mut ctx = LoanCtx {
            sigs: &sigs,
            fn_name: &f.name,
            facts: &mut facts,
            returns_view: sigs.get(&f.name).is_some_and(|sig| sig.returns_view),
            return_owners: sigs
                .get(&f.name)
                .map(|sig| {
                    sig.owner_params
                        .iter()
                        .filter_map(|(index, _)| f.params.get(*index).map(|param| param.name.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            block_results: HashMap::new(),
            input_borrows: f
                .params
                .iter()
                .filter_map(|param| {
                    let ty = param.ty.as_ref()?;
                    let owner_type = view_storage_type(ty)?;
                    Some((
                        param.name.clone(),
                        BorrowSource {
                            owner: param.name.clone(),
                            origin: f.name.clone(),
                            owner_type,
                            temporary: false,
                        },
                    ))
                })
                .collect(),
            return_callable: f.ret.as_ref().and_then(borrow_sig_from_fn_type).map(Box::new),
        };
        let callable_params: HashMap<String, BorrowSig> = f
            .params
            .iter()
            .filter_map(|param| {
                borrow_sig_from_fn_type(param.ty.as_ref()?).map(|sig| (param.name.clone(), sig))
            })
            .collect();
        ctx.check_block_with(&f.body, &[], &callable_params, true)?;

        let mut lambdas = Vec::new();
        collect_lambdas(&f.body, &mut lambdas);
        for (index, (params, body, ret)) in lambdas.into_iter().enumerate() {
            check_lambda_body(
                params,
                body,
                ret,
                &format!("lambda {} in {}", index + 1, short_name(&f.name)),
                is_opt_function(&f.name, &module.modes),
                &sigs,
                &mut facts,
            )?;
        }
    }
    Ok(facts)
}

fn check_lambda_body(
    params: &[Param],
    body: &Block,
    ret: Option<&Type>,
    name: &str,
    opt: bool,
    sigs: &HashMap<String, BorrowSig>,
    facts: &mut LoanFacts,
) -> Result<(), TypeError> {
    let forwarded = forwarding_lambda_sig(params, body, sigs);
    let forwarded = forwarded.as_ref();
    let explicitly_uses_view = params
        .iter()
        .filter_map(|param| param.ty.as_ref())
        .any(type_mentions_view)
        || ret.is_some_and(type_mentions_view);
    if explicitly_uses_view && !opt {
        return Err(terr(format!("borrowed views in `{name}` require `mode opt`")));
    }
    for ty in params.iter().filter_map(|param| param.ty.as_ref()) {
        validate_nested_fn_borrows(ty, name)?;
    }
    if let Some(ret) = ret {
        validate_nested_fn_borrows(ret, name)?;
    }
    let ret_life = ret.and_then(view_lifetime);
    let return_owners: Vec<String> = if let Some(life) = ret_life {
        params
            .iter()
            .filter(|param| {
                param.ty.as_ref().and_then(view_lifetime).is_some_and(|input| input == life)
            })
            .map(|param| param.name.clone())
            .collect()
    } else {
        forwarded
            .filter(|sig| sig.returns_view)
            .into_iter()
            .flat_map(|sig| sig.owner_params.iter())
            .filter_map(|(index, _)| params.get(*index).map(|param| param.name.clone()))
            .collect()
    };
    if ret_life.is_some() && return_owners.is_empty() {
        return Err(terr(format!(
            "`{name}` returns a view whose lifetime is not bound by a lambda parameter"
        )));
    }
    let callable_params: HashMap<String, BorrowSig> = params
        .iter()
        .filter_map(|param| {
            borrow_sig_from_fn_type(param.ty.as_ref()?).map(|sig| (param.name.clone(), sig))
        })
        .collect();
    let input_borrows = params
        .iter()
        .filter_map(|param| {
            let owner_type = view_storage_type(param.ty.as_ref()?)?;
            Some((
                param.name.clone(),
                BorrowSource {
                    owner: param.name.clone(),
                    origin: name.to_string(),
                    owner_type,
                    temporary: false,
                },
            ))
        })
        .collect();
    let mut ctx = LoanCtx {
        sigs,
        fn_name: name,
        facts,
        returns_view: ret_life.is_some() || forwarded.is_some_and(|sig| sig.returns_view),
        return_owners,
        block_results: HashMap::new(),
        input_borrows,
        return_callable: ret.and_then(borrow_sig_from_fn_type).map(Box::new),
    };
    ctx.check_block_with(body, &[], &callable_params, true)
}

/// Recover the typed contract of a pure forwarding lambda. The linker represents
/// an imported function value such as `api.view` as
/// `fn(__eta0): api.view(__eta0)`; the callee signature remains the authority for
/// conventions and output-to-input loans.
fn forwarding_lambda_sig(
    params: &[Param],
    body: &Block,
    sigs: &HashMap<String, BorrowSig>,
) -> Option<BorrowSig> {
    let [Stmt::Expr(Expr::Call { name, args })] = body.stmts.as_slice() else {
        return None;
    };
    let sig = sigs.get(name)?;
    if params.len() != args.len() || params.len() != sig.conventions.len() {
        return None;
    }
    let forwards_positionally = params
        .iter()
        .zip(args)
        .zip(&sig.conventions)
        .all(|((param, arg), convention)| {
            param.convention == *convention
                && matches!(arg, Expr::Var(name) if name == &param.name)
        });
    forwards_positionally.then(|| sig.clone())
}

fn collect_lambdas<'a>(
    block: &'a Block,
    out: &mut Vec<(&'a [Param], &'a Block, Option<&'a Type>)>,
) {
    walk_block(block, &mut |expr| {
        if let Expr::Lambda { params, body, ret } = expr {
            out.push((params, body, ret.as_ref()));
        }
    });
}

/// Validate one function's view syntax and compute its borrow relation.
fn validate_signature(f: &Function, opt: bool) -> Result<BorrowSig, TypeError> {
    // Input lifetimes declared by borrowed parameters: name -> param indices.
    let mut input_lifetimes: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut uses_view = false;
    for (i, p) in f.params.iter().enumerate() {
        if let Some(ty) = &p.ty {
            validate_nested_fn_borrows(ty, &f.name)?;
            uses_view |= type_mentions_view(ty);
            if let Some(life) = view_lifetime(ty) {
                input_lifetimes.entry(life).or_default().push(i);
            }
        }
    }
    if let Some(ret) = &f.ret {
        validate_nested_fn_borrows(ret, &f.name)?;
        uses_view |= type_mentions_view(ret);
    }
    let ret_life = f.ret.as_ref().and_then(|t| view_lifetime(t));

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
            Some(idxs) => idxs
                .iter()
                .filter_map(|&idx| {
                    f.params.get(idx)?.ty.as_ref().map(|ty| (idx, ty.clone()))
                })
                .collect(),
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

    Ok(BorrowSig {
        returns_view: ret_life.is_some(),
        owner_params,
        conventions: f.params.iter().map(|param| param.convention).collect(),
        view_type: f.ret.as_ref().and_then(view_storage_type),
        callable_params: f
            .params
            .iter()
            .map(|param| {
                param.ty.as_ref().and_then(borrow_sig_from_fn_type).map(Box::new)
            })
            .collect(),
        callable_return: f.ret.as_ref().and_then(borrow_sig_from_fn_type).map(Box::new),
    })
}

fn type_mentions_view(ty: &Type) -> bool {
    match ty {
        Type::Qualified(TypeQual::Borrow(_), _) => true,
        Type::Qualified(_, inner) => type_mentions_view(inner),
        Type::Named(_, args) | Type::Tuple(args) => args.iter().any(type_mentions_view),
        Type::Dyn(_, args) => args.iter().any(type_mentions_view),
        Type::RecordCompose { base, fields } => {
            type_mentions_view(base)
                || fields.iter().any(|(_, ty)| type_mentions_view(ty))
        }
        Type::Fn(params, ret, _) => {
            params.iter().any(type_mentions_view) || type_mentions_view(ret)
        }
    }
}

fn validate_nested_fn_borrows(ty: &Type, context: &str) -> Result<(), TypeError> {
    match ty {
        Type::Fn(params, ret, _) => {
            if let Some(life) = view_lifetime(ret) {
                let bound = params
                    .iter()
                    .any(|param| view_lifetime(param).is_some_and(|input| input == life));
                if !bound {
                    return Err(terr(format!(
                        "function type in `{}` returns a view with lifetime `'{life}`, but no \
                         function parameter borrows with that lifetime",
                        short_name(context)
                    )));
                }
            }
            for param in params {
                validate_nested_fn_borrows(param, context)?;
            }
            validate_nested_fn_borrows(ret, context)
        }
        Type::Qualified(_, inner) => validate_nested_fn_borrows(inner, context),
        Type::Named(_, args) | Type::Tuple(args) => {
            for arg in args {
                validate_nested_fn_borrows(arg, context)?;
            }
            Ok(())
        }
        Type::Dyn(_, args) => {
            for arg in args {
                validate_nested_fn_borrows(arg, context)?;
            }
            Ok(())
        }
        Type::RecordCompose { base, fields } => {
            validate_nested_fn_borrows(base, context)?;
            for (_, ty) in fields {
                validate_nested_fn_borrows(ty, context)?;
            }
            Ok(())
        }
    }
}

fn params_default_conventions(len: usize) -> Vec<Convention> {
    vec![Convention::Let; len]
}

fn borrow_sig_from_fn_type(ty: &Type) -> Option<BorrowSig> {
    let Type::Fn(params, ret, conventions) = ty.unqualified() else {
        return None;
    };
    let ret_life = view_lifetime(ret);
    let owner_params = ret_life
        .map(|life| {
            params
                .iter()
                .enumerate()
                .filter(|(_, param)| view_lifetime(param).is_some_and(|input| input == life))
                .map(|(index, param)| (index, param.clone()))
                .collect()
        })
        .unwrap_or_default();
    let conventions = if conventions.len() == params.len() {
        conventions.clone()
    } else {
        params_default_conventions(params.len())
    };
    Some(BorrowSig {
        returns_view: ret_life.is_some(),
        owner_params,
        conventions,
        view_type: view_storage_type(ret),
        callable_params: params
            .iter()
            .map(|param| borrow_sig_from_fn_type(param).map(Box::new))
            .collect(),
        callable_return: borrow_sig_from_fn_type(ret).map(Box::new),
    })
}

/// A single open loan: a view binding that borrows an owner local.
#[derive(Clone, Debug, PartialEq)]
struct Loan {
    /// The local variable that received the borrowed result (the view).
    view: String,
    /// The owner local whose storage the view borrows.
    owner: String,
    /// Callee whose return type created this loan (for diagnostics).
    origin: String,
    owner_type: Type,
}

/// One owner borrowed by a `let` right-hand side, with the borrowing callee.
#[derive(Clone)]
struct BorrowSource {
    owner: String,
    origin: String,
    owner_type: Type,
    temporary: bool,
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
    fn_name: &'a str,
    facts: &'a mut LoanFacts,
    returns_view: bool,
    return_owners: Vec<String>,
    /// Borrowed result provenance for already-checked nested blocks, keyed by
    /// exact block identity. This connects a block-local alias to an enclosing
    /// `if`/block result without re-running a second lifetime analysis.
    block_results: HashMap<usize, Vec<BorrowSource>>,
    /// Borrowed function parameters are provenance roots too. Recording all of
    /// them lets body checking reject returning a `'b` input under a declared
    /// `'a` result relation.
    input_borrows: HashMap<String, BorrowSource>,
    return_callable: Option<Box<BorrowSig>>,
}

impl LoanCtx<'_> {
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
    fn check_block_with(
        &mut self,
        block: &Block,
        inherited: &[Loan],
        inherited_callables: &HashMap<String, BorrowSig>,
        function_body: bool,
    ) -> Result<(), TypeError> {
        let mut local: Vec<Loan> = Vec::new();
        let mut callables = inherited_callables.clone();
        for (idx, stmt) in block.stmts.iter().enumerate() {
            // Drop local loans whose view is never mentioned again from here on.
            local.retain(|loan| self.view_used_from(loan, &block.stmts[idx..]));

            // Everything live at this statement: inherited (whole-block) + local.
            let live: Vec<Loan> = inherited.iter().chain(local.iter()).cloned().collect();
            self.facts.active.insert(
                stmt_key(stmt),
                live.iter().cloned().map(LoanEvent::from).collect(),
            );

            // A conflicting operation on any live loan's owner (in this statement's
            // own expressions, not counting nested blocks) is rejected.
            self.reject_conflicts(stmt, &live, &callables)?;
            self.reject_callable_boundaries(stmt, &callables)?;

            // Recurse into nested expression blocks, carrying the loans live here so
            // a conflict inside them is caught against the enclosing loans too.
            self.check_nested_blocks(stmt, &live, &callables)?;

            if let Stmt::LetPattern { value, .. } = stmt
                && let Some(source) = self.aggregate_borrow_source(value, &callables, &live)
            {
                return Err(self.aggregate_view_storage(&source.origin));
            }

            let returned = match stmt {
                Stmt::Return(Some(value)) => Some(value),
                Stmt::Expr(value) if function_body && idx + 1 == block.stmts.len() => Some(value),
                _ => None,
            };
            if let Some(value) = returned {
                let mut sources = self.borrow_sources(value, &callables, &live);
                self.collect_alias_sources(value, &live, &mut sources);
                self.validate_return_sources(&sources)?;
                if let Some(expected) = &self.return_callable
                    && let Some((_, source)) = self.callable_expr_sig(value, &callables)
                {
                    self.require_same_callable("returned function value", &source, expected)?;
                }
            }

            // Opening loans: `let v = <expr borrowing one or more owners>`. Any
            // view-producing right-hand side (a direct call, a wrapper call, or an
            // `if`/`match`/block whose branches return views) opens a loan per
            // distinct owner it borrows.
            if let Stmt::Let { name, ty, value, mutable } = stmt {
                if projection_of_live_view(value, &live) {
                    return Err(terr(format!(
                        "in `{}`: a projection of a borrowed view cannot be persisted because its +                         storage layout is not carried by the alias — materialize the view with +                         `.owned()` before projecting it",
                        short_name(self.fn_name),
                    )));
                }
                let mut sources = self.borrow_sources(value, &callables, &live);
                self.collect_alias_sources(value, &live, &mut sources);
                if let Some(source) = self.aggregate_borrow_source(value, &callables, &live) {
                    return Err(self.aggregate_view_storage(&source.origin));
                }
                if *mutable && !sources.is_empty() {
                    return Err(self.mutable_view_storage(name));
                }
                for owner in sources {
                    if owner.temporary {
                        return Err(self.temporary_owner(&owner.origin));
                    }
                    let loan = Loan {
                        view: name.clone(),
                        owner: owner.owner,
                        origin: owner.origin,
                        owner_type: owner.owner_type,
                    };
                    let event = LoanEvent::from(loan.clone());
                    self.facts.opens_after.entry(stmt_key(stmt)).or_default().push(event.clone());

                    // A never-used view closes at its binding. Otherwise its root
                    // remains through the statement containing its final mention.
                    let close_idx = block.stmts[idx + 1..]
                        .iter()
                        .rposition(|s| stmt_mentions(s, &loan.view))
                        .map(|offset| idx + 1 + offset)
                        .unwrap_or(idx);
                    self.facts
                        .closes_after
                        .entry(stmt_key(&block.stmts[close_idx]))
                        .or_default()
                        .push(event);
                    local.push(loan);
                }
                self.reject_callable_erasure(name, value, ty.as_ref(), None, &callables)?;
                if let Some(sig) = self.callable_value_sig(value, ty.as_ref(), &callables) {
                    callables.insert(name.clone(), sig);
                } else {
                    callables.remove(name);
                }
            } else if let Stmt::Assign { name, value } = stmt {
                let mut sources = self.borrow_sources(value, &callables, &live);
                self.collect_alias_sources(value, &live, &mut sources);
                if !sources.is_empty() {
                    return Err(self.mutable_view_storage(name));
                }
                self.reject_callable_erasure(
                    name,
                    value,
                    None,
                    callables.get(name),
                    &callables,
                )?;
                if let Some(sig) = self.callable_value_sig(value, None, &callables) {
                    callables.insert(name.clone(), sig);
                } else {
                    callables.remove(name);
                }
            }
        }

        let mut result = Vec::new();
        if let Some(tail) = block_tail(block) {
            let live: Vec<Loan> = inherited.iter().chain(local.iter()).cloned().collect();
            result = self.borrow_sources(tail, &callables, &live);
            self.collect_alias_sources(tail, &live, &mut result);
        }
        self.block_results.insert(block_key(block), result);
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
    fn borrow_sources(
        &self,
        value: &Expr,
        callables: &HashMap<String, BorrowSig>,
        live: &[Loan],
    ) -> Vec<BorrowSource> {
        let mut out: Vec<BorrowSource> = Vec::new();
        self.collect_view_owners(value, callables, live, &mut out);
        out
    }

    /// Result-position propagation for an already-bound view. `let next = view`
    /// transfers the same owner obligation to `next`; it must not silently end
    /// the loan merely because the original name's last use is the aliasing let.
    fn collect_alias_sources(&self, value: &Expr, live: &[Loan], out: &mut Vec<BorrowSource>) {
        match value {
            Expr::Var(name) => {
                if let Some(source) = self.input_borrows.get(name)
                    && !out.iter().any(|existing| {
                        existing.owner == source.owner && existing.origin == source.origin
                    })
                {
                    out.push(source.clone());
                }
                for loan in live.iter().filter(|loan| loan.view == *name) {
                    if !out.iter().any(|source| {
                        source.owner == loan.owner && source.origin == loan.origin
                    }) {
                        out.push(BorrowSource {
                            owner: loan.owner.clone(),
                            origin: loan.origin.clone(),
                            owner_type: loan.owner_type.clone(),
                            temporary: false,
                        });
                    }
                }
            }
            Expr::If { then_block, else_block, .. } => {
                if let Some(tail) = block_tail(then_block) {
                    self.collect_alias_sources(tail, live, out);
                }
                if let Some(tail) = else_block.as_ref().and_then(block_tail) {
                    self.collect_alias_sources(tail, live, out);
                }
            }
            Expr::Match { arms, .. } => {
                for arm in arms {
                    self.collect_alias_sources(&arm.body, live, out);
                }
            }
            Expr::Block(block) => {
                if let Some(tail) = block_tail(block) {
                    self.collect_alias_sources(tail, live, out);
                }
            }
            Expr::Field { base, .. } | Expr::Index { base, .. } => {
                self.collect_alias_sources(base, live, out);
            }
            _ => {}
        }
    }

    fn validate_return_sources(&self, sources: &[BorrowSource]) -> Result<(), TypeError> {
        for source in sources {
            if source.temporary {
                return Err(self.temporary_owner(&source.origin));
            }
            if !self.returns_view || !self.return_owners.contains(&source.owner) {
                return Err(terr(format!(
                    "in `{}`: returned value still borrows owner `{}` through `{}`, but the \
                     function signature does not return a view tied to that input — declare \
                     `View(T, 'a)` with a matching `let('a) T` parameter, or materialize the \
                     value with `.owned()` before returning",
                    short_name(self.fn_name),
                    source.owner,
                    short_name(&source.origin),
                )));
            }
        }
        Ok(())
    }

    /// Append the owner roots that `e`'s RESULT value borrows (with the borrowing
    /// callee for diagnostics), if `e` evaluates to a view. `origin` is threaded so
    /// the outermost view-returning callee names the loan.
    fn collect_view_owners(
        &self,
        e: &Expr,
        callables: &HashMap<String, BorrowSig>,
        live: &[Loan],
        out: &mut Vec<BorrowSource>,
    ) {
        match e {
            Expr::Call { name: callee, args } => {
                let Some(sig) = self.sigs.get(callee).or_else(|| callables.get(callee)) else {
                    return;
                };
                self.collect_call_owners(callee, args, sig, callables, live, out);
            }
            Expr::Apply { func, args } => {
                let Some((callee, sig)) = self.callable_expr_sig(func, callables) else {
                    return;
                };
                self.collect_call_owners(&callee, args, &sig, callables, live, out);
            }
            Expr::If { then_block, else_block, .. } => {
                self.collect_block_result(then_block, callables, live, out);
                if let Some(block) = else_block {
                    self.collect_block_result(block, callables, live, out);
                }
            }
            Expr::Match { arms, .. } => {
                for a in arms {
                    self.collect_view_owners(&a.body, callables, live, out);
                }
            }
            Expr::Block(block) => self.collect_block_result(block, callables, live, out),
            _ => {}
        }
    }

    fn collect_block_result(
        &self,
        block: &Block,
        callables: &HashMap<String, BorrowSig>,
        live: &[Loan],
        out: &mut Vec<BorrowSource>,
    ) {
        if let Some(sources) = self.block_results.get(&block_key(block)) {
            for source in sources {
                if !out.iter().any(|existing| existing.owner == source.owner) {
                    out.push(source.clone());
                }
            }
        } else if let Some(tail) = block_tail(block) {
            self.collect_view_owners(tail, callables, live, out);
        }
    }

    fn collect_call_owners(
        &self,
        callee: &str,
        args: &[Expr],
        sig: &BorrowSig,
        callables: &HashMap<String, BorrowSig>,
        live: &[Loan],
        out: &mut Vec<BorrowSource>,
    ) {
        if !sig.returns_view {
            return; // an owned result (e.g. `view.owned()`) borrows nothing
        }
        for (i, _) in &sig.owner_params {
            let Some(arg) = args.get(*i) else { continue };
            if let Some(root) = expr_root(arg) {
                let aliases: Vec<&Loan> =
                    live.iter().filter(|loan| loan.view == root).collect();
                if aliases.is_empty() {
                    self.push_source(
                        BorrowSource {
                            owner: root.to_string(),
                            origin: callee.to_string(),
                            owner_type: sig.view_type.clone().expect("view result type"),
                            temporary: false,
                        },
                        out,
                    );
                } else {
                    for alias in aliases {
                        self.push_source(
                            BorrowSource {
                                owner: alias.owner.clone(),
                                origin: callee.to_string(),
                                owner_type: sig.view_type.clone().expect("view result type"),
                                temporary: false,
                            },
                            out,
                        );
                    }
                }
            } else {
                // The borrowed argument is itself a view expression (e.g.
                // `outer(borrow(s))`); its own result owners are this
                // binding's owners, keeping the outer callee as the origin.
                let mut inner = Vec::new();
                self.collect_view_owners(arg, callables, live, &mut inner);
                if inner.is_empty() {
                    inner.push(BorrowSource {
                        owner: String::new(),
                        origin: callee.to_string(),
                        owner_type: sig.view_type.clone().expect("view result type"),
                        temporary: true,
                    });
                }
                for mut src in inner {
                    src.origin = callee.to_string();
                    src.owner_type = sig.view_type.clone().expect("view result type");
                    self.push_source(src, out);
                }
            }
        }
    }

    fn push_source(&self, source: BorrowSource, out: &mut Vec<BorrowSource>) {
        if !out.iter().any(|existing| existing.owner == source.owner) {
            out.push(source);
        }
    }

    fn temporary_owner(&self, origin: &str) -> TypeError {
        terr(format!(
            "in `{}`: `{}` returns a borrowed view of a temporary value with no stable owner — \
             bind the owner first, or materialize the result with `.owned()` in the same expression",
            short_name(self.fn_name),
            short_name(origin),
        ))
    }

    fn mutable_view_storage(&self, binding: &str) -> TypeError {
        terr(format!(
            "in `{}`: mutable binding `{binding}` cannot store a borrowed view — keep the \
             view in an immutable `let` binding, or materialize it with `.owned()` first",
            short_name(self.fn_name),
        ))
    }

    fn aggregate_view_storage(&self, origin: &str) -> TypeError {
        terr(format!(
            "in `{}`: the borrowed result from `{}` is stored in an owned aggregate — \
             materialize the view with `.owned()` before storing it",
            short_name(self.fn_name),
            short_name(origin),
        ))
    }

    fn aggregate_borrow_source(
        &self,
        value: &Expr,
        callables: &HashMap<String, BorrowSig>,
        live: &[Loan],
    ) -> Option<BorrowSource> {
        let mut inspect = |expr: &Expr| {
            let mut sources = self.borrow_sources(expr, callables, live);
            self.collect_alias_sources(expr, live, &mut sources);
            sources.into_iter().next()
        };
        match value {
            Expr::List(items) | Expr::Tuple(items) => items
                .iter()
                .find_map(&mut inspect)
                .or_else(|| items.iter().find_map(|item| self.aggregate_borrow_source(item, callables, live))),
            Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => args
                .iter()
                .find_map(&mut inspect)
                .or_else(|| args.iter().find_map(|arg| self.aggregate_borrow_source(arg, callables, live))),
            Expr::Record { fields, spread, .. } => fields
                .iter()
                .find_map(|(_, field)| inspect(field))
                .or_else(|| spread.as_deref().and_then(&mut inspect))
                .or_else(|| {
                    fields.iter().find_map(|(_, field)| {
                        self.aggregate_borrow_source(field, callables, live)
                    })
                }),
            Expr::RecordUpdate { base, fields, .. } => inspect(base)
                .or_else(|| fields.iter().find_map(|(_, field)| inspect(field)))
                .or_else(|| self.aggregate_borrow_source(base, callables, live))
                .or_else(|| {
                    fields.iter().find_map(|(_, field)| {
                        self.aggregate_borrow_source(field, callables, live)
                    })
                }),
            Expr::Call { args, .. } | Expr::Apply { args, .. } => args
                .iter()
                .find_map(|arg| self.aggregate_borrow_source(arg, callables, live)),
            Expr::LabeledCall { args, .. } => args
                .iter()
                .find_map(|(_, arg)| self.aggregate_borrow_source(arg, callables, live)),
            Expr::MethodCall { receiver, args, .. } => self
                .aggregate_borrow_source(receiver, callables, live)
                .or_else(|| {
                    args.iter()
                        .find_map(|arg| self.aggregate_borrow_source(arg, callables, live))
                }),
            Expr::If { then_block, else_block, .. } => self
                .aggregate_borrow_source_in_block(then_block, callables, live)
                .or_else(|| {
                    else_block.as_ref().and_then(|block| {
                        self.aggregate_borrow_source_in_block(block, callables, live)
                    })
                }),
            Expr::Match { arms, .. } => arms
                .iter()
                .find_map(|arm| self.aggregate_borrow_source(&arm.body, callables, live)),
            Expr::Block(block) => self.aggregate_borrow_source_in_block(block, callables, live),
            Expr::As { expr, .. } => self.aggregate_borrow_source(expr, callables, live),
            _ => None,
        }
    }

    fn aggregate_borrow_source_in_block(
        &self,
        block: &Block,
        callables: &HashMap<String, BorrowSig>,
        live: &[Loan],
    ) -> Option<BorrowSource> {
        block_tail(block)
            .and_then(|tail| self.aggregate_borrow_source(tail, callables, live))
    }

    fn callable_expr_sig(
        &self,
        expr: &Expr,
        callables: &HashMap<String, BorrowSig>,
    ) -> Option<(String, BorrowSig)> {
        match expr {
            Expr::Var(name) => self
                .sigs
                .get(name)
                .or_else(|| callables.get(name))
                .cloned()
                .map(|sig| (name.clone(), sig)),
            Expr::Call { name, .. } => self
                .sigs
                .get(name)
                .or_else(|| callables.get(name))
                .and_then(|sig| sig.callable_return.as_deref())
                .cloned()
                .map(|sig| (name.clone(), sig)),
            Expr::Apply { func, .. } => self
                .callable_expr_sig(func, callables)
                .and_then(|(name, sig)| {
                    sig.callable_return
                        .as_deref()
                        .cloned()
                        .map(|returned| (name, returned))
                }),
            Expr::As { ty, .. } => {
                borrow_sig_from_fn_type(ty).map(|sig| ("indirect function".into(), sig))
            }
            Expr::Lambda { params, body, ret } => ret
                .as_ref()
                .and_then(|ret| {
                    let conventions = params.iter().map(|param| param.convention).collect();
                    let params: Vec<Type> = params
                        .iter()
                        .map(|param| {
                            param.ty.clone().unwrap_or_else(|| Type::Named("a".into(), vec![]))
                        })
                        .collect();
                    borrow_sig_from_fn_type(&Type::Fn(
                        params,
                        Box::new(ret.clone()),
                        conventions,
                    ))
                })
                .or_else(|| forwarding_lambda_sig(params, body, self.sigs))
                .map(|sig| ("closure".into(), sig)),
            _ => None,
        }
    }

    fn callable_value_sig(
        &self,
        value: &Expr,
        declared: Option<&Type>,
        callables: &HashMap<String, BorrowSig>,
    ) -> Option<BorrowSig> {
        declared
            .and_then(borrow_sig_from_fn_type)
            .or_else(|| self.callable_expr_sig(value, callables).map(|(_, sig)| sig))
    }

    fn reject_callable_erasure(
        &self,
        binding: &str,
        value: &Expr,
        declared: Option<&Type>,
        existing: Option<&BorrowSig>,
        callables: &HashMap<String, BorrowSig>,
    ) -> Result<(), TypeError> {
        let source = self.callable_expr_sig(value, callables).map(|(_, sig)| sig);
        let expected = declared.and_then(borrow_sig_from_fn_type).or_else(|| existing.cloned());
        let (Some(source), Some(expected)) = (source, expected) else {
            return Ok(());
        };
        self.require_same_callable(&format!("function value `{binding}`"), &source, &expected)
    }

    fn same_callable_contract(left: &BorrowSig, right: &BorrowSig) -> bool {
        let relation = |sig: &BorrowSig| {
            sig.owner_params.iter().map(|(index, _)| *index).collect::<Vec<_>>()
        };
        left.returns_view == right.returns_view
            && relation(left) == relation(right)
            && left.conventions == right.conventions
            && left.callable_params.len() == right.callable_params.len()
            && left
                .callable_params
                .iter()
                .zip(&right.callable_params)
                .all(|(a, b)| match (a, b) {
                    (Some(a), Some(b)) => Self::same_callable_contract(a, b),
                    (None, None) => true,
                    _ => false,
                })
            && match (&left.callable_return, &right.callable_return) {
                (Some(a), Some(b)) => Self::same_callable_contract(a, b),
                (None, None) => true,
                _ => false,
            }
    }

    fn require_same_callable(
        &self,
        context: &str,
        source: &BorrowSig,
        expected: &BorrowSig,
    ) -> Result<(), TypeError> {
        if Self::same_callable_contract(source, expected) {
            return Ok(());
        }
        Err(terr(format!(
            "{context} erases or changes its borrow/convention relation — function types must \
             preserve whether the result borrows an input, the owning parameter positions, \
             nested callable relations, and every `let`/`var`/`own` convention"
        )))
    }

    fn reject_callable_boundaries(
        &self,
        stmt: &Stmt,
        callables: &HashMap<String, BorrowSig>,
    ) -> Result<(), TypeError> {
        let mut result = Ok(());
        walk_stmt_exprs(stmt, &mut |expr| {
            if result.is_err() {
                return;
            }
            match expr {
                Expr::As { expr: inner, ty } => {
                    if let (Some((_, source)), Some(expected)) = (
                        self.callable_expr_sig(inner, callables),
                        borrow_sig_from_fn_type(ty),
                    ) {
                        result = self.require_same_callable("function cast", &source, &expected);
                    }
                }
                Expr::Call { name, args } => {
                    if let Some(sig) = self.sigs.get(name).or_else(|| callables.get(name)) {
                        result = self.check_callable_arguments(name, args, sig, callables);
                    }
                }
                Expr::Apply { func, args } => {
                    if let Some((name, sig)) = self.callable_expr_sig(func, callables) {
                        result = self.check_callable_arguments(&name, args, &sig, callables);
                    }
                }
                _ => {}
            }
        });
        result
    }

    fn check_callable_arguments(
        &self,
        callee: &str,
        args: &[Expr],
        signature: &BorrowSig,
        callables: &HashMap<String, BorrowSig>,
    ) -> Result<(), TypeError> {
        for (index, (arg, expected)) in args.iter().zip(&signature.callable_params).enumerate() {
            let Some(expected) = expected else { continue };
            if let Some((_, source)) = self.callable_expr_sig(arg, callables) {
                self.require_same_callable(
                    &format!("argument {} passed to `{}`", index + 1, short_name(callee)),
                    &source,
                    expected,
                )?;
            }
        }
        Ok(())
    }

    /// Reject a statement that moves, mutates, reassigns, or lets escape the owner
    /// of any live loan.
    fn reject_conflicts(
        &self,
        stmt: &Stmt,
        open: &[Loan],
        callables: &HashMap<String, BorrowSig>,
    ) -> Result<(), TypeError> {
        if let Some(source) = self.escape_call_source(stmt, callables, open) {
            return Err(terr(format!(
                "in `{}`: the borrowed result from `{}` escapes through a task or channel — \
                 materialize it with `.owned()` before sending or spawning it",
                short_name(self.fn_name),
                short_name(&source.origin),
            )));
        }
        if matches!(stmt, Stmt::Break | Stmt::Continue)
            && let Some(loan) = open.first()
        {
            let edge = if matches!(stmt, Stmt::Break) { "break" } else { "continue" };
            return Err(terr(format!(
                "in `{}`: `{edge}` would leave the borrowed view `{}` (from `{}`) live \
                 across a loop control-flow edge while it borrows `{}` — finish using the \
                 view before the edge, or materialize it with `{}.owned()`",
                short_name(self.fn_name),
                loan.view,
                short_name(&loan.origin),
                loan.owner,
                loan.view,
            )));
        }
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
        self.reject_owner_transfer(stmt, open, callables)?;
        Ok(())
    }

    fn escape_call_source(
        &self,
        stmt: &Stmt,
        callables: &HashMap<String, BorrowSig>,
        live: &[Loan],
    ) -> Option<BorrowSource> {
        let mut found = None;
        walk_stmt_exprs(stmt, &mut |expr| {
            if found.is_some() {
                return;
            }
            let Expr::Call { name, args } = expr else { return };
            if !matches!(short_name(name), "send" | "spawn") {
                return;
            }
            for arg in args {
                let mut sources = self.borrow_sources(arg, callables, live);
                self.collect_alias_sources(arg, live, &mut sources);
                if let Some(source) = sources.into_iter().next() {
                    found = Some(source);
                    break;
                }
                if let Some(source) = self.aggregate_borrow_source(arg, callables, live) {
                    found = Some(source);
                    break;
                }
            }
        });
        found
    }

    /// Reject `move owner` and passing `owner` to a `var`/`own` parameter, walked
    /// over every expression in the statement.
    fn reject_owner_transfer(
        &self,
        stmt: &Stmt,
        open: &[Loan],
        callables: &HashMap<String, BorrowSig>,
    ) -> Result<(), TypeError> {
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
                if let Some(convs) = self.owner_conventions(callee, callables) {
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
            if let Expr::Apply { func, args } = e
                && let Some((callee, sig)) = self.callable_expr_sig(func, callables)
            {
                for (arg, conv) in args.iter().zip(&sig.conventions) {
                    if !conv.binds_mutable() {
                        continue;
                    }
                    if let Some(root) = expr_root(arg)
                        && let Some(loan) = open.iter().find(|loan| loan.owner == root)
                    {
                        let kind = if *conv == Convention::Var { "`var`" } else { "`own`" };
                        result = Err(self.conflict(
                            loan,
                            &format!("passed to a {kind} parameter of `{callee}`"),
                        ));
                    }
                }
            }
        });
        result
    }

    /// Recurse into every block nested in this statement's expressions, carrying
    /// the loans live at this point so an owner conflict inside a nested block is
    /// caught and the nested block may open (and end) its own loans.
    fn check_nested_blocks(
        &mut self,
        stmt: &Stmt,
        open: &[Loan],
        callables: &HashMap<String, BorrowSig>,
    ) -> Result<(), TypeError> {
        let mut nested: Vec<&Block> = Vec::new();
        collect_nested_blocks_in_stmt(stmt, &mut nested);
        for b in nested {
            self.check_block_with(b, open, callables, false)?;
        }
        Ok(())
    }

    /// The parameter conventions of a callee, if known.
    fn owner_conventions<'b>(
        &'b self,
        callee: &str,
        callables: &'b HashMap<String, BorrowSig>,
    ) -> Option<&'b [Convention]> {
        self.sigs
            .get(callee)
            .or_else(|| callables.get(callee))
            .map(|sig| sig.conventions.as_slice())
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
             channel, or is stored in an owned aggregate/mutable binding, while it still \
             borrows `{}` — a view cannot outlive its owner. \
             Materialize it with `{}.owned()` first to send an owned value",
            short_name(self.fn_name),
            loan.view,
            short_name(&loan.origin),
            loan.owner,
            loan.view,
        ))
    }
}

impl From<Loan> for LoanEvent {
    fn from(loan: Loan) -> Self {
        Self {
            view: loan.view,
            owner: loan.owner,
            origin: loan.origin,
            owner_type: loan.owner_type,
        }
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
    match stmt {
        Stmt::Assign { value, .. }
            if expr_mentions_var(value, view) && !expr_materializes_view(value, view) =>
        {
            return true;
        }
        Stmt::Yield(value) if expr_mentions_var(value, view) => return true,
        _ => {}
    }
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
            // An owned aggregate would let the view outlive this local loan.
            // Temporary or not, requiring `.owned()` keeps one uniform rule and
            // avoids smuggling a view through a tuple/record/list constructor.
            Expr::List(items) | Expr::Tuple(items) => {
                if items.iter().any(|item| expr_result_is_var(item, view)) {
                    escapes = true;
                }
            }
            Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
                if args.iter().any(|arg| expr_result_is_var(arg, view)) {
                    escapes = true;
                }
            }
            Expr::Record { fields, spread, .. } => {
                if fields.iter().any(|(_, value)| expr_result_is_var(value, view))
                    || spread.as_ref().is_some_and(|value| expr_result_is_var(value, view))
                {
                    escapes = true;
                }
            }
            Expr::RecordUpdate { base, fields, .. } => {
                if expr_result_is_var(base, view)
                    || fields.iter().any(|(_, value)| expr_result_is_var(value, view))
                {
                    escapes = true;
                }
            }
            _ => {}
        }
    });
    escapes
}

fn expr_materializes_view(expr: &Expr, view: &str) -> bool {
    let is_owned = |name: &str| short_name(name) == "owned" || name.ends_with("__owned");
    match expr {
        Expr::MethodCall { receiver, method, .. } => {
            is_owned(method) && expr_root(receiver) == Some(view)
        }
        Expr::Call { name, args } => {
            is_owned(name) && args.first().and_then(expr_root) == Some(view)
        }
        _ => false,
    }
}

fn projection_of_live_view(expr: &Expr, live: &[Loan]) -> bool {
    match expr {
        Expr::Field { base, .. } | Expr::Index { base, .. } => {
            expr_root(base).is_some_and(|root| live.iter().any(|loan| loan.view == root))
        }
        Expr::As { expr, .. } => projection_of_live_view(expr, live),
        _ => false,
    }
}

fn expr_result_is_var(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Var(var) => var == name,
        Expr::As { expr, .. } => expr_result_is_var(expr, name),
        Expr::If { then_block, else_block, .. } => {
            block_tail(then_block).is_some_and(|tail| expr_result_is_var(tail, name))
                || else_block
                    .as_ref()
                    .and_then(block_tail)
                    .is_some_and(|tail| expr_result_is_var(tail, name))
        }
        Expr::Match { arms, .. } => {
            arms.iter().any(|arm| expr_result_is_var(&arm.body, name))
        }
        Expr::Block(block) => {
            block_tail(block).is_some_and(|tail| expr_result_is_var(tail, name))
        }
        _ => false,
    }
}

fn expr_mentions_var(expr: &Expr, name: &str) -> bool {
    let mut found = false;
    walk_expr(expr, &mut |nested| {
        if matches!(nested, Expr::Var(var) if var == name) {
            found = true;
        }
    });
    found
}

fn block_mentions_var(block: &Block, name: &str) -> bool {
    block.stmts.iter().any(|s| stmt_mentions(s, name))
}

/// Collect the blocks nested directly in a statement's expressions — the bodies
/// of `if`/`while`/`for`/`while let`/bare-block and each `match` arm (an arm body
/// is an expression, wrapped in a one-statement block so it re-uses the block
/// path). Only the FIRST block level is collected; a block found here recurses
/// via `check_block_with`, which collects its own nested blocks in turn.
fn collect_nested_blocks_in_stmt<'a>(stmt: &'a Stmt, out: &mut Vec<&'a Block>) {
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
fn push_own_blocks<'a>(e: &'a Expr, out: &mut Vec<&'a Block>) {
    match e {
        Expr::If { then_block, else_block, .. } => {
            out.push(then_block);
            if let Some(b) = else_block {
                out.push(b);
            }
        }
        Expr::While { body, .. }
        | Expr::For { body, .. }
        | Expr::Block(body)
        | Expr::WhileLet { body, .. } => out.push(body),
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
        Expr::ExistentialCall { receiver, args, .. } => {
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
        Expr::Match { scrutinee, arms } => {
            stack.push(scrutinee);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    stack.push(guard);
                }
                stack.push(&arm.body);
            }
        }
        _ => {}
    }
}

/// Visit every expression in a statement (pre-order), including nested ones, so a
/// callback can inspect uses without each caller re-implementing the walk.
fn walk_stmt_exprs<'a>(stmt: &'a Stmt, f: &mut impl FnMut(&'a Expr)) {
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

fn walk_expr<'a>(e: &'a Expr, f: &mut impl FnMut(&'a Expr)) {
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
        Expr::ExistentialCall { receiver, args, .. } => {
            walk_expr(receiver, f);
            args.iter().for_each(|a| walk_expr(a, f));
        }
        Expr::Apply { func, args } => {
            walk_expr(func, f);
            args.iter().for_each(|a| walk_expr(a, f));
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => walk_expr(expr, f),
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

fn walk_block<'a>(b: &'a Block, f: &mut impl FnMut(&'a Expr)) {
    for s in &b.stmts {
        walk_stmt_exprs(s, f);
    }
}

#[cfg(test)]
#[path = "loans_tests.rs"]
mod tests;
