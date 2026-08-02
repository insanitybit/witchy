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

use witchy_syntax::ast::{
    Block, Convention, Expr, Function, Item, Module, Param, Pattern, Stmt, Type, TypeQual, UnOp,
};

pub use crate::access::{LoanProjection, LoanProjectionStep};
use crate::access::{AccessKind, AccessSignature, BorrowRelation, BorrowRelationCatalog};
use crate::typeck::TypeError;

fn terr(message: String) -> TypeError {
    TypeError { message }
}

/// The output-to-input borrow relation of one function, read off its signature.
#[derive(Clone)]
struct BorrowSig {
    /// Canonical callable identity. Inferred legacy declarations without a
    /// finalized AST signature retain `None`; every typed callable path uses
    /// this authority for exact projected-relation comparison.
    access: Option<AccessSignature>,
    /// `true` when the return type is a borrowed view.
    returns_view: bool,
    /// Parameter indices whose borrow lifetime matches the returned view's
    /// lifetime — the owners a call's result loans. Empty when the return is not
    /// a view (or, after signature validation, never empty when it is).
    owner_params: Vec<(usize, Type)>,
    /// Exact output-slot to input-slot relations for fixed borrowed values.
    relations: Vec<BorrowRelation>,
    conventions: Vec<Convention>,
    callable_params: Vec<Option<Box<BorrowSig>>>,
    callable_return: Option<Box<BorrowSig>>,
}

#[derive(Clone)]
struct ReturnBorrowRelation {
    output_projection: LoanProjection,
    owners: Vec<ReturnOwnerPosition>,
}

#[derive(Clone)]
struct ReturnOwnerPosition {
    name: String,
    input_projection: LoanProjection,
}

/// The borrow qualifier's lifetime name on a parameter/return type, if any.
fn view_lifetime(ty: &Type) -> Option<&str> {
    match ty {
        Type::Qualified(TypeQual::Borrow(life), _) => Some(life),
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
    /// The statically known region of `owner` reached by this view. An empty
    /// projection borrows the whole owner. Lowering must retain `owner`; this
    /// descriptor is never an owning RC base.
    pub projection: LoanProjection,
    /// The fixed field/tuple slot of `view` whose reads depend on this owner.
    /// Empty means the complete borrowed value depends on the owner.
    pub borrower_projection: LoanProjection,
    pub origin: String,
    pub owner_type: Type,
}

fn named_return_relations(sig: &BorrowSig, params: &[Param]) -> Vec<ReturnBorrowRelation> {
    sig.relations
        .iter()
        .map(|relation| ReturnBorrowRelation {
            output_projection: relation.output_projection().clone(),
            owners: relation
                .owners()
                .iter()
                .filter_map(|owner| {
                    params.get(owner.position()).map(|param| ReturnOwnerPosition {
                        name: param.name.clone(),
                        input_projection: owner.input_projection().clone(),
                    })
                })
                .collect(),
        })
        .collect()
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
    let catalog = BorrowRelationCatalog::from_module(module);
    let mut sigs: HashMap<String, BorrowSig> = HashMap::new();

    // Pass 1: validate signatures and record each function's borrow relation.
    for item in &module.items {
        let Item::Function(f) = item else { continue };
        let sig = validate_signature(f, is_opt_function(&f.name, &module.modes), &catalog)?;
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
            catalog: &catalog,
            return_relations: sigs
                .get(&f.name)
                .map(|sig| named_return_relations(sig, &f.params))
                .unwrap_or_default(),
            block_results: HashMap::new(),
            input_borrows: f
                .params
                .iter()
                .filter_map(|param| {
                    let ty = param.ty.as_ref()?;
                    let slots = catalog.slots(ty);
                    if slots.is_empty() {
                        return None;
                    }
                    Some((
                        param.name.clone(),
                        slots
                            .into_iter()
                            .map(|slot| BorrowSource {
                                owner: param.name.clone(),
                                projection: slot.projection.clone(),
                                borrower_projection: slot.projection,
                                origin: f.name.clone(),
                                owner_type: slot.storage_type,
                                temporary: false,
                            })
                            .collect(),
                    ))
                })
                .collect(),
            return_callable: f
                .ret
                .as_ref()
                .and_then(|ty| borrow_sig_from_fn_type(ty, &catalog))
                .map(Box::new),
        };
        let callable_params: HashMap<String, BorrowSig> = f
            .params
            .iter()
            .filter_map(|param| {
                borrow_sig_from_fn_type(param.ty.as_ref()?, &catalog)
                    .map(|sig| (param.name.clone(), sig))
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
                LoanEnvironment { sigs: &sigs, catalog: &catalog },
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
    environment: LoanEnvironment<'_>,
    facts: &mut LoanFacts,
) -> Result<(), TypeError> {
    let LoanEnvironment { sigs, catalog } = environment;
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
            borrow_sig_from_fn_type(param.ty.as_ref()?, catalog)
                .map(|sig| (param.name.clone(), sig))
        })
        .collect();
    let input_borrows = params
        .iter()
        .filter_map(|param| {
            let slots = catalog.slots(param.ty.as_ref()?);
            if slots.is_empty() {
                return None;
            }
            Some((
                param.name.clone(),
                slots
                    .into_iter()
                    .map(|slot| BorrowSource {
                        owner: param.name.clone(),
                        projection: slot.projection.clone(),
                        borrower_projection: slot.projection,
                        origin: name.to_string(),
                        owner_type: slot.storage_type,
                        temporary: false,
                    })
                    .collect(),
            ))
        })
        .collect();
    let mut ctx = LoanCtx {
        sigs,
        fn_name: name,
        facts,
        catalog,
        return_relations: if let Some(lifetime) = ret_life {
            vec![ReturnBorrowRelation {
                output_projection: LoanProjection::default(),
                owners: params
                    .iter()
                    .filter(|param| {
                        param
                            .ty
                            .as_ref()
                            .and_then(view_lifetime)
                            .is_some_and(|input| input == lifetime)
                    })
                    .map(|param| ReturnOwnerPosition {
                        name: param.name.clone(),
                        input_projection: LoanProjection::default(),
                    })
                    .collect(),
            }]
        } else {
            forwarded.map(|sig| named_return_relations(sig, params)).unwrap_or_default()
        },
        block_results: HashMap::new(),
        input_borrows,
        return_callable: ret
            .and_then(|ty| borrow_sig_from_fn_type(ty, catalog))
            .map(Box::new),
    };
    ctx.check_block_with(body, &[], &callable_params, true)
}

#[derive(Clone, Copy)]
struct LoanEnvironment<'a> {
    sigs: &'a HashMap<String, BorrowSig>,
    catalog: &'a BorrowRelationCatalog,
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
fn validate_signature(
    f: &Function,
    opt: bool,
    catalog: &BorrowRelationCatalog,
) -> Result<BorrowSig, TypeError> {
    // Input lifetimes declared by direct views or fixed borrowed aggregate
    // slots. The canonical access signature below records their exact places;
    // this set is retained only for the established source diagnostic.
    let mut input_lifetimes = Vec::new();
    let mut uses_view = false;
    for p in &f.params {
        if let Some(ty) = &p.ty {
            validate_nested_fn_borrows(ty, &f.name)?;
            let slots = catalog.slots(ty);
            uses_view |= type_mentions_view(ty) || !slots.is_empty();
            for slot in slots {
                if !input_lifetimes.contains(&slot.lifetime) {
                    input_lifetimes.push(slot.lifetime);
                }
            }
        }
    }
    let return_slots = f.ret.as_ref().map(|ret| catalog.slots(ret)).unwrap_or_default();
    if let Some(ret) = &f.ret {
        validate_nested_fn_borrows(ret, &f.name)?;
        uses_view |= type_mentions_view(ret) || !return_slots.is_empty();
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
        if p.ty.as_ref().is_some_and(|ty| !catalog.slots(ty).is_empty())
            && p.convention.binds_mutable()
        {
            return Err(terr(format!(
                "parameter `{}` of `{}` is a borrowed view (read-only) but its convention \
                 is mutable (`var`/`own`) — a view cannot be mutated or consumed",
                p.name,
                short_name(&f.name)
            )));
        }
    }

    for slot in return_slots {
        if !input_lifetimes.contains(&slot.lifetime) {
            return Err(terr(format!(
                "`{}` returns borrowed storage with lifetime `'{}`, but no parameter borrows \
                 with that lifetime — an output borrow must come from an input owner",
                short_name(&f.name),
                slot.lifetime,
            )));
        }
    }

    let params = f
        .params
        .iter()
        .map(|parameter| parameter.ty.clone())
        .collect::<Option<Vec<_>>>();
    if let (Some(params), Some(result)) = (params, f.ret.clone()) {
        let signature = AccessSignature::from_parts_with_catalog(
            params,
            result,
            f.params.iter().map(|parameter| parameter.convention).collect(),
            catalog,
        )
        .map_err(|error| terr(format!("access signature for `{}` is invalid: {error}", f.name)))?;
        return Ok(borrow_sig_from_access(signature, catalog));
    }

    Ok(BorrowSig {
        access: None,
        returns_view: false,
        owner_params: Vec::new(),
        relations: Vec::new(),
        conventions: f.params.iter().map(|param| param.convention).collect(),
        callable_params: f
            .params
            .iter()
            .map(|param| {
                param
                    .ty
                    .as_ref()
                    .and_then(|ty| borrow_sig_from_fn_type(ty, catalog))
                    .map(Box::new)
            })
            .collect(),
        callable_return: f
            .ret
            .as_ref()
            .and_then(|ty| borrow_sig_from_fn_type(ty, catalog))
            .map(Box::new),
    })
}

fn borrow_sig_from_access(
    signature: AccessSignature,
    catalog: &BorrowRelationCatalog,
) -> BorrowSig {
    let relations = signature.borrow_relations().to_vec();
    let mut owner_params = Vec::new();
    for owner in relations.iter().flat_map(|relation| relation.owners()) {
        if owner_params
            .iter()
            .any(|(position, _)| *position == owner.position())
        {
            continue;
        }
        if let Some(parameter) = signature.params().get(owner.position()) {
            owner_params.push((owner.position(), parameter.ty().clone()));
        }
    }
    BorrowSig {
        access: Some(signature.clone()),
        returns_view: !relations.is_empty(),
        owner_params,
        relations,
        conventions: signature
            .params()
            .iter()
            .map(|parameter| match parameter.kind() {
                AccessKind::OwnedImmutable => Convention::Let,
                AccessKind::SharedBorrow => Convention::Borrow,
                AccessKind::ExclusiveWriteback => Convention::Var,
                AccessKind::Consuming => Convention::Own,
            })
            .collect(),
        callable_params: signature
            .params()
            .iter()
            .map(|parameter| {
                borrow_sig_from_fn_type(parameter.ty(), catalog).map(Box::new)
            })
            .collect(),
        callable_return: borrow_sig_from_fn_type(signature.result().ty(), catalog)
            .map(Box::new),
    }
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

fn borrow_sig_from_fn_type(
    ty: &Type,
    catalog: &BorrowRelationCatalog,
) -> Option<BorrowSig> {
    let signature = AccessSignature::from_function_type_with_catalog(ty, catalog).ok()?;
    Some(borrow_sig_from_access(signature, catalog))
}

/// A single open loan: a view binding that borrows an owner local.
#[derive(Clone, Debug, PartialEq)]
struct Loan {
    /// The local variable that received the borrowed result (the view).
    view: String,
    /// The owner local whose storage the view borrows.
    owner: String,
    /// The owner-relative storage region borrowed by this view.
    projection: LoanProjection,
    /// The part of an aggregate view whose use depends on this owner. Empty for
    /// an ordinary direct view; fixed aggregates use field/tuple paths.
    borrower_projection: LoanProjection,
    /// Callee whose return type created this loan (for diagnostics).
    origin: String,
    owner_type: Type,
}

/// One owner borrowed by a `let` right-hand side, with the borrowing callee.
#[derive(Clone)]
struct BorrowSource {
    owner: String,
    projection: LoanProjection,
    borrower_projection: LoanProjection,
    origin: String,
    owner_type: Type,
    temporary: bool,
}

fn same_source(left: &BorrowSource, right: &BorrowSource) -> bool {
    left.owner == right.owner
        && left.projection == right.projection
        && left.borrower_projection == right.borrower_projection
        && left.origin == right.origin
}

fn strip_projection_prefix(
    projection: &LoanProjection,
    prefix: &LoanProjection,
) -> Option<LoanProjection> {
    if prefix.steps.len() > projection.steps.len()
        || !projection
            .steps
            .iter()
            .zip(&prefix.steps)
            .all(|(left, right)| projection_steps_equal(left, right))
    {
        return None;
    }
    Some(LoanProjection { steps: projection.steps[prefix.steps.len()..].to_vec() })
}

/// Restrict one aggregate owner contribution to `requested`, which is relative
/// to the borrower. Projecting inside an ordinary view composes the remainder
/// onto the owner path; projecting a fixed aggregate selects and re-roots only
/// the owner contributions beneath that field/tuple slot.
fn project_source(mut source: BorrowSource, requested: &LoanProjection) -> Option<BorrowSource> {
    if let Some(remainder) = strip_projection_prefix(requested, &source.borrower_projection) {
        source.projection = source.projection.extended(&remainder);
        source.borrower_projection = LoanProjection::default();
        return Some(source);
    }
    let remainder = strip_projection_prefix(&source.borrower_projection, requested)?;
    source.borrower_projection = remainder;
    Some(source)
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum PlaceProjection {
    Fixed(LoanProjection),
    Dynamic,
}

fn fixed_range(index: &Expr) -> Option<LoanProjectionStep> {
    let Expr::Range { lo, hi, inclusive } = index else { return None };
    let (Expr::Int(lo), Expr::Int(hi)) = (lo.as_ref(), hi.as_ref()) else {
        return None;
    };
    Some(LoanProjectionStep::Range { lo: *lo, hi: *hi, inclusive: *inclusive })
}

/// Extract a root and a checked projection. A dynamic index is retained as an
/// explicit failure state so callers never silently widen a persisted interior
/// view into a whole-owner fact.
fn expr_place(expr: &Expr) -> Option<(&str, PlaceProjection)> {
    fn walk<'a>(
        expr: &'a Expr,
        steps: &mut Vec<LoanProjectionStep>,
    ) -> Option<(&'a str, bool)> {
        match expr {
            Expr::Var(name) => Some((name, false)),
            Expr::Field { base, field } => {
                let (root, dynamic) = walk(base, steps)?;
                steps.push(LoanProjectionStep::Field(field.clone()));
                Some((root, dynamic))
            }
            Expr::Index { base, index } => {
                let (root, mut dynamic) = walk(base, steps)?;
                match index.as_ref() {
                    Expr::Int(value) => steps.push(LoanProjectionStep::Index(*value)),
                    range @ Expr::Range { .. } => {
                        if let Some(range) = fixed_range(range) {
                            steps.push(range);
                        } else {
                            dynamic = true;
                        }
                    }
                    _ => dynamic = true,
                }
                Some((root, dynamic))
            }
            Expr::As { expr, .. } => walk(expr, steps),
            _ => None,
        }
    }

    let mut steps = Vec::new();
    let (root, dynamic) = walk(expr, &mut steps)?;
    let projection = if dynamic {
        PlaceProjection::Dynamic
    } else {
        PlaceProjection::Fixed(LoanProjection { steps })
    };
    Some((root, projection))
}

fn projection_steps_equal(left: &LoanProjectionStep, right: &LoanProjectionStep) -> bool {
    left == right
        || match (left, right) {
            (LoanProjectionStep::Tuple(left), LoanProjectionStep::Index(right)) => {
                *left as i64 == *right
            }
            (LoanProjectionStep::Index(left), LoanProjectionStep::Tuple(right)) => {
                *left == *right as i64
            }
            _ => false,
        }
}

#[cfg(test)]
fn fixed_interval(step: &LoanProjectionStep) -> Option<(i128, i128)> {
    match step {
        LoanProjectionStep::Index(value) => {
            let lo = i128::from(*value);
            Some((lo, lo + 1))
        }
        LoanProjectionStep::Tuple(value) => {
            let lo = *value as i128;
            Some((lo, lo + 1))
        }
        LoanProjectionStep::Range { lo, hi, inclusive } => {
            let lo = i128::from(*lo);
            let mut hi = i128::from(*hi);
            if *inclusive {
                hi += 1;
            }
            Some((lo, hi.max(lo)))
        }
        LoanProjectionStep::Field(_) => None,
    }
}

#[cfg(test)]
fn projection_steps_overlap(left: &LoanProjectionStep, right: &LoanProjectionStep) -> bool {
    let left_interval = fixed_interval(left);
    let right_interval = fixed_interval(right);
    if left_interval.is_some_and(|(lo, hi)| lo >= hi)
        || right_interval.is_some_and(|(lo, hi)| lo >= hi)
    {
        return false;
    }
    if projection_steps_equal(left, right) {
        return true;
    }
    match (left, right) {
        (LoanProjectionStep::Field(left), LoanProjectionStep::Field(right)) => left == right,
        _ => match (left_interval, right_interval) {
            (Some((left_lo, left_hi)), Some((right_lo, right_hi))) => {
                left_lo < right_hi && right_lo < left_hi
            }
            _ => true,
        },
    }
}

#[cfg(test)]
fn projections_overlap(left: &LoanProjection, right: &LoanProjection) -> bool {
    for (left, right) in left.steps.iter().zip(&right.steps) {
        if !projection_steps_overlap(left, right) {
            return false;
        }
    }
    true
}

fn projection_display(projection: &LoanProjection) -> String {
    if projection.steps.is_empty() {
        return "<root>".to_string();
    }
    let mut display = String::new();
    for step in &projection.steps {
        match step {
            LoanProjectionStep::Field(field) => {
                display.push('.');
                display.push_str(field);
            }
            LoanProjectionStep::Tuple(index) => {
                display.push('[');
                display.push_str(&index.to_string());
                display.push(']');
            }
            LoanProjectionStep::Index(index) => {
                display.push('[');
                display.push_str(&index.to_string());
                display.push(']');
            }
            LoanProjectionStep::Range { lo, hi, inclusive } => {
                display.push('[');
                display.push_str(&lo.to_string());
                display.push_str(if *inclusive { "..=" } else { ".." });
                display.push_str(&hi.to_string());
                display.push(']');
            }
        }
    }
    display
}

fn index_projection(index: &Expr) -> Option<LoanProjectionStep> {
    match index {
        Expr::Int(value) => Some(LoanProjectionStep::Index(*value)),
        range @ Expr::Range { .. } => fixed_range(range),
        _ => None,
    }
}

fn pattern_bindings(
    pattern: &Pattern,
    catalog: &BorrowRelationCatalog,
    projection: &LoanProjection,
    out: &mut Vec<(String, LoanProjection)>,
) {
    match pattern {
        Pattern::Var(name) if name != "_" => out.push((name.clone(), projection.clone())),
        Pattern::Tuple(items) => {
            for (index, item) in items.iter().enumerate() {
                pattern_bindings(
                    item,
                    catalog,
                    &projection.extended(&LoanProjection {
                        steps: vec![LoanProjectionStep::Tuple(index)],
                    }),
                    out,
                );
            }
        }
        Pattern::Ctor { name, args } if catalog.borrowed_constructor(name) => {
            for (index, arg) in args.iter().enumerate() {
                pattern_bindings(
                    arg,
                    catalog,
                    &projection.extended(&LoanProjection {
                        steps: vec![catalog.constructor_step(name, index)],
                    }),
                    out,
                );
            }
        }
        Pattern::Wildcard | Pattern::Var(_) => {}
        Pattern::Ctor { .. }
        | Pattern::AnonCtor { .. }
        | Pattern::List { .. }
        | Pattern::Or(_)
        | Pattern::Int(_)
        | Pattern::Str(_)
        | Pattern::Bool(_)
        | Pattern::Duration(_)
        | Pattern::IntRange { .. } => {}
    }
}

struct LoanCtx<'a> {
    sigs: &'a HashMap<String, BorrowSig>,
    catalog: &'a BorrowRelationCatalog,
    fn_name: &'a str,
    facts: &'a mut LoanFacts,
    /// Declared output-slot to named input-slot relations. Body checking keeps
    /// this shape intact so two lifetimes cannot be swapped merely because both
    /// owner names occur somewhere in the return type.
    return_relations: Vec<ReturnBorrowRelation>,
    /// Borrowed result provenance for already-checked nested blocks, keyed by
    /// exact block identity. This connects a block-local alias to an enclosing
    /// `if`/block result without re-running a second lifetime analysis.
    block_results: HashMap<usize, Vec<BorrowSource>>,
    /// Borrowed function parameters are provenance roots too. Recording all of
    /// them lets body checking reject returning a `'b` input under a declared
    /// `'a` result relation.
    input_borrows: HashMap<String, Vec<BorrowSource>>,
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
                if self.has_dynamic_borrow_projection(value, &callables, &live) {
                    return Err(self.dynamic_projection());
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
                        projection: owner.projection,
                        borrower_projection: owner.borrower_projection,
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
            } else if let Stmt::LetPattern { pattern, value } = stmt {
                if self.has_dynamic_borrow_projection(value, &callables, &live) {
                    return Err(self.dynamic_projection());
                }
                let mut sources = self.borrow_sources(value, &callables, &live);
                self.collect_alias_sources(value, &live, &mut sources);
                let mut bindings = Vec::new();
                pattern_bindings(pattern, self.catalog, &LoanProjection::default(), &mut bindings);
                for (name, projection) in bindings {
                    for source in sources
                        .iter()
                        .cloned()
                        .filter_map(|source| project_source(source, &projection))
                    {
                        if source.temporary {
                            return Err(self.temporary_owner(&source.origin));
                        }
                        let loan = Loan {
                            view: name.clone(),
                            owner: source.owner,
                            projection: source.projection,
                            borrower_projection: source.borrower_projection,
                            origin: source.origin,
                            owner_type: source.owner_type,
                        };
                        let event = LoanEvent::from(loan.clone());
                        self.facts
                            .opens_after
                            .entry(stmt_key(stmt))
                            .or_default()
                            .push(event.clone());
                        let close_idx = block.stmts[idx + 1..]
                            .iter()
                            .rposition(|statement| stmt_mentions(statement, &loan.view))
                            .map(|offset| idx + 1 + offset)
                            .unwrap_or(idx);
                        self.facts
                            .closes_after
                            .entry(stmt_key(&block.stmts[close_idx]))
                            .or_default()
                            .push(event);
                        local.push(loan);
                    }
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
                if let Some(sources) = self.input_borrows.get(name) {
                    for source in sources {
                        if !out.iter().any(|existing| same_source(existing, source)) {
                            out.push(source.clone());
                        }
                    }
                }
                for loan in live.iter().filter(|loan| loan.view == *name) {
                    if !out.iter().any(|source| {
                        source.owner == loan.owner
                            && source.projection == loan.projection
                            && source.borrower_projection == loan.borrower_projection
                            && source.origin == loan.origin
                    }) {
                        out.push(BorrowSource {
                            owner: loan.owner.clone(),
                            projection: loan.projection.clone(),
                            borrower_projection: loan.borrower_projection.clone(),
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
            Expr::Field { .. } | Expr::Index { .. } => {
                let Some((root, PlaceProjection::Fixed(requested))) = expr_place(value) else {
                    return;
                };
                let mut root_sources = Vec::new();
                self.collect_alias_sources(&Expr::Var(root.to_string()), live, &mut root_sources);
                for source in root_sources {
                    if let Some(projected) = project_source(source, &requested) {
                        self.push_source(projected, out);
                    }
                }
            }
            _ => {}
        }
    }

    fn validate_return_sources(&self, sources: &[BorrowSource]) -> Result<(), TypeError> {
        for source in sources {
            if source.temporary {
                return Err(self.temporary_owner(&source.origin));
            }
            let output_relations: Vec<&ReturnBorrowRelation> = self
                .return_relations
                .iter()
                .filter(|relation| relation.output_projection == source.borrower_projection)
                .collect();
            let relation_matches = output_relations.iter().any(|relation| {
                relation.owners.iter().any(|owner| {
                    owner.name == source.owner
                        && strip_projection_prefix(
                            &source.projection,
                            &owner.input_projection,
                        )
                        .is_some()
                })
            });
            if !relation_matches {
                let expected = output_relations
                    .iter()
                    .flat_map(|relation| &relation.owners)
                    .map(|owner| {
                        format!(
                            "owner `{}` projection `{}`",
                            owner.name,
                            projection_display(&owner.input_projection),
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" or ");
                let expected = if expected.is_empty() {
                    "no borrowed owner".to_string()
                } else {
                    expected
                };
                return Err(terr(format!(
                    "in `{}`: returned borrow at output projection `{}` comes from owner `{}` \
                     projection `{}` through `{}`, but that output declares {expected}; the \
                     function signature does not return a view tied to that input and output \
                     slot — preserve the declared lifetime relation, or materialize the value \
                     with `.owned()` before returning",
                    short_name(self.fn_name),
                    projection_display(&source.borrower_projection),
                    source.owner,
                    projection_display(&source.projection),
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
            Expr::Tuple(items) => {
                for (index, item) in items.iter().enumerate() {
                    self.collect_aggregate_slot(
                        item,
                        LoanProjectionStep::Tuple(index),
                        callables,
                        live,
                        out,
                    );
                }
            }
            Expr::Ctor { name, args } if self.catalog.borrowed_constructor(name) => {
                for (index, arg) in args.iter().enumerate() {
                    self.collect_aggregate_slot(
                        arg,
                        self.catalog.constructor_step(name, index),
                        callables,
                        live,
                        out,
                    );
                }
            }
            Expr::Record { name, fields, .. } if self.catalog.borrowed_record(name) => {
                for (field, value) in fields {
                    self.collect_aggregate_slot(
                        value,
                        LoanProjectionStep::Field(field.clone()),
                        callables,
                        live,
                        out,
                    );
                }
            }
            Expr::Field { base, field } => self.collect_projected_result(
                base,
                LoanProjectionStep::Field(field.clone()),
                callables,
                live,
                out,
            ),
            Expr::Index { base, index } => {
                if let Some(step) = index_projection(index) {
                    self.collect_projected_result(base, step, callables, live, out);
                }
            }
            Expr::As { expr, .. } => self.collect_view_owners(expr, callables, live, out),
            _ => {}
        }
    }

    fn collect_aggregate_slot(
        &self,
        value: &Expr,
        step: LoanProjectionStep,
        callables: &HashMap<String, BorrowSig>,
        live: &[Loan],
        out: &mut Vec<BorrowSource>,
    ) {
        let mut sources = self.borrow_sources(value, callables, live);
        self.collect_alias_sources(value, live, &mut sources);
        for mut source in sources {
            source.borrower_projection = source.borrower_projection.prefixed(step.clone());
            self.push_source(source, out);
        }
    }

    fn collect_projected_result(
        &self,
        base: &Expr,
        step: LoanProjectionStep,
        callables: &HashMap<String, BorrowSig>,
        live: &[Loan],
        out: &mut Vec<BorrowSource>,
    ) {
        let mut sources = self.borrow_sources(base, callables, live);
        self.collect_alias_sources(base, live, &mut sources);
        let requested = LoanProjection { steps: vec![step] };
        for source in sources {
            if let Some(source) = project_source(source, &requested) {
                self.push_source(source, out);
            }
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
                if !out.iter().any(|existing| same_source(existing, source)) {
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
        for relation in &sig.relations {
            for owner in relation.owners() {
                let Some(arg) = args.get(owner.position()) else { continue };
                let mut sources = Vec::new();
                self.collect_alias_sources(arg, live, &mut sources);
                if sources.is_empty() {
                    if let Some((root, PlaceProjection::Fixed(argument_projection))) =
                        expr_place(arg)
                    {
                        sources.push(BorrowSource {
                            owner: root.to_string(),
                            projection: argument_projection.extended(owner.input_projection()),
                            borrower_projection: LoanProjection::default(),
                            origin: callee.to_string(),
                            owner_type: relation.storage_type().clone(),
                            temporary: false,
                        });
                    } else {
                        self.collect_view_owners(arg, callables, live, &mut sources);
                        sources = sources
                            .into_iter()
                            .filter_map(|source| {
                                project_source(source, owner.input_projection())
                            })
                            .collect();
                    }
                } else {
                    sources = sources
                        .into_iter()
                        .filter_map(|source| project_source(source, owner.input_projection()))
                        .collect();
                }
                if sources.is_empty() {
                    sources.push(BorrowSource {
                        owner: String::new(),
                        projection: LoanProjection::default(),
                        borrower_projection: relation.output_projection().clone(),
                        origin: callee.to_string(),
                        owner_type: relation.storage_type().clone(),
                        temporary: true,
                    });
                }
                for mut source in sources {
                    source.borrower_projection = relation.output_projection().clone();
                    source.origin = callee.to_string();
                    source.owner_type = relation.storage_type().clone();
                    self.push_source(source, out);
                }
            }
        }
    }

    fn push_source(&self, source: BorrowSource, out: &mut Vec<BorrowSource>) {
        if !out.iter().any(|existing| same_source(existing, &source)) {
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

    fn dynamic_projection(&self) -> TypeError {
        terr(format!(
            "in `{}`: a borrowed projection with a dynamic index cannot be persisted — \
             use a fixed field/index/range, shorten the view to this expression, or \
             materialize it with `.owned()` first",
            short_name(self.fn_name),
        ))
    }

    fn has_dynamic_borrow_projection(
        &self,
        value: &Expr,
        callables: &HashMap<String, BorrowSig>,
        live: &[Loan],
    ) -> bool {
        let mut dynamic = false;
        walk_expr(value, &mut |expr| {
            if dynamic {
                return;
            }
            match expr {
                Expr::Field { .. } | Expr::Index { .. } => {
                    let Some((root, projection)) = expr_place(expr) else { return };
                    if matches!(projection, PlaceProjection::Dynamic)
                        && (self.input_borrows.contains_key(root)
                            || live.iter().any(|loan| loan.view == root))
                    {
                        dynamic = true;
                    }
                }
                Expr::Call { name, args } => {
                    let Some(sig) = self.sigs.get(name).or_else(|| callables.get(name)) else {
                        return;
                    };
                    if sig.returns_view
                        && sig.owner_params.iter().any(|(index, _)| {
                            args.get(*index).is_some_and(|arg| {
                                matches!(expr_place(arg), Some((_, PlaceProjection::Dynamic)))
                            })
                        })
                    {
                        dynamic = true;
                    }
                }
                _ => {}
            }
        });
        dynamic
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
            Expr::List(items) => items
                .iter()
                .find_map(&mut inspect)
                .or_else(|| items.iter().find_map(|item| self.aggregate_borrow_source(item, callables, live))),
            Expr::Tuple(items) => items
                .iter()
                .find_map(|item| self.aggregate_borrow_source(item, callables, live)),
            Expr::Ctor { name, args } if self.catalog.borrowed_constructor(name) => args
                .iter()
                .find_map(|arg| self.aggregate_borrow_source(arg, callables, live)),
            Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => args
                .iter()
                .find_map(&mut inspect)
                .or_else(|| args.iter().find_map(|arg| self.aggregate_borrow_source(arg, callables, live))),
            Expr::Record { name, fields, spread } if self.catalog.borrowed_record(name) => fields
                .iter()
                .find_map(|(_, field)| self.aggregate_borrow_source(field, callables, live))
                .or_else(|| {
                    spread
                        .as_deref()
                        .and_then(|base| self.aggregate_borrow_source(base, callables, live))
                }),
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
            // RFC-0082 Dynamic is an owned persistence boundary. A view may
            // cross it only after explicit `.owned()` materialization; otherwise
            // the erased runtime representation would outlive its checked root.
            Expr::Call { name, args } if name == "dynamic.dynamic" => args
                .iter()
                .find_map(&mut inspect)
                .or_else(|| {
                    args.iter().find_map(|arg| {
                        self.aggregate_borrow_source(arg, callables, live)
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
                borrow_sig_from_fn_type(ty, self.catalog)
                    .map(|sig| ("indirect function".into(), sig))
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
                    borrow_sig_from_fn_type(
                        &Type::Fn(params, Box::new(ret.clone()), conventions),
                        self.catalog,
                    )
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
            .and_then(|ty| borrow_sig_from_fn_type(ty, self.catalog))
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
        let expected = declared
            .and_then(|ty| borrow_sig_from_fn_type(ty, self.catalog))
            .or_else(|| existing.cloned());
        let (Some(source), Some(expected)) = (source, expected) else {
            return Ok(());
        };
        self.require_same_callable(&format!("function value `{binding}`"), &source, &expected)
    }

    fn same_callable_contract(left: &BorrowSig, right: &BorrowSig) -> bool {
        let legacy_top_level_matches = || {
            let owners = |sig: &BorrowSig| {
                sig.owner_params
                    .iter()
                    .map(|(index, _)| *index)
                    .collect::<Vec<_>>()
            };
            left.returns_view == right.returns_view
                && owners(left) == owners(right)
                && left.conventions == right.conventions
        };
        let top_level_matches = match (&left.access, &right.access) {
            (Some(left_access), Some(right_access)) => {
                left.conventions == right.conventions
                    && left_access.has_same_projected_borrow_relations(right_access)
            }
            _ => legacy_top_level_matches(),
        };
        top_level_matches
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
                        borrow_sig_from_fn_type(ty, self.catalog),
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
            if stmt_stores_view_in_dynamic(stmt, &loan.view) {
                return Err(terr(format!(
                    "in `{}`: borrowed view `{}` from `{}` cannot be stored in Dynamic — \
                     materialize it with `{}.owned()` before calling `dynamic.dynamic`",
                    short_name(self.fn_name),
                    loan.view,
                    short_name(&loan.origin),
                    loan.view,
                )));
            }
            // The view escaping through a closure/task/channel while its loan is
            // live requires materialization (the owner may not outlive the view's
            // new home). Detect the view captured by a lambda or sent/spawned.
            if stmt_lets_view_escape(stmt, &loan.view, self.catalog) {
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
                if let Some((root, _)) = expr_place(expr) {
                    if let Some(loan) = open.iter().find(|loan| loan.owner == root) {
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
                        if let Some((root, _)) = expr_place(arg) {
                            if let Some(loan) = open.iter().find(|loan| loan.owner == root) {
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
                    if let Some((root, _)) = expr_place(arg)
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
            projection: loan.projection,
            borrower_projection: loan.borrower_projection,
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
fn stmt_lets_view_escape(
    stmt: &Stmt,
    view: &str,
    catalog: &BorrowRelationCatalog,
) -> bool {
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
            Expr::List(items) => {
                if items.iter().any(|item| expr_result_is_var(item, view)) {
                    escapes = true;
                }
            }
            Expr::Ctor { name, .. } if catalog.borrowed_constructor(name) => {}
            Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
                if args.iter().any(|arg| expr_result_is_var(arg, view)) {
                    escapes = true;
                }
            }
            Expr::Record { name, .. } if catalog.borrowed_record(name) => {}
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

/// RFC-0082 Dynamic is an owned persistence boundary. A borrowed view may
/// reach it only after `.owned()` has ended the loan and produced ordinary data.
fn stmt_stores_view_in_dynamic(stmt: &Stmt, view: &str) -> bool {
    let mut stores = false;
    walk_stmt_exprs(stmt, &mut |expr| {
        let Expr::Call { name, args } = expr else { return };
        if (name == "dynamic.dynamic" || name.starts_with("dynamic.dynamic__"))
            && args.iter().any(|arg| expr_result_is_var(arg, view))
        {
            stores = true;
        }
    });
    stores
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
