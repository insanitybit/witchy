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

// foldhash (not SipHash): all keys are compiler-internal names/ids, never
// attacker-controlled — see witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};

use witchy_syntax::ast::{
    is_lifetime_param, BinOp, Block, Convention, Expr, Function, Item, Module, Param, Pattern,
    Stmt, Type, TypeQual, UnOp,
};
use witchy_syntax::intrinsics;

pub use crate::access::{BorrowKind, LoanProjection, LoanProjectionStep};
use crate::access::{AccessKind, AccessSignature, BorrowRelation, BorrowRelationCatalog};
use crate::typeck::{TypeError, TypeTable, ty_to_ast};

fn terr(message: String) -> TypeError {
    TypeError { message }
}

/// Explicit reference carriers remain executable values when stored in a
/// mutable aggregate. Legacy `View` shells still require an immutable binding
/// or explicit materialization; only the first-class `&`/`&mut` relation opts
/// into the place-carrier path.
fn type_contains_explicit_reference_relation(ty: &Type) -> bool {
    match ty {
        Type::Qualified(TypeQual::Borrow(_) | TypeQual::BorrowMut(_), _) => true,
        Type::Qualified(_, inner) => type_contains_explicit_reference_relation(inner),
        Type::Named(_, arguments) | Type::Tuple(arguments) | Type::Dyn(_, arguments) => arguments
            .iter()
            .any(type_contains_explicit_reference_relation),
        Type::RecordCompose { base, fields } => {
            type_contains_explicit_reference_relation(base)
                || fields
                    .iter()
                    .any(|(_, field)| type_contains_explicit_reference_relation(field))
        }
        Type::Fn(_, _, _) => false,
    }
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
    /// `true` when the result is a nominal value containing authenticated
    /// lifetime-linked fields. This remains available to the untyped loan pass,
    /// which validates the compiler-lowered AST after type checking.
    returns_borrowed_shell: bool,
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
        Type::Qualified(
            TypeQual::Borrow(life) | TypeQual::LegacyBorrow(life) | TypeQual::BorrowMut(life),
            _,
        ) => Some(life),
        Type::Qualified(_, inner) => view_lifetime(inner),
        _ => None,
    }
}

/// The direct reference capability written at a callable boundary. Nested
/// reference fields are handled by the catalog's slot relations; call arguments
/// need this direct check because checker `Ty` intentionally erases runtime
/// representation qualifiers.
fn direct_reference_kind(ty: &Type) -> Option<BorrowKind> {
    match ty {
        // `Borrow` still also represents the RFC-0083 `let('a)` surface while
        // the migration is live. Legacy calls remain source-compatible through
        // `LegacyBorrow`; the direct `&'a T` spelling is unambiguous and must
        // receive a reference handle, never an implicitly borrowed value.
        Type::Qualified(TypeQual::Borrow(_), _) => Some(BorrowKind::Shared),
        Type::Qualified(TypeQual::BorrowMut(_), _) => Some(BorrowKind::Exclusive),
        Type::Qualified(_, inner) => direct_reference_kind(inner),
        _ => None,
    }
}

const EXPLICIT_REFERENCE_ORIGIN: &str = "explicit borrow";

fn source_is_direct_reference(source: &BorrowSource) -> bool {
    source.origin == EXPLICIT_REFERENCE_ORIGIN
        || source.root_type.as_ref().and_then(direct_reference_kind).is_some()
}

fn is_opt_function(name: &str, modes: &[String]) -> bool {
    // Trait lowering can append several identity segments to the generated
    // function name (`main.Trait__main.Type__method`). The first segment is
    // still the linked source module; using the last dot would mistake the
    // generated trait/type path for a module and drop its opt marker.
    if let Some((module, _)) = name.split_once('.') {
        return modes.iter().any(|mode| mode == &format!("@opt:{module}"));
    }
    modes.iter().any(|mode| mode == "opt")
}

/// A short callable name for diagnostics: the last `.`-segment of the canonical
/// `module.fn` name.
fn short_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BorrowEscapeBoundary {
    ChannelSend,
    TaskSpawn,
}

/// Authenticate the compiler-owned operation that transfers a value into a
/// channel or another task. Linked calls carry their canonical `module.fn`
/// identity; a `__...` suffix can only be introduced by monomorphization because
/// source declarations containing `__` are reserved. The private channel bridge
/// is authenticated separately by the intrinsic catalog and its caller allowlist.
///
/// Do not fall back to the short name here: `server.send`, `http` methods, and a
/// user's local `send`/`spawn` helpers are ordinary calls, not escape boundaries.
fn authenticated_borrow_escape_boundary(name: &str) -> Option<BorrowEscapeBoundary> {
    if witchy_syntax::intrinsics::lookup(name)
        .is_some_and(|spec| spec.id == witchy_syntax::intrinsics::IntrinsicId::ChannelSend)
    {
        return Some(BorrowEscapeBoundary::ChannelSend);
    }

    let (module, identity) = name.rsplit_once('.')?;
    let is_identity = |canonical: &str| {
        identity == canonical
            || identity
                .strip_prefix(canonical)
                .is_some_and(|suffix| suffix.starts_with("__") && suffix.len() > 2)
    };
    match module {
        "chan" if is_identity("send") => Some(BorrowEscapeBoundary::ChannelSend),
        "chan" | "task" if is_identity("spawn") => Some(BorrowEscapeBoundary::TaskSpawn),
        _ => None,
    }
}

/// Whether a function belongs to the bundled standard library (the optimized
/// substrate, exempt from the `mode opt` gate exactly as the linker's import rule
/// exempts it).
fn is_std_fn(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(m, _)| witchy_syntax::linker::STD_MODULES.contains(&m))
}

/// The stable owner-object base that keeps borrowed storage alive.
///
/// `local` always names an owning root. An interior field, element, range, or
/// view address belongs in [`LoanPlace::projection`], never in this identity.
/// Lowering must retain/drop this base rather than reconstructing a root from a
/// borrowed shell or a projection bias.
#[derive(Clone, Debug, PartialEq)]
pub struct LoanOwnerRoot {
    pub local: String,
    /// Checked type of the owner local's own pointer representation. `None`
    /// means no exact root-layout proof was available to the caller publishing
    /// these facts; lowering must not infer one from projected storage.
    /// This is intentionally the root type (for example `Holder`), never the
    /// projected storage type (`Holder.values: Dict`), so an interior layout
    /// cannot impose its bias on the containing owner.
    pub direct_storage_type: Option<Type>,
}

/// One statically checked place within an owner object.
#[derive(Clone, Debug, PartialEq)]
pub struct LoanPlace {
    pub root: LoanOwnerRoot,
    pub projection: LoanProjection,
    /// The storage reached by `projection`, not the type of the owner object.
    pub storage_type: Type,
}

/// One logical field/range contribution carried by a hidden root companion.
#[derive(Clone, Debug, PartialEq)]
pub struct LoanRootContribution {
    /// The place within the owning object that the logical view reads.
    pub place: LoanPlace,
    /// The field/tuple path within the borrowed shell that reads `place`.
    pub borrower_projection: LoanProjection,
}

/// A compiler-owned root retained for a logical borrowed shell.
///
/// Companions are ordered by their first checked owner contribution. Multiple
/// logical fields that borrow the same owner base share one companion, while
/// retaining all of their projection relations in `contributions`.
#[derive(Clone, Debug, PartialEq)]
pub struct LoanRootCompanion {
    pub ordinal: usize,
    pub root: LoanOwnerRoot,
    pub contributions: Vec<LoanRootContribution>,
}

/// The runtime-independent shape of one borrowed value opened by a statement.
/// `shell` is the logical user value; `roots` are hidden compiler-owned
/// companions. The shell is never itself promoted to an owning root.
#[derive(Clone, Debug, PartialEq)]
pub struct BorrowedValueShape {
    pub shell: String,
    pub roots: Vec<LoanRootCompanion>,
}

/// A checked in-place update of a borrowed aggregate's logical shell.
///
/// A checked in-place update of a borrowed shell's hidden root set.
///
/// `roots_before` are live while the record update reads its base. `roots_after`
/// are the companions owned by the updated shell. A scalar update transports
/// the set unchanged; a declared borrowed-field replacement closes precisely
/// the retired field contributions and opens precisely the replacements after
/// the write-back. Publishing both sets prevents lowering from inferring roots
/// from the physical record representation.
#[derive(Clone, Debug, PartialEq)]
pub struct LoanShellMutation {
    pub shell: String,
    pub fields: Vec<String>,
    pub roots_before: Vec<LoanEvent>,
    pub roots_after: Vec<LoanEvent>,
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
    /// Shared loans permit overlapping reads; exclusive loans reserve the
    /// logical place and carry an affine mutable-reference capability.
    pub kind: BorrowKind,
    /// Type of the projected storage. This is deliberately not part of
    /// [`LoanOwnerRoot`]: a field's storage type cannot classify or bias the
    /// containing owner object's RC base.
    pub owner_type: Type,
    owner_root: LoanOwnerRoot,
}

impl LoanEvent {
    /// Construct one event from the checker-owned root/place split. Backend
    /// adapters use this boundary to preserve the root's own checked layout
    /// independently of the projected storage type.
    pub fn from_checked_place(
        view: String,
        place: LoanPlace,
        borrower_projection: LoanProjection,
        origin: String,
    ) -> Self {
        let owner = place.root.local.clone();
        Self {
            view,
            owner,
            projection: place.projection,
            borrower_projection,
            origin,
            kind: BorrowKind::Shared,
            owner_type: place.storage_type,
            owner_root: place.root,
        }
    }

    /// The only object base that may be retained or released for this event.
    pub fn owner_root(&self) -> LoanOwnerRoot {
        self.owner_root.clone()
    }

    /// The checked interior place. Its projection is descriptive and may not be
    /// used as an owning RC base.
    pub fn owner_place(&self) -> LoanPlace {
        LoanPlace {
            root: self.owner_root(),
            projection: self.projection.clone(),
            storage_type: self.owner_type.clone(),
        }
    }
}

/// The phase of one exact checked-statement control-flow point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LoanPointPhase {
    Entry,
    Completion,
}

/// A point in the checked loan control-flow graph. Region-owning statements use
/// a distinct completion point, so their entry cannot bypass an `if` arm or loop
/// body and result-binding loans cannot open before the region completes.
/// `None` as an edge destination is the enclosing function exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LoanPoint {
    statement: usize,
    pub phase: LoanPointPhase,
}

/// Why control can leave a statement on one checked edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoanEdgeKind {
    Fallthrough,
    BranchThen,
    BranchElse,
    MatchArm(usize),
    LoopEnter,
    LoopBack,
    LoopExit,
    Break,
    Continue,
    Return,
    Propagate,
}

/// Projection-aware loan transfer facts for one CFG edge.
///
/// `carries` remain live at the destination, `opens` originate at the source,
/// `closes` are released before taking the edge, and `transfers` move hidden
/// root companions into a returned borrowed shell. A propagation edge closes
/// local roots; it never silently transfers them to the error/none result.
#[derive(Clone, Debug, PartialEq)]
pub struct LoanEdgeFacts {
    pub from: LoanPoint,
    pub to: Option<LoanPoint>,
    pub kind: LoanEdgeKind,
    pub carries: Vec<LoanEvent>,
    pub opens: Vec<LoanEvent>,
    pub closes: Vec<LoanEvent>,
    pub transfers: Vec<LoanEvent>,
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
    return_transfers: HashMap<usize, Vec<LoanEvent>>,
    shell_mutations: HashMap<usize, LoanShellMutation>,
    edges: HashMap<LoanPoint, Vec<LoanEdgeFacts>>,
}

/// Stable aggregate counts from the checked loan graph.
///
/// These are intentionally facts, rather than timings: a corpus runner can
/// pair them with its own wall-clock and allocation measurements without
/// re-walking a source module or depending on lowerer internals. The current
/// graph has no origin-subset solver, so `subset_edges` remains zero until that
/// precision phase publishes real subset facts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoanFactTelemetry {
    pub active_points: usize,
    pub active_events: usize,
    pub opens: usize,
    pub closes: usize,
    pub return_transfers: usize,
    pub shell_mutations: usize,
    pub control_flow_edges: usize,
    pub subset_edges: usize,
}

fn stmt_key(stmt: &Stmt) -> usize {
    stmt as *const Stmt as usize
}

fn block_key(block: &Block) -> usize {
    block as *const Block as usize
}

impl LoanFacts {
    /// Count the authoritative events and control-flow edges retained for this
    /// checked module. No address-keyed identity escapes this summary.
    pub fn telemetry(&self) -> LoanFactTelemetry {
        LoanFactTelemetry {
            active_points: self.active.values().filter(|events| !events.is_empty()).count(),
            active_events: self.active.values().map(Vec::len).sum(),
            opens: self.opens_after.values().map(Vec::len).sum(),
            closes: self.closes_after.values().map(Vec::len).sum(),
            return_transfers: self.return_transfers.values().map(Vec::len).sum(),
            shell_mutations: self.shell_mutations.len(),
            control_flow_edges: self.edges.values().map(Vec::len).sum(),
            subset_edges: 0,
        }
    }

    pub fn point(&self, stmt: &Stmt) -> LoanPoint {
        LoanPoint { statement: stmt_key(stmt), phase: LoanPointPhase::Entry }
    }

    pub fn completion_point(&self, stmt: &Stmt) -> LoanPoint {
        LoanPoint { statement: stmt_key(stmt), phase: LoanPointPhase::Completion }
    }

    pub fn active_at(&self, stmt: &Stmt) -> &[LoanEvent] {
        self.active.get(&stmt_key(stmt)).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn opens_after(&self, stmt: &Stmt) -> &[LoanEvent] {
        self.opens_after.get(&stmt_key(stmt)).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn closes_after(&self, stmt: &Stmt) -> &[LoanEvent] {
        self.closes_after.get(&stmt_key(stmt)).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn shell_mutation_after(&self, stmt: &Stmt) -> Option<&LoanShellMutation> {
        self.shell_mutations.get(&stmt_key(stmt))
    }

    pub fn edges_from(&self, stmt: &Stmt) -> &[LoanEdgeFacts] {
        self.edges_from_point(self.point(stmt))
    }

    pub fn edges_from_completion(&self, stmt: &Stmt) -> &[LoanEdgeFacts] {
        self.edges_from_point(self.completion_point(stmt))
    }

    pub fn edges_from_point(&self, point: LoanPoint) -> &[LoanEdgeFacts] {
        self.edges.get(&point).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Logical shells and their ordered, distinct owner-root companions opened
    /// by this statement. Projection paths remain contributions beneath a root;
    /// they are never returned as roots themselves.
    pub fn borrowed_value_shapes_after(&self, stmt: &Stmt) -> Vec<BorrowedValueShape> {
        let mut shapes: Vec<BorrowedValueShape> = Vec::new();
        for event in self.opens_after(stmt) {
            let shape = if let Some(shape) = shapes.iter_mut().find(|shape| shape.shell == event.view)
            {
                shape
            } else {
                shapes.push(BorrowedValueShape { shell: event.view.clone(), roots: Vec::new() });
                shapes.last_mut().expect("the borrowed shape was just inserted")
            };
            let root = event.owner_root();
            let companion = if let Some(companion) = shape
                .roots
                .iter_mut()
                .find(|candidate| candidate.root.local == root.local)
            {
                companion
            } else {
                let ordinal = shape.roots.len();
                shape.roots.push(LoanRootCompanion {
                    ordinal,
                    root,
                    contributions: Vec::new(),
                });
                shape.roots.last_mut().expect("the root companion was just inserted")
            };
            let contribution = LoanRootContribution {
                place: event.owner_place(),
                borrower_projection: event.borrower_projection.clone(),
            };
            if !companion.contributions.contains(&contribution) {
                companion.contributions.push(contribution);
            }
        }
        shapes
    }

    /// Identity key for a statement that carries any lowering-relevant loan
    /// fact. Unknown/cloned statements return `None`; lowering compares the set
    /// consumed by each compile unit with the set collected from the checked AST.
    pub fn event_key(&self, stmt: &Stmt) -> Option<usize> {
        let key = stmt_key(stmt);
        (self.active.contains_key(&key)
            || self.opens_after.contains_key(&key)
            || self.closes_after.contains_key(&key)
            || self.shell_mutations.contains_key(&key))
            .then_some(key)
    }
}

#[derive(Clone, Copy)]
struct FlowTarget<'a> {
    point: LoanPoint,
    stmt: &'a Stmt,
}

#[derive(Clone, Copy)]
struct LoopTargets<'a> {
    header: FlowTarget<'a>,
    exit: FlowTarget<'a>,
}

enum ControlRegion<'a> {
    Branch {
        then_block: &'a Block,
        else_block: Option<&'a Block>,
    },
    Match(Vec<&'a Expr>),
    Loop(&'a Block),
    Block(&'a Block),
}

fn entry_point(stmt: &Stmt) -> LoanPoint {
    LoanPoint { statement: stmt_key(stmt), phase: LoanPointPhase::Entry }
}

fn completion_point(stmt: &Stmt) -> LoanPoint {
    LoanPoint { statement: stmt_key(stmt), phase: LoanPointPhase::Completion }
}

fn target_entry(stmt: &Stmt) -> FlowTarget<'_> {
    FlowTarget { point: entry_point(stmt), stmt }
}

fn target_completion(stmt: &Stmt) -> FlowTarget<'_> {
    FlowTarget { point: completion_point(stmt), stmt }
}

fn push_unique_event(events: &mut Vec<LoanEvent>, event: LoanEvent) {
    if !events.contains(&event) {
        events.push(event);
    }
}

fn events_before(facts: &LoanFacts, stmt: &Stmt) -> Vec<LoanEvent> {
    facts.active_at(stmt).to_vec()
}

fn events_after(facts: &LoanFacts, stmt: &Stmt) -> Vec<LoanEvent> {
    let mut events = facts.active_at(stmt).to_vec();
    for event in facts.opens_after(stmt) {
        push_unique_event(&mut events, event.clone());
    }
    events.retain(|event| !facts.closes_after(stmt).contains(event));
    events
}

fn target_events(facts: &LoanFacts, target: FlowTarget<'_>) -> Vec<LoanEvent> {
    match target.point.phase {
        LoanPointPhase::Entry | LoanPointPhase::Completion => events_before(facts, target.stmt),
    }
}

/// Record one exact edge. `completed` is the statement whose evaluation
/// finishes on this edge; its opens/closes therefore attach here. Region-entry
/// selection edges pass `None`, so a result loan cannot open before an arm/body
/// completes.
fn record_edge(
    facts: &mut LoanFacts,
    from: LoanPoint,
    source_stmt: &Stmt,
    to: FlowTarget<'_>,
    kind: LoanEdgeKind,
    completed: Option<&Stmt>,
) {
    let source = completed
        .map(|stmt| events_after(facts, stmt))
        .unwrap_or_else(|| events_before(facts, source_stmt));
    let destination = target_events(facts, to);
    let mut carries = Vec::new();
    let mut closes = completed
        .map(|stmt| facts.closes_after(stmt).to_vec())
        .unwrap_or_default();
    for event in &source {
        if destination.contains(event) {
            push_unique_event(&mut carries, event.clone());
        } else {
            push_unique_event(&mut closes, event.clone());
        }
    }
    let opens = completed
        .map(|stmt| facts.opens_after(stmt).to_vec())
        .unwrap_or_default();
    facts.edges.entry(from).or_default().push(LoanEdgeFacts {
        from,
        to: Some(to.point),
        kind,
        carries,
        opens,
        closes,
        transfers: Vec::new(),
    });
}

fn record_return_edge(facts: &mut LoanFacts, from: LoanPoint, stmt: &Stmt) {
    let candidates = events_before(facts, stmt);
    let transfers = facts
        .return_transfers
        .get(&stmt_key(stmt))
        .cloned()
        .unwrap_or_default();
    let mut closes: Vec<LoanEvent> = facts
        .closes_after(stmt)
        .iter()
        .filter(|event| !transfers.contains(event))
        .cloned()
        .collect();
    for event in candidates {
        if !transfers.contains(&event) {
            push_unique_event(&mut closes, event);
        }
    }
    let opens = facts.opens_after(stmt).to_vec();
    facts.edges.entry(from).or_default().push(LoanEdgeFacts {
        from,
        to: None,
        kind: LoanEdgeKind::Return,
        carries: Vec::new(),
        opens,
        closes,
        transfers,
    });
}

fn record_propagation_edge(facts: &mut LoanFacts, stmt: &Stmt) {
    let from = entry_point(stmt);
    let closes = events_before(facts, stmt);
    facts.edges.entry(from).or_default().push(LoanEdgeFacts {
        from,
        to: None,
        kind: LoanEdgeKind::Propagate,
        carries: Vec::new(),
        opens: Vec::new(),
        closes,
        transfers: Vec::new(),
    });
}

fn statement_has_try(stmt: &Stmt) -> bool {
    let mut stack = stmt_top_exprs(stmt);
    while let Some(expr) = stack.pop() {
        if matches!(expr, Expr::Try(_)) {
            return true;
        }
        // Child blocks and lambdas own separate CFGs. A `?` within either must
        // not manufacture a propagation edge in this enclosing statement.
        push_shallow_children(expr, &mut stack);
    }
    false
}

fn statement_value(stmt: &Stmt) -> Option<&Expr> {
    match stmt {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Return(Some(value))
        | Stmt::Yield(value)
        | Stmt::Expr(value) => Some(value),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => None,
    }
}

fn expr_contains_exclusive_borrow(expr: &Expr) -> bool {
    let mut found = false;
    walk_expr(expr, &mut |candidate| {
        found |= matches!(candidate, Expr::Unary { op: UnOp::BorrowMut, .. });
    });
    found
}

fn transparent_value(mut value: &Expr) -> &Expr {
    loop {
        match value {
            Expr::Unary { op: UnOp::Borrow | UnOp::Deref, expr } => value = expr,
            Expr::As { expr, .. } | Expr::Try(expr) => value = expr,
            _ => return value,
        }
    }
}

/// The control region whose completion is the statement's value. Buried
/// regions in ordinary operands do not own statement completion and are indexed
/// by their containing lowering phase rather than pretending to be alternatives
/// to the complete statement.
fn control_region(stmt: &Stmt) -> Option<ControlRegion<'_>> {
    match transparent_value(statement_value(stmt)?) {
        Expr::If { then_block, else_block, .. } => Some(ControlRegion::Branch {
            then_block,
            else_block: else_block.as_ref(),
        }),
        Expr::Match { arms, .. } => {
            Some(ControlRegion::Match(arms.iter().map(|arm| &arm.body).collect()))
        }
        Expr::While { body, .. }
        | Expr::For { body, .. }
        | Expr::WhileLet { body, .. } => Some(ControlRegion::Loop(body)),
        Expr::Block(block) => Some(ControlRegion::Block(block)),
        _ => None,
    }
}

fn index_block_control_flow<'a>(
    block: &'a Block,
    continuation: Option<FlowTarget<'a>>,
    loop_back: Option<FlowTarget<'a>>,
    loop_targets: Option<LoopTargets<'a>>,
    function_body: bool,
    facts: &mut LoanFacts,
) {
    for (index, stmt) in block.stmts.iter().enumerate() {
        let local_next = block.stmts.get(index + 1).map(target_entry);
        let next = local_next.or(continuation);
        let implicit_return = function_body
            && index + 1 == block.stmts.len()
            && matches!(stmt, Stmt::Expr(_));

        if statement_has_try(stmt) {
            record_propagation_edge(facts, stmt);
        }

        if matches!(stmt, Stmt::Break) {
            if let Some(targets) = loop_targets {
                record_edge(
                    facts,
                    entry_point(stmt),
                    stmt,
                    targets.exit,
                    LoanEdgeKind::Break,
                    Some(stmt),
                );
            }
            continue;
        }
        if matches!(stmt, Stmt::Continue) {
            if let Some(targets) = loop_targets {
                record_edge(
                    facts,
                    entry_point(stmt),
                    stmt,
                    targets.header,
                    LoanEdgeKind::Continue,
                    Some(stmt),
                );
            }
            continue;
        }

        let Some(region) = control_region(stmt) else {
            if matches!(stmt, Stmt::Return(_)) || implicit_return {
                record_return_edge(facts, entry_point(stmt), stmt);
            } else if let Some(target) = next.or(loop_back) {
                let kind = if local_next.is_none() && loop_back.is_some() {
                    LoanEdgeKind::LoopBack
                } else {
                    LoanEdgeKind::Fallthrough
                };
                record_edge(
                    facts,
                    entry_point(stmt),
                    stmt,
                    target,
                    kind,
                    Some(stmt),
                );
            }
            continue;
        };

        let completed = target_completion(stmt);
        match region {
            ControlRegion::Branch { then_block, else_block } => {
                let then_target = then_block.stmts.first().map(target_entry).unwrap_or(completed);
                record_edge(
                    facts,
                    entry_point(stmt),
                    stmt,
                    then_target,
                    LoanEdgeKind::BranchThen,
                    None,
                );
                index_block_control_flow(
                    then_block,
                    Some(completed),
                    None,
                    loop_targets,
                    false,
                    facts,
                );
                let else_target = else_block
                    .and_then(|block| block.stmts.first().map(target_entry))
                    .unwrap_or(completed);
                record_edge(
                    facts,
                    entry_point(stmt),
                    stmt,
                    else_target,
                    LoanEdgeKind::BranchElse,
                    None,
                );
                if let Some(else_block) = else_block {
                    index_block_control_flow(
                        else_block,
                        Some(completed),
                        None,
                        loop_targets,
                        false,
                        facts,
                    );
                }
            }
            ControlRegion::Match(arms) => {
                for (arm_index, arm) in arms.into_iter().enumerate() {
                    let target = match transparent_value(arm) {
                        Expr::Block(block) => {
                            let target = block.stmts.first().map(target_entry).unwrap_or(completed);
                            index_block_control_flow(
                                block,
                                Some(completed),
                                None,
                                loop_targets,
                                false,
                                facts,
                            );
                            target
                        }
                        _ => completed,
                    };
                    record_edge(
                        facts,
                        entry_point(stmt),
                        stmt,
                        target,
                        LoanEdgeKind::MatchArm(arm_index),
                        None,
                    );
                }
            }
            ControlRegion::Loop(body) => {
                let body_target = body.stmts.first().map(target_entry).unwrap_or(target_entry(stmt));
                record_edge(
                    facts,
                    entry_point(stmt),
                    stmt,
                    body_target,
                    LoanEdgeKind::LoopEnter,
                    None,
                );
                record_edge(
                    facts,
                    entry_point(stmt),
                    stmt,
                    completed,
                    LoanEdgeKind::LoopExit,
                    None,
                );
                index_block_control_flow(
                    body,
                    None,
                    Some(target_entry(stmt)),
                    Some(LoopTargets { header: target_entry(stmt), exit: completed }),
                    false,
                    facts,
                );
            }
            ControlRegion::Block(child) => {
                let target = child.stmts.first().map(target_entry).unwrap_or(completed);
                record_edge(
                    facts,
                    entry_point(stmt),
                    stmt,
                    target,
                    LoanEdgeKind::Fallthrough,
                    None,
                );
                index_block_control_flow(
                    child,
                    Some(completed),
                    None,
                    loop_targets,
                    false,
                    facts,
                );
            }
        }

        if matches!(stmt, Stmt::Return(_)) || implicit_return {
            record_return_edge(facts, completed.point, stmt);
        } else if let Some(target) = next.or(loop_back) {
            let kind = if local_next.is_none() && loop_back.is_some() {
                LoanEdgeKind::LoopBack
            } else {
                LoanEdgeKind::Fallthrough
            };
            record_edge(
                facts,
                completed.point,
                stmt,
                target,
                kind,
                Some(stmt),
            );
        }
    }
}

fn index_control_flow(block: &Block, function_body: bool, facts: &mut LoanFacts) {
    index_block_control_flow(block, None, None, None, function_body, facts);
}

/// Validate every function and return the exact events consumed by lowering.
pub fn facts(module: &Module) -> Result<LoanFacts, TypeError> {
    facts_impl(module, None)
}

/// Validate every function and retain exact checked root-local types for
/// lowering. The table must belong to this exact module allocation.
pub fn facts_with_types(
    module: &Module,
    type_table: &TypeTable,
) -> Result<LoanFacts, TypeError> {
    facts_impl(module, Some(type_table))
}

fn facts_impl(
    module: &Module,
    type_table: Option<&TypeTable>,
) -> Result<LoanFacts, TypeError> {
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
            type_table,
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
                                root_type: Some(ty.clone()),
                                projection: slot.projection.clone(),
                                borrower_projection: slot.projection,
                                origin: f.name.clone(),
                                kind: slot.kind,
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
        ctx.check_block_with(&f.body, &[], &callable_params, true, &[])?;
        index_control_flow(&f.body, true, &mut facts);

        let mut lambdas = Vec::new();
        collect_lambdas(&f.body, &mut lambdas);
        for (index, (params, body, ret)) in lambdas.into_iter().enumerate() {
            check_lambda_body(
                params,
                body,
                ret,
                &format!("lambda {} in {}", index + 1, short_name(&f.name)),
                is_opt_function(&f.name, &module.modes),
                LoanEnvironment { sigs: &sigs, catalog: &catalog, type_table },
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
    let LoanEnvironment { sigs, catalog, type_table } = environment;
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
                        root_type: Some(
                            param.ty.as_ref().expect("borrowed parameter type").clone(),
                        ),
                        projection: slot.projection.clone(),
                        borrower_projection: slot.projection,
                        origin: name.to_string(),
                        kind: slot.kind,
                        owner_type: slot.storage_type,
                        temporary: false,
                    })
                    .collect(),
            ))
        })
        .collect();
    let mut ctx = LoanCtx {
        sigs,
        type_table,
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
    ctx.check_block_with(body, &[], &callable_params, true, &[])?;
    index_control_flow(body, true, facts);
    Ok(())
}

#[derive(Clone, Copy)]
struct LoanEnvironment<'a> {
    sigs: &'a HashMap<String, BorrowSig>,
    catalog: &'a BorrowRelationCatalog,
    type_table: Option<&'a TypeTable>,
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

    // A shared reference is read-only, so `var`/`own` on it is a contradiction.
    // An exclusive reference is affine and may intentionally be transferred by
    // `own`; later affine-state checking validates that transfer.
    for p in &f.params {
        if p.ty.as_ref().is_some_and(|ty| {
            catalog.slots(ty).iter().any(|slot| slot.kind == BorrowKind::Shared)
        })
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
        returns_borrowed_shell: false,
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
        returns_borrowed_shell: matches!(
            signature.result().ty(),
            Type::Named(name, _) if catalog.borrowed_record(name)
        ),
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
        Type::Qualified(TypeQual::Borrow(_) | TypeQual::LegacyBorrow(_), _) => true,
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

fn type_has_generic_leaf(ty: &Type) -> bool {
    match ty {
        Type::Named(name, arguments) => {
            (arguments.is_empty()
                && !name.contains('.')
                && name.chars().next().is_some_and(char::is_lowercase))
                || arguments.iter().any(type_has_generic_leaf)
        }
        Type::Qualified(_, inner) => type_has_generic_leaf(inner),
        Type::Tuple(items) | Type::Dyn(_, items) => items.iter().any(type_has_generic_leaf),
        Type::Fn(parameters, result, _) => {
            parameters.iter().any(type_has_generic_leaf) || type_has_generic_leaf(result)
        }
        Type::RecordCompose { base, fields } => {
            type_has_generic_leaf(base)
                || fields.iter().any(|(_, field)| type_has_generic_leaf(field))
        }
    }
}

/// Compiler-owned collection reads are non-escaping. Their generic arguments
/// describe the collection's elements, keys, or values, not ownership slots for
/// the outer reference passed at this call site. This permits `&mut xs[i]` to
/// preserve an already-checked place relation while keeping arbitrary generic
/// calls relation-erasing.
fn authenticated_non_escaping_generic_read(
    callee: &str,
    index: usize,
    access: &AccessSignature,
) -> bool {
    if !is_std_fn(callee) || index != 0 || !access.borrow_relations().is_empty() {
        return false;
    }
    match callee {
        "list.length" => {
            access.params().len() == 1
                && matches!(
                    access.params()[0].ty().unqualified(),
                    Type::Named(name, arguments) if name == "List" && arguments.len() == 1
                )
                && matches!(
                    access.result().ty().unqualified(),
                    Type::Named(name, arguments) if name == "Int" && arguments.is_empty()
                )
        }
        witchy_syntax::intrinsics::LIST_AT => {
            access.params().len() == 2
                && matches!(
                    access.params()[0].ty().unqualified(),
                    Type::Named(name, arguments) if name == "List" && arguments.len() == 1
                )
                && type_has_generic_leaf(access.result().ty())
        }
        _ => false,
    }
}

/// The compiler-owned list slot setter preserves explicit reference carriers
/// in both the list receiver and replacement element. Its source-level generic
/// `a` is a real carrier slot here, unlike an arbitrary user generic function
/// that could erase a relation at a call boundary.
fn authenticated_non_escaping_generic_write(
    callee: &str,
    index: usize,
    access: &AccessSignature,
    sources: &[BorrowSource],
) -> bool {
    let is_set_at = matches!(callee, "list.set_at" | "list.__set_at")
        && matches!(index, 0 | 2)
        && access.params().len() == 3;
    let is_push = matches!(callee, "list.push" | "list.__push")
        && matches!(index, 0 | 1)
        && access.params().len() == 2;
    if !is_std_fn(callee)
        || (!is_set_at && !is_push)
        || !access.borrow_relations().is_empty()
        || !sources.iter().all(source_is_direct_reference)
    {
        return false;
    }
    let Type::Named(name, arguments) = access.params()[0].ty().unqualified() else {
        return false;
    };
    let Some(element) = arguments.first() else { return false };
    let element_index = if is_set_at { 2 } else { 1 };
    name == "List"
        && arguments.len() == 1
        && type_has_generic_leaf(element)
        && access.params()[element_index].ty().unqualified() == element.unqualified()
}

/// The bundled `borrow.Owned` blanket implementation is the one authenticated
/// generic materializer: it returns the same logical value as an owned result,
/// without retaining any relation to the borrowed argument. Authenticate the
/// compiler-generated callable identity from its exact generic leaf so neither
/// a user-defined `owned` method nor a lookalike mangled suffix is trusted.
fn parse_generic_materializer_name(identity: &str) -> Option<(&str, bool)> {
    let Some(core) = identity.strip_prefix("Owned__") else {
        return None;
    };
    if core.ends_with("__owned_companion") {
        let generic = core
            .strip_suffix("__owned_companion")
            .expect("suffix check ensures strip works");
        return Some((generic, true));
    }
    core.strip_suffix("__owned").map(|generic| (generic, false))
}

fn generic_materializer_key(ty: &Type) -> Option<String> {
    let ty = ty.unqualified();
    let normalize = |name: &str| name.strip_prefix('\'').unwrap_or(name).to_owned();
    match ty {
        Type::Named(name, args) if args.is_empty() => Some(normalize(name)),
        Type::Named(_, args) if args.len() == 1 => match args[0].unqualified() {
            Type::Named(name, nested_args) if nested_args.is_empty() && is_lifetime_param(name) => {
                Some(normalize(name))
            }
            _ => None,
        },
        _ => None,
    }
}

fn authenticated_generic_materializer(
    callee: &str,
    index: usize,
    access: &AccessSignature,
    sources: &[BorrowSource],
) -> bool {
    let Some((module, identity)) = callee.rsplit_once('.') else {
        return false;
    };
    let Some((generic, companion)) = parse_generic_materializer_name(identity) else {
        return false;
    };
    if !is_std_fn(callee) && !companion {
        return false;
    }
    if !companion && module != "borrow" {
        return false;
    }
    if index != 0 || access.params().len() != 1 || !access.borrow_relations().is_empty() {
        return false;
    }

    let parameter = &access.params()[0];
    let result = access.result();
    let Some(materializer_generic) = generic_materializer_key(parameter.ty()) else {
        return false;
    };

    if generic != materializer_generic {
        return false;
    }
    if !companion && parameter.ty() != result.ty() {
        return false;
    }
    if !companion && !matches!(parameter.ty().unqualified(), Type::Named(_, args) if args.is_empty()) {
        return false;
    }

    let common = type_has_generic_leaf(parameter.ty())
        && parameter.kind() == AccessKind::OwnedImmutable
        && parameter.qualifiers().is_empty()
        && result.qualifiers().is_empty()
        && parameter.borrow_lifetimes().is_empty()
        && result.borrow_lifetimes().is_empty()
        && parameter.ownership().input().is_none()
        && parameter.ownership().writeback().is_none()
        && result.ownership_output().is_none()
        && sources.iter().all(|source| source.borrower_projection.steps.is_empty());

    if companion {
        common
    } else {
        module == "borrow" && common && parameter.ty() == result.ty()
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
    /// Checked type of the owner local itself. `None` means this untyped facts
    /// consumer did not provide the exact checked expression table.
    root_type: Option<Type>,
    /// The owner-relative storage region borrowed by this view.
    projection: LoanProjection,
    /// The part of an aggregate view whose use depends on this owner. Empty for
    /// an ordinary direct view; fixed aggregates use field/tuple paths.
    borrower_projection: LoanProjection,
    /// Callee whose return type created this loan (for diagnostics).
    origin: String,
    kind: BorrowKind,
    owner_type: Type,
}

/// One owner borrowed by a `let` right-hand side, with the borrowing callee.
#[derive(Clone)]
struct BorrowSource {
    owner: String,
    root_type: Option<Type>,
    projection: LoanProjection,
    borrower_projection: LoanProjection,
    origin: String,
    kind: BorrowKind,
    owner_type: Type,
    temporary: bool,
}

fn same_source(left: &BorrowSource, right: &BorrowSource) -> bool {
    left.owner == right.owner
        && left.root_type == right.root_type
        && left.projection == right.projection
        && left.borrower_projection == right.borrower_projection
        && left.origin == right.origin
        && left.kind == right.kind
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

fn projection_has_suffix(projection: &LoanProjection, suffix: &LoanProjection) -> bool {
    projection.steps.len() >= suffix.steps.len()
        && projection.steps[projection.steps.len() - suffix.steps.len()..]
            .iter()
            .zip(&suffix.steps)
            .all(|(left, right)| projection_steps_equal(left, right))
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

/// Return the stable source identity used for a fixed aggregate callable
/// projection. Dynamic indices deliberately have no callable identity: their
/// checked type may be callable, but the element contract cannot be recovered
/// from a single source slot without a runtime-disjointness proof.
fn callable_projection_key(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Var(name) => Some(name.clone()),
        Expr::Field { base, field } => {
            callable_projection_key(base).map(|base| format!("{base}.{field}"))
        }
        Expr::Index { base, index } => match index.as_ref() {
            Expr::Int(index) => callable_projection_key(base).map(|base| format!("{base}[{index}]")),
            _ => None,
        },
        Expr::Call { name, args }
            if matches!(name.as_str(), intrinsics::LIST_AT | intrinsics::DICT_AT)
                && args.len() == 2
                && matches!(args[1], Expr::Int(_)) => {
            let Expr::Int(index) = args[1] else { unreachable!() };
            callable_projection_key(&args[0]).map(|base| format!("{base}[{index}]"))
        }
        Expr::As { expr, .. } => callable_projection_key(expr),
        _ => None,
    }
}

fn expr_root_node(expr: &Expr) -> Option<&Expr> {
    match expr {
        Expr::Var(_) => Some(expr),
        Expr::Field { base, .. } | Expr::Index { base, .. } | Expr::As { expr: base, .. } => {
            expr_root_node(base)
        }
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
                steps.push(field_projection_step(field));
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

/// Numeric field syntax is the source spelling for tuple projection. Keep its
/// loan identity aligned with tuple-pattern and aggregate-slot facts, which use
/// `Tuple`, while ordinary record fields retain their named `Field` identity.
fn field_projection_step(field: &str) -> LoanProjectionStep {
    field
        .parse::<usize>()
        .map(|index| LoanProjectionStep::Tuple(index))
        .unwrap_or_else(|_| LoanProjectionStep::Field(field.to_string()))
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
            (LoanProjectionStep::AnyIndex, LoanProjectionStep::Index(_))
            | (LoanProjectionStep::Index(_), LoanProjectionStep::AnyIndex) => true,
            _ => false,
        }
}

/// Signature slots may name the homogeneous element position as `[*]`, while
/// expression provenance necessarily carries the selected concrete index.
/// Keep that comparison in the same matcher used by prefix/overlap checks so
/// a `List(&'a T)` return relation authenticates each returned element.
fn projections_equal(left: &LoanProjection, right: &LoanProjection) -> bool {
    left.steps.len() == right.steps.len()
        && left
            .steps
            .iter()
            .zip(&right.steps)
            .all(|(left, right)| projection_steps_equal(left, right))
}

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
        LoanProjectionStep::Field(_) | LoanProjectionStep::AnyIndex => None,
    }
}

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

fn projections_overlap_any(left: &LoanProjection, right: &LoanProjection) -> bool {
    for (left, right) in left.steps.iter().zip(&right.steps) {
        if !projection_steps_overlap(left, right) {
            return false;
        }
    }
    true
}

#[cfg(test)]
fn projections_overlap(left: &LoanProjection, right: &LoanProjection) -> bool {
    projections_overlap_any(left, right)
}

/// A parenthetical clause naming the interior field/slot of a borrowed aggregate
/// whose live use keeps the owner borrowed (RFC-0112 row 7 diagnostics). Only a
/// fixed borrowed aggregate carries a non-empty `borrower_projection`; an
/// ordinary whole-owner direct view has an empty one and yields no clause, so
/// existing scalar-view messages are unchanged. The clause names only the
/// user-visible field path — never a view address or hidden root local — so it
/// tells the author exactly which projected field to shorten, destructure, or
/// materialize with `.owned()`.
fn aggregate_locus(loan: &Loan) -> String {
    if loan.borrower_projection.steps.is_empty() {
        return String::new();
    }
    format!(
        " (through its borrowed-aggregate field `{}`)",
        projection_display(&loan.borrower_projection)
    )
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
            LoanProjectionStep::AnyIndex => display.push_str("[*]"),
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
    type_table: Option<&'a TypeTable>,
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
    fn checked_root_type(&self, place: &Expr) -> Option<Type> {
        let root = expr_root_node(place)?;
        self.type_table?.type_of(root).and_then(ty_to_ast)
    }

    fn is_direct_borrowed_shell_value(
        &self,
        value: &Expr,
        callables: &HashMap<String, BorrowSig>,
    ) -> bool {
        if let Some(ty) = self
            .type_table
            .and_then(|table| table.type_of(value))
            .and_then(ty_to_ast)
        {
            return type_contains_explicit_reference_relation(&ty)
                || matches!(ty, Type::Named(name, _) if self.catalog.borrowed_record(&name))
                || matches!(
                    value,
                    Expr::Unary { op: UnOp::Borrow | UnOp::BorrowMut, .. }
                );
        }
        match value {
            // An explicit reference is a first-class value. `List(&'a T)`
            // retains the element's owner contribution just like the legacy
            // borrowed shell, rather than erasing it at aggregate storage.
            Expr::Unary { op: UnOp::Borrow | UnOp::BorrowMut, .. } => true,
            Expr::Ctor { name, .. } => self.catalog.borrowed_constructor(name),
            Expr::Record { name, .. } => self.catalog.borrowed_record(name),
            Expr::List(items) | Expr::Tuple(items) => {
                !items.is_empty()
                    && items
                        .iter()
                        .all(|item| self.is_direct_borrowed_shell_value(item, callables))
            }
            Expr::Call { name, .. } => self
                .sigs
                .get(name)
                .or_else(|| callables.get(name))
                .is_some_and(|sig| sig.returns_borrowed_shell),
            _ => false,
        }
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
    fn check_block_with(
        &mut self,
        block: &Block,
        inherited: &[Loan],
        inherited_callables: &HashMap<String, BorrowSig>,
        function_body: bool,
        seeded: &[Loan],
    ) -> Result<(), TypeError> {
        let mut local: Vec<Loan> = seeded.to_vec();
        // Exclusive references are affine. Once a local handle moves, its old
        // spelling must not become an alias that reopens the same exclusive
        // loan later in the block.
        let mut moved_exclusive: HashSet<String> = HashSet::new();
        // A tuple/list shell built from exclusive borrows is affine even though
        // its individual handles are not represented by one `Loan` named after
        // the shell. Track that shell so binding patterns cannot copy it.
        let mut affine_aggregates: HashSet<String> = HashSet::new();
        // A mutable reborrow suspends, rather than consumes, its parent handle.
        // The child remains the only live exclusive loan until its final use;
        // then the parent becomes usable again without manufacturing a second
        // owner relation.
        let mut suspended_exclusive: HashMap<String, (String, Loan)> = HashMap::new();
        // Nested loop/branch bodies receive enclosing loans as `inherited`.
        // A mutable reborrow must temporarily hide that inherited parent from
        // the nested body's live set too; otherwise the conflict pass rejects
        // the reborrow before the suspension transfer below can run.
        let mut suspended_inherited: HashSet<String> = HashSet::new();
        let mut callables = inherited_callables.clone();
        for (idx, stmt) in block.stmts.iter().enumerate() {
            // Drop local loans whose view is never mentioned again from here on.
            local.retain(|loan| self.view_used_from(loan, &block.stmts[idx..]));
            let resumed: Vec<String> = suspended_exclusive
                .iter()
                .filter_map(|(parent, (child, _))| {
                    (!local.iter().any(|loan| {
                        loan.view == *child && loan.kind == BorrowKind::Exclusive
                    }))
                    .then(|| parent.clone())
                })
                .collect();
            for parent in resumed {
                let Some((_, loan)) = suspended_exclusive.remove(&parent) else { continue };
                moved_exclusive.remove(&parent);
                let event = LoanEvent::from(loan.clone());
                push_unique_event(
                    self.facts.opens_after.entry(stmt_key(stmt)).or_default(),
                    event.clone(),
                );
                self.schedule_close(block, idx, &event);
                local.push(loan);
            }

            // Everything live at this statement: inherited (whole-block) + local.
            // Keep the parent in this source set so `&mut *parent` can recover
            // its checked owner relation. The conflict view below removes that
            // parent only for the reborrow binding itself, where suspension is
            // being established.
            let reborrow_parent = match stmt {
                Stmt::Let {
                    value:
                        Expr::Unary {
                            op: UnOp::BorrowMut,
                            expr,
                        },
                    ..
                } => match expr.as_ref() {
                    Expr::Unary {
                        op: UnOp::Deref,
                        expr,
                    } => match expr.as_ref() {
                        Expr::Var(parent) => Some(parent.as_str()),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            };
            let relinquished_exclusive = match stmt {
                Stmt::Let { value, .. } => {
                    self.relinquished_exclusive_arguments(value, &callables)
                }
                _ => Vec::new(),
            };
            let live: Vec<Loan> = inherited
                .iter()
                .filter(|loan| !suspended_inherited.contains(&loan.view))
                .chain(local.iter())
                .cloned()
                .collect();
            let conflict_live: Vec<Loan> = live
                .iter()
                .filter(|loan| {
                    !(reborrow_parent == Some(loan.view.as_str())
                        && loan.kind == BorrowKind::Exclusive)
                        && !(loan.kind == BorrowKind::Exclusive
                            && relinquished_exclusive.iter().any(|name| name == &loan.view))
                })
                .cloned()
                .collect();
            self.facts.active.insert(
                stmt_key(stmt),
                live.iter().cloned().map(LoanEvent::from).collect(),
            );

            if let Some(name) = moved_exclusive.iter().find(|name| stmt_mentions(stmt, name)) {
                return Err(terr(format!(
                    "in `{}`: moved exclusive reference `{name}` cannot be used again",
                    short_name(self.fn_name),
                )));
            }

            // `for` establishes its element binding inside the body.  Check it
            // through the dedicated path below so the outer scan does not visit
            // `*value` before list provenance has been rebound to `value`.
            if matches!(stmt, Stmt::Expr(Expr::For { .. })) {
                self.check_nested_blocks(stmt, &live, &callables)?;
                continue;
            }

            // A conflicting operation on any live loan's owner (in this statement's
            // own expressions, not counting nested blocks) is rejected.
            self.reject_conflicts(stmt, &conflict_live, &callables)?;
            self.reject_callable_boundaries(stmt, &callables, &live)?;
            self.record_direct_list_push(stmt, block, idx, &mut local, &callables, &live)?;

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
                for source in &sources {
                    let event = if let Some(loan) = live.iter().find(|loan| {
                        loan.owner == source.owner
                            && loan.projection == source.projection
                            && loan.borrower_projection == source.borrower_projection
                    }) {
                        LoanEvent::from(loan.clone())
                    } else {
                        LoanEvent {
                            view: expr_root(value).unwrap_or("$return").to_string(),
                            owner: source.owner.clone(),
                            projection: source.projection.clone(),
                            borrower_projection: source.borrower_projection.clone(),
                            origin: source.origin.clone(),
                            kind: source.kind,
                            owner_type: source.owner_type.clone(),
                            owner_root: LoanOwnerRoot {
                                local: source.owner.clone(),
                                direct_storage_type: source.root_type.clone(),
                            },
                        }
                    };
                    let transfers = self
                        .facts
                        .return_transfers
                        .entry(stmt_key(stmt))
                        .or_default();
                    push_unique_event(transfers, event);
                }
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
                if expr_contains_exclusive_borrow(value) {
                    affine_aggregates.insert(name.clone());
                } else if let Expr::Var(source) = value
                    && affine_aggregates.remove(source)
                {
                    moved_exclusive.insert(source.clone());
                    affine_aggregates.insert(name.clone());
                }
                if let Expr::Unary {
                    op: UnOp::BorrowMut,
                    expr,
                } = value
                    && let Expr::Unary {
                        op: UnOp::Deref,
                        expr: source,
                    } = expr.as_ref()
                    && let Expr::Var(source) = source.as_ref()
                {
                    let mut reborrowed = Vec::new();
                    for loan in inherited.iter().chain(local.iter()).filter(|loan| {
                        loan.view == *source && loan.kind == BorrowKind::Exclusive
                    }) {
                        if !reborrowed.contains(loan) {
                            reborrowed.push(loan.clone());
                        }
                    }
                    if !reborrowed.is_empty() {
                        if inherited.iter().any(|loan| {
                            loan.view == *source && loan.kind == BorrowKind::Exclusive
                        }) {
                            suspended_inherited.insert(source.clone());
                        }
                        for old in &reborrowed {
                            let old_event = LoanEvent::from(old.clone());
                            self.remove_scheduled_close(&old_event);
                            push_unique_event(
                                self.facts.closes_after.entry(stmt_key(stmt)).or_default(),
                                old_event,
                            );
                            let mut next = old.clone();
                            next.view = name.clone();
                            let next_event = LoanEvent::from(next.clone());
                            push_unique_event(
                                self.facts.opens_after.entry(stmt_key(stmt)).or_default(),
                                next_event.clone(),
                            );
                            self.schedule_close(block, idx, &next_event);
                            local.push(next);
                        }
                        local.retain(|loan| {
                            !(loan.view == *source && loan.kind == BorrowKind::Exclusive)
                        });
                        moved_exclusive.insert(source.clone());
                        suspended_exclusive.insert(source.clone(), (name.clone(), reborrowed[0].clone()));
                        continue;
                    }
                }
                // Extracting a fixed element/field from an affine aggregate
                // transfers only the selected owner contributions to the new
                // binding. Opening a second loan for the projection would
                // incorrectly overlap the aggregate's existing exclusive
                // contribution while leaving unrelated aggregate elements
                // unavailable for later use.
                if !matches!(value, Expr::Var(_)) {
                    let mut projected_sources = Vec::new();
                    self.collect_alias_sources(value, &live, &mut projected_sources);
                    projected_sources.retain(|source| source.kind == BorrowKind::Exclusive);
                    let mut transfers: Vec<(Loan, BorrowSource)> = Vec::new();
                    for source in projected_sources {
                        let Some(old) = local.iter().find(|loan| {
                            loan.kind == BorrowKind::Exclusive
                                && loan.owner == source.owner
                                && loan.projection == source.projection
                                && loan.origin == source.origin
                                && projection_has_suffix(
                                    &loan.borrower_projection,
                                    &source.borrower_projection,
                                )
                                && !transfers.iter().any(|(known, _)| known == *loan)
                        }) else {
                            transfers.clear();
                            break;
                        };
                        transfers.push((old.clone(), source));
                    }
                    if !transfers.is_empty() {
                        for (old, _) in &transfers {
                            let old_event = LoanEvent::from(old.clone());
                            self.remove_scheduled_close(&old_event);
                            push_unique_event(
                                self.facts.closes_after.entry(stmt_key(stmt)).or_default(),
                                old_event,
                            );
                        }
                        local.retain(|loan| {
                            !transfers.iter().any(|(old, _)| old == loan)
                        });
                        for (_, source) in transfers {
                            let next = Loan {
                                view: name.clone(),
                                owner: source.owner,
                                root_type: source.root_type,
                                projection: source.projection,
                                borrower_projection: source.borrower_projection,
                                origin: source.origin,
                                kind: source.kind,
                                owner_type: source.owner_type,
                            };
                            let next_event = LoanEvent::from(next.clone());
                            push_unique_event(
                                self.facts.opens_after.entry(stmt_key(stmt)).or_default(),
                                next_event.clone(),
                            );
                            self.schedule_close(block, idx, &next_event);
                            local.push(next);
                        }
                        affine_aggregates.insert(name.clone());
                        if let Some(source) = expr_root(value)
                            && !local.iter().any(|loan| loan.view == source)
                        {
                            moved_exclusive.insert(source.to_string());
                        }
                        continue;
                    }
                }
                if let Expr::Var(source) = value {
                    let transferred: Vec<Loan> = local
                        .iter()
                        .filter(|loan| loan.view == *source && loan.kind == BorrowKind::Exclusive)
                        .cloned()
                        .collect();
                    if !transferred.is_empty() {
                        for old in &transferred {
                            let old_event = LoanEvent::from(old.clone());
                            self.remove_scheduled_close(&old_event);
                            push_unique_event(
                                self.facts.closes_after.entry(stmt_key(stmt)).or_default(),
                                old_event,
                            );

                            let mut next = old.clone();
                            next.view = name.clone();
                            let next_event = LoanEvent::from(next.clone());
                            push_unique_event(
                                self.facts.opens_after.entry(stmt_key(stmt)).or_default(),
                                next_event.clone(),
                            );
                            self.schedule_close(block, idx, &next_event);
                            local.push(next);
                        }
                        local.retain(|loan| {
                            !(loan.view == *source && loan.kind == BorrowKind::Exclusive)
                        });
                        moved_exclusive.insert(source.clone());
                        for (child, _) in suspended_exclusive.values_mut() {
                            if child == source {
                                *child = name.clone();
                            }
                        }
                        self.reject_callable_erasure(name, value, ty.as_ref(), None, &callables)?;
                        if let Some(sig) = self.callable_value_sig(value, ty.as_ref(), &callables) {
                            callables.insert(name.clone(), sig);
                        } else {
                            callables.remove(name);
                        }
                        continue;
                    }
                }
                // Returning an exclusive reference from an exclusive argument
                // transfers the affine handle. The returned place may be a
                // projection selected by the callee body, but its owner root is
                // still the same checked input loan; retaining the old handle
                // would manufacture an overlapping `&mut` loan at the caller.
                let returned_exclusive = self.returned_exclusive_arguments(value, &callables);
                if !returned_exclusive.is_empty() {
                    let mut transferred_any = false;
                    for source in returned_exclusive {
                        let transferred: Vec<Loan> = local
                            .iter()
                            .filter(|loan| {
                                loan.view == source && loan.kind == BorrowKind::Exclusive
                            })
                            .cloned()
                            .collect();
                        for old in &transferred {
                            transferred_any = true;
                            let old_event = LoanEvent::from(old.clone());
                            self.remove_scheduled_close(&old_event);
                            push_unique_event(
                                self.facts.closes_after.entry(stmt_key(stmt)).or_default(),
                                old_event,
                            );

                            let mut next = old.clone();
                            next.view = name.clone();
                            let next_event = LoanEvent::from(next.clone());
                            push_unique_event(
                                self.facts.opens_after.entry(stmt_key(stmt)).or_default(),
                                next_event.clone(),
                            );
                            self.schedule_close(block, idx, &next_event);
                            local.push(next);
                        }
                        if !transferred.is_empty() {
                            local.retain(|loan| {
                                !(loan.view == source && loan.kind == BorrowKind::Exclusive)
                            });
                            moved_exclusive.insert(source);
                        }
                    }
                    if transferred_any {
                        self.reject_callable_erasure(name, value, ty.as_ref(), None, &callables)?;
                        if let Some(sig) = self.callable_value_sig(value, ty.as_ref(), &callables) {
                            callables.insert(name.clone(), sig);
                        } else {
                            callables.remove(name);
                        }
                        continue;
                    }
                }
                // Returning an exclusive reference from an exclusive argument
                // transfers the affine handle. The returned place may be a
                // projection selected by the callee body, but its owner root is
                // still the same checked input loan; retaining the old handle
                // would manufacture an overlapping `&mut` loan at the caller.
                let returned_exclusive = self.returned_exclusive_arguments(value, &callables);
                if !returned_exclusive.is_empty() {
                    let mut transferred_any = false;
                    for source in returned_exclusive {
                        let transferred: Vec<Loan> = local
                            .iter()
                            .filter(|loan| {
                                loan.view == source && loan.kind == BorrowKind::Exclusive
                            })
                            .cloned()
                            .collect();
                        for old in &transferred {
                            transferred_any = true;
                            let old_event = LoanEvent::from(old.clone());
                            self.remove_scheduled_close(&old_event);
                            push_unique_event(
                                self.facts.closes_after.entry(stmt_key(stmt)).or_default(),
                                old_event,
                            );

                            let mut next = old.clone();
                            next.view = name.clone();
                            let next_event = LoanEvent::from(next.clone());
                            push_unique_event(
                                self.facts.opens_after.entry(stmt_key(stmt)).or_default(),
                                next_event.clone(),
                            );
                            self.schedule_close(block, idx, &next_event);
                            local.push(next);
                        }
                        if !transferred.is_empty() {
                            local.retain(|loan| {
                                !(loan.view == source && loan.kind == BorrowKind::Exclusive)
                            });
                            moved_exclusive.insert(source);
                        }
                    }
                    if transferred_any {
                        self.reject_callable_erasure(name, value, ty.as_ref(), None, &callables)?;
                        if let Some(sig) = self.callable_value_sig(value, ty.as_ref(), &callables) {
                            callables.insert(name.clone(), sig);
                        } else {
                            callables.remove(name);
                        }
                        continue;
                    }
                }
                if self.has_dynamic_borrow_projection(value, &callables, &live) {
                    return Err(self.dynamic_projection());
                }
                let mut sources = self.borrow_sources(value, &callables, &live);
                self.collect_alias_sources(value, &live, &mut sources);
                // Returning a shared reference from an exclusive input consumes
                // the exclusive capability. The shared result keeps the owner
                // loan, but the caller can no longer use the old `&mut` handle.
                for source in self.relinquished_exclusive_arguments(value, &callables) {
                    let retired: Vec<Loan> = local
                        .iter()
                        .filter(|loan| loan.view == source && loan.kind == BorrowKind::Exclusive)
                        .cloned()
                        .collect();
                    for old in &retired {
                        let event = LoanEvent::from(old.clone());
                        self.remove_scheduled_close(&event);
                        push_unique_event(
                            self.facts.closes_after.entry(stmt_key(stmt)).or_default(),
                            event,
                        );
                    }
                    if !retired.is_empty() {
                        local.retain(|loan| {
                            !(loan.view == source && loan.kind == BorrowKind::Exclusive)
                        });
                        moved_exclusive.insert(source);
                    }
                }
                if let Some(source) = self.aggregate_borrow_source(value, &callables, &live) {
                    return Err(self.aggregate_view_storage(&source.origin));
                }
                if *mutable
                    && !sources.is_empty()
                    && !self.is_direct_borrowed_shell_value(value, &callables)
                    && !sources.iter().all(source_is_direct_reference)
                {
                    return Err(self.mutable_view_storage(name));
                }
                for owner in sources {
                    if owner.temporary {
                        return Err(self.temporary_owner(&owner.origin));
                    }
                    let loan = Loan {
                        view: name.clone(),
                        owner: owner.owner,
                        root_type: owner.root_type,
                        projection: owner.projection,
                        borrower_projection: owner.borrower_projection,
                        origin: owner.origin,
                        kind: owner.kind,
                        owner_type: owner.owner_type,
                    };
                    let conflicts_with_live = conflict_live.iter().any(|open| {
                        open.owner == loan.owner
                            && projections_overlap_any(&open.projection, &loan.projection)
                            && (loan.kind == BorrowKind::Exclusive || open.kind == BorrowKind::Exclusive)
                    });
                    if conflicts_with_live
                    {
                        return Err(self.exclusive_overlap(&loan));
                    }
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
                self.remember_callable_projections(name, value, &mut callables);
            } else if let Stmt::LetPattern { pattern, value } = stmt {
                if self.has_dynamic_borrow_projection(value, &callables, &live) {
                    return Err(self.dynamic_projection());
                }
                if let Expr::Var(source) = value
                    && affine_aggregates.contains(source)
                {
                    moved_exclusive.insert(source.clone());
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
                            root_type: source.root_type,
                            projection: source.projection,
                            borrower_projection: source.borrower_projection,
                            origin: source.origin,
                            kind: source.kind,
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
                if let Some(mutation) = self.replace_shell_roots(
                    block,
                    idx,
                    stmt,
                    name,
                    value,
                    &live,
                    &mut local,
                    &callables,
                )? {
                    self.facts.shell_mutations.insert(
                        stmt_key(stmt),
                        mutation,
                    );
                } else {
                    let mut sources = self.borrow_sources(value, &callables, &live);
                    self.collect_alias_sources(value, &live, &mut sources);
                    if !sources.is_empty() {
                        if sources.iter().any(|source| !source_is_direct_reference(source)) {
                            return Err(self.mutable_view_storage(name));
                        }
                        let replaced: Vec<Loan> = local
                            .iter()
                            .filter(|loan| loan.view == *name)
                            .cloned()
                            .collect();
                        for old in replaced {
                            let event = LoanEvent::from(old.clone());
                            self.remove_scheduled_close(&event);
                            push_unique_event(
                                self.facts.closes_after.entry(stmt_key(stmt)).or_default(),
                                event,
                            );
                        }
                        local.retain(|loan| loan.view != *name);
                        for source in sources {
                            if source.temporary {
                                return Err(self.temporary_owner(&source.origin));
                            }
                            let loan = Loan {
                                view: name.clone(),
                                owner: source.owner,
                                root_type: source.root_type,
                                projection: source.projection,
                                borrower_projection: source.borrower_projection,
                                origin: source.origin,
                                kind: source.kind,
                                owner_type: source.owner_type,
                            };
                            let conflicts_with_live = live.iter().any(|open| {
                                open.view != *name
                                    && open.owner == loan.owner
                                    && projections_overlap_any(&open.projection, &loan.projection)
                                    && (loan.kind == BorrowKind::Exclusive
                                        || open.kind == BorrowKind::Exclusive)
                            });
                            if conflicts_with_live {
                                return Err(self.exclusive_overlap(&loan));
                            }
                            let event = LoanEvent::from(loan.clone());
                            self.facts
                                .opens_after
                                .entry(stmt_key(stmt))
                                .or_default()
                                .push(event.clone());
                            self.schedule_close(block, idx, &event);
                            local.push(loan);
                        }
                    }
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

    /// Publish an explicit reference appended through `list.push` as a
    /// borrower-indexed list loan. Generic list calls are relation-erasing by
    /// default; the authenticated direct-reference carrier path is the one
    /// exception, and it must update the same affine state used by `refs[i]`.
    fn record_direct_list_push(
        &mut self,
        stmt: &Stmt,
        block: &Block,
        idx: usize,
        local: &mut Vec<Loan>,
        callables: &HashMap<String, BorrowSig>,
        live: &[Loan],
    ) -> Result<(), TypeError> {
        let (name, args, assigned_list) = match stmt {
            Stmt::Expr(Expr::Call { name, args }) => (name, args, None),
            Stmt::Assign { name, value: Expr::Call { name: call, args } } => {
                (call, args, Some(name.as_str()))
            }
            _ => return Ok(()),
        };
        if !matches!(name.as_str(), witchy_syntax::intrinsics::LIST_PUSH | "list.push")
            || args.len() != 2
        {
            return Ok(());
        }
        let list_name = assigned_list.or_else(|| match &args[0] {
            Expr::Var(name) => Some(name.as_str()),
            _ => None,
        });
        let Some(list_name) = list_name else { return Ok(()) };
        let mut sources = self.borrow_sources(&args[1], callables, live);
        self.collect_alias_sources(&args[1], live, &mut sources);
        if sources.is_empty() || sources.iter().any(|source| !source_is_direct_reference(source)) {
            return Ok(());
        }

        let next_index = local
            .iter()
            .filter(|loan| loan.view == list_name)
            .filter_map(|loan| loan.borrower_projection.steps.last())
            .filter_map(|step| match step {
                LoanProjectionStep::Index(index) => Some(*index),
                _ => None,
            })
            .max()
            .map_or(0, |index| index + 1);
        let index_projection = LoanProjection {
            steps: vec![LoanProjectionStep::Index(next_index)],
        };
        for source in sources {
            let loan = Loan {
                view: list_name.to_owned(),
                owner: source.owner,
                root_type: source.root_type,
                projection: source.projection,
                borrower_projection: source.borrower_projection.extended(&index_projection),
                origin: source.origin,
                kind: source.kind,
                owner_type: source.owner_type,
            };
            if local.iter().any(|open| {
                open.kind == BorrowKind::Exclusive
                    && loan.kind == BorrowKind::Exclusive
                    && open.owner == loan.owner
                    && projections_overlap_any(&open.projection, &loan.projection)
            }) {
                return Err(self.exclusive_overlap(&loan));
            }
            let event = LoanEvent::from(loan.clone());
            push_unique_event(
                self.facts.opens_after.entry(stmt_key(stmt)).or_default(),
                event.clone(),
            );
            let close_idx = block.stmts[idx + 1..]
                .iter()
                .rposition(|statement| stmt_mentions(statement, list_name))
                .map(|offset| idx + 1 + offset)
                .unwrap_or(idx);
            self.facts
                .closes_after
                .entry(stmt_key(&block.stmts[close_idx]))
                .or_default()
                .push(event);
            local.push(loan);
        }
        Ok(())
    }

    /// Turn an authenticated `shell = Shell(field: replacement, ..shell)` into
    /// a root-set transition. This is deliberately fact-driven: the type
    /// checker proves that the updated field declares the shell's lifetime
    /// relation, while this pass proves which owner roots retire and which open.
    fn replace_shell_roots(
        &mut self,
        block: &Block,
        idx: usize,
        stmt: &Stmt,
        name: &str,
        value: &Expr,
        live: &[Loan],
        local: &mut Vec<Loan>,
        callables: &HashMap<String, BorrowSig>,
    ) -> Result<Option<LoanShellMutation>, TypeError> {
        let Expr::RecordUpdate { base, fields, .. } = value else { return Ok(None) };
        if !matches!(base.as_ref(), Expr::Var(base) if base == name)
            || !live.iter().any(|loan| loan.view == name)
        {
            return Ok(None);
        }

        let mut roots_before = Vec::new();
        for loan in live.iter().filter(|loan| loan.view == name) {
            push_unique_event(&mut roots_before, LoanEvent::from(loan.clone()));
        }

        for (field, replacement) in fields {
            let field_projection = LoanProjection {
                steps: vec![LoanProjectionStep::Field(field.clone())],
            };
            let old: Vec<Loan> = local
                .iter()
                .filter(|loan| {
                    loan.view == name
                        && strip_projection_prefix(&loan.borrower_projection, &field_projection)
                            .is_some()
                })
                .cloned()
                .collect();

            let mut sources = self.borrow_sources(replacement, callables, live);
            self.collect_alias_sources(replacement, live, &mut sources);
            // A direct owner local (rather than a `let('a)` parameter or an
            // existing view) has no alias source. For a field that already
            // carries a checked borrowed relation, recover that direct root
            // from its exact place instead of silently materializing it.
            if sources.is_empty() && !old.is_empty() {
                if let Some((root, PlaceProjection::Fixed(projection))) = expr_place(replacement)
                {
                    for old_loan in &old {
                        self.push_source(
                            BorrowSource {
                                owner: root.to_string(),
                                root_type: self.checked_root_type(replacement),
                                projection: projection.clone(),
                                borrower_projection: LoanProjection::default(),
                                origin: old_loan.origin.clone(),
                                kind: old_loan.kind,
                                owner_type: old_loan.owner_type.clone(),
                                temporary: false,
                            },
                            &mut sources,
                        );
                    }
                }
            }

            let mut replacements = Vec::new();
            for mut source in sources {
                if source.temporary {
                    return Err(self.temporary_owner(&source.origin));
                }
                source.borrower_projection = source
                    .borrower_projection
                    .prefixed(LoanProjectionStep::Field(field.clone()));
                let loan = Loan {
                    view: name.to_string(),
                    owner: source.owner,
                    root_type: source.root_type,
                    projection: source.projection,
                    borrower_projection: source.borrower_projection,
                    origin: source.origin,
                    kind: source.kind,
                    owner_type: source.owner_type,
                };
                if !replacements.contains(&loan) {
                    replacements.push(loan);
                }
            }

            for old_loan in &old {
                let event = LoanEvent::from(old_loan.clone());
                if replacements.iter().any(|loan| LoanEvent::from(loan.clone()) == event) {
                    continue;
                }
                self.remove_scheduled_close(&event);
                push_unique_event(
                    self.facts.closes_after.entry(stmt_key(stmt)).or_default(),
                    event,
                );
                local.retain(|loan| loan != old_loan);
            }
            for replacement in replacements {
                let event = LoanEvent::from(replacement.clone());
                if old.iter().any(|loan| LoanEvent::from(loan.clone()) == event) {
                    continue;
                }
                push_unique_event(
                    self.facts.opens_after.entry(stmt_key(stmt)).or_default(),
                    event.clone(),
                );
                self.schedule_close(block, idx, &event);
                local.push(replacement);
            }
        }

        let mut roots_after = Vec::new();
        for loan in local.iter().filter(|loan| loan.view == name) {
            push_unique_event(&mut roots_after, LoanEvent::from(loan.clone()));
        }
        Ok(Some(LoanShellMutation {
            shell: name.to_string(),
            fields: fields.iter().map(|(field, _)| field.clone()).collect(),
            roots_before,
            roots_after,
        }))
    }

    fn remove_scheduled_close(&mut self, event: &LoanEvent) {
        let keys: Vec<usize> = self.facts.closes_after.keys().copied().collect();
        for key in keys {
            let remove = if let Some(events) = self.facts.closes_after.get_mut(&key) {
                events.retain(|existing| existing != event);
                events.is_empty()
            } else {
                false
            };
            if remove {
                self.facts.closes_after.remove(&key);
            }
        }
    }

    fn schedule_close(&mut self, block: &Block, idx: usize, event: &LoanEvent) {
        let close_idx = block.stmts[idx + 1..]
            .iter()
            .rposition(|statement| stmt_mentions(statement, &event.view))
            .map(|offset| idx + 1 + offset)
            .unwrap_or(idx);
        push_unique_event(
            self.facts
                .closes_after
                .entry(stmt_key(&block.stmts[close_idx]))
                .or_default(),
            event.clone(),
        );
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
            // Borrowing a view preserves its owner relation; dereferencing one
            // materializes the referent value and therefore must not publish a
            // second borrowed result. `&*view` reaches this helper through the
            // outer borrow arm above, which deliberately recovers the live
            // source from its operand.
            Expr::Unary { op: UnOp::Borrow, expr } => {
                self.collect_alias_sources(expr, live, out);
            }
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
                            root_type: loan.root_type.clone(),
                            projection: loan.projection.clone(),
                            borrower_projection: loan.borrower_projection.clone(),
                            origin: loan.origin.clone(),
                            kind: loan.kind,
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
            // A list is a dynamic container, but the checked list binding owns
            // the complete, finite set of companion roots for its borrowed
            // elements.  A constant index selects its exact contribution;
            // a dynamic index deliberately transfers every contribution.  That
            // conservative path keeps an extracted shell alive after the list's
            // own final use without inventing an interior object root.
            Expr::Call { name, args }
                if witchy_syntax::intrinsics::canonical_operation_name(name)
                    == witchy_syntax::intrinsics::LIST_AT
                    && args.len() == 2 =>
            {
                let mut list_sources = Vec::new();
                self.collect_alias_sources(&args[0], live, &mut list_sources);
                let requested = index_projection(&args[1])
                    .map(|step| LoanProjection { steps: vec![step] });
                for mut source in list_sources {
                    if let Some(requested) = &requested {
                        if let Some(selected) = project_source(source, requested) {
                            self.push_source(selected, out);
                        }
                    } else {
                        source.borrower_projection = LoanProjection::default();
                        self.push_source(source, out);
                    }
                }
            }
            Expr::List(items) => {
                for (index, item) in items.iter().enumerate() {
                    let mut item_sources = Vec::new();
                    self.collect_alias_sources(item, live, &mut item_sources);
                    let index_step = LoanProjection {
                        steps: vec![LoanProjectionStep::Index(index as i64)],
                    };
                    for mut source in item_sources {
                        source.borrower_projection = source.borrower_projection.extended(&index_step);
                        self.push_source(source, out);
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
                .filter(|relation| {
                    projections_equal(&relation.output_projection, &source.borrower_projection)
                })
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
            // RFC-0122 explicit shared-borrow origin. It uses the same stable
            // root/projection fact as an older view-returning call, so all of the
            // established last-use, conflict, aggregate, and root-lifetime logic
            // applies without a parallel borrow checker.
            Expr::Unary { op: UnOp::Borrow | UnOp::BorrowMut, expr } => {
                if let Some((root, place_projection)) = expr_place(expr) {
                    // A dynamic index still has a stable owner. The runtime
                    // carrier captures its evaluated offset; the checker
                    // conservatively loans the whole owner because a later
                    // dynamic index cannot prove a disjoint projection.
                    let projection = match place_projection {
                        PlaceProjection::Fixed(projection) => projection,
                        PlaceProjection::Dynamic => LoanProjection::default(),
                    };
                    let root_type = self.checked_root_type(expr);
                    self.push_source(BorrowSource {
                        owner: root.to_string(),
                        root_type: root_type.clone(),
                        projection,
                        borrower_projection: LoanProjection::default(),
                        origin: EXPLICIT_REFERENCE_ORIGIN.into(),
                        kind: if matches!(e, Expr::Unary { op: UnOp::BorrowMut, .. }) {
                            BorrowKind::Exclusive
                        } else {
                            BorrowKind::Shared
                        },
                        owner_type: root_type.unwrap_or_else(|| Type::Named("Unknown".into(), Vec::new())),
                        temporary: false,
                    }, out);
                } else {
                    // `&*view` and `&view` are shared reborrows. Their owner
                    // relation comes from the live/input source, not from the
                    // reference-handle variable itself.
                    let source = match expr.as_ref() {
                        Expr::Unary { op: UnOp::Deref, expr } => expr.as_ref(),
                        source => source,
                    };
                    self.collect_alias_sources(source, live, out);
                    if out.is_empty() {
                        self.push_source(BorrowSource {
                            owner: String::new(),
                            root_type: None,
                            projection: LoanProjection::default(),
                            borrower_projection: LoanProjection::default(),
                            origin: EXPLICIT_REFERENCE_ORIGIN.into(),
                            kind: BorrowKind::Shared,
                            owner_type: Type::Named("Unknown".into(), Vec::new()),
                            temporary: true,
                        }, out);
                    }
                }
            }
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
            Expr::Binary { op: BinOp::Coalesce, lhs, rhs } => {
                // `Option(&mut T) ?? &mut fallback` yields the same executable
                // reference carrier as either branch. Recover the selected
                // source from a live aggregate binding and retain the fallback
                // source for the other control-flow edge.
                self.collect_alias_sources(lhs, live, out);
                self.collect_view_owners(lhs, callables, live, out);
                self.collect_view_owners(rhs, callables, live, out);
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
            Expr::List(items) => {
                for (index, item) in items.iter().enumerate() {
                    self.collect_aggregate_slot(
                        item,
                        LoanProjectionStep::Index(index as i64),
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
                field_projection_step(field),
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
                // A first-class reference argument denotes its referent place.
                // Recover that place before alias propagation so `f(&owner)`
                // remains rooted in `owner` when `f` returns the same relation.
                // Treating the handle itself as a non-place would manufacture a
                // temporary owner for every direct shared-reference call.
                let (owner_arg, argument_place) = match arg {
                    Expr::Unary { op: UnOp::Borrow | UnOp::BorrowMut, expr } => {
                        (expr.as_ref(), expr_place(expr))
                    }
                    _ => (arg, expr_place(arg)),
                };
                let mut sources = Vec::new();
                self.collect_alias_sources(arg, live, &mut sources);
                if sources.is_empty() {
                    if let Some((root, PlaceProjection::Fixed(argument_projection))) =
                        argument_place
                    {
                        sources.push(BorrowSource {
                            owner: root.to_string(),
                            root_type: self.checked_root_type(owner_arg).or_else(|| {
                                argument_projection.steps.is_empty().then(|| {
                                    sig.owner_params
                                        .iter()
                                        .find(|(position, _)| *position == owner.position())
                                        .map(|(_, ty)| ty.clone())
                                        .unwrap_or_else(|| relation.storage_type().clone())
                                })
                            }),
                            projection: argument_projection.extended(owner.input_projection()),
                            borrower_projection: LoanProjection::default(),
                            origin: callee.to_string(),
                            kind: relation.kind(),
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
                        root_type: Some(relation.storage_type().clone()),
                        projection: LoanProjection::default(),
                        borrower_projection: relation.output_projection().clone(),
                        origin: callee.to_string(),
                        kind: relation.kind(),
                        owner_type: relation.storage_type().clone(),
                        temporary: true,
                    });
                }
                for mut source in sources {
                    source.borrower_projection = relation.output_projection().clone();
                    source.origin = callee.to_string();
                    source.kind = relation.kind();
                    source.owner_type = relation.storage_type().clone();
                    self.push_source(source, out);
                }
            }
        }
    }

    /// Names of local exclusive handles that a direct call converts into a
    /// returned shared reference. This is a capability transition, not a copy:
    /// the resulting shared loan remains live, while the `&mut` spelling is
    /// retired at the call site.
    fn relinquished_exclusive_arguments(
        &self,
        value: &Expr,
        callables: &HashMap<String, BorrowSig>,
    ) -> Vec<String> {
        let (signature, args) = match value {
            Expr::Call { name, args } => {
                let Some(signature) = self.sigs.get(name).or_else(|| callables.get(name)) else {
                    return Vec::new();
                };
                (signature, args)
            }
            Expr::Apply { func, args } => {
                let Some((_, signature)) = self.callable_expr_sig(func, callables) else {
                    return Vec::new();
                };
                return self.relinquished_exclusive_arguments_from_signature(&signature, args);
            }
            _ => return Vec::new(),
        };
        self.relinquished_exclusive_arguments_from_signature(signature, args)
    }

    fn relinquished_exclusive_arguments_from_signature(
        &self,
        signature: &BorrowSig,
        args: &[Expr],
    ) -> Vec<String> {
        let mut result = Vec::new();
        for relation in signature
            .relations
            .iter()
            .filter(|relation| relation.kind() == BorrowKind::Shared)
        {
            for owner in relation.owners() {
                let Some(Expr::Var(name)) = args.get(owner.position()) else { continue };
                if !result.contains(name) {
                    result.push(name.clone());
                }
            }
        }
        result
    }

    /// Local exclusive handles passed to a direct call whose result retains an
    /// exclusive relation. The result is an affine reborrow, so the caller must
    /// move the handle to the result binding instead of opening an overlapping
    /// second exclusive loan.
    fn returned_exclusive_arguments(
        &self,
        value: &Expr,
        callables: &HashMap<String, BorrowSig>,
    ) -> Vec<String> {
        let (signature, args) = match value {
            Expr::Call { name, args } => {
                let Some(signature) = self.sigs.get(name).or_else(|| callables.get(name)) else {
                    return Vec::new();
                };
                (signature, args)
            }
            Expr::Apply { func, args } => {
                let Some((_, signature)) = self.callable_expr_sig(func, callables) else {
                    return Vec::new();
                };
                return self.returned_exclusive_arguments_from_signature(&signature, args);
            }
            _ => return Vec::new(),
        };
        self.returned_exclusive_arguments_from_signature(signature, args)
    }

    fn returned_exclusive_arguments_from_signature(
        &self,
        signature: &BorrowSig,
        args: &[Expr],
    ) -> Vec<String> {
        let mut result = Vec::new();
        for relation in signature
            .relations
            .iter()
            .filter(|relation| relation.kind() == BorrowKind::Exclusive)
        {
            for owner in relation.owners() {
                let Some(Expr::Var(name)) = args.get(owner.position()) else { continue };
                if !result.contains(name) {
                    result.push(name.clone());
                }
            }
        }
        result
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
            // `List(B('a))` is the one owned aggregate that has an explicit
            // element-root representation. Its element contributions are
            // published by `collect_view_owners` and travel with the list
            // binding, so do not treat it as an erased owned aggregate.
            Expr::List(items)
                if items.iter().all(|item| self.is_direct_borrowed_shell_value(item, callables)) =>
            {
                None
            }
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
            Expr::Call { name, .. } => callable_projection_key(expr)
                .and_then(|key| callables.get(&key).cloned().map(|sig| (key, sig)))
                .or_else(|| {
                    self.sigs
                        .get(name)
                        .or_else(|| callables.get(name))
                        .and_then(|sig| sig.callable_return.as_deref())
                        .cloned()
                        .map(|sig| (name.clone(), sig))
                }),
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
            Expr::Field { base, field } => {
                callable_projection_key(expr)
                    .and_then(|key| callables.get(&key).cloned().map(|sig| (key, sig)))
                    .or_else(|| {
                        let ty = self
                            .type_table
                            .and_then(|table| table.type_of(base))
                            .and_then(ty_to_ast)
                            .and_then(|base| self.catalog.field_type(&base, field))
                            .or_else(|| {
                                self.type_table
                                    .and_then(|table| table.type_of(expr))
                                    .and_then(ty_to_ast)
                            });
                        let sig = ty
                            .as_ref()
                            .and_then(|ty| borrow_sig_from_fn_type(ty, self.catalog));
                        sig.map(|sig| ("projected function".into(), sig))
                    })
            }
            Expr::Index { .. } => callable_projection_key(expr)
                .and_then(|key| callables.get(&key).cloned().map(|sig| (key, sig)))
                .or_else(|| {
                    self.type_table
                        .and_then(|table| table.type_of(expr))
                        .and_then(ty_to_ast)
                        .and_then(|ty| borrow_sig_from_fn_type(&ty, self.catalog))
                        .map(|sig| ("projected function".into(), sig))
                }),
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

    fn remember_callable_projections(
        &self,
        binding: &str,
        value: &Expr,
        callables: &mut HashMap<String, BorrowSig>,
    ) {
        match value {
            Expr::Tuple(items) | Expr::List(items) => {
                for (index, item) in items.iter().enumerate() {
                    let key = match value {
                        Expr::Tuple(_) => format!("{binding}.{index}"),
                        Expr::List(_) => format!("{binding}[{index}]"),
                        _ => unreachable!(),
                    };
                    if let Some((_, sig)) = self.callable_expr_sig(item, callables) {
                        callables.insert(key.clone(), sig);
                    }
                    self.remember_callable_projections(&key, item, callables);
                }
            }
            Expr::Record { fields, .. } => {
                for (field, item) in fields {
                    let key = format!("{binding}.{field}");
                    if let Some((_, sig)) = self.callable_expr_sig(item, callables) {
                        callables.insert(key.clone(), sig);
                    }
                    self.remember_callable_projections(&key, item, callables);
                }
            }
            _ => {}
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
        live: &[Loan],
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
                        result = self.check_callable_arguments(name, args, sig, callables, live);
                    }
                }
                Expr::Apply { func, args } => {
                    if let Some((name, sig)) = self.callable_expr_sig(func, callables) {
                        result =
                            self.check_callable_arguments(&name, args, &sig, callables, live);
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
        live: &[Loan],
    ) -> Result<(), TypeError> {
        for (index, arg) in args.iter().enumerate() {
            let mut sources = self.borrow_sources(arg, callables, live);
            self.collect_alias_sources(arg, live, &mut sources);
            let preserves_explicit_list_relation = signature.access.as_ref().is_some_and(|access| {
                authenticated_non_escaping_generic_write(callee, index, access, &sources)
            });
            if let Some(convention) = signature.conventions.get(index)
                && convention.binds_mutable()
                && let Some(source) = sources.first()
                && signature
                    .access
                    .as_ref()
                    .and_then(|access| access.params().get(index))
                    .is_none_or(|parameter| self.catalog.slots(parameter.ty()).is_empty())
                && !preserves_explicit_list_relation
            {
                let convention = if *convention == Convention::Var { "`var`" } else { "`own`" };
                return Err(terr(format!(
                    "argument {} passed to `{}` carries a {} reference to `{}` but the {convention} \
                     parameter has no matching reference relation; copy the referent with `.owned()` \
                     before the call or declare the matching reference parameter",
                    index + 1,
                    short_name(callee),
                    match source.kind {
                        BorrowKind::Shared => "shared",
                        BorrowKind::Exclusive => "exclusive",
                    },
                    source.owner,
                )));
            }
            if let Some(access) = signature.access.as_ref()
                && let Some(parameter) = access.params().get(index)
                && let Some(required) = direct_reference_kind(parameter.ty())
            {
                let supplied = sources.iter().map(|source| source.kind).max_by_key(|kind| {
                    matches!(kind, BorrowKind::Exclusive)
                });
                let compatible = matches!(
                    (required, supplied),
                    (BorrowKind::Shared, Some(BorrowKind::Shared | BorrowKind::Exclusive))
                        | (BorrowKind::Exclusive, Some(BorrowKind::Exclusive))
                );
                if !compatible {
                    let required = match required {
                        BorrowKind::Shared => "a shared reference (`&place`)",
                        BorrowKind::Exclusive => "an exclusive reference (`&mut place`)",
                    };
                    return Err(terr(format!(
                        "argument {} passed to `{}` must be {required}; ordinary values do not \
                         implicitly become references",
                        index + 1,
                        short_name(callee),
                    )));
                }
            }
            if sources.is_empty() {
                continue;
            }
            let Some(access) = signature.access.as_ref() else { continue };
            let Some(parameter) = access.params().get(index) else { continue };
            if (!type_has_generic_leaf(parameter.ty())
                && !sources.iter().any(source_is_direct_reference))
                || authenticated_non_escaping_generic_read(callee, index, access)
                || preserves_explicit_list_relation
                || authenticated_generic_materializer(callee, index, access, &sources)
            {
                continue;
            }
            let declared_slots = self.catalog.slots(parameter.ty());
            if let Some(source) = sources.iter().find(|source| {
                !declared_slots
                    .iter()
                    .any(|slot| slot.projection == source.borrower_projection)
            }) {
                return Err(terr(format!(
                    "argument {} passed to `{}` carries a borrowed owner relation from `{}` \
                     at projection `{}`, but the parameter type erases that relation; declare \
                     the matching lifetime-bearing fixed shell/view parameter, or materialize \
                     an owned value before the call",
                    index + 1,
                    short_name(callee),
                    source.owner,
                    projection_display(&source.borrower_projection),
                )));
            }
        }
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
        for loan in open.iter().filter(|loan| loan.kind == BorrowKind::Exclusive) {
            let mut uses = 0;
            walk_stmt_exprs(stmt, &mut |expr| {
                if matches!(expr, Expr::Var(name) if name == &loan.view) {
                    uses += 1;
                }
            });
            if uses > 1 {
                return Err(terr(format!(
                    "in `{}`: exclusive reference `{}` is used more than once in one expression; \
                     an exclusive reference is affine and cannot be copied",
                    short_name(self.fn_name),
                    loan.view,
                )));
            }
        }

        // The parser represents `*reference = value` as a private call so it
        // survives all existing expression walkers. Its first operand must
        // still name a live exclusive relation: a shared view (or an ordinary
        // value after an erased reference contract) must never become writable
        // merely because the surface dereference was desugared.
        let mut write_error = None;
        // Nested control-flow blocks are checked separately with their own
        // inherited/local loan set. Inspect only a statement-level write here;
        // walking into a nested body would validate its local child against the
        // enclosing block and lose a valid mutable reborrow.
        if let Stmt::Expr(Expr::Call { name, args }) = stmt
            && name == intrinsics::REFERENCE_WRITE
        {
            let Some(reference) = args.first() else {
                return Err(terr("internal: malformed reference write".into()));
            };
            let mut sources = self.borrow_sources(reference, callables, open);
            self.collect_alias_sources(reference, open, &mut sources);
            match sources.iter().find(|source| source.kind == BorrowKind::Exclusive) {
                Some(_) => {}
                None if sources.is_empty() => {
                    write_error = Some(terr(
                        "cannot assign through this value: `*place = value` requires a live `&mut` reference".into(),
                    ));
                }
                None => {
                    write_error = Some(terr(
                        "cannot assign through a shared reference; create an exclusive `&mut place` instead".into(),
                    ));
                }
            }
        }
        if let Some(error) = write_error {
            return Err(error);
        }
        if let Some(source) = self.escape_call_source(stmt, callables, open) {
            return Err(terr(format!(
                "in `{}`: the borrowed result from `{}` escapes through a task or channel — \
                 materialize it with `.owned()` before sending or spawning it",
                short_name(self.fn_name),
                short_name(&source.origin),
            )));
        }
        if let Some(source) = self.input_reference_dynamic_source(stmt) {
            return Err(terr(format!(
                "in `{}`: reference parameter from `{}` cannot be stored in Dynamic — \
                 materialize it with `.owned()` before calling `dynamic.dynamic`",
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

    fn input_reference_dynamic_source(&self, stmt: &Stmt) -> Option<&BorrowSource> {
        let mut input = None;
        walk_stmt_exprs(stmt, &mut |expr| {
            if input.is_some() {
                return;
            }
            let Expr::Call { name, args } = expr else { return };
            if name != "dynamic.dynamic" && !name.starts_with("dynamic.dynamic__") {
                return;
            }
            input = args.iter().find_map(|argument| match argument {
                Expr::Var(name) => self
                    .input_borrows
                    .get(name)
                    .and_then(|sources| sources.first()),
                _ => None,
            });
        });
        input
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
            if authenticated_borrow_escape_boundary(name).is_none() {
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
                let mut sources = self.borrow_sources(expr, callables, open);
                self.collect_alias_sources(expr, open, &mut sources);
                if let Some(source) = sources
                    .iter()
                    .find(|source| source.kind == BorrowKind::Shared)
                {
                    result = Err(terr(format!(
                        "in `{}`: shared reference from `{}` to `{}` cannot be consumed with `move`; \
                         shared references are copyable handles, so pass it directly or materialize \
                         the referent with `.owned()`",
                        short_name(self.fn_name),
                        short_name(&source.origin),
                        source.owner,
                    )));
                    return;
                }
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
        // Iterating a borrowed list exposes one of its element handles on each
        // trip through the body.  The list binding carries every owner relation
        // (with an index/wildcard borrower projection); rebind those same facts
        // to the loop variable so a dereference in the body remains an actual
        // reference operation rather than an untyped ordinary value.
        if let Stmt::Expr(Expr::For { var, iter, body }) = stmt {
            let mut sources = self.borrow_sources(iter, callables, open);
            self.collect_alias_sources(iter, open, &mut sources);
            if !sources.is_empty() {
                let mut inherited = open.to_vec();
                for source in sources {
                    inherited.push(Loan {
                        view: var.clone(),
                        owner: source.owner,
                        root_type: source.root_type,
                        projection: source.projection,
                        borrower_projection: LoanProjection::default(),
                        origin: source.origin,
                        kind: source.kind,
                        owner_type: source.owner_type,
                    });
                }
                return self.check_block_with(body, open, callables, false, &inherited);
            }
        }
        let mut nested: Vec<&Block> = Vec::new();
        collect_nested_blocks_in_stmt(stmt, &mut nested);
        for b in nested {
            self.check_block_with(b, open, callables, false, &[])?;
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
            "in `{}`: owner `{}` is {what} while the borrowed view `{}` (from `{}`){} is still \
             live — a view keeps its owner borrowed until its last use. End the view's use \
             first, or materialize it with `{}.owned()` before touching `{}`",
            short_name(self.fn_name),
            loan.owner,
            loan.view,
            short_name(&loan.origin),
            aggregate_locus(loan),
            loan.view,
            loan.owner,
        ))
    }

    fn exclusive_overlap(&self, loan: &Loan) -> TypeError {
        terr(format!(
            "in `{}`: cannot create exclusive reference `{}` to `{}` at `{}` while overlapping \
             access is live — `&mut` requires sole access to its referent until its final use",
            short_name(self.fn_name),
            loan.view,
            loan.owner,
            projection_display(&loan.projection),
        ))
    }

    fn escape(&self, loan: &Loan) -> TypeError {
        terr(format!(
            "in `{}`: the borrowed view `{}` (from `{}`){} escapes through a closure, task, or \
             channel, or is stored in an owned aggregate/mutable binding, while it still \
             borrows `{}` — a view cannot outlive its owner. \
             Materialize it with `{}.owned()` first to send an owned value",
            short_name(self.fn_name),
            loan.view,
            short_name(&loan.origin),
            aggregate_locus(loan),
            loan.owner,
            loan.view,
        ))
    }
}

impl From<Loan> for LoanEvent {
    fn from(loan: Loan) -> Self {
        let owner_root = LoanOwnerRoot {
            local: loan.owner.clone(),
            direct_storage_type: loan.root_type.clone(),
        };
        Self {
            view: loan.view,
            owner: loan.owner,
            projection: loan.projection,
            borrower_projection: loan.borrower_projection,
            origin: loan.origin,
            kind: loan.kind,
            owner_type: loan.owner_type,
            owner_root,
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
    // Field assignment is represented as assignment of a RecordUpdate back to
    // the same local. The type checker has already authenticated which fields
    // may change; at the loan layer this shape transports the shell's existing
    // roots instead of storing the shell elsewhere.
    let self_shell_update = matches!(
        stmt,
        Stmt::Assign {
            name,
            value: Expr::RecordUpdate { base, .. },
        } if name == view && matches!(base.as_ref(), Expr::Var(base) if base == view)
    );
    match stmt {
        Stmt::Assign { value, .. }
            if !self_shell_update
                && expr_result_is_var(value, view)
                && !expr_materializes_view(value, view) =>
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
            // Sent through the authenticated std channel/task boundary.
            Expr::Call { name, args } => {
                if authenticated_borrow_escape_boundary(name).is_some()
                    && args.iter().any(|a| expr_root(a) == Some(view))
                {
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
                if (!self_shell_update && expr_result_is_var(base, view))
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
    let is_owned = |name: &str| {
        let short = short_name(name);
        short == "owned"
            || name.ends_with("__owned")
            || parse_generic_materializer_name(short).is_some()
    };
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
        Expr::LabeledMethodCall { receiver, args, .. } => {
            walk_expr(receiver, f);
            for (_, argument) in args {
                walk_expr(argument, f);
            }
        }
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

pub(crate) fn walk_block<'a>(b: &'a Block, f: &mut impl FnMut(&'a Expr)) {
    for s in &b.stmts {
        walk_stmt_exprs(s, f);
    }
}

#[cfg(test)]
#[path = "loans_tests.rs"]
mod tests;
