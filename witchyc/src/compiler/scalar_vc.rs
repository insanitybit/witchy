//! Proof obligations for scalar code, read from the typed body the compiler emits.
//!
//! Every function is walked once. Unsigned, I64 and Bool values are followed exactly through
//! bindings, assignments, branches, loops, matches and calls, whether they are locals or fields
//! of a record (a local's own or one behind a reference); anything else is an unknown value of
//! its type. A call brings in what the callee's contract says about numbers: its own clauses if
//! it is a scalar contract, the comparisons of lengths, capacities and codes if it is a memory
//! or record contract. Each obligation is a sequent: the facts known on the way to it, and what must
//! hold there. A function with a scalar contract must show its arithmetic cannot trap, its loop
//! invariants, and its `ensures` at every return. Any function must show the `requires` of each
//! scalar contract it calls.
//!
//! An unsigned number is a natural number in a statement and an I64 is an integer; the two never
//! meet in one comparison. I64 `+ - *` are exact, `/` and `%` give an unknown value in range, and
//! the bits of an I64 are unknown.
//!
//! What is assumed, never proved here: a callee's `ensures` after a call, an arithmetic result
//! being in range after the operation (it traps otherwise), and a loop invariant at the loop head.
mod facts;
mod arithmetic;
mod boxes;
mod expressions;
mod calls;
mod claims;
mod elements;
mod stores;
mod flow;
mod render;
mod scan;

use super::{contract::{Math, Prop, RESULT}, logical_body::*, logical_place::{Place, Projection}, proof::{Goal, SourceIdentity}, semantic_types::Types};
use crate::diagnostic::Diagnostic;
use std::collections::{BTreeMap, BTreeSet};

mod view;
mod member;

type Result<T> = std::result::Result<T, Diagnostic>;
fn error(message: &'static str) -> Diagnostic { Diagnostic::new("W0401", 0..0, message) }
const NODES: usize = 65_536;

/// `Product`: a bound on the product of two unsigned values, `a * b <= A * B` from `a <= A` and
/// `b <= B`, which `omega` cannot show (it is not linear). It is a goal of its own, proved by
/// `Nat.mul_le_mul`, and then a fact for what follows.
/// `SignedProduct`: the same for two I64 values, `|a * b| <= X * Y` from `|a| <= X` and `|b| <= Y`,
/// shown through `Int.natAbs_mul`; its prover names the two factors, so it keeps them.
/// `Quotient`: `m * L / y <= L` from `m <= y` (a part scaled over its whole: `part * 100 / whole`),
/// which `omega` cannot show (the divisor is a variable); proved by `Nat.div_le_of_le_mul`, whose
/// premise `m * L <= y * L` is linear, then a fact.
/// `Lemma`: a fact of a quotient or remainder by a variable (`a / b <= a`, `a % b < b`,
/// `a % b <= a`), proved by the core lemma named in its prover, then a fact.
enum Kind { Body, Call { site: u32, callee: String }, Product, Quotient, Lemma(&'static str), SignedProduct { a: Math, b: Math, x: u64, y: u64 } }
struct Obligation { hypotheses: Vec<Prop>, goal: Prop, kind: Kind,
    /// What run-time check or claim this is, for a refusal that names the part that failed.
    what: &'static str,
    /// The local whose new value it arose in, when it arose in an assignment.
    near: Option<u32>,
    /// For one `requires` clause of a callee alone in diagnostics: that clause's span.
    clause: Option<(usize, usize)>,
    /// In diagnostics only: the `return` it is shown at, and the loops around or before it
    /// (`Diagnosis::note`). Not part of the goal.
    note: String,
    /// Where the statement it arose in was written, when that is known (an `unreachable`, an
    /// owner assignment, a `memory.vacant`): a refusal points there. Not part of the goal.
    site: Option<(usize, usize)>,
    /// Names in the conditions on the way here. Facts about them are kept even when the goal does not
    /// name them: they may show this point is never reached.
    seeds: Vec<u32> }
#[derive(PartialEq)]
enum Flow { Falls, Ends }

/// `Some(Some(max))` for an unsigned type, `Some(None)` for Bool, `None` for everything else.
fn scalar(ty: Ty) -> Option<Option<u64>> {
    match ty { Ty::Unsigned(s) => Some(Some(s.layout().maximum())), Ty::Bool => Some(None), _ => None }
}
/// A place whose value the walk follows: an unsigned number, a Bool or an I64.
fn tracked(ty: Ty) -> bool { scalar(ty).is_some() || ty == Ty::I64 }
fn implies(a: &Prop, b: Prop) -> Prop { Prop::Binary("→", Box::new(a.clone()), Box::new(b)) }
fn not(p: &Prop) -> Prop { Prop::Not(Box::new(p.clone())) }
fn equal(a: Math, b: Math) -> Prop { Prop::Compare("=", a, b) }
fn all(mut parts: Vec<Prop>) -> Prop {
    if parts.is_empty() { return Prop::Bool(true); }
    let mut result = parts.remove(0);
    for part in parts { result = Prop::Binary("∧", Box::new(result), Box::new(part)); }
    result
}
/// A scalar place: a local and the fields that lead to the value. A reference local stands for what it points at.
type Key = (u32, Vec<u32>);
/// The field path of a place, unless it indexes an array.
/// A path step for an array element at a literal index: above every field ordinal.
pub(super) const ELEMENT: u32 = 1 << 31;
/// A step into the value a returned sum's variant holds: `ARM | k` for variant `k` (a contract's
/// `match result: Ok(made): made.low <= made.high`). Below `OBSERVED_FLOOR`.
pub(super) const ARM: u32 = 1 << 29;
/// The fields of a place, an element at a literal index being a step too (`xs[0]`); `None`
/// for one at a computed index, which a proof does not follow.
fn fields(place: &Place) -> Option<Vec<u32>> {
    place.projections.iter().map(|p| match p {
        Projection::Field { ordinal } => Some(*ordinal),
        Projection::Index { value, .. } => match value.kind { Value::U64(k) if k < ELEMENT as u64 => Some(ELEMENT | k as u32), _ => None },
    }).collect()
}
/// The fields of a place up to its first index: everything a store through it may change.
fn prefix(place: &Place) -> Vec<u32> {
    place.projections.iter().map_while(|p| match p { Projection::Field { ordinal } => Some(*ordinal), Projection::Index { .. } => None }).collect()
}
/// Whether an argument names a place whose every step is a field.
fn fields_known(e: &Expr) -> bool {
    matches!(&e.kind, Value::Address(place) | Value::ReadPlace(place) | Value::Move { place, .. } if fields(place).is_some())
}
fn join(path: &[u32], rest: &[u32]) -> Vec<u32> { path.iter().chain(rest).copied().collect() }
/// The root under which a by-value argument built in a call is gathered: no local's, and
/// nothing is ever stored under it.
const CARRIED: u32 = RESULT - 1;
/// The scalar parts of `piece` put at `path`: a value given to the call brings what was known
/// of it (`carried`, by parameter); a variant built by the callee says which it is, as
/// `gather` says of one built here.
fn placed(piece: &super::rearrangement::Piece, path: &[u32], carried: &BTreeMap<u32, Vec<(Vec<u32>, Math)>>, writes: &mut Vec<(Vec<u32>, Math)>) {
    use super::rearrangement::Piece;
    match piece {
        Piece::Given(index) => for (rest, value) in carried.get(index).into_iter().flatten() { writes.push((join(path, rest), value.clone())); },
        Piece::Variant(arm, inner) => {
            writes.push((join(path, &[SUM_TAG]), Math::Literal(u64::from(*arm))));
            placed(inner, &join(path, &[ARM + u32::from(*arm)]), carried, writes);
        }
        Piece::Unit => {}
    }
}
// A scalar observation of the owning word. Equal addresses never imply equal
// allocations: the only refinements using this observation test nullness.
// Temporary path component in a source clause. A read-only observer is
// resolved against its typed body before any clause enters a proof.
pub(super) const VIEW_METHOD: u32 = 1 << 26;
/// Temporary path step in a source clause: a method of the value reached so far that answers a
/// number (`items.len()`). Resolved before any clause enters a proof, to the field the method's
/// one-line body returns (`projection`); a method that is not such a projection is refused.
/// Its range is past `view::FOLD`'s and below `SLICE_LENGTH`.
pub(super) const OBSERVE_METHOD: u32 = 3 * VIEW_METHOD;
/// Temporary path step in a type invariant over a container's elements: any one element, as the
/// container's `index` method (this plus its definition) reads it. Resolved before a clause
/// enters a proof to the buffer that method's body reads, then `ANY_ELEMENT`
/// (elements.rs). Its range is past `SLICE_LENGTH` and the box steps, below `ARM`.
pub(super) const ELEMENT_METHOD: u32 = 5 * VIEW_METHOD;
/// A path step for any one cell of the buffer the path so far leads to.
pub(super) const ANY_ELEMENT: u32 = (1 << 28) + 16;
pub(super) const BOX_ADDRESS: u32 = (1 << 28) + 1;
// Erased allocation incarnation. This projection has no source numeric operation.
pub(super) const BOX_ID: u32 = BOX_ADDRESS + 1;
const BOX_VALUE: u32 = BOX_ID + 1;
// Logical variant index. It is independent of the chosen physical encoding.
pub(super) const SUM_TAG: u32 = BOX_VALUE + 1;
pub(super) const MEMBER_STAMP: u32 = SUM_TAG + 1;
pub(super) const ROOT_STAMP: u32 = MEMBER_STAMP + 1;

fn member_path(path: &[u32], root: bool) -> Option<Vec<u32>> {
    let at = path.iter().position(|s| view::folded(*s))?;
    if path.last() != Some(&BOX_VALUE) || path[at + 1..].iter().any(|s| view::folded(*s)) { return None; }
    let mut key = if root { path[..at].to_vec() } else { Vec::new() };
    key.push(if root { ROOT_STAMP } else { MEMBER_STAMP });
    key.extend_from_slice(&path[at..]);
    Some(key)
}
fn box_place(e: &Expr) -> Option<&Place> {
    match &e.kind {
        Value::Address(p) | Value::ReadPlace(p) | Value::Move { place: p, .. } => Some(p),
        _ => None,
    }
}
/// A method that only returns one scalar field of its receiver: the fields that lead to it.
fn projection(functions: &[Function], function: &Function) -> Option<Vec<u32>> {
    // Follow only direct field reads and a shared observer applied to a field.
    // Every call in the chain must itself be such a projection.
    let mut function = function;
    let mut path = Vec::new();
    for _ in 0..32 {
        if function.params.len() != 1 || !tracked(function.result) { return None; }
        let [Stmt::Return(e, _, _)] = function.body.as_slice() else { return None };
        match &e.kind {
            Value::ReadPlace(place) if place.root == 0 => {
                path.extend(fields(place)?);
                return Some(path);
            }
            Value::BoxOp(super::operations::BoxOp::Address | super::operations::BoxOp::WeakBits, _, args) => {
                let place = box_place(args.first()?)?;
                if place.root != 0 { return None; }
                path.extend(fields(place)?);
                path.push(BOX_ADDRESS);
                return Some(path);
            }
            Value::Call(call) => {
                let [arg] = call.args.as_slice() else { return None };
                let Value::Address(place) = &arg.kind else { return None };
                if place.root != 0 || !matches!(arg.ty, Ty::Borrow { mutable: false, .. }) { return None; }
                path.extend(fields(place)?);
                function = functions.get(call.definition.0 as usize)?;
            }
            _ => return None,
        }
    }
    None
}
/// A pure trait method of one receiver and a number (`pure fn len(self: &Self) -> U64`): it has no
/// body here, so it is read as a value of its receiver, under a path no field has. A call on an
/// unchanged receiver gives the same value (implementations are pure); a change forgets it.
fn observed(function: &Function) -> Option<Vec<u32>> {
    (function.required && function.pure && function.params.len() == 1 && tracked(function.result)).then(|| vec![OBSERVED_FLOOR + function.definition.0])
}
/// The paths `observed` uses are this plus the method's definition, below `ELEMENT` (literal array
/// indexes): no record has this many fields and no program this many definitions, so the range
/// is the observations' alone at any size (review: `u32::MAX - 2 - definition` left it past 4096
/// definitions).
const OBSERVED_FLOOR: u32 = 1 << 30;
/// Whether a path step is an observation's.
fn observation(step: u32) -> bool { (OBSERVED_FLOOR..ELEMENT).contains(&step) }
fn depends_on(observed: &[u32], write: &[u32]) -> bool {
    observed.iter().position(|step| observation(*step) || view::folded(*step) || *step == ROOT_STAMP)
        .is_some_and(|at| write.starts_with(&observed[..at]))
}
/// Interpret the shared method body using the same rules for used and unused specifications.
pub(super) fn view_projection(functions: &[Function], id: u32) -> Result<Vec<u32>> {
    let observer = &functions[id as usize];
    if let Some(path) = view::direct(observer) {
        return Ok(path);
    }
    if view::cases(functions, id).is_some() {
        return Ok(vec![view::FOLD + id]);
    }
    let [Stmt::Return(e, _, _)] = observer.body.as_slice() else {
        return Err(error("a proof view must return a fixed shared projection"));
    };
    let Value::BoxOp(super::operations::BoxOp::FromBuffer, _, args) = &e.kind else {
        return Err(error("a proof view must return a checked cell loan or buffer pointer"));
    };
    let [receiver] = args.as_slice() else {
        return Err(error("a proof view must project its shared receiver"));
    };
    let source = box_place(receiver)
        .filter(|p| p.root == 0 && p.projections.iter().all(|p| matches!(p, Projection::Field { .. })))
        .ok_or_else(|| error("a proof view must project its own shared receiver"))?;
    Ok(prefix(source))
}
/// What a clause reads besides a parameter's own value: the local, the fields that lead to the
/// number, its type, and whether it is read as it was on entry. A method call is the field its
/// one-line body returns.
fn read(functions: &[Function], m: &Math) -> Result<Option<(u32, Vec<u32>, Ty, bool)>> {
    Ok(match m {
        Math::Observe { root, function, before } => {
            let observer = &functions[function.0 as usize];
            let path = projection(functions, observer).or_else(|| observed(observer)).ok_or_else(|| Diagnostic::new("W0401", 0..0, format!("function contracts clause may call only a method that returns a field of its receiver: `{}` is not one, since its body is not a single `return` of a field of its one `&` parameter (as `Vec.len`'s `return (*value).length`)", observer.name)))?;
            Some((*root, path, observer.result, *before))
        }
        Math::Field { root, path, ty, before } => {
            let mut projected = Vec::new();
            for step in path {
                // A method answering a number, read as the field its one-line body returns.
                if (OBSERVE_METHOD..OBSERVE_METHOD + VIEW_METHOD).contains(step) {
                    let observer = functions.get((step - OBSERVE_METHOD) as usize).ok_or_else(|| error("function contracts clause may call only a method that returns a field of its receiver"))?;
                    projected.extend(projection(functions, observer).ok_or_else(|| Diagnostic::new("W0401", 0..0, format!("function contracts clause may call only a method that returns a field of its receiver: `{}` is not one, since its body is not a single `return` of a field of its one `&` parameter (as `Vec.len`'s `return (*value).length`)", observer.name)))?);
                    continue;
                }
                // Any one element, as a container's `index` method reads it: a cell of the
                // buffer that method's body reads (elements.rs).
                if (ELEMENT_METHOD..ELEMENT_METHOD + VIEW_METHOD).contains(step) {
                    let (cells, _) = elements::storage(functions, step - ELEMENT_METHOD).ok_or_else(|| Diagnostic::new("W0401", 0..0, "a type invariant over elements reads them through an `index` method of the form `return c.view()[i]`, the view `return memory.slice_unchecked(&c.buffer, 0, c.length)`"))?;
                    projected.extend(cells);
                    projected.push(ANY_ELEMENT);
                    continue;
                }
                if !(VIEW_METHOD..2*VIEW_METHOD).contains(step) { projected.push(*step); continue; }
                projected.extend(view_projection(functions,step-VIEW_METHOD)?);
            }
            let normalized = if projected.last() == Some(&ROOT_STAMP) {
                member_path(&projected[..projected.len()-1], true)
            } else if let Some(at) = projected.iter().position(|s| *s == MEMBER_STAMP) {
                member_path(&projected[at+1..], false).map(|member| join(&projected[..at], &member))
            } else { Some(projected) };
            Some((*root, normalized.ok_or_else(|| error("membership needs a structurally selected initialized cell"))?, *ty, *before))
        }
        _ => None,
    })
}
/// An argument as the callee's contract sees it.
#[derive(Clone)]
enum Argument { Scalar(Math), Place { root: u32, path: Vec<u32>, changed: bool }, Other }
/// What a call says about a result that is not one number, kept until the result is bound or matched.
#[derive(Clone, Default)]
struct Outcome {
    /// Scalar parts of the result, by variant (`None`: the result is not a `Result`) and field path.
    values: BTreeMap<(Option<u8>, Vec<u32>), Math>,
    /// What holds when the result is this variant.
    facts: Vec<(u8, Prop)>,
    /// What the call says of elements (elements.rs).
    elements: elements::Elements,
}
/// What is known of the variant a `return` hands back.
enum Returned { Unknown, Variant(u8), Outcome(Outcome) }
fn scalar_contract(function: &Function) -> Option<&super::contract::Contract> {
    function.contract.as_ref().filter(|c| c.storage.is_none() && c.record.is_none() && c.memory.is_none())
}

/// A loop as a refusal names it: where it is written, whether it has invariants, and the
/// names of the locals it assigns (what is unknown on each pass and after it without one).
#[derive(Clone)]
struct LoopNote { at: (usize, usize), invariants: bool, names: Vec<String> }
/// What diagnostics record of where an obligation arose, beyond its own site; made only when
/// the parts of a refused body are checked one at a time (`split_diagnostics`).
#[derive(Default)]
struct Diagnosis {
    /// The `return` being walked, where it is written.
    returning: Option<(usize, usize)>,
    /// The loops being walked, innermost last.
    enclosing: Vec<LoopNote>,
    /// The loops walked before this point on the way here, most recent last.
    passed: Vec<LoopNote>,
}
impl Diagnosis {
    /// The obligation's note: `r=START,END` for its `return`, then one `l=INSIDE,INVARIANTS,START,END,NAMES` a loop (innermost or most recent first).
    fn note(&self) -> String {
        let mut out = Vec::new();
        if let Some((start, end)) = self.returning { out.push(format!("r={start},{end}")); }
        for (inside, note) in self.enclosing.iter().rev().map(|n| (true, n)).chain(self.passed.iter().rev().map(|n| (false, n))) {
            out.push(format!("l={},{},{},{},{}", u8::from(inside), u8::from(note.invariants), note.at.0, note.at.1, note.names.join(";")));
        }
        out.join("\u{2}")
    }
}
/// A body part's label in diagnostics: what it checks, where it was written, and its note. The
/// fields are joined by `\u{1}` (`part_label`, read back by `PartLabel::read`).
pub(super) fn part_label(what: &str, site: Option<(usize, usize)>, note: &str) -> String {
    let (start, end) = site.map_or((String::new(), String::new()), |(s, e)| (s.to_string(), e.to_string()));
    format!("{what}\u{1}{start}\u{1}{end}\u{1}{note}")
}
/// A loop named in a part's note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NotedLoop { pub inside: bool, pub invariants: bool, pub at: std::ops::Range<usize>, pub names: Vec<String> }
/// A body part's label, read back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PartLabel { pub what: String, pub site: Option<std::ops::Range<usize>>, pub returned: Option<std::ops::Range<usize>>, pub loops: Vec<NotedLoop> }
impl PartLabel {
    pub(super) fn read(label: &str) -> Self {
        let mut fields = label.split('\u{1}');
        let what = fields.next().unwrap_or_default().to_owned();
        let number = |field: Option<&str>| field.and_then(|n| n.parse::<usize>().ok());
        let site = match (number(fields.next()), number(fields.next())) { (Some(start), Some(end)) => Some(start..end), _ => None };
        let (mut returned, mut loops) = (None, Vec::new());
        for item in fields.next().unwrap_or_default().split('\u{2}') {
            if let Some(rest) = item.strip_prefix("r=") {
                let mut n = rest.split(',').map(|n| n.parse::<usize>().ok());
                if let (Some(Some(start)), Some(Some(end))) = (n.next(), n.next()) { returned = Some(start..end); }
            } else if let Some(rest) = item.strip_prefix("l=") {
                let parts: Vec<&str> = rest.splitn(5, ',').collect();
                if let [inside, invariants, start, end, names] = parts.as_slice() && let (Ok(start), Ok(end)) = (start.parse::<usize>(), end.parse::<usize>()) {
                    loops.push(NotedLoop { inside: *inside == "1", invariants: *invariants == "1", at: start..end,
                        names: names.split(';').filter(|n| !n.is_empty()).map(str::to_owned).collect() });
                }
            }
        }
        Self { what, site, returned, loops }
    }
}
struct Context<'a> {
    split_diagnostics: bool,
    /// Kept only when `split_diagnostics`: where obligations arise, for a refusal's place.
    diagnosis: Option<Diagnosis>,
    /// Set for the obligation being made: what it checks (see `Obligation::what`).
    what: &'static str,
    /// The local the statement being walked assigns, if it assigns one (`Obligation::near`).
    near: Option<u32>,
    functions: &'a [Function],
    types: &'a Types,
    function: &'a Function,
    /// A function with a scalar contract shows that its arithmetic cannot trap.
    proving: bool,
    /// The unsigned `+ - *` nodes (and compound assignments) whose range this proof shows, by address.
    covered: BTreeSet<usize>,
    checked_traps: BTreeSet<usize>,
    /// An `ensures` reads a field of the result.
    parts: bool,
    /// The invariants of each loop being walked, innermost last: `break` and `continue` show them.
    loops: Vec<Vec<Prop>>,
    /// The current value of each scalar place read or written so far. A Bool is 1 when true.
    state: BTreeMap<Key, Math>,
    /// The value on entry of each field a clause reads through a method.
    entry: BTreeMap<Key, Math>,
    /// Results of calls held in locals and not yet matched.
    pending: BTreeMap<u32, Outcome>,
    /// Locals lent with `&mut` other than straight to a call: a store through the reference may
    /// change them at any later point, so they are never followed again.
    lent: BTreeSet<u32>,
    /// For each local in `lent`, what of it was lent: the fields down to the lent place, or to
    /// its first index (`prefix`); empty for the whole local. A reference to a place reaches
    /// that place's storage and what it owns, never a sibling field, so a scalar part that is
    /// not under, over or computed from a lent path is still followed (`unfollowed`). Every
    /// other question asked of a lent local is asked of `lent`, the whole local, as before.
    lent_under: BTreeMap<u32, Vec<Vec<u32>>>,
    /// The names that stand for an I64: integers in a statement, where every other name is a natural number.
    signed: BTreeSet<u32>,
    hypotheses: Vec<Prop>,
    /// Which hypotheses are conditions for being at this point: indexes into `hypotheses`.
    guards: Vec<usize>,
    member_goals: Vec<Goal>,
    obligations: Vec<Obligation>,
    /// Invariants over the elements of what this function receives (elements.rs).
    watched: Vec<elements::Watched>,
    /// The buffers this function's `elements.` clauses speak of: a `&mut` parameter and the
    /// fields to the buffer. `tags.frames` has what the body has done to each.
    declared: Vec<(u32, Vec<u32>)>,
    tags: elements::Tags,
    /// A call is being walked, which says itself what it does to elements.
    element_guard: bool,
    /// The `&mut` being kept is to one cell of a followed buffer.
    excused: bool,
    /// The first change to elements that was not followed: refused when the walk ends.
    element_refusal: Option<Diagnostic>,
    /// Parameters written since entry.
    dirty: BTreeSet<u32>,
    stamps: u64,
    next: u32,
    nodes: usize,
    bytes: usize,
}
/// Locals a loop body may assign, and whether it leaves by `break` or `continue`.
fn invariant_observes(statements: &[Stmt], out: &mut Vec<Math>) {
    for statement in statements {
        match statement {
            Stmt::If(_, yes, no) => { invariant_observes(yes, out); invariant_observes(no, out); }
            Stmt::While(_, body, invariants) => { for clause in &invariants.scalar { clause.observes(out); } invariant_observes(body, out); }
            Stmt::Match { arms, .. } => for arm in arms { invariant_observes(&arm.body, out); },
            _ => {}
        }
    }
}
// `assumption` at reducible transparency first: at default transparency it unfolds literal
// arithmetic (`p0 * 3877 <= 18446744073709551615` against `p0 < 139968`) until Lean's recursion
// limit, an error `first` does not catch, so `omega`, which proves such goals, never ran (fasta's
// generator under `requires *seed < 139968`). The full `assumption` stays, after `omega`.
/// What a body obligation checks when no site says more.
const BODY: &str = "a condition of the body";
/// What a refused owner assignment over a field, and a refused `memory.vacant`, are called.
pub(super) const OVERWRITE: &str = "an owner assigned over a value that may still hold one";
pub(super) const DISCARD: &str = "a value discarded that may still hold an owner";
/// What a goal checks when it is a clause shown at a return: an `ensures`, or a type's invariant.
pub(super) const ENSURES: &str = "an `ensures` clause where the function returns";
/// What an `unreachable` that is not shown never to be reached is called.
pub(super) const UNREACHED: &str = "an `unreachable` that some path reaches";
const CLOSE: &str = "(intros; first | with_reducible assumption | witchy_inline_omega | assumption | simp_all)";
/// Goals for one function: its body obligations as one conjunction, then one goal per contracted call.
/// Its goals, and whether they show every unsigned `+ - *` of its body in range.
/// The low bit of a node's address marks an array index site (an `Expr` is aligned, so the bit
/// is otherwise 0): those are counted apart from arithmetic.
const BOX_NODE: usize = 3;
const INDEX_NODE: usize = 1;
/// In the set `prepare` returns, a definition whose every array index is proved below its length.
pub(super) const INDEX_PROVED: u32 = 1 << 31;
/// In the proved set: every slice element this function reads (`s[i]`) is shown below the
/// slice's length, so those reads are emitted without their check.
pub(super) const SLICE_PROVED: u32 = 1 << 30;
pub(super) const UNREACHABLE_PROVED: u32 = 1 << 29;

/// A slice index site among the counted nodes (an array's is `INDEX_NODE`).
const SLICE_NODE: usize = 2;
/// The step of a slice local's length (`s.len()`), followed like a field: forgotten when the
/// local is assigned.
pub(super) const SLICE_LENGTH: u32 = 1 << 28;
fn function(functions: &[Function], types: &Types, function: &Function, split_diagnostics: bool) -> Result<(Vec<Goal>, bool, bool, bool, bool)> {
    let contract = scalar_contract(function);
    let mut parts = false;
    if let Some(contract) = contract {
        for clause in &contract.ensures { clause.value.rewrite(&mut |m| { parts |= matches!(m, Math::Field { root: RESULT, .. }); Ok(m.clone()) })?; }
    }
    let mut context = Context { split_diagnostics, diagnosis: split_diagnostics.then(Diagnosis::default), what: BODY, near: None, functions, types, function, proving: contract.is_some(), covered: BTreeSet::new(), checked_traps: BTreeSet::new(), parts, loops: Vec::new(), state: BTreeMap::new(), entry: BTreeMap::new(), pending: BTreeMap::new(), lent: BTreeSet::new(), lent_under: BTreeMap::new(), signed: BTreeSet::new(), hypotheses: Vec::new(), guards: Vec::new(),
        watched: Vec::new(), declared: Vec::new(), tags: elements::Tags::default(), element_guard: false, excused: false, element_refusal: None, dirty: BTreeSet::new(), stamps: 0,
        member_goals: Vec::new(), obligations: Vec::new(), next: function.locals.len() as u32, nodes: contract.map_or(0, |c| c.nodes), bytes: super::MAX_SOURCE_BYTES };
    for local in function.locals.iter().take(function.params.len()) {
        if local.ty == Ty::I64 { context.state.insert((local.id, Vec::new()), Math::Binding(local.id)); context.integer(local.id); continue; }
        let Some(range) = scalar(local.ty) else { continue };
        context.state.insert((local.id, Vec::new()), Math::Binding(local.id));
        if let Some(maximum) = range { context.hypotheses.push(Prop::Compare("≤", Math::Binding(local.id), Math::Literal(maximum))); }
    }
    // Every field a clause reads through a method has a value on entry.
    let mut observed = Vec::new();
    if let Some(contract) = contract {
        for clause in contract.requires.iter().chain(&contract.ensures).chain(&contract.assumed) {
            let mut own = Vec::new();
            clause.value.observes(&mut own);
            // A method a clause may not read through is refused at the clause (a type's
            // invariant, at its own line).
            for m in &own { read(functions, m).map_err(|mut refusal| { if refusal.span == (0..0) { refusal.span = clause.span.clone(); } refusal })?; }
            observed.extend(own);
        }
    }
    invariant_observes(&function.body, &mut observed);
    for m in observed {
        let Some((root, path, ty, _)) = read(functions, &m)? else { continue };
        if root as usize >= function.params.len() { continue; }
        let value = context.leaf((root, path.clone()), ty);
        context.entry.insert((root, path), value);
    }
    if let Some(contract) = contract {
        // `assumed` (a type's invariant on what is received) is a fact here as a `requires` is,
        // and unlike one, no caller shows it.
        for clause in contract.requires.iter().chain(&contract.assumed) {
            let fact = clause.value.rewrite(&mut |m| match m { Math::Observe { root, function, .. } => context.observe(&Math::Observe { root: *root, function: *function, before: true }),
                Math::Field { root, path, ty, .. } => context.observe(&Math::Field { root: *root, path: path.clone(), ty: *ty, before: true }), other => Ok(other.clone()) })
                .map_err(|refusal| super::proof_advice::at_clause(refusal, &clause.span))?;
            context.hypotheses.push(fact);
        }
    }
    if let Some(contract) = contract { context.watch(contract)?; }
    if context.block(&function.body)? == Flow::Falls { context.returned(None, Returned::Unknown)?; }
    if let Some(refusal) = context.element_refusal.take() { return Err(refusal); }
    let identity = |kind: &'static str, callee: Option<String>, site: Option<u32>| SourceIdentity { kind, function: function.name.clone(), callee, site, storage: false, record: false, memory: false };
    let obligations = std::mem::take(&mut context.obligations);
    let mut goals = std::mem::take(&mut context.member_goals);
    let mut body = Vec::new();
    for obligation in &obligations {
        let (statement, case_split) = context.sequent(obligation)?;
        match &obligation.kind {
            Kind::Body => {
                let what = match obligation.near {
                    Some(RESULT) if obligation.what != ENSURES => format!("{}, in the value returned", obligation.what),
                    Some(id) => match function.local_names.get(&id) {
                        Some(name) => format!("{}, in what is assigned to `{name}`", obligation.what),
                        None => obligation.what.to_owned(),
                    },
                    None => obligation.what.to_owned(),
                };
                // With where it was written, for `proof::body_parts` (used for diagnostics).
                let what = if split_diagnostics { part_label(&what, obligation.site, &obligation.note) } else { what };
                let direct = match &obligation.goal {
                    Prop::Bool(true) => Some("  intros\n  exact True.intro\n"),
                    Prop::Compare("=", left, right) if left == right =>
                        Some("  intros\n  exact Eq.refl _\n"),
                    Prop::Compare("≤" | "≥", left, right) if left == right =>
                        Some(if context.is_integer(left) {
                            "  intros\n  exact Int.le_refl _\n"
                        } else {
                            "  intros\n  exact Nat.le_refl _\n"
                        }),
                    _ => None,
                };
                body.push((statement, what, direct, case_split))
            }
            Kind::SignedProduct { a, b, x, y } => {
                let (mut left, mut right) = (String::new(), String::new());
                context.math(&mut left, a, true)?; context.math(&mut right, b, true)?;
                // `intros` would make the names inaccessible: the variables, as the statement
                // binds them (`∀ (p0 : Int) (v3 : Nat) , ...`), are introduced by name first.
                let binders = statement.strip_prefix("∀ ").and_then(|rest| rest.split_once(" , ")).map(|(list, _)| list).unwrap_or("");
                let names = binders.split(") (").map(|binder| binder.trim_matches(|c| c == '(' || c == ')').split(" : ").next().unwrap_or("").to_string()).filter(|name| !name.is_empty()).collect::<Vec<_>>();
                let introduce = if names.is_empty() { String::new() } else { format!("  intro {}\n", names.join(" ")) };
                let prover = format!("{introduce}  intros\n  have witchy_product : ({left} * {right}).natAbs ≤ {x} * {y} := by\n    rw [Int.natAbs_mul]\n    apply Nat.mul_le_mul <;> witchy_inline_omega\n  witchy_inline_omega\n");
                goals.push(Goal { source_name: function.name.clone(), statement, nodes: context.nodes,
                    source: Some(identity("function_body", None, None)), prover: Some(prover) });
            }
            Kind::Product => goals.push(Goal { source_name: function.name.clone(), statement, nodes: context.nodes,
                source: Some(identity("function_body", None, None)), prover: Some("  intros\n  apply Nat.mul_le_mul <;> witchy_inline_omega\n".to_owned()) }),
            Kind::Lemma(prover) => goals.push(Goal { source_name: function.name.clone(), statement, nodes: context.nodes,
                source: Some(identity("function_body", None, None)), prover: Some((*prover).to_owned()) }),
            Kind::Quotient => goals.push(Goal { source_name: function.name.clone(), statement, nodes: context.nodes,
                source: Some(identity("function_body", None, None)), prover: Some("  intros\n  apply Nat.div_le_of_le_mul\n  witchy_inline_omega\n".to_owned()) }),
            Kind::Call { site, callee } => goals.push(Goal { source_name: function.name.clone(), statement, nodes: context.nodes,
                source: Some(match obligation.clause {
                    // One clause in diagnostics: the callee and the clause's span, for `proof::isolate`.
                    Some((start, end)) => identity("call_precondition_part", Some(format!("{callee}\u{1}{start}\u{1}{end}")), Some(*site)),
                    None => identity("call_precondition", Some(callee.clone()), Some(*site)),
                }), prover: None }),
        }
    }
    if contract.is_some() && split_diagnostics {
        // Each part alone, saying what it checks, for a refusal that names the failing ones
        // (`proof::isolate`); a build proves the conjunction below.
        for (index, (statement, what, _, _)) in body.into_iter().enumerate() {
            goals.insert(index, Goal { source_name: function.name.clone(), statement, nodes: context.nodes, source: Some(identity("function_body_part", Some(what), None)), prover: None });
        }
    } else if contract.is_some() {
        // Large conjunctions spend the kernel's per-declaration heartbeat
        // budget unfolding sibling goals. Check each sequent independently.
        if body.iter().map(|(statement, _, _, _)| statement.len()).sum::<usize>() > 16_384 {
            for (statement, _, direct, case_split) in body {
                let prover = direct.map(str::to_owned).or(case_split);
                goals.push(Goal { source_name: function.name.clone(), statement, nodes: context.nodes,
                    source: Some(identity("function_body_sequent", None, None)), prover });
            }
        } else {
            let (statement, prover) = match body.len() {
                0 => ("True".to_owned(), None),
                1 => {
                    let (statement, _, direct, _) = body.pop().unwrap();
                    (statement, direct.map(str::to_owned))
                },
                parts => (body.iter().map(|(part, _, _, _)| format!("({part})")).collect::<Vec<_>>().join(" ∧ "),
                    Some(format!("  refine ⟨{}⟩ <;> {CLOSE}\n", vec!["?_"; parts].join(", ")))),
            };
            goals.insert(0, Goal { source_name: function.name.clone(), statement, nodes: context.nodes, source: Some(identity("function_body", None, None)), prover });
        }
    }
    // Audit every qualifying site against the exact node that produced its goal.
    let coverage = scan::coverage(&function.body, &context.covered);
    if !coverage.boxes { return Err(error("box refinement requires a proof at every site")); }
    let proved = contract.is_some();
    let unreachable = contract.is_some() && !context.checked_traps.is_empty() && traps_covered(&function.body, &context.checked_traps);
    Ok((goals, proved && coverage.arithmetic, proved && coverage.indexes, proved && coverage.slices, unreachable))
}
/// Every function with a scalar contract, and every function that calls one; and the functions
/// whose goals show every unsigned `+ - *` of their bodies in range, which may be emitted
/// without checking them.
pub(super) fn prepare(program: &Program) -> Result<(Vec<Goal>, BTreeSet<u32>)> {
    prepare_goals(program, false)
}

pub(super) fn diagnostic_parts(program: &Program) -> Result<Vec<Goal>> {
    std::thread::scope(|scope| {
        super::parallel::spawn(scope, || prepare_goals(program, true).map(|(goals, _)| goals))
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

fn prepare_goals(program: &Program, split_diagnostics: bool) -> Result<(Vec<Goal>, BTreeSet<u32>)> {
    let contracted = program.functions.iter().map(|f| scalar_contract(f).is_some()).collect::<Vec<_>>();
    let mut proved = BTreeSet::new();
    if !contracted.iter().any(|c| *c) { return Ok((Vec::new(), proved)); }
    // A proof may use a callee's contract only if the callee's own proof does not lean back on it.
    let edges: Vec<Vec<usize>> = program.functions.iter().zip(&contracted).map(|(f, &has_contract)| {
        if !has_contract { return Vec::new(); }
        f.calls.iter().map(|call| call.definition.0 as usize).filter(|&callee| contracted[callee]).collect()
    }).collect();
    let components = super::call_graph::components(&edges);
    if let Some((caller, _)) = edges.iter().enumerate().find(|(caller, callees)| {
        callees.iter().any(|&callee| components[*caller] == components[callee])
    }) {
        return Err(super::proof_advice::at_function(error("function contracts require an acyclic selected contract graph"), &program.functions[caller]));
    }
    let mut goals = Vec::new();
    for f in &program.functions {
        if f.required || (!contracted[f.definition.0 as usize] && !f.calls.iter().any(|edge| contracted[edge.definition.0 as usize])) { continue; }
        // A body the record model proves has each of its calls checked there: what a numeric
        // callee requires is shown with this body's own contract in view (`record_memory.rs`,
        // `scalar_called`), and a call it cannot read is stated as never reached.
        if f.contract.as_ref().is_some_and(|c| c.record.is_some()) { continue; }
        let (more, covered, indexed, sliced, unreachable) = function(&program.functions, &program.types, f, split_diagnostics).map_err(|refusal| super::proof_advice::at_function(refusal, f))?;
        goals.extend(more);
        if covered { proved.insert(f.definition.0); }
        if indexed { proved.insert(f.definition.0 | INDEX_PROVED); }
        if sliced { proved.insert(f.definition.0 | SLICE_PROVED); }
        if unreachable { proved.insert(f.definition.0 | UNREACHABLE_PROVED); }
    }
    Ok((goals, proved))
}
